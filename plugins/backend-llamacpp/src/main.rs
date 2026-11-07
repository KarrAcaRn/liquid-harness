//! Blocking llama.cpp adapter. The state mutex is held throughout a request:
//! the TTL worker can never evict a model during loading or inference.
use lh_plugin_sdk::{Context, Handler};
mod audio;
use lh_protocol::{
    ModelSpec, RpcError,
    json::{self, Value, json},
};
use std::os::unix::process::CommandExt;
use std::{
    collections::BTreeMap,
    net::TcpListener,
    process::{Child, Command, Stdio},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

fn invalid(e: impl ToString) -> RpcError {
    RpcError::new(-32602, e.to_string())
}
fn failed(e: impl ToString) -> RpcError {
    RpcError::new(-32001, e.to_string())
}
fn internal(e: impl ToString) -> RpcError {
    RpcError::new(-32603, e.to_string())
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        // Every server is its own process group. Kill descendants as well, then reap.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Resident {
    process: Option<Process>,
    model: ModelSpec,
    port: u16,
    pinned: bool,
    used: Instant,
}
struct State {
    models: BTreeMap<String, Resident>,
    ttl: Duration,
    stop: bool,
}
impl State {
    fn reap(&mut self) {
        self.models.retain(|_, r| {
            let alive = r
                .process
                .as_mut()
                .is_none_or(|p| matches!(p.0.try_wait(), Ok(None)));
            alive && (r.pinned || r.used.elapsed() < self.ttl)
        });
    }
    fn room(&mut self, limit: usize) -> Result<(), RpcError> {
        while self.models.len() >= limit {
            let key = self
                .models
                .iter()
                .filter(|(_, r)| !r.pinned)
                .min_by_key(|(_, r)| r.used)
                .map(|(k, _)| k.clone())
                .ok_or_else(|| RpcError::new(-32002, "all resident models are pinned"))?;
            self.models.remove(&key);
        }
        Ok(())
    }
}
struct Backend {
    state: Arc<(Mutex<State>, Condvar)>,
    worker: Option<thread::JoinHandle<()>>,
    executable: String,
    tts_executable: Option<String>,
    ffmpeg: String,
    request_timeout: Duration,
    models_dir: Option<String>,
    limit: usize,
    load_timeout: Duration,
    agent: ureq::Agent,
    initialized: bool,
    defaults: [u32; 3],
}
impl Default for Backend {
    fn default() -> Self {
        let state = Arc::new((
            Mutex::new(State {
                models: BTreeMap::new(),
                ttl: Duration::from_secs(600),
                stop: false,
            }),
            Condvar::new(),
        ));
        let shared = state.clone();
        let worker = thread::spawn(move || {
            let (lock, wake) = &*shared;
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            while !state.stop {
                state = wake
                    .wait_timeout(state, Duration::from_millis(250))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
                if !state.stop {
                    state.reap();
                }
            }
        });
        Self {
            state,
            worker: Some(worker),
            executable: "llama-server".into(),
            tts_executable: None,
            ffmpeg: "ffmpeg".into(),
            request_timeout: Duration::from_secs(600),
            models_dir: None,
            limit: 2,
            load_timeout: Duration::from_secs(600),
            agent: agent(600),
            initialized: false,
            defaults: [4096, 512, 8],
        }
    }
}
fn agent(seconds: u64) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(seconds))
        .redirects(0)
        .build()
}
impl Drop for Backend {
    fn drop(&mut self) {
        let (lock, wake) = &*self.state;
        {
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            state.stop = true;
            state.models.clear();
        }
        wake.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn positive(config: &Value, key: &str, default: u64) -> Result<u64, RpcError> {
    match config.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or_else(|| invalid(format!("{key} must be positive"))),
    }
}
fn expand(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    path.into()
}
fn arguments(model: &ModelSpec, port: u16) -> Result<Vec<String>, RpcError> {
    let mut args = vec![
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
    ];
    if let Some(source) = model.source.strip_prefix("hf:") {
        if source.is_empty() || source.starts_with('-') {
            return Err(invalid("empty/invalid hf source"));
        }
        args.extend(["-hf".into(), source.into()]);
    } else if let Some(path) = model.source.strip_prefix("file:") {
        if path.is_empty() {
            return Err(invalid("empty model path"));
        }
        args.extend(["-m".into(), expand(path)]);
    } else {
        return Err(invalid("model.source must use hf: or file:"));
    }
    for (flag, value) in [
        ("-c", model.ctx),
        ("-b", model.batch),
        ("-t", model.threads),
    ] {
        if let Some(n) = value {
            if n == 0 {
                return Err(invalid(format!("{flag} must be positive")));
            }
            args.extend([flag.into(), n.to_string()]);
        }
    }
    if model.mmproj.is_none() {
        args.push("--no-mmproj".into());
    }
    if let Some(mmproj) = &model.mmproj {
        if mmproj == "auto" {
            // llama.cpp automatically discovers the projector for -hf repositories.
            if !model.source.starts_with("hf:") {
                return Err(invalid(
                    "mmproj auto requires an hf source; use an explicit projector for file models",
                ));
            }
        } else {
            if mmproj.is_empty() {
                return Err(invalid("empty mmproj"));
            }
            args.extend([
                "--mmproj".into(),
                expand(mmproj.strip_prefix("file:").unwrap_or(mmproj)),
            ]);
        }
    }
    Ok(args)
}
fn spawn(executable: &str, args: &[String], models_dir: Option<&str>) -> Result<Process, RpcError> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if let Some(dir) = models_dir {
        command.env("LLAMA_CACHE", expand(dir));
    }
    let parent = std::process::id() as libc::pid_t;
    // Only async-signal-safe syscalls in the fork/exec window. Check parent again
    // after prctl to close the race where the plugin died immediately after fork.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::other("plugin parent died"));
            }
            Ok(())
        });
    }
    command.spawn().map(Process).map_err(failed)
}
fn ready(
    process: &mut Process,
    port: u16,
    timeout: Duration,
    mut progress: impl FnMut() -> Result<(), RpcError>,
) -> Result<(), RpcError> {
    let start = Instant::now();
    let mut notice = Instant::now();
    let http = agent(1);
    progress()?;
    loop {
        if let Some(status) = process.0.try_wait().map_err(failed)? {
            return Err(failed(format!("llama-server exited: {status}")));
        }
        if http
            .get(&format!("http://127.0.0.1:{port}/health"))
            .call()
            .is_ok()
        {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(failed("llama-server health timeout"));
        }
        if notice.elapsed() >= Duration::from_secs(1) {
            progress()?;
            notice = Instant::now();
        }
        thread::sleep(Duration::from_millis(100));
    }
}
fn http_error(e: ureq::Error) -> RpcError {
    match e {
        ureq::Error::Status(429 | 503, _) => {
            RpcError::new(-32002, "llama-server overloaded or unavailable")
        }
        other => failed(other),
    }
}
impl Handler for Backend {
    fn handle(
        &mut self,
        method: &str,
        params: Value,
        ctx: &mut Context<'_>,
    ) -> Result<Value, RpcError> {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().map_err(internal)?;
        if method == "initialize" {
            if self.initialized {
                return Err(invalid("already initialized"));
            }
            if params["protocol"] != 1 {
                return Err(invalid("unsupported protocol"));
            }
            let config = params.get("config").cloned().unwrap_or(json!({}));
            let map = config
                .as_object()
                .ok_or_else(|| invalid("config must be an object"))?;
            for key in map.keys() {
                if ![
                    "llama_server",
                    "llama_tts",
                    "ffmpeg",
                    "models_dir",
                    "max_resident",
                    "idle_ttl_secs",
                    "load_timeout_secs",
                    "request_timeout_secs",
                    "threads",
                    "ctx",
                    "batch",
                ]
                .contains(&key.as_str())
                {
                    return Err(invalid(format!("unknown config: {key}")));
                }
            }
            for key in ["llama_server", "llama_tts", "ffmpeg", "models_dir"] {
                if let Some(v) = config.get(key)
                    && v.as_str().is_none_or(str::is_empty)
                {
                    return Err(invalid(format!("{key} must be a nonempty string")));
                }
            }
            self.limit = usize::try_from(positive(&config, "max_resident", 2)?).map_err(invalid)?;
            for (i, key) in ["ctx", "batch", "threads"].iter().enumerate() {
                self.defaults[i] = u32::try_from(positive(&config, key, self.defaults[i] as u64)?)
                    .map_err(invalid)?;
            }
            self.load_timeout = Duration::from_secs(positive(&config, "load_timeout_secs", 600)?);
            state.ttl = Duration::from_secs(positive(&config, "idle_ttl_secs", 600)?);
            self.agent = agent(positive(&config, "request_timeout_secs", 600)?);
            self.request_timeout =
                Duration::from_secs(positive(&config, "request_timeout_secs", 600)?);
            self.tts_executable = config["llama_tts"].as_str().map(expand);
            if let Some(v) = config["ffmpeg"].as_str() {
                self.ffmpeg = expand(v);
            }
            if let Some(v) = config["llama_server"].as_str() {
                self.executable = expand(v);
            }
            self.models_dir = config["models_dir"].as_str().map(str::to_owned);
            self.initialized = true;
            return Ok(
                json!({"name":"llamacpp","version":env!("CARGO_PKG_VERSION"),"provides":["backend"],"capabilities":["chat","decide","stream","transcribe","speak"]}),
            );
        }
        if !self.initialized {
            return Err(invalid("initialize first"));
        }
        state.reap();
        if method == "backend/status" {
            return Ok(
                json!({"loaded":state.models.iter().map(|(key,r)| json!({"key":key,"pid":r.process.as_ref().map(|p|p.0.id()),"port":r.process.as_ref().map(|_|r.port),"idle_secs":r.used.elapsed().as_secs(),"pinned":r.pinned})).collect::<Vec<_>>()}),
            );
        }
        if ![
            "backend/load",
            "backend/unload",
            "backend/chat",
            "backend/decide",
            "backend/transcribe",
            "backend/speak",
        ]
        .contains(&method)
        {
            return Err(RpcError::new(-32601, "Method not found"));
        }
        let key = params["key"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("missing key"))?;
        if method == "backend/unload" {
            state.models.remove(key);
            return Ok(json!({}));
        }
        if method == "backend/load" {
            let mut model: ModelSpec =
                json::from_value(params["model"].clone()).map_err(invalid)?;
            model.ctx.get_or_insert(self.defaults[0]);
            model.batch.get_or_insert(self.defaults[1]);
            model.threads.get_or_insert(self.defaults[2]);
            let pinned = match params.get("pinned") {
                None => false,
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| invalid("pinned must be boolean"))?,
            };
            let listener = TcpListener::bind("127.0.0.1:0").map_err(failed)?;
            let port = listener.local_addr().map_err(failed)?.port();
            let args = arguments(&model, port)?;
            if let Some(r) = state.models.get_mut(key) {
                if r.model != model {
                    return Err(invalid(
                        "key already loaded with a different model; unload first",
                    ));
                }
                if pinned && r.process.is_none() {
                    drop(listener);
                    let mut process = spawn(&self.executable, &args, self.models_dir.as_deref())?;
                    ready(&mut process, port, self.load_timeout, || {
                        ctx.progress("Reloading suspended model for pinning", None)
                            .map_err(internal)
                    })?;
                    r.port = port;
                    r.process = Some(process);
                }
                r.pinned |= pinned;
                r.used = Instant::now();
                return Ok(
                    json!({"key":key,"ready":true,"info":{"pid":r.process.as_ref().map(|p|p.0.id()),"port":r.process.as_ref().map(|_|r.port)}}),
                );
            }
            state.room(self.limit)?;
            drop(listener); // llama-server must bind itself; health timeout handles bind failure.
            let mut process = spawn(&self.executable, &args, self.models_dir.as_deref())?;
            ready(&mut process, port, self.load_timeout, || {
                ctx.progress("Waiting for llama-server (loading/downloading model)", None)
                    .map_err(internal)
            })?;
            ctx.progress("Model ready", Some(1.0)).map_err(internal)?;
            let pid = process.0.id();
            state.models.insert(
                key.into(),
                Resident {
                    process: Some(process),
                    model,
                    port,
                    pinned,
                    used: Instant::now(),
                },
            );
            return Ok(json!({"key":key,"ready":true,"info":{"pid":pid,"port":port}}));
        }
        if method == "backend/speak" {
            return audio::speak(self, &mut state, &params);
        }
        let resident = state
            .models
            .get_mut(key)
            .ok_or_else(|| failed("model not loaded"))?;
        if resident.process.is_none() {
            let listener = TcpListener::bind("127.0.0.1:0").map_err(failed)?;
            resident.port = listener.local_addr().map_err(failed)?.port();
            let args = arguments(&resident.model, resident.port)?;
            drop(listener);
            let mut process = spawn(&self.executable, &args, self.models_dir.as_deref())?;
            ready(&mut process, resident.port, self.load_timeout, || Ok(()))?;
            resident.process = Some(process);
        }
        let base = format!("http://127.0.0.1:{}", resident.port);
        let result = if method == "backend/transcribe" {
            audio::transcribe(self, &base, &params)
        } else if method == "backend/decide" {
            let _: lh_protocol::DecideRequest =
                json::from_value(params.clone()).map_err(invalid)?;
            let mut body = params.clone();
            body.as_object_mut().unwrap().remove("key");
            self.agent
                .post(&format!("{base}/v1/systemone"))
                .send_json(body)
                .map_err(http_error)
                .and_then(|r| r.into_json::<Value>().map_err(failed))
        } else {
            chat(&self.agent, &base, &params, |delta| {
                ctx.stream(delta).map_err(internal)
            })
        };
        resident.used = Instant::now();
        result
    }
}
fn chat(
    http: &ureq::Agent,
    base: &str,
    params: &Value,
    emit: impl FnMut(Value) -> Result<(), RpcError>,
) -> Result<Value, RpcError> {
    let request: lh_protocol::ChatRequest = json::from_value(params.clone()).map_err(invalid)?;
    let mut body = request
        .params
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("params must be an object"))?;
    body.insert("messages".into(), params["messages"].clone());
    body.insert("stream".into(), json!(request.stream));
    if let Some(tools) = request.tools {
        body.insert("tools".into(), json!(tools));
    }
    if request.stream {
        body.insert("stream_options".into(), json!({"include_usage":true}));
    }
    let response = http
        .post(&format!("{base}/v1/chat/completions"))
        .send_json(Value::Object(body))
        .map_err(http_error)?;
    if request.stream {
        return sse(response.into_reader(), emit);
    }
    let value: Value = response.into_json().map_err(failed)?;
    let choice = &value["choices"][0];
    if !choice["message"].is_object() {
        return Err(failed("missing chat choice/message"));
    }
    Ok(
        json!({"message":choice["message"],"finish_reason":choice["finish_reason"],"usage":value["usage"]}),
    )
}
use lh_protocol::chat::sse;
fn main() -> std::io::Result<()> {
    lh_plugin_sdk::serve(Backend::default())
}

#[cfg(all(test, target_os = "linux"))]
mod crash_tests;
#[cfg(test)]
mod tests;
