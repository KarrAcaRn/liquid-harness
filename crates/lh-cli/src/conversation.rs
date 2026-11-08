//! Conversation adaptation shared by the REPL and HTTP adapter.
use lh_core::{Engine, Session};
use lh_protocol::{
    RouteDecision, RouteInput,
    json::{Value, json},
};
use tokio::sync::mpsc::UnboundedSender;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub(crate) fn route_input(messages: &[Value], model: &str) -> RouteInput {
    let mut input = RouteInput {
        specialist: Some(model.into()),
        ..Default::default()
    };
    // Text describes the latest user intent; retained images remain relevant to follow-ups.
    for message in messages {
        let user = message["role"] == "user";
        if user {
            input.prompt.clear();
        }
        if user && let Some(text) = message["content"].as_str() {
            input.prompt.push_str(text);
        }
        if let Some(parts) = message["content"].as_array() {
            for part in parts {
                match part["type"].as_str() {
                    Some("text") if user => {
                        input.prompt.push_str(part["text"].as_str().unwrap_or(""));
                        input.prompt.push('\n');
                    }
                    Some("image_url") => {
                        if let Some(url) = part["image_url"]["url"].as_str() {
                            input.images.push(url.into());
                        }
                    }
                    Some("input_audio") => input.audio.push(format!(
                        "data:audio/{};base64,{}",
                        part["input_audio"]["format"].as_str().unwrap_or("wav"),
                        part["input_audio"]["data"].as_str().unwrap_or("")
                    )),
                    _ => {}
                }
            }
        }
    }
    input
}

pub(crate) fn routing_input(engine: &Engine, messages: &[Value], model: &str) -> RouteInput {
    let mut input = route_input(messages, model);
    if let Some(specialist) = engine.registry.specialists.get(model) {
        // Explicit switches may omit incompatible OLD media, but never discard a
        // new unsupported attachment just to make a forced model accept the turn.
        let latest = messages.iter().rfind(|m| m["role"] == "user").cloned();
        let current = route_input(&latest.into_iter().collect::<Vec<_>>(), model);
        if !specialist.inputs.iter().any(|s| s == "image") {
            input.images = current.images;
        }
        if !specialist.inputs.iter().any(|s| s == "audio") {
            input.audio = current.audio;
        }
    }
    // ASR precedes vision when both modalities are present. The transcript and
    // retained picture then go together to a vision-capable chat specialist.
    let asr = engine
        .registry
        .specialists
        .get(model)
        .is_some_and(|s| s.task == "transcribe")
        || (model == "auto"
            && engine
                .registry
                .specialists
                .values()
                .any(|s| s.task == "transcribe"));
    if asr && !input.audio.is_empty() {
        input.images.clear();
    }
    input
}

#[cfg(feature = "server")]
pub async fn complete(
    engine: &mut Engine,
    messages: Vec<Value>,
    model: &str,
    session: Option<&mut Session>,
    params: Value,
    deltas: Option<UnboundedSender<Value>>,
) -> Result<(RouteDecision, Value)> {
    complete_control(engine, messages, model, session, params, deltas, None).await
}

