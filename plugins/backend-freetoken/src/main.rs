//! Serial FreeToken adapter. The shared lock protects active loads/inference
//! from eviction by the idle worker. Only explicitly configured models can run.
use lh_plugin_sdk::{Context, Handler};
use lh_protocol::{
    ModelSpec, RpcError,
    json::{self, Value, json},
};
use serde::Deserialize;
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

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    #[serde(alias = "base_url")]
    url: Option<String>,
    model: Option<String>,
    spawn: Option<Spawn>,
    token_env: Option<String>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Spawn {
    #[serde(default = "ft")]
    command: String,
    venv: Option<String>,
    model_path: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}
fn ft() -> String {
    "ft".into()
}
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    models: BTreeMap<String, Endpoint>,
    max_resident: usize,
    idle_ttl_secs: u64,
    load_timeout_secs: u64,
    request_timeout_secs: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            models: BTreeMap::new(),
            max_resident: 2,
            idle_ttl_secs: 600,
            load_timeout_secs: 600,
            request_timeout_secs: 600,
        }
    }
}
impl Config {
    fn validate(&self) -> Result<(), RpcError> {
        if self.max_resident == 0
            || self.idle_ttl_secs == 0
            || self.load_timeout_secs == 0
            || self.request_timeout_secs == 0
        {
            return Err(invalid("resident limit and timeouts must be positive"));
        }
        for (name, e) in &self.models {
            if e.model.as_ref().is_some_and(String::is_empty) {
                return Err(invalid("endpoint model must be nonempty"));
            }
            if name.is_empty() || e.url.is_some() == e.spawn.is_some() {
                return Err(invalid(
                    "each model requires a name and exactly one of url or spawn",
                ));
            }
            if let Some(url) = &e.url {
                // Reject credentials, queries and fragments so a base URL cannot
                // accidentally leak credentials or alter API routes.
                let parsed = url.as_str();
                if !(parsed.starts_with("http://") || parsed.starts_with("https://"))
                    || url.contains(['?', '#', '@'])
                    || url.trim() != url
                {
                    return Err(invalid(
                        "url must be an http(s) base URL without credentials, query or fragment",
                    ));
                }
                let authority = url
                    .split("://")
                    .nth(1)
                    .unwrap_or("")
                    .split('/')
                    .next()
                    .unwrap_or("");
                if authority.is_empty() {
                    return Err(invalid("url has no host"));
                }
            }
            if e.token_env
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.contains(['=', '\0']))
            {
                return Err(invalid("invalid token_env name"));
            }
            if let Some(s) = &e.spawn {
                if s.command.is_empty()
                    || s.model_path.is_empty()
                    || s.model_path.starts_with('-')
                    || s.venv.as_ref().is_some_and(String::is_empty)
                {
                    return Err(invalid(
                        "spawn command, model_path and venv must be nonempty; model_path must not begin with '-'",
                    ));
                }
                if s.args.iter().any(|a| {
                    ["--port", "--model-path"]
                        .iter()
                        .any(|flag| a == flag || a.starts_with(&format!("{flag}=")))
                }) {
                    return Err(invalid(
                        "spawn args must not override --port or --model-path",
                    ));
                }
                if s.env
                    .iter()
                    .any(|(k, v)| k.is_empty() || k.contains(['=', '\0']) || v.contains('\0'))
                {
                    return Err(invalid("invalid spawn environment"));
                }
            }
        }
        Ok(())
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
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn spawn(config: &Spawn, port: u16) -> Result<Process, RpcError> {
    let executable = match &config.venv {
        Some(venv) if !config.command.contains('/') => {
            format!("{}/bin/{}", expand(venv), config.command)
        }
        _ => expand(&config.command),
    };
    let mut cmd = Command::new(executable);
    cmd.args([
        "serve",
        "--model-path",
        &expand(&config.model_path),
        "--port",
        &port.to_string(),
    ])
    .args(&config.args)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::inherit());
    if let Some(venv) = &config.venv {
        let venv = expand(venv);
        cmd.env("VIRTUAL_ENV", &venv).env(
            "PATH",
            format!("{venv}/bin:{}", std::env::var("PATH").unwrap_or_default()),
        );
    }
    cmd.envs(&config.env);
    let parent = std::process::id() as libc::pid_t;
    // Only async-signal-safe operations between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
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
    cmd.spawn().map(Process).map_err(failed)
}
struct Resident {
    process: Option<Process>,
    model: ModelSpec,
    api_model: String,
    base: String,
    token: Option<String>,
    port: Option<u16>,
    pinned: bool,
    used: Instant,
}
impl Resident {
    fn info(&self) -> Value {
        json!({"pid":self.process.as_ref().map(|p|p.0.id()),"port":self.port})
    }
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
fn agent(seconds: u64) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(seconds))
        .redirects(0)
        .build()
}
fn auth(request: ureq::Request, token: Option<&str>) -> ureq::Request {
    match token {
        Some(token) => request.set("Authorization", &format!("Bearer {token}")),
        None => request,
    }
}
fn http_error(e: ureq::Error) -> RpcError {
    match e {
        ureq::Error::Status(429 | 503, _) => {
            RpcError::new(-32002, "FreeToken overloaded or unavailable")
        }
        // Do not include request URLs or bearer tokens in errors.
        ureq::Error::Status(code, _) => failed(format!("FreeToken HTTP {code}")),
        ureq::Error::Transport(_) => failed("FreeToken HTTP transport failure"),
    }
}
fn ready(
    resident: &mut Resident,
    timeout: Duration,
    mut progress: impl FnMut() -> Result<(), RpcError>,
) -> Result<(), RpcError> {
    let start = Instant::now();
    let mut notice = Instant::now();
    let http = agent(1);
    progress()?;
    loop {
        if let Some(p) = &mut resident.process
            && let Some(status) = p.0.try_wait().map_err(failed)?
        {
            return Err(failed(format!("ft exited: {status}")));
        }
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(failed(
                "FreeToken readiness timeout (cache state is not serving)",
            ));
        }
        let response = auth(
            http.get(&format!("{}/v1/cache/status", resident.base))
                .timeout(remaining.min(Duration::from_secs(1))),
            resident.token.as_deref(),
        )
        .call();
        match response {
            Ok(response) => {
                if let Ok(value) = response.into_json::<Value>()
                    && value["state"] == "serving"
                {
                    return Ok(());
                }
            }
            Err(e @ ureq::Error::Status(401 | 403, _)) => return Err(http_error(e)),
            _ => {}
        }
        if notice.elapsed() >= Duration::from_secs(1) {
            progress()?;
            notice = Instant::now();
        }
        thread::sleep(Duration::from_millis(100).min(timeout.saturating_sub(start.elapsed())));
    }
}
struct Backend {
    state: Arc<(Mutex<State>, Condvar)>,
    worker: Option<thread::JoinHandle<()>>,
    config: Config,
    http: ureq::Agent,
    initialized: bool,
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
            config: Config::default(),
            http: agent(600),
            initialized: false,
        }
    }
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
impl Handler for Backend {
    fn handle(
        &mut self,
        method: &str,
        params: Value,
        ctx: &mut Context<'_>,
    ) -> Result<Value, RpcError> {
        let mut state = self.state.0.lock().map_err(internal)?;
        if method == "initialize" {
            if self.initialized || params["protocol"] != 1 {
                return Err(invalid("already initialized or unsupported protocol"));
            }
            let config: Config =
                json::from_value(params.get("config").cloned().unwrap_or(json!({})))
                    .map_err(invalid)?;
            config.validate()?;
            state.ttl = Duration::from_secs(config.idle_ttl_secs);
            self.http = agent(config.request_timeout_secs);
            self.config = config;
            self.initialized = true;
            return Ok(
                json!({"name":"freetoken","version":env!("CARGO_PKG_VERSION"),"provides":["backend"],"capabilities":["chat"]}),
            );
        }
        if !self.initialized {
            return Err(invalid("initialize first"));
        }
        state.reap();
        if method == "backend/decide" {
            return Err(RpcError::new(-32003, "FreeToken supports chat, not decide"));
        }
        if method == "backend/status" {
            return Ok(
                json!({"loaded": state.models.iter().map(|(key,r)| json!({"key":key,"pid":r.process.as_ref().map(|p|p.0.id()),"port":r.port,"pinned":r.pinned,"idle_secs":r.used.elapsed().as_secs()})).collect::<Vec<_>>()}),
            );
        }
        if !["backend/load", "backend/unload", "backend/chat"].contains(&method) {
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
            let model: ModelSpec = json::from_value(params["model"].clone()).map_err(invalid)?;
            if model.ctx.is_some()
                || model.batch.is_some()
                || model.threads.is_some()
                || model.mmproj.is_some()
            {
                return Err(invalid(
                    "FreeToken uses spawn.args, not llama.cpp ModelSpec options",
                ));
            }
            let pinned = params
                .get("pinned")
                .map(|v| v.as_bool().ok_or_else(|| invalid("pinned must be boolean")))
                .transpose()?
                .unwrap_or(false);
            if let Some(r) = state.models.get_mut(key) {
                if r.model != model {
                    return Err(invalid(
                        "key already loaded with a different model; unload first",
                    ));
                }
                r.pinned |= pinned;
                r.used = Instant::now();
                return Ok(json!({"key":key,"ready":true,"info":r.info()}));
            }
            let endpoint = self
                .config
                .models
                .get(&model.source)
                .ok_or_else(|| failed("model.source has no configured FreeToken endpoint"))?;
            let token = endpoint
                .token_env
                .as_ref()
                .map(|name| {
                    std::env::var(name).map_err(|_| {
                        invalid(format!("token environment variable {name} is missing"))
                    })
                })
                .transpose()?;
            if token
                .as_ref()
                .is_some_and(|s| s.is_empty() || !s.bytes().all(|b| (0x21..=0x7e).contains(&b)))
            {
                return Err(invalid("bearer token must be nonempty visible ASCII"));
            }
            state.room(self.config.max_resident)?;
            let (process, base, port) = if let Some(config) = &endpoint.spawn {
                let listener = TcpListener::bind("127.0.0.1:0").map_err(failed)?;
                let port = listener.local_addr().map_err(failed)?.port();
                drop(listener); // ft must bind itself; readiness handles a port race.
                (
                    Some(spawn(config, port)?),
                    format!("http://127.0.0.1:{port}"),
                    Some(port),
                )
            } else {
                (
                    None,
                    endpoint
                        .url
                        .as_ref()
                        .unwrap()
                        .trim_end_matches('/')
                        .to_owned(),
                    None,
                )
            };
            let mut resident = Resident {
                process,
                api_model: endpoint.model.clone().unwrap_or_else(|| {
                    endpoint
                        .spawn
                        .as_ref()
                        .map(|s| s.model_path.clone())
                        .unwrap_or_else(|| {
                            model
                                .source
                                .strip_prefix("hf:")
                                .unwrap_or(&model.source)
                                .to_owned()
                        })
                }),
                model,
                base,
                token,
                port,
                pinned,
                used: Instant::now(),
            };
            ready(
                &mut resident,
                Duration::from_secs(self.config.load_timeout_secs),
                || {
                    ctx.progress("Waiting for FreeToken cache state serving", None)
                        .map_err(internal)
                },
            )?;
            ctx.progress("Model ready", Some(1.0)).map_err(internal)?;
            resident.used = Instant::now();
            let info = resident.info();
            state.models.insert(key.into(), resident);
            return Ok(json!({"key":key,"ready":true,"info":info}));
        }
        let resident = state
            .models
            .get_mut(key)
            .ok_or_else(|| failed("model not loaded"))?;
        let result = chat(&self.http, resident, &params, |delta| {
            ctx.stream(delta).map_err(internal)
        });
        resident.used = Instant::now();
        result
    }
}
fn chat(
    http: &ureq::Agent,
    resident: &Resident,
    params: &Value,
    emit: impl FnMut(Value) -> Result<(), RpcError>,
) -> Result<Value, RpcError> {
    let request: lh_protocol::ChatRequest = json::from_value(params.clone()).map_err(invalid)?;
    for message in &request.messages {
        if let Some(lh_protocol::Content::Parts(parts)) = &message.content
            && parts
                .iter()
                .any(|part| !matches!(part, lh_protocol::ContentPart::Text { .. }))
        {
            return Err(RpcError::new(-32003, "FreeToken supports text only"));
        }
    }
    let mut body = request
        .params
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("params must be an object"))?;
    body.insert("model".into(), json!(resident.api_model));
    body.insert("messages".into(), params["messages"].clone());
    body.insert("stream".into(), json!(request.stream));
    if let Some(tools) = request.tools {
        body.insert("tools".into(), json!(tools));
    }
    if request.stream {
        body.insert("stream_options".into(), json!({"include_usage":true}));
    }
    let response = auth(
        http.post(&format!("{}/v1/chat/completions", resident.base)),
        resident.token.as_deref(),
    )
    .send_json(Value::Object(body))
    .map_err(http_error)?;
    if request.stream {
        return lh_protocol::chat::sse(response.into_reader(), emit);
    }
    let value: Value = response.into_json().map_err(failed)?;
    let choice = &value["choices"][0];
    if !choice["message"].is_object() {
        return Err(failed("missing chat choice/message"));
    }
    let mut result = json!({"message":choice["message"],"finish_reason":choice["finish_reason"],"usage":value["usage"]});
    if value["timings"].is_object() {
        result["timings"] = value["timings"].clone();
    }
    Ok(result)
}
fn main() -> std::io::Result<()> {
    lh_plugin_sdk::serve(Backend::default())
}

#[cfg(test)]
mod tests;
