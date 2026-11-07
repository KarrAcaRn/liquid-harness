//! One actor per process. Calls may be in flight concurrently; wire IDs correlate
//! responses. A broken transport fails the entire generation, never replays calls.
use crate::{Error, Result, registry::Entry};
use lh_protocol::{
    Notification, Request, Response,
    json::{self, Value, json},
};
use std::{
    collections::{BTreeMap, VecDeque},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
};

const MAX_FRAME: usize = 8 * 1024 * 1024;
type Reply = oneshot::Sender<Result<Value>>;
enum CommandMessage {
    Call(String, Value, Reply),
    Shutdown(oneshot::Sender<()>),
}
#[derive(Clone)]
pub struct Host {
    tx: mpsc::Sender<CommandMessage>,
    notifications: broadcast::Sender<Notification>,
}
impl Host {
    pub fn new(entry: Entry, config: BTreeMap<String, Value>) -> Self {
        Self::with_timeout(entry, config, Duration::from_secs(30))
    }
    pub fn with_timeout(entry: Entry, config: BTreeMap<String, Value>, timeout: Duration) -> Self {
        let (tx, rx) = mpsc::channel(64);
        let (notifications, _) = broadcast::channel(256);
        tokio::spawn(actor(entry, config, timeout, rx, notifications.clone()));
        Self { tx, notifications }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.notifications.subscribe()
    }
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(CommandMessage::Call(method.into(), params, tx))
            .await
            .map_err(|_| Error::new(-32603, "plugin host closed"))?;
        rx.await
            .map_err(|_| Error::new(-32603, "plugin host closed"))?
    }
    pub async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(CommandMessage::Shutdown(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
}
struct Process {
    child: Child,
    stdin: mpsc::Sender<Vec<u8>>,
    frames: mpsc::Receiver<Result<Value>>,
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
    stderr: JoinHandle<()>,
}
impl Process {
    fn send(&mut self, value: Value) -> Result<()> {
        let mut bytes = json::to_vec(&value)?;
        if bytes.len() >= MAX_FRAME {
            return Err(Error::new(-32602, "JSON-RPC frame exceeds size limit"));
        }
        bytes.push(b'\n');
        self.stdin.try_send(bytes).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => Error::new(-32002, "plugin write queue full"),
            mpsc::error::TrySendError::Closed(_) => Error::new(-32603, "plugin stdin closed"),
        })
    }
    async fn stop(mut self) {
        // Give a cooperative plugin a short grace period, then kill and reap it.
        let _ = self.send(json!({"jsonrpc":"2.0","method":"shutdown"}));
        if tokio::time::timeout(Duration::from_millis(200), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
        self.reader.abort();
        self.writer.abort();
        self.stderr.abort();
        let _ = self.reader.await;
        let _ = self.writer.await;
        let _ = self.stderr.await;
    }
}
async fn spawn(entry: &Entry, config: &BTreeMap<String, Value>) -> Result<Process> {
    let run = entry
        .manifest
        .run
        .as_ref()
        .ok_or_else(|| Error::new(-32602, "declarative plugin cannot be spawned"))?;
    let local = entry.path.parent().unwrap().join(&run.command);
    let executable = if local.is_file() {
        local
    } else {
        run.command.clone().into()
    };
    let mut child = Command::new(executable)
        .args(&run.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut child_stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, frames) = mpsc::channel(64);
    let (stdin, mut writes) = mpsc::channel::<Vec<u8>>(64);
    let write_errors = tx.clone();
    // Keep reading responses while stdin is backpressured. A serial plugin may
    // need to finish a large streamed response before it reads its next request.
    let writer = tokio::spawn(async move {
        while let Some(bytes) = writes.recv().await {
            let result = async {
                child_stdin.write_all(&bytes).await?;
                child_stdin.flush().await
            }
            .await;
            if let Err(e) = result {
                let _ = write_errors.send(Err(e.into())).await;
                break;
            }
        }
    });
    let reader = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut frame = Vec::new();
            let result = match (&mut reader)
                .take((MAX_FRAME + 1) as u64)
                .read_until(b'\n', &mut frame)
                .await
            {
                Ok(0) => Err(Error::new(-32603, "plugin closed stdout")),
                Ok(n) if n > MAX_FRAME || frame.last() != Some(&b'\n') => {
                    Err(Error::new(-32603, "invalid or oversized plugin frame"))
                }
                Ok(_) => json::from_slice(&frame).map_err(Error::from),
                Err(e) => Err(e.into()),
            };
            let failed = result.is_err();
            if tx.send(result).await.is_err() || failed {
                break;
            }
        }
    });
    let name = entry.manifest.plugin.id.clone();
    let stderr = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        loop {
            let mut bytes = Vec::new();
            match (&mut reader).take(8192).read_until(b'\n', &mut bytes).await {
                Ok(0) | Err(_) => break,
                Ok(_) => eprintln!("[{name}] {}", String::from_utf8_lossy(&bytes).trim_end()),
            }
        }
    });
    let mut process = Process {
        child,
        stdin,
        frames,
        reader,
        writer,
        stderr,
    };
    let mut merged = entry.manifest.config.clone();
    merged.extend(config.clone());
    if let Err(e) = process.send(json::to_value(Request::new(0, "initialize", json!({"protocol":1,"plugin_id":entry.manifest.plugin.id,"config":merged,"core_version":env!("CARGO_PKG_VERSION")})))?) {
        process.stop().await;
        return Err(e);
    }
    Ok(process)
}
fn response(value: Value) -> Result<Response> {
    let r: Response = json::from_value(value)?;
    if r.jsonrpc != "2.0" || r.result.is_some() == r.error.is_some() {
        return Err(Error::new(-32603, "invalid JSON-RPC response"));
    }
    Ok(r)
}
async fn initialize(
    process: &mut Process,
    notifications: &broadcast::Sender<Notification>,
) -> Result<()> {
    loop {
        let frame = process
            .frames
            .recv()
            .await
            .ok_or_else(|| Error::new(-32603, "plugin exited during initialize"))??;
        if frame.get("method").is_some() {
            forward(frame, notifications)?;
            continue;
        }
        let r = response(frame)?;
        let result = r
            .result
            .ok_or_else(|| Error::new(-32603, "initialize rejected"))?;
        if r.id != 0
            || !result["name"].is_string()
            || !result["version"].is_string()
            || !result["provides"].is_array()
            || !result["capabilities"].is_object()
        {
            return Err(Error::new(-32603, "invalid initialize handshake"));
        }
        return Ok(());
    }
}
fn forward(value: Value, notifications: &broadcast::Sender<Notification>) -> Result<()> {
    if value.get("id").is_some() {
        return Err(Error::new(-32603, "plugin sent an unsupported request"));
    }
    let n: Notification = json::from_value(value)?;
    if n.jsonrpc != "2.0" {
        return Err(Error::new(-32603, "invalid notification version"));
    }
    let _ = notifications.send(n);
    Ok(())
}
async fn actor(
    entry: Entry,
    config: BTreeMap<String, Value>,
    timeout: Duration,
    mut rx: mpsc::Receiver<CommandMessage>,
    notifications: broadcast::Sender<Notification>,
) {
    let mut process: Option<Process> = None;
    let mut pending: BTreeMap<u64, (Instant, Reply)> = BTreeMap::new();
    let mut next_id = 1;
    let mut starts = 0;
    let mut restarts = VecDeque::new();
    let mut tick = tokio::time::interval(Duration::from_millis(25));
    loop {
        let mut failure = None;
        tokio::select! {
            cmd = rx.recv() => match cmd {
                None => break,
                Some(CommandMessage::Shutdown(reply)) => {
                    if let Some(p) = process.take() { p.stop().await; }
                    let _ = reply.send(());
                    break;
                }
                Some(CommandMessage::Call(method, params, reply)) => {
                    // An idle child may have exited before its EOF event is handled.
                    // Detect this here so the next use starts a fresh generation.
                    if let Some(p) = process.as_mut()
                        && !matches!(p.child.try_wait(), Ok(None)) {
                            for (_, (_, waiting)) in std::mem::take(&mut pending) {
                                let _ = waiting.send(Err(Error::new(-32603, "plugin exited")));
                            }
                            if let Some(p) = process.take() { p.stop().await; }
                    }
                    if process.is_none() {
                        while restarts.front().is_some_and(|t: &Instant| t.elapsed() >= Duration::from_secs(60)) { restarts.pop_front(); }
                        if starts > 0 && restarts.len() >= 3 {
                            let _ = reply.send(Err(Error::new(-32603, "plugin restart limit: 3 per 60 seconds")));
                            continue;
                        }
                        if starts > 0 { restarts.push_back(Instant::now()); }
                        starts += 1;
                        // Bound startup writes as well as the handshake. Process drop kills
                        // the child if startup is cancelled, and readers see EOF.
                        match tokio::time::timeout(timeout, spawn(&entry, &config)).await {
                            Ok(Ok(mut p)) => {
                                let initialized = tokio::time::timeout(timeout, initialize(&mut p, &notifications)).await;
                                match initialized {
                                    Ok(Ok(())) => process = Some(p),
                                    other => {
                                        p.stop().await;
                                        let error = match other { Ok(Err(e)) => e, _ => Error::new(-32603, "initialize timed out") };
                                        let _ = reply.send(Err(error));
                                        continue;
                                    }
                                }
                            }
                            other => {
                                let error = match other { Ok(Err(e)) => e, _ => Error::new(-32603, "spawn timed out") };
                                let _ = reply.send(Err(error));
                                continue;
                            }
                        }
                    }
                    if pending.len() >= 64 {
                        let _ = reply.send(Err(Error::new(-32002, "plugin has too many in-flight calls")));
                        continue;
                    }
                    let id = next_id;
                    next_id += 1;
                    pending.insert(id, (Instant::now(), reply));
                    let request = json::to_value(Request::new(id, method, params)).expect("request serialization");
                    if let Err(e) = process.as_mut().unwrap().send(request) {
                        if e.code == -32002 || e.code == -32602 {
                            if let Some((_, reply)) = pending.remove(&id) { let _ = reply.send(Err(e)); }
                        } else { failure = Some(e); }
                    }
                }
            },
            frame = async { process.as_mut().unwrap().frames.recv().await }, if process.is_some() => {
                match frame {
                    Some(Ok(v)) if v.get("method").is_some() => { if let Err(e) = forward(v, &notifications) { failure = Some(e); } },
                    Some(Ok(v)) => match response(v) {
                        Ok(r) => {
                            if let Some((_, reply)) = pending.remove(&r.id) {
                                let result = match r.error { Some(e) => Err(Error::new(e.code, e.message)), None => Ok(r.result.unwrap()) };
                                let _ = reply.send(result);
                            } else { failure = Some(Error::new(-32603, "response has unknown request id")); }
                        }
                        Err(e) => failure = Some(e),
                    },
                    Some(Err(e)) => failure = Some(e),
                    None => failure = Some(Error::new(-32603, "plugin transport closed")),
                }
            },
            _ = tick.tick() => {
                if pending.values().any(|(start, _)| start.elapsed() >= timeout) { failure = Some(Error::new(-32603, "plugin call timed out")); }
            }
        }
        if let Some(e) = failure {
            for (_, (_, reply)) in std::mem::take(&mut pending) {
                let _ = reply.send(Err(e.clone()));
            }
            if let Some(p) = process.take() {
                p.stop().await;
            }
        }
    }
    for (_, (_, reply)) in pending {
        let _ = reply.send(Err(Error::new(-32603, "plugin host shut down")));
    }
    if let Some(p) = process {
        p.stop().await;
    }
}