pub async fn complete_control(
    engine: &mut Engine,
    mut messages: Vec<Value>,
    model: &str,
    session: Option<&mut Session>,
    params: Value,
    deltas: Option<UnboundedSender<Value>>,
    agent_control: Option<(
        std::sync::Arc<lh_core::agent::Control>,
        UnboundedSender<Value>,
    )>,
) -> Result<(RouteDecision, Value)> {
    if messages.is_empty() {
        return Err("messages must not be empty".into());
    }
    let input = routing_input(engine, &messages, model);
    let mut decision = match session {
        Some(session) => engine.route_session(&input, None, session).await?,
        None => engine.route(&input, None).await?,
    };
    let task = engine.registry.specialists[&decision.specialist]
        .task
        .clone();
    let host = engine.load_specialist(&mut decision).await?;
    if task == "speak" {
        let speech = engine
            .dispatch(&decision, json!({"text":input.prompt}))
            .await?;
        let response = json!({"message":{"role":"assistant","content":"","audio":speech},"finish_reason":"stop","usage":{},"lh_messages":messages});
        if let Some(tx) = &deltas {
            let _ = tx.send(response["message"].clone());
        }
        return Ok((decision, response));
    }
    let mut execution = decision.clone();
    if task == "transcribe" {
        // Convert every audio part into text in its original message, preserving roles
        // and ordering. REPL stores this adapted history to avoid repeat transcription.
        for message in &mut messages {
            if let Some(parts) = message["content"].as_array_mut() {
                for part in parts {
                    if part["type"] == "input_audio" {
                        let audio = format!(
                            "data:audio/{};base64,{}",
                            part["input_audio"]["format"].as_str().unwrap_or("wav"),
                            part["input_audio"]["data"].as_str().unwrap_or("")
                        );
                        let transcript = engine.dispatch(&decision, json!({"audio":audio})).await?;
                        *part = json!({"type":"text","text":format!("Audio transcript:\n{}", transcript["text"].as_str().ok_or("transcription missing text")?)});
                    }
                }
            }
        }
        let mut followup = route_input(&messages, "auto");
        if followup.images.is_empty() {
            followup.specialist = Some(engine.config.default_specialist.clone());
        }
        execution = engine.route(&followup, None).await?;
        if engine.registry.specialists[&execution.specialist].task != "chat" {
            return Err("default specialist must support chat for audio pipeline".into());
        }
    } else if task != "chat" {
        return Err("chat requires chat, transcribe or speak specialist".into());
    }
    let specialist = &engine.registry.specialists[&execution.specialist];
    let mut adapted = messages.clone();
    for message in &mut adapted {
        if let Some(parts) = message["content"].as_array_mut() {
            parts.retain(|part| match part["type"].as_str() {
                Some("image_url") => specialist.inputs.iter().any(|s| s == "image"),
                Some("input_audio") => specialist.inputs.iter().any(|s| s == "audio"),
                _ => true,
            });
            if parts.iter().all(|p| p["type"] == "text") {
                message["content"] = json!(
                    parts
                        .iter()
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
        }
    }
    if (specialist.agent || !specialist.tools.is_empty())
        && let Some((control, events)) = agent_control
    {
        if execution.specialist != decision.specialist {
            engine.load_specialist(&mut execution).await?;
        }
        let response =
            lh_core::agent::run(engine, &execution, adapted, params, events, control).await?;
        return Ok((decision, response));
    }
    let mut sampling = specialist.params.as_object().cloned().unwrap_or_default();
    // `[self] context_in_chat`: the agent's "Harness environment" block for
    // plain chats too, merged into a leading system message.
    if engine.config.self_info.context_in_chat {
        let block = lh_core::selfinfo::context_block(&engine.self_info());
        match adapted.first_mut() {
            Some(first) if first["role"] == "system" && first["content"].is_string() => {
                let text = format!("{}\n\n{block}", first["content"].as_str().unwrap_or(""));
                first["content"] = json!(text);
            }
            _ => adapted.insert(0, json!({"role":"system","content":block})),
        }
    }
    let mut tools = None;
    if let Some(params) = params.as_object() {
        for (key, value) in params {
            if key == "tools" {
                tools = Some(value.clone());
            } else {
                sampling.insert(key.clone(), value.clone());
            }
        }
    }
    // All callers serialize entire calls, so notifications belong to this request.
    let chat_host = if execution.specialist == decision.specialist {
        host
    } else {
        engine.load_specialist(&mut execution).await?
    };
    let mut notifications = chat_host.subscribe();
    let started = std::time::Instant::now();
    let ms = |at: std::time::Instant| at.duration_since(started).as_secs_f64() * 1000.0;
    let mut timing = lh_protocol::stats::CallTiming::default();
    let call = engine.dispatch(
        &execution,
        json!({"messages":adapted,"tools":tools,"params":sampling,"stream":deltas.is_some()}),
    );
    tokio::pin!(call);
    let mut emitted = false;
    let mut response = loop {
        tokio::select! {
            biased;
            notification = notifications.recv(), if deltas.is_some() => {
                let notification = notification.map_err(|e| format!("lost chat stream: {e}"))?;
                if notification.method == "$/stream" {
                    emitted = true;
                    let now = Some(ms(std::time::Instant::now()));
                    timing.first_delta_ms = timing.first_delta_ms.or(now);
                    timing.last_delta_ms = now;
                    if let Some(tx) = &deltas { let _ = tx.send(notification.params["delta"].clone()); }
                }
            }
            result = &mut call => break result?,
        }
    };
    timing.elapsed_ms = Some(ms(std::time::Instant::now()));
    response["lh_stats"] = lh_protocol::stats::generation_stats(&response, timing);
    if !emitted && let Some(tx) = &deltas {
        let _ = tx.send(response["message"].clone());
    }
    response["lh_messages"] = json!(messages);
    Ok((decision, response))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn route_uses_latest_intent_and_retained_media() {
        let messages = vec![
            json!({"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AA=="}},{"type":"text","text":"old"}]}),
            json!({"role":"assistant","content":"old answer"}),
            json!({"role":"user","content":"followup"}),
        ];
        let input = route_input(&messages, "auto");
        assert_eq!(input.prompt, "followup");
        assert_eq!(input.images.len(), 1);
    }
}
