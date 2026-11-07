use lh_core::{Engine, Session};
use lh_protocol::{
    RouteDecision,
    json::{self, Value, json},
};
use std::io::{self, BufRead, IsTerminal, Write};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Debug, PartialEq)]
enum Command<'a> {
    Image(&'a str),
    Audio(&'a str),
    Model(&'a str),
    Speak,
    Explain,
    Reset,
    Quit,
    Turn(&'a str),
}
fn parse(line: &str) -> Result<Command<'_>> {
    let line = line.trim();
    let (verb, arg) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    let arg = arg.trim();
    Ok(match (verb, arg) {
        ("/image", "") | ("/audio", "") | ("/model", "") => {
            return Err("command requires an argument".into());
        }
        ("/image", arg) => Command::Image(arg),
        ("/audio", arg) => Command::Audio(arg),
        ("/model", arg) => Command::Model(arg),
        ("/speak", "") => Command::Speak,
        ("/explain", "") => Command::Explain,
        ("/reset", "") => Command::Reset,
        ("/quit", "") => Command::Quit,
        _ if line.starts_with('/') => return Err("unknown command or unexpected argument".into()),
        _ => Command::Turn(line),
    })
}

pub async fn run(engine: &mut Engine, mut model: String) -> Result<()> {
    let mut session = Session::default();
    let mut history = Vec::<Value>::new();
    let mut pending = Vec::<Value>::new();
    let mut speak = false;
    let mut last: Option<RouteDecision> = None;
    let interactive = io::stdin().is_terminal();
    // Reading a line is intentionally blocking only while no inference is active.
    loop {
        if interactive {
            print!("lh> ");
            io::stdout().flush()?;
        }
        let mut line = String::new();
        if io::stdin().lock().read_line(&mut line)? == 0 {
            break;
        }
        let command = match parse(&line) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("lh: {e}");
                continue;
            }
        };
        let mut speech_text = None;
        match command {
            Command::Quit => break,
            Command::Reset => {
                session.reset();
                history.clear();
                pending.clear();
                last = None;
            }
            Command::Model(id) => {
                if id != "auto" && !engine.registry.specialists.contains_key(id) {
                    eprintln!("lh: unknown model {id}");
                    continue;
                }
                model = id.into();
                session.reset();
            }
            Command::Speak => {
                speak = !speak;
                eprintln!("spoken replies: {speak}");
            }
            Command::Explain => eprintln!("{}", json::to_string_pretty(&last)?),
            Command::Image(path) => match crate::inference::image_url(path) {
                Ok(url) => pending.push(json!({"type":"image_url","image_url":{"url":url}})),
                Err(e) => eprintln!("lh: {e}"),
            },
            Command::Audio(path) => {
                match crate::inference::audio_url(path) {
                    Ok(url) => {
                        let (header, data) = url.split_once(',').ok_or("invalid audio URL")?;
                        let format = header
                            .strip_prefix("data:audio/")
                            .and_then(|h| h.strip_suffix(";base64"))
                            .ok_or("invalid audio format")?;
                        pending.push(json!({"type":"input_audio","input_audio":{"format":format,"data":data}}));
                    }
                    Err(e) => eprintln!("lh: {e}"),
                }
            }
            Command::Turn(text) => {
                if text.is_empty() && pending.is_empty() {
                    continue;
                }
                let mut parts = pending.clone();
                parts.insert(0, json!({"type":"text","text":text}));
                let mut messages = history.clone();
                messages.push(json!({"role":"user","content":parts}));
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let (events, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
                let control = std::sync::Arc::new(lh_core::agent::Control::default());
                let timeout = engine.config.agent.approval_timeout_secs;
                let call = crate::conversation::complete_control(
                    engine,
                    messages,
                    &model,
                    Some(&mut session),
                    json!({}),
                    Some(tx),
                    Some((control.clone(), events)),
                );
                tokio::pin!(call);
                let result = loop {
                    tokio::select! {
                        biased;
                        Some(event)=event_rx.recv()=>crate::agent_ui::event(event,&control,timeout).await?,
                        delta = rx.recv(), if !rx.is_closed() || !rx.is_empty() => if let Some(delta) = delta {
                            if let Some(text) = delta["content"].as_str() { print!("{text}"); io::stdout().flush()?; }
                            if let Some(tools) = delta.get("tool_calls") { eprintln!("tool_calls: {tools}"); }
                        },
                        result = &mut call => break result,
                    }
                };
                while let Ok(event) = event_rx.try_recv() {
                    crate::agent_ui::event(event, &control, timeout).await?;
                }
                while let Ok(delta) = rx.try_recv() {
                    if let Some(text) = delta["content"].as_str() {
                        print!("{text}");
                    }
                    if let Some(tools) = delta.get("tool_calls") {
                        eprintln!("tool_calls: {tools}");
                    }
                }
                println!();
                match result {
                    Ok((decision, response)) => {
                        eprintln!("[route] {}", json::to_string(&decision)?);
                        last = Some(decision);
                        history = response["lh_messages"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        history.push(response["message"].clone());
                        pending.clear();
                        if let Some(audio) = response["message"]["audio"]["audio"].as_str() {
                            println!("{}", crate::inference::save_audio(audio, None)?.display());
                        } else if speak {
                            speech_text =
                                response["message"]["content"].as_str().map(str::to_owned);
                        }
                    }
                    Err(e) => eprintln!("lh: {e}"),
                }
            }
        }
        if let Some(text) = speech_text {
            let result = async {
                let id = crate::inference::task_specialist(engine, "speak")?;
                let decision = engine.prepare(&id).await?;
                let response = engine.dispatch(&decision, json!({"text":text})).await?;
                let audio = response["audio"].as_str().ok_or("speech missing audio")?;
                println!("{}", crate::inference::save_audio(audio, None)?.display());
                Ok::<_, Box<dyn std::error::Error>>(())
            }
            .await;
            if let Err(e) = result {
                eprintln!("lh: {e}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_and_paths_with_spaces() {
        assert_eq!(
            parse(" /image a b.png ").unwrap(),
            Command::Image("a b.png")
        );
        assert_eq!(parse("/audio a.wav").unwrap(), Command::Audio("a.wav"));
        assert_eq!(parse("/model auto").unwrap(), Command::Model("auto"));
        assert_eq!(parse("hello").unwrap(), Command::Turn("hello"));
        for s in ["/speak", "/reset", "/explain", "/quit"] {
            assert!(parse(s).is_ok());
        }
        for s in ["/image", "/audio", "/model", "/quit x", "/oops"] {
            assert!(parse(s).is_err());
        }
    }
}
