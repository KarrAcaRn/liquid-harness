use base64::{Engine as _, engine::general_purpose::STANDARD};
use lh_core::Engine;
use lh_protocol::{
    RouteInput,
    json::{Value, json},
};
use std::{
    io::{self, Write},
    path::Path,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn image_url(reference: &str) -> Result<String> {
    if reference.starts_with("data:image/")
        || reference.starts_with("https://")
        || reference.starts_with("http://")
    {
        return Ok(reference.into());
    }
    let path = Path::new(reference.strip_prefix("file:").unwrap_or(reference));
    let mime = match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => return Err(format!("unsupported image extension: {}", path.display()).into()),
    };
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
}

pub async fn pull(engine: &mut Engine, id: &str) -> Result<Value> {
    let (backend, key, model, pinned) = if let Some(s) = engine.registry.specialists.get(id) {
        (s.backend.clone(), s.id.clone(), json!(s.model), false)
    } else if let Some(r) = engine.registry.routers.get(id) {
        (
            r.backend.clone().ok_or("router has no backend")?,
            format!("router:{}", r.id),
            json!(r.model.as_ref().ok_or("router has no model")?),
            true,
        )
    } else {
        return Err(format!("unknown specialist or router '{id}'").into());
    };
    let host = engine.host(&backend)?;
    let result = host
        .call(
            "backend/load",
            json!({"key":key,"model":model,"pinned":pinned}),
        )
        .await?;
    if result["ready"] != true {
        return Err("backend/load did not report ready".into());
    }
    Ok(result)
}

pub async fn run(
    engine: &mut Engine,
    model: String,
    images: Vec<String>,
    router: Option<&str>,
    prompt: String,
) -> Result<()> {
    let images = images
        .iter()
        .map(|s| image_url(s))
        .collect::<Result<Vec<_>>>()?;
    let decision = engine
        .route(
            &RouteInput {
                prompt: prompt.clone(),
                images: images.clone(),
                audio: vec![],
                specialist: Some(model),
            },
            router,
        )
        .await?;
    let specialist = engine
        .registry
        .specialists
        .get(&decision.specialist)
        .ok_or("router selected an unknown specialist")?
        .clone();
    if specialist.task != "chat" {
        return Err(format!(
            "specialist '{}' uses task '{}'; lh run currently supports chat only (audio is M4)",
            specialist.id, specialist.task
        )
        .into());
    }
    pull(engine, &specialist.id).await?;
    let host = engine.host(&specialist.backend)?;
    let content = if images.is_empty() {
        json!(prompt)
    } else {
        let mut parts = vec![json!({"type":"text","text":prompt})];
        parts.extend(
            images
                .into_iter()
                .map(|url| json!({"type":"image_url","image_url":{"url":url}})),
        );
        json!(parts)
    };
    // This one-shot CLI has exactly one active call on this host. Subscribe after
    // loading so only this chat's stream notifications are consumed.
    let mut notifications = host.subscribe();
    let call = host.call("backend/chat", json!({"key":specialist.id,"messages":[{"role":"user","content":content}],"params":specialist.params,"stream":true}));
    tokio::pin!(call);
    let mut stdout = io::stdout().lock();
    let mut streamed = false;
    let response = loop {
        tokio::select! {
            // Drain queued chunks before accepting the final response.
            biased;
            notification = notifications.recv() => {
                let notification = notification.map_err(|e| format!("chat notification stream lost: {e}"))?;
                if notification.method == "$/stream" {
                    if let Some(text) = notification.params["delta"]["content"].as_str() {
                        stdout.write_all(text.as_bytes())?;
                        stdout.flush()?;
                        streamed |= !text.is_empty();
                    }
                    if let Some(tools) = notification.params["delta"].get("tool_calls") {
                        eprintln!("tool_calls: {tools}");
                    }
                }
            }
            result = &mut call => break result?,
        }
    };
    if !streamed && let Some(text) = response["message"]["content"].as_str() {
        stdout.write_all(text.as_bytes())?;
    }
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encodes_local_images_as_base64_data_urls() {
        let path = std::env::temp_dir().join(format!("lh-image-{}.PNG", std::process::id()));
        std::fs::write(&path, [0, 1, 2, 253, 254, 255]).unwrap();
        let result = image_url(path.to_str().unwrap());
        std::fs::remove_file(path).unwrap();
        assert_eq!(result.unwrap(), "data:image/png;base64,AAEC/f7/");
    }
    #[test]
    fn preserves_remote_and_data_urls() {
        for url in [
            "https://example.test/image.png",
            "data:image/png;base64,AA==",
        ] {
            assert_eq!(image_url(url).unwrap(), url);
        }
        assert!(image_url("bad.txt").is_err());
        assert!(image_url("missing.png").is_err());
    }
}
