//! One actor per process. Calls may be in flight concurrently; wire IDs correlate
//! responses. A broken transport fails the entire generation, never replays calls.
use crate::{Error, Result, registry::Entry};
use lh_protocol::{
    Notification, Request, Response,
    json::{self, Value, json},
};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
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
    Reset(oneshot::Sender<()>),
    Call(String, Value, Reply),
    Status(Reply),
    Shutdown(oneshot::Sender<()>),
}
#[derive(Clone)]
pub struct Host {
    tx: mpsc::Sender<CommandMessage>,
    notifications: broadcast::Sender<Notification>,
}
impl Host {
    pub fn new(entry: Entry, config: BTreeMap<String, Value>) -> Self {
        let timeout = config
            .get("host_timeout_secs")
            .or_else(|| entry.manifest.config.get("host_timeout_secs"))
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
            .unwrap_or(30);
        Self::with_timeout(entry, config, Duration::from_secs(timeout))
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
    /// Lazily initialize without forwarding a second initialize request.
    pub async fn capabilities(&self) -> Result<Value> {
        self.call("host/capabilities", Value::Null).await
    }
    /// Inspect this host without initializing or restarting its process.
    pub async fn status(&self) -> Result<Value> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(CommandMessage::Status(tx))
            .await
            .map_err(|_| Error::new(-32603, "plugin host closed"))?;
        rx.await
            .map_err(|_| Error::new(-32603, "plugin host closed"))?
    }
    pub async fn reset(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(CommandMessage::Reset(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
    pub async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(CommandMessage::Shutdown(tx)).await.is_ok() {
            let _ = rx.await;
        }
    }
}
struct Process {
    mcp: bool,
    capabilities: Vec<String>,
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
        if self.mcp {
            // MCP has no shutdown method. Dropping the ordered writer closes stdin.
            self.writer.abort();
        } else {
            let _ = self.send(json!({"jsonrpc":"2.0","method":"shutdown"}));
        }
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
/// Where a process plugin's `[run] command` resolves, in spawn order: the
/// plugin directory, the running `lh` executable's directory, then `$PATH`.
/// Returns the selected path (the bare command for a PATH lookup), the rule
/// that matched (`plugin-dir`, `lh-dir`, `PATH` or `missing`) and, for PATH,
/// the file actually found there (if any).
pub fn resolve_command(entry: &Entry) -> Option<(PathBuf, &'static str, Option<PathBuf>)> {
    let run = entry.manifest.run.as_ref()?;
    let local = entry.path.parent()?.join(&run.command);
    if local.is_file() {
        return Some((local, "plugin-dir", None));
    }
    if let Some(sibling) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join(&run.command)))
        .filter(|p| p.is_file())
    {
        return Some((sibling, "lh-dir", None));
    }
    let found = which(&run.command);
    let how = if found.is_some() { "PATH" } else { "missing" };
    Some((run.command.clone().into(), how, found))
}
/// `$PATH` lookup of a bare command name; a name with a slash is checked as is.
pub fn which(command: &str) -> Option<PathBuf> {
    if command.contains('/') {
        let path = PathBuf::from(command);
        return path.is_file().then_some(path);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(command))
        .find(|p| p.is_file())
}
async fn spawn(entry: &Entry, config: &BTreeMap<String, Value>) -> Result<Process> {
    let run = entry
        .manifest
        .run
        .as_ref()
        .ok_or_else(|| Error::new(-32602, "declarative plugin cannot be spawned"))?;
    let local = entry.path.parent().unwrap().join(&run.command);
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join(&run.command)));
    let executable = resolve_command(entry)
        .map(|(path, _, _)| path)
        .unwrap_or_else(|| run.command.clone().into());
    let mut child = Command::new(&executable)
        .args(&run.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn().map_err(|error| Error::new(-32603, format!(
            "plugin '{}' could not spawn command '{}': {error}; selected '{}'; searched plugin path '{}', executable sibling '{}', PATH={:?}",
            entry.manifest.plugin.id, run.command, executable.display(), local.display(),
            sibling.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "<current executable unavailable>".into()),
            std::env::var_os("PATH").unwrap_or_default()
        )))?;
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
        mcp: entry.manifest.plugin.provides.iter().any(|p| p == "tool"),
        capabilities: vec![],
        child,
        stdin,
        frames,
        reader,
        writer,
        stderr,
    };
    let mut merged = entry.manifest.config.clone();
    merged.extend(config.clone());
    // Reserved host setting, not part of the plugin-owned configuration.
    merged.remove("host_timeout_secs");
    let params = if process.mcp {
        json!({"protocolVersion":"2025-03-26", "capabilities":{},
            "clientInfo":{"name":"liquid-harness","version":env!("CARGO_PKG_VERSION")},
            "config":merged})
    } else {
        json!({"protocol":1,"plugin_id":entry.manifest.plugin.id,"config":merged,"core_version":env!("CARGO_PKG_VERSION")})
    };
    if let Err(e) = process.send(json::to_value(Request::new(0, "initialize", params))?) {
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
        if process.mcp {
            if r.id != 0
                || result["protocolVersion"] != "2025-03-26"
                || !result["serverInfo"]["name"].is_string()
                || !result["serverInfo"]["version"].is_string()
                || !result["capabilities"].is_object()
            {
                return Err(Error::new(-32603, "invalid MCP initialize handshake"));
            }
            process.capabilities = result["capabilities"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            process.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))?;
            return Ok(());
        }
        if r.id != 0
            || !result["name"].is_string()
            || !result["version"].is_string()
            || !result["provides"].is_array()
            || !(result["capabilities"].is_object()
                || result["capabilities"]
                    .as_array()
                    .is_some_and(|a| a.iter().all(Value::is_string)))
        {
            return Err(Error::new(-32603, "invalid initialize handshake"));
        }
        process.capabilities = if let Some(array) = result["capabilities"].as_array() {
            array
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        } else {
            result["capabilities"]
                .as_object()
                .unwrap()
                .iter()
                .filter(|(_, value)| **value == true)
                .map(|(name, _)| name.clone())
                .collect()
        };
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
    let mut starts: u64 = 0;
    let mut restarts = VecDeque::new();
    let mut tick = tokio::time::interval(Duration::from_millis(25));
    loop {
        let mut failure = None;
        tokio::select! {
            cmd = rx.recv() => match cmd {
                None => break,
                Some(CommandMessage::Reset(reply)) => {
                    for (_, (_, waiting)) in std::mem::take(&mut pending) {
                        let _ = waiting.send(Err(Error::new(-32004, "plugin generation cancelled")));
                    }
                    if let Some(p) = process.take() { p.stop().await; }
                    let _ = reply.send(());
                    continue;
                }
                Some(CommandMessage::Status(reply)) => {
                    let pid = process.as_mut().and_then(|p| {
                        matches!(p.child.try_wait(), Ok(None)).then(|| p.child.id()).flatten()
                    });
                    let state = if pid.is_some() { "running" } else if starts == 0 { "not_started" } else { "failed" };
                    let _ = reply.send(Ok(json!({"state":state,"pid":pid,"restarts":starts.saturating_sub(1)})));
                }
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
                    let capabilities = &process.as_ref().unwrap().capabilities;
                    if method == "host/capabilities" {
                        let _ = reply.send(Ok(json!(capabilities)));
                        continue;
                    }
                    if let Some(capability) = method.strip_prefix("backend/").filter(|m| matches!(*m, "chat" | "decide" | "transcribe" | "speak"))
                        && !capabilities.iter().any(|c| c == capability) {
                        let _ = reply.send(Err(Error::new(-32003, format!("backend '{}' does not support {capability}", entry.manifest.plugin.id))));
                        continue;
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

#[cfg(test)]
mod tests {
    use super::*;

    const MCP: &str = r#"
import json, os, sys, time
initialized = False
config = {}
for line in sys.stdin:
    r = json.loads(line)
    method = r['method']
    if method == 'initialize':
        assert r['params']['protocolVersion'] == '2025-03-26'
        assert r['params']['capabilities'] == {}
        assert r['params']['clientInfo']['name'] == 'liquid-harness'
        config = r['params']['config']
        assert 'host_timeout_secs' not in config
        result = {'protocolVersion':'2025-03-26','serverInfo':{'name':'test','version':'1'},'capabilities':{'tools':{}}}
    elif method == 'notifications/initialized':
        assert 'id' not in r
        initialized = True
        continue
    else:
        assert initialized
        assert method != 'shutdown'
        if method == 'crash':
            sys.exit(2)
        if method == 'hang':
            time.sleep(10)
        result = {'cwd':os.getcwd(),'config':config,'params':r.get('params')}
    print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}), flush=True)
if 'eof_marker' in config:
    with open(config['eof_marker'], 'w') as f:
        f.write('EOF')
"#;

    fn entry(script: &str) -> Entry {
        let mut manifest: lh_protocol::Manifest = lh_protocol::toml::from_str(
            r#"
[plugin]
id = "test-mcp"
version = "1"
protocol = 1
provides = ["tool"]
[run]
command = "python3"
"#,
        )
        .unwrap();
        manifest.run.as_mut().unwrap().args = vec!["-u".into(), "-c".into(), script.into()];
        Entry {
            path: std::env::temp_dir().join("nonexistent-plugin/plugin.toml"),
            manifest,
        }
    }

    #[tokio::test]
    async fn mcp_lifecycle_status_cwd_config_and_eof() {
        let marker = std::env::temp_dir().join(format!("lh-mcp-eof-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let host = Host::new(
            entry(MCP),
            BTreeMap::from([
                ("eof_marker".into(), json!(marker)),
                ("host_timeout_secs".into(), json!(2)),
            ]),
        );
        assert_eq!(
            host.status().await.unwrap(),
            json!({"state":"not_started","pid":null,"restarts":0})
        );
        assert_eq!(host.capabilities().await.unwrap(), json!(["tools"]));
        let result = host.call("echo", json!({"hello":true})).await.unwrap();
        assert_eq!(result["cwd"], json!(std::env::current_dir().unwrap()));
        assert_eq!(result["params"], json!({"hello":true}));
        let status = host.status().await.unwrap();
        assert_eq!(status["state"], "running");
        assert!(status["pid"].is_u64());
        host.shutdown().await;
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "EOF");
        std::fs::remove_file(marker).unwrap();
    }

    #[tokio::test]
    async fn mcp_crash_restart_budget_and_timeout() {
        let host = Host::with_timeout(entry(MCP), BTreeMap::new(), Duration::from_secs(1));
        assert!(
            host.call("hang", json!({}))
                .await
                .unwrap_err()
                .message
                .contains("timed out")
        );
        for _ in 0..3 {
            assert!(host.call("crash", json!({})).await.is_err());
        }
        let error = host.call("echo", json!({})).await.unwrap_err();
        assert!(error.message.contains("restart limit"));
        let status = host.status().await.unwrap();
        assert_eq!(status["restarts"], 3);
        assert_eq!(status["state"], "failed");
        host.shutdown().await;
    }

    #[tokio::test]
    async fn mcp_rejects_incompatible_version() {
        let script = MCP.replace(
            "'protocolVersion':'2025-03-26'",
            "'protocolVersion':'invalid'",
        );
        let host = Host::new(entry(&script), BTreeMap::new());
        assert!(
            host.capabilities()
                .await
                .unwrap_err()
                .message
                .contains("invalid MCP")
        );
        host.shutdown().await;
    }
}
