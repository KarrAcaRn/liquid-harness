#![cfg(feature = "server")]
//! Model lifecycle against the real `lh serve` binary and the mock backend in
//! slow-load mode: warm start, single load under concurrency, memory-budget
//! eviction, crash restart, status states, loading events and shutdown.
use lh_protocol::json::{self, Value, json};
use std::{
    io::{Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Server {
    child: Child,
    directory: PathBuf,
    address: String,
}
impl Server {
    fn start(config: &str) -> Self {
        Self::start_with(config, None)
    }
    /// `extra`: an additional plugin manifest (e.g. an agent specialist).
    fn start_with(config: &str, extra: Option<&str>) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "lh-lifecycle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(directory.join("liquid-harness")).unwrap();
        std::fs::write(
            directory.join("liquid-harness/config.toml"),
            format!("default_specialist='mock-text'\n{config}"),
        )
        .unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut plugins = vec![
            root.join("plugins/mock-backend"),
            root.join("tests/fixtures/mock-pack"),
        ];
        if let Some(manifest) = extra {
            let dir = directory.join("extra");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("plugin.toml"), manifest).unwrap();
            plugins.push(dir);
        }
        let stderr = std::fs::File::create(directory.join("server.stderr")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_lh"))
            .current_dir(&directory)
            .env_remove("LH_CONFIG")
            .env("XDG_CONFIG_HOME", &directory)
            .env("LH_PLUGIN_PATH", std::env::join_paths(plugins).unwrap())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    Path::new(env!("CARGO_BIN_EXE_lh"))
                        .parent()
                        .unwrap()
                        .display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .args(["serve", "--bind", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .unwrap();
        let mut server = Self {
            child,
            directory,
            address: String::new(),
        };
        let start = Instant::now();
        loop {
            let error = server.stderr();
            if let Some(status) = server.child.try_wait().unwrap() {
                panic!("server exited {status}: {}", server.stderr());
            }
            if let Some(address) = error.split_inclusive('\n').find_map(|line| {
                line.strip_suffix('\n')?
                    .strip_prefix("lh server listening on ")
                    .map(str::to_owned)
            }) {
                server.address = address;
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "startup: {error}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        server
    }
    fn stderr(&self) -> String {
        std::fs::read_to_string(self.directory.join("server.stderr")).unwrap_or_default()
    }
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> (u16, String, String) {
        let mut socket = TcpStream::connect(&self.address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        write!(socket, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        let mut response = Vec::new();
        socket
            .read_to_end(&mut response)
            .unwrap_or_else(|e| panic!("{method} {path}: {e}; stderr={}", self.stderr()));
        let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let headers = String::from_utf8(response[..split].to_vec())
            .unwrap()
            .to_lowercase();
        let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut body = response[split + 4..].to_vec();
        if headers.contains("transfer-encoding: chunked") {
            let mut decoded = Vec::new();
            let mut rest = body.as_slice();
            loop {
                let end = rest.windows(2).position(|w| w == b"\r\n").unwrap();
                let size =
                    usize::from_str_radix(std::str::from_utf8(&rest[..end]).unwrap(), 16).unwrap();
                if size == 0 {
                    break;
                }
                rest = &rest[end + 2..];
                decoded.extend_from_slice(&rest[..size]);
                rest = &rest[size + 2..];
            }
            body = decoded;
        }
        (status, headers, String::from_utf8(body).unwrap())
    }
    fn status(&self) -> Value {
        let (code, _, body) = self.request("GET", "/lh/v1/status", None);
        assert_eq!(code, 200, "{body}");
        json::from_str(&body).unwrap()
    }
    fn chat(&self, model: &str, stream: bool) -> (u16, String) {
        let (code, _, body) = self.request(
            "POST",
            "/v1/chat/completions",
            Some(json!({"model":model,"stream":stream,"lh_events":stream,
                "messages":[{"role":"user","content":format!("hello {model}")}]})),
        );
        (code, body)
    }
    fn wait_for(&self, what: &str, timeout: Duration, ready: impl Fn(&Value) -> bool) -> Value {
        let start = Instant::now();
        loop {
            let status = self.status();
            if ready(&status) {
                return status;
            }
            assert!(
                start.elapsed() < timeout,
                "timed out waiting for {what}: {status}\n{}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    /// SIGTERM and wait for a clean exit.
    fn terminate(&mut self) -> std::process::ExitStatus {
        signal(self.child.id(), "TERM");
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "no exit after SIGTERM"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            signal(self.child.id(), "TERM");
            std::thread::sleep(Duration::from_millis(300));
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn signal(pid: u32, name: &str) {
    let ok = Command::new("kill")
        .args([format!("-{name}"), pid.to_string()])
        .status()
        .unwrap()
        .success();
    assert!(ok, "kill -{name} {pid}");
}
fn model<'a>(status: &'a Value, key: &str) -> &'a Value {
    status["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["key"] == key)
        .unwrap_or_else(|| panic!("no model {key} in {status}"))
}
fn state<'a>(status: &'a Value, key: &str) -> &'a str {
    model(status, key)["state"].as_str().unwrap()
}
fn mock_status(status: &Value) -> &Value {
    status["resident_models"]
        .as_array()
        .and_then(|backends| backends.iter().find(|b| b["backend"] == "mock"))
        .unwrap_or_else(|| panic!("no mock backend status in {status}"))
}
/// Fresh (non-idempotent) loads the mock backend performed for `key`.
fn backend_loads(status: &Value, key: &str) -> u64 {
    mock_status(status)["loaded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["key"] == key)
        .and_then(|m| m["loads"].as_u64())
        .unwrap_or_else(|| panic!("{key} not loaded in {status}"))
}
/// A process that is gone or a zombie no longer holds memory.
fn alive(pid: u64) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| s.rsplit_once(") ").map(|(_, r)| !r.starts_with('Z')))
        .unwrap_or(false)
}
fn model_pids(status: &Value) -> Vec<u64> {
    status["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["pid"].as_u64())
        .collect()
}
const SLOW: &str = "[plugins.mock.config]\nload_delay_ms = 1200\nmodel_processes = true\nram_bytes = 1000\nenable_test_methods = true\n";

#[test]
fn warm_start_is_nonblocking_then_router_and_preload_become_ready() {
    let mut server = Server::start(&format!(
        "router='mock-router'\n[lifecycle]\npreload=['mock-text']\nkeep_warm=true\n{SLOW}"
    ));
    // The HTTP server answers while the router is still loading.
    let started = Instant::now();
    let first = server.status();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(first["warmup"]["state"], "running", "{first}");
    let loading = server.wait_for("router loading", Duration::from_secs(5), |s| {
        state(s, "router:mock-router") == "loading"
            && model(s, "router:mock-router")["progress"].is_f64()
    });
    assert_eq!(loading["engine"], "busy");
    let router = model(&loading, "router:mock-router");
    assert_eq!(router["kind"], "router");
    assert_eq!(router["pinned"], true);
    assert_eq!(router["ram_estimate_bytes"], 1000);
    assert!(router["progress"].as_f64().is_some(), "{router}");
    assert_eq!(router["progress_source"], "backend");
    assert_eq!(state(&loading, "mock-vision"), "stopped");
    let ready = server.wait_for("warm-up done", Duration::from_secs(10), |s| {
        s["warmup"]["state"] == "done" && s["engine"] == "idle"
    });
    assert_eq!(state(&ready, "router:mock-router"), "ready");
    assert_eq!(state(&ready, "mock-text"), "ready");
    assert_eq!(model(&ready, "mock-text")["keep_warm"], true);
    assert_eq!(model(&ready, "mock-text")["preload"], true);
    assert_eq!(state(&ready, "mock-vision"), "stopped");
    assert_eq!(ready["budget"]["resident_estimate_bytes"], 2000);
    // Both are already resident: the first request starts nothing.
    let started = Instant::now();
    let (code, body) = server.chat("mock-text", false);
    assert_eq!(code, 200, "{body}");
    assert!(started.elapsed() < Duration::from_millis(1000));
    let after = server.status();
    assert_eq!(mock_status(&after)["loaded"].as_array().unwrap().len(), 2);
    assert_eq!(backend_loads(&after, "router:mock-router"), 1, "{after}");
    assert_eq!(backend_loads(&after, "mock-text"), 1, "{after}");
    let pids = model_pids(&after);
    assert_eq!(pids.len(), 2, "{after}");
    assert!(server.terminate().success(), "{}", server.stderr());
    std::thread::sleep(Duration::from_millis(200));
    for pid in pids {
        assert!(!alive(pid), "model process {pid} survived shutdown");
    }
}

#[test]
fn concurrent_requests_during_a_load_start_one_process_and_see_loading_events() {
    let server = Server::start(SLOW);
    let bodies: Vec<(u16, String)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..3)
            .map(|_| scope.spawn(|| server.chat("mock-text", true)))
            .collect();
        workers.into_iter().map(|w| w.join().unwrap()).collect()
    });
    for (code, body) in &bodies {
        assert_eq!(*code, 200, "{body}");
        assert!(body.contains("data: [DONE]"), "{body}");
        assert!(body.contains(r#""finish_reason":"stop""#), "{body}");
        // Keep-alive comments for every OpenAI stream, opt-in named events.
        assert!(body.contains(": lh model_loading mock-text"), "{body}");
        let events: Vec<Value> = body
            .split("\n\n")
            .filter_map(|frame| frame.strip_prefix("event: model_loading\ndata: "))
            .map(|data| json::from_str(data).unwrap())
            .collect();
        assert!(!events.is_empty(), "no model_loading event: {body}");
        assert!(events.iter().all(|e| e["specialist"] == "mock-text"));
        // The first tick may precede the backend's first `$/progress`; a
        // 1.2 s load at a 500 ms tick reports progress on a later one.
        assert!(
            events.iter().any(|e| e["progress"].as_f64().is_some()),
            "{body}"
        );
    }
    let status = server.status();
    assert_eq!(backend_loads(&status, "mock-text"), 1, "{status}");
    assert_eq!(model_pids(&status).len(), 1, "{status}");
    assert_eq!(model(&status, "mock-text")["loads"], 1);
    // Without opt-in an OpenAI client only sees comments.
    let (code, body) = server.chat("mock-vision", true);
    assert_eq!(code, 200);
    let (_, _, plain) = server.request(
        "POST",
        "/v1/chat/completions",
        Some(
            json!({"model":"mock-audio","stream":true,"messages":[{"role":"user","content":"x"}]}),
        ),
    );
    assert!(plain.contains(": lh model_loading mock-audio"), "{plain}");
    assert!(!plain.contains("event: model_loading"), "{plain}");
    assert!(body.contains("event: model_loading"));
}

#[test]
fn memory_budget_evicts_least_recently_used_never_the_router() {
    let server = Server::start(
        "router='mock-router'\n[lifecycle]\nmemory_budget=3500\n[plugins.mock.config]\nram_bytes=1000\nmodel_processes=true\n",
    );
    server.wait_for("router", Duration::from_secs(5), |s| {
        state(s, "router:mock-router") == "ready"
    });
    for id in ["mock-text", "mock-vision", "mock-text"] {
        assert_eq!(server.chat(id, false).0, 200);
    }
    let both = server.status();
    assert_eq!(both["budget"]["memory_budget_bytes"], 3500);
    assert_eq!(both["budget"]["resident_estimate_bytes"], 3000);
    // Router + text + vision fill 3000 of 3500: audio forces one eviction,
    // the least recently used specialist (vision; text was used after it).
    assert_eq!(server.chat("mock-audio", false).0, 200);
    let status = server.status();
    assert_eq!(state(&status, "router:mock-router"), "ready");
    assert_eq!(state(&status, "mock-text"), "ready");
    assert_eq!(state(&status, "mock-audio"), "ready");
    assert_eq!(state(&status, "mock-vision"), "stopped");
    assert_eq!(
        model(&status, "mock-vision")["reason"],
        "evicted (memory budget)"
    );
    let loaded: Vec<&str> = mock_status(&status)["loaded"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["key"].as_str().unwrap())
        .collect();
    assert_eq!(loaded, ["mock-audio", "mock-text", "router:mock-router"]);
    assert!(
        server
            .stderr()
            .contains("lh: unloaded mock-vision (memory budget)")
    );
}

#[test]
fn crashed_model_process_is_restarted_on_the_next_request() {
    let server = Server::start("[plugins.mock.config]\nmodel_processes=true\n");
    assert_eq!(server.chat("mock-text", false).0, 200);
    let before = server.status();
    let pid = model(&before, "mock-text")["pid"].as_u64().unwrap();
    signal(pid as u32, "KILL");
    let stopped = server.wait_for("crash noticed", Duration::from_secs(5), |s| {
        state(s, "mock-text") == "stopped"
    });
    assert!(
        model(&stopped, "mock-text")["reason"]
            .as_str()
            .unwrap()
            .contains("no longer resident"),
        "{stopped}"
    );
    let (code, body) = server.chat("mock-text", false);
    assert_eq!(code, 200, "{body}");
    let after = server.status();
    assert_eq!(state(&after, "mock-text"), "ready");
    assert_eq!(backend_loads(&after, "mock-text"), 2);
    let restarted = model(&after, "mock-text")["pid"].as_u64().unwrap();
    assert_ne!(restarted, pid);
    assert!(alive(restarted));
}

#[test]
fn failed_load_is_reported_in_status() {
    let server = Server::start("[plugins.mock.config]\nfail_load_keys=['mock-vision']\n");
    let (code, body) = server.chat("mock-vision", false);
    assert_eq!(code, 502, "{body}");
    let status = server.status();
    let vision = model(&status, "mock-vision");
    assert_eq!(vision["state"], "failed");
    assert!(
        vision["error"]
            .as_str()
            .unwrap()
            .contains("configured load failure"),
        "{vision}"
    );
    assert_eq!(state(&status, "mock-text"), "stopped");
}

#[test]
fn agent_run_waits_for_warm_up_and_streams_model_loading() {
    let server = Server::start_with(
        &format!("router='mock-router'\n{SLOW}"),
        Some(
            "[plugin]\nid='agent-pack'\nversion='0.1.0'\nprovides=['specialist']\n[[specialist]]\nid='mock-agent'\nroute='Agent'\ntask='chat'\ninputs=['text']\noutputs=['text']\nagent=true\nbackend='mock'\nmodel={source='file:mock-agent'}\n",
        ),
    );
    // Arrives during the router warm-up: waits (no 409) and reports the
    // router and then its own specialist loading as events.
    let (code, headers, body) = server.request(
        "POST",
        "/lh/v1/agent/runs",
        Some(json!({"model":"mock-agent","messages":[{"role":"user","content":"hi"}]})),
    );
    assert_eq!(code, 200, "{body}");
    assert!(headers.contains("x-lh-run-id"));
    let events: Vec<Value> = body
        .split("\n\n")
        .filter_map(|frame| frame.split_once("\ndata: ").map(|(_, data)| data))
        .map(|data| json::from_str(data).unwrap())
        .collect();
    let loading: Vec<&Value> = events
        .iter()
        .filter(|e| e["event"] == "model_loading")
        .collect();
    assert!(
        loading
            .iter()
            .any(|e| e["data"]["specialist"] == "mock-agent"),
        "{body}"
    );
    assert!(loading.iter().all(|e| e["run_id"].is_string()));
    assert_eq!(events.last().unwrap()["event"], "done", "{body}");
    let status = server.status();
    assert_eq!(state(&status, "router:mock-router"), "ready", "{status}");
    assert_eq!(state(&status, "mock-agent"), "ready", "{status}");
}

#[test]
fn agent_run_rejected_after_waiting_for_warm_up() {
    let server = Server::start(&format!("router='mock-router'\n{SLOW}"));
    let (code, _, body) = server.request(
        "POST",
        "/lh/v1/agent/runs",
        Some(json!({"model":"mock-text","messages":[{"role":"user","content":"hi"}]})),
    );
    // mock-text is not an agent specialist: the request waited for warm-up
    // (no 409) and was then rejected as a normal validation error.
    assert_eq!(code, 400, "{body}");
    assert!(body.contains("not an agent"), "{body}");
    let status = server.status();
    assert_eq!(state(&status, "router:mock-router"), "ready", "{status}");
}
