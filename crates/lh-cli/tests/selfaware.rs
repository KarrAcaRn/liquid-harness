#![cfg(feature = "server")]
//! Self-description and in-place restart against the real `lh serve` binary
//! with the mock backend: `GET /lh/v1/self`, `POST /lh/v1/admin/restart`,
//! SIGHUP, and the agent's `harness_info`/`harness_restart` tools with approval.
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

const AGENT_PACK: &str = "[plugin]\nid='agent-pack'\nversion='0.1.0'\nprovides=['specialist']\n[[specialist]]\nid='mock-agent'\nroute='Agent'\ntask='chat'\ninputs=['text']\noutputs=['text']\nagent=true\nbackend='mock'\nmodel={source='file:mock-agent'}\n";

struct Server {
    child: Child,
    directory: PathBuf,
    address: String,
    token: Option<String>,
}
impl Server {
    fn start(config: &str, token: Option<&str>) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "lh-selfaware-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(directory.join("liquid-harness")).unwrap();
        std::fs::write(
            directory.join("liquid-harness/config.toml"),
            format!("default_specialist='mock-text'\n{config}"),
        )
        .unwrap();
        let extra = directory.join("extra");
        std::fs::create_dir_all(&extra).unwrap();
        std::fs::write(extra.join("plugin.toml"), AGENT_PACK).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let plugins = [
            root.join("plugins/mock-backend"),
            root.join("tests/fixtures/mock-pack"),
            extra,
        ];
        let stderr = std::fs::File::create(directory.join("server.stderr")).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_lh"));
        command
            .current_dir(&directory)
            .env_remove("LH_CONFIG")
            .env_remove("LH_RESTART_COUNT")
            .env_remove("LH_RESTART_REASON")
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
            .stderr(stderr);
        if let Some(token) = token {
            command.env("LH_TEST_SERVER_TOKEN", token);
        }
        let child = command.spawn().unwrap();
        let mut server = Self {
            child,
            directory,
            address: String::new(),
            token: token.map(str::to_owned),
        };
        server.address = server.wait_listening(1);
        server
    }
    fn stderr(&self) -> String {
        std::fs::read_to_string(self.directory.join("server.stderr")).unwrap_or_default()
    }
    /// The address from the `count`-th "listening" line (1 = first start).
    fn wait_listening(&mut self, count: usize) -> String {
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("server exited {status}: {}", self.stderr());
            }
            let lines: Vec<String> = self
                .stderr()
                .lines()
                .filter_map(|l| l.strip_prefix("lh server listening on ").map(str::to_owned))
                .collect();
            if lines.len() >= count {
                return lines[count - 1].clone();
            }
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "startup #{count}: {}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn send(&self, socket: &mut TcpStream, method: &str, path: &str, body: Option<&Value>) {
        Self::send_as(self.token.as_deref(), socket, method, path, body)
    }
    fn send_as(
        token: Option<&str>,
        socket: &mut TcpStream,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) {
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let auth = token
            .map(|t| format!("Authorization: Bearer {t}\r\n"))
            .unwrap_or_default();
        write!(socket, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    }
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> (u16, String) {
        self.request_as(self.token.as_deref(), method, path, body)
    }
    fn request_as(
        &self,
        token: Option<&str>,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, String) {
        let mut socket = TcpStream::connect(&self.address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        Self::send_as(token, &mut socket, method, path, body.as_ref());
        let mut response = Vec::new();
        socket
            .read_to_end(&mut response)
            .unwrap_or_else(|e| panic!("{method} {path}: {e}; stderr={}", self.stderr()));
        let text = String::from_utf8_lossy(&response).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, body.to_owned())
    }
    fn json(&self, path: &str) -> Value {
        // Management reads fail fast (409) while the engine is briefly busy.
        let start = Instant::now();
        loop {
            let (code, body) = self.request("GET", path, None);
            if code == 409 && start.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            assert_eq!(code, 200, "{path}: {body}");
            return json::from_str(&body).unwrap();
        }
    }
    /// Waits until `/lh/v1/self` answers with the given restart count.
    fn wait_restarted(&mut self, count: u64) -> Value {
        self.wait_listening(count as usize + 1);
        let start = Instant::now();
        loop {
            if let Ok(mut socket) = TcpStream::connect(&self.address) {
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                self.send(&mut socket, "GET", "/lh/v1/self", None);
                let mut response = Vec::new();
                if socket.read_to_end(&mut response).is_ok() {
                    let text = String::from_utf8_lossy(&response);
                    if let Some((_, body)) = text.split_once("\r\n\r\n")
                        && let Ok(info) = json::from_str::<Value>(body)
                        && info["build"]["restart_count"] == count
                    {
                        return info;
                    }
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "no restart #{count}: {}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    /// Live plugin host and model process PIDs (all owned by this server).
    fn child_pids(&self) -> Vec<u64> {
        let plugins = self.json("/lh/v1/plugins");
        let status = self.json("/lh/v1/status");
        plugins["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["process"]["pid"].as_u64())
            .chain(
                status["models"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|m| m["pid"].as_u64()),
            )
            .collect()
    }
    fn wait_ready(&self, key: &str) -> Value {
        let start = Instant::now();
        loop {
            let status = self.json("/lh/v1/status");
            let ready = status["models"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["key"] == key && m["state"] == "ready");
            if ready && status["engine"] == "idle" {
                return status;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "{key} not ready: {status}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
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
fn alive(pid: u64) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| s.rsplit_once(") ").map(|(_, r)| !r.starts_with('Z')))
        .unwrap_or(false)
}
fn wait_gone(pids: &[u64]) {
    let start = Instant::now();
    while pids.iter().any(|p| alive(*p)) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "orphans survived the restart: {:?}",
            pids.iter().filter(|p| alive(**p)).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

const MODELS: &str =
    "[lifecycle]\npreload=['mock-text']\n[plugins.mock.config]\nmodel_processes=true\n";

#[test]
fn api_restart_re_executes_in_place_without_orphans() {
    let mut server = Server::start(MODELS, None);
    let pid = server.child.id() as u64;
    server.wait_ready("mock-text");
    let info = server.json("/lh/v1/self");
    assert_eq!(info["build"]["pid"], pid);
    assert_eq!(info["build"]["restart_count"], 0);
    assert_eq!(info["service"]["bind"], server.address);
    assert_eq!(info["service"]["restart"]["method"], "exec");
    assert_eq!(info["config"]["chosen_by"], "xdg-default");
    assert!(
        info["config"]["path"]
            .as_str()
            .unwrap()
            .ends_with("liquid-harness/config.toml")
    );
    let text = info["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "mock-text")
        .unwrap();
    assert_eq!(text["state"], "ready", "{text}");
    let sources: Vec<&str> = info["discovery"]["plugin_search_path"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["source"].as_str().unwrap())
        .collect();
    assert_eq!(sources[..3], ["LH_PLUGIN_PATH"; 3], "{sources:?}");
    let children = server.child_pids();
    assert!(
        children.len() >= 2,
        "plugin host + model process: {children:?}"
    );
    let (code, body) = server.request(
        "POST",
        "/lh/v1/admin/restart",
        Some(json!({"reason":"test"})),
    );
    assert_eq!(code, 200, "{body}");
    let reply: Value = json::from_str(&body).unwrap();
    assert_eq!(reply["restarting"], true);
    assert_eq!(reply["pid"], pid);
    let after = server.wait_restarted(1);
    assert_eq!(after["build"]["pid"], pid, "exec keeps the PID");
    assert_eq!(after["build"]["last_restart_reason"], "api: test");
    assert_eq!(
        after["service"]["bind"], info["service"]["bind"],
        "same port"
    );
    wait_gone(&children);
    // The M5a warm start applies again: the preload comes back by itself.
    server.wait_ready("mock-text");
    let restarted = server.child_pids();
    assert!(restarted.iter().all(|p| !children.contains(p)));
    let log = server.stderr();
    assert!(log.contains("lh: restarting (api: test)"), "{log}");
    assert!(log.contains("re-executing"), "{log}");
    // SIGHUP: the same full restart. Repeated, because the serve loop polls the
    // signal listener on every iteration (a completed listener once panicked
    // there depending on `select!` order, killing the server).
    let mut previous = restarted;
    for count in 2..=5 {
        server.wait_ready("mock-text");
        signal(pid as u32, "HUP");
        let hup = server.wait_restarted(count);
        assert_eq!(hup["build"]["pid"], pid);
        assert_eq!(hup["build"]["last_restart_reason"], "SIGHUP");
        wait_gone(&previous);
        previous = server.child_pids();
    }
    assert!(!server.stderr().contains("panicked"), "{}", server.stderr());
    server.wait_ready("mock-text");
    let last = server.child_pids();
    assert!(server.terminate().success(), "{}", server.stderr());
    wait_gone(&last);
}

#[test]
fn restart_drain_finishes_short_requests_and_cancels_long_ones() {
    for (delay, drain, expect) in [(600, 10, 200), (4000, 1, 503)] {
        let mut server = Server::start(
            &format!(
                "[service]\ndrain_timeout_secs={drain}\n[plugins.mock.config]\nload_delay_ms={delay}\n"
            ),
            None,
        );
        let (code, body) = std::thread::scope(|scope| {
            let slow = scope.spawn(|| {
                server.request(
                    "POST",
                    "/v1/chat/completions",
                    Some(json!({"model":"mock-vision","messages":[{"role":"user","content":"slow"}]})),
                )
            });
            std::thread::sleep(Duration::from_millis(200));
            let (code, body) = server.request("POST", "/lh/v1/admin/restart", Some(json!({})));
            assert_eq!(code, 200, "{body}");
            // New work is refused or not accepted at all while draining.
            if let Ok(mut socket) = TcpStream::connect(&server.address) {
                server.send(&mut socket, "GET", "/v1/models", None);
                let mut response = Vec::new();
                let _ = socket.read_to_end(&mut response);
                let text = String::from_utf8_lossy(&response);
                assert!(!text.starts_with("HTTP/1.1 200"), "{text}");
            }
            slow.join().unwrap()
        });
        assert_eq!(code, expect, "delay {delay}: {body}");
        if expect == 503 {
            assert!(body.contains("drain timeout"), "{body}");
        } else {
            assert!(body.contains("slow"), "{body}");
        }
        let after = server.wait_restarted(1);
        assert_eq!(after["build"]["pid"], server.child.id());
        assert!(server.terminate().success(), "{}", server.stderr());
    }
}

#[test]
fn self_and_restart_require_the_token_and_never_show_it() {
    let token = "selfaware-secret-token-value";
    let server = Server::start("server_token_env='LH_TEST_SERVER_TOKEN'\n", Some(token));
    assert_eq!(server.request_as(None, "GET", "/lh/v1/self", None).0, 401);
    assert_eq!(
        server
            .request_as(None, "POST", "/lh/v1/admin/restart", Some(json!({})))
            .0,
        401
    );
    let (code, body) = server.request("GET", "/lh/v1/self", None);
    assert_eq!(code, 200);
    assert!(!body.contains(token), "token leaked");
    let info: Value = json::from_str(&body).unwrap();
    assert_eq!(info["env"]["secrets"]["LH_TEST_SERVER_TOKEN"], "set");
    assert_eq!(info["config"]["server_token"]["value"], "set");
    let (code, body) = server.request("POST", "/lh/v1/admin/restart", Some(json!({"reason":5})));
    assert_eq!(code, 400, "{body}");
}

/// Streams an agent run; at each `approval_required` answers with `allow`.
fn agent_run(server: &Server, allow: bool) -> (String, Vec<Value>) {
    let mut socket = TcpStream::connect(&server.address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let body =
        json!({"model":"mock-agent","messages":[{"role":"user","content":"restart yourself"}]});
    server.send(&mut socket, "POST", "/lh/v1/agent/runs", Some(&body));
    let mut raw = Vec::new();
    let mut answered = 0;
    let mut chunk = [0u8; 4096];
    loop {
        let n = socket.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&raw).into_owned();
        let pending: Vec<&str> = text.matches("event: approval_required").collect();
        if pending.len() > answered {
            let run = text
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("x-lh-run-id: ")
                        .map(str::to_owned)
                })
                .unwrap();
            let data = text
                .split("event: approval_required\ndata: ")
                .nth(answered + 1)
                .and_then(|r| r.split("\n\n").next())
                .unwrap();
            let event: Value = json::from_str(data).unwrap();
            assert_eq!(event["data"]["name"], "harness_restart", "{event}");
            // Status and self-description answer while the run holds the engine.
            assert_eq!(server.json("/lh/v1/self")["build"]["restart_count"], 0);
            let (code, reply) = server.request(
                "POST",
                &format!("/lh/v1/agent/runs/{run}/approve"),
                Some(json!({"call_id":event["data"]["call_id"],"allow":allow,"always":true})),
            );
            assert_eq!(code, 200, "{reply}");
            answered += 1;
        }
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let events = text
        .split("\ndata: ")
        .skip(1)
        .filter_map(|d| json::from_str::<Value>(d.split("\n\n").next()?.trim()).ok())
        .collect();
    (text, events)
}

#[test]
fn agent_restart_tool_needs_approval_and_restarts_the_server() {
    let script = r#"[
  {role='assistant',content='',tool_calls=[{id='c1',type='function',function={name='harness_info',arguments='{"section":"service"}'}}]},
  {role='assistant',content='',tool_calls=[{id='c2',type='function',function={name='harness_restart',arguments='{"reason":"new binary"}'}}]},
  {role='assistant',content='Restart was not approved.'},
]"#;
    let mut server = Server::start(
        &format!(
            "[agent]\napproval='auto-edit'\ncommand_allow=['*']\n[plugins.mock.config]\nscripted_chat={script}\n"
        ),
        None,
    );
    let pid = server.child.id() as u64;
    // Denied: the model sees an error and finishes; the server keeps running.
    let (text, events) = agent_run(&server, false);
    let info = events
        .iter()
        .find(|e| e["event"] == "tool_result" && e["data"]["call_id"] == "c1")
        .unwrap_or_else(|| panic!("no harness_info result: {text}"));
    let service: Value = json::from_str(info["data"]["result"].as_str().unwrap()).unwrap();
    assert_eq!(service["section"], "service");
    assert_eq!(service["service"]["running"], true);
    assert!(text.contains("approval denied"), "{text}");
    assert!(text.contains("Restart was not approved."), "{text}");
    assert_eq!(server.json("/lh/v1/self")["build"]["restart_count"], 0);
    // Approved: the run ends with a restart notice, then the server re-executes.
    let (text, events) = agent_run(&server, true);
    let done = events.iter().rev().find(|e| e["event"] == "done").unwrap();
    assert_eq!(
        done["data"]["response"]["finish_reason"], "restart",
        "{text}"
    );
    assert!(text.contains("restarting now"), "{text}");
    let after = server.wait_restarted(1);
    assert_eq!(after["build"]["pid"], pid);
    assert_eq!(after["build"]["last_restart_reason"], "agent: new binary");
    // "Always allow" never carries over: a new process has no memory anyway,
    // and the key includes the call id (unit-tested in lh-core).
    assert!(server.terminate().success(), "{}", server.stderr());
}
