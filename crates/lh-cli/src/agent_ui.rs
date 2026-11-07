use lh_core::{
    Engine,
    agent::{self, Control},
};
use lh_protocol::{
    RouteDecision,
    json::{Value, json},
};
use std::{
    io::{self, IsTerminal, Read, Write},
    sync::Arc,
    time::{Duration, Instant},
};

pub async fn event(event: Value, control: &Control, timeout: u64) -> io::Result<()> {
    match event["event"].as_str() {
        Some("delta") => {
            if let Some(text) = event["data"]["content"].as_str() {
                print!("{text}");
                io::stdout().flush()?;
            }
        }
        Some("approval_required") => {
            let data = &event["data"];
            eprintln!(
                "Approve {} {}? [y/n/always]",
                data["name"], data["arguments"]
            );
            let answer = answer(timeout).await.unwrap_or_default();
            control
                .approve(
                    data["call_id"].as_str().unwrap_or(""),
                    matches!(answer.as_str(), "y" | "yes" | "always"),
                    answer == "always",
                )
                .await;
        }
        Some("tool_call" | "tool_result" | "error") => {
            eprintln!("[{}] {}", event["event"].as_str().unwrap(), event["data"])
        }
        _ => {}
    }
    Ok(())
}
/// Never consume piped conversation lines as approvals. Poll the controlling tty
/// without blocking the runtime, so unanswered prompts can actually expire.
async fn answer(timeout: u64) -> io::Result<String> {
    if !io::stdin().is_terminal() {
        return Ok(String::new());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(0o4000)
            .open("/dev/tty")?;
        let start = Instant::now();
        let mut line = Vec::new();
        while start.elapsed() < Duration::from_secs(timeout) {
            let mut byte = [0];
            match tty.read(&mut byte) {
                Ok(1) if byte[0] == b'\n' => break,
                Ok(1) => {
                    if line.len() < 64 {
                        line.push(byte[0]);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Ok(String::from_utf8_lossy(&line).trim().to_lowercase())
    }
    #[cfg(not(unix))]
    {
        let _ = timeout;
        Ok(String::new())
    }
}
pub async fn run(
    engine: &mut Engine,
    route: &RouteDecision,
    messages: Vec<Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    let control = Arc::new(Control::default());
    let timeout = engine.config.agent.approval_timeout_secs;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let call = agent::run(engine, route, messages, json!({}), tx, control.clone());
    tokio::pin!(call);
    let result = loop {
        tokio::select! {biased;
            Some(value)=rx.recv()=>event(value,&control,timeout).await?,
            result=&mut call=>break result,
        }
    };
    while let Ok(value) = rx.try_recv() {
        event(value, &control, timeout).await?;
    }
    println!();
    result?;
    Ok(())
}
