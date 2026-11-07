use super::*;
use std::io::{BufRead, BufReader, Read, Write};

fn rpc(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","method":method,"params":params})
}
fn run(config: Value, requests: Vec<Value>) -> Vec<Value> {
    let mut frames = vec![rpc("initialize", json!({"protocol":1,"config":config}))];
    frames.extend(requests);
    let input = frames
        .into_iter()
        .enumerate()
        .map(|(i, mut v)| {
            v["id"] = json!(i + 1);
            format!("{v}\n")
        })
        .collect::<String>()
        + "{\"jsonrpc\":\"2.0\",\"method\":\"shutdown\"}\n";
    let mut output = Vec::new();
    lh_plugin_sdk::serve_io(Backend::default(), input.as_bytes(), &mut output).unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|s| json::from_str(s).unwrap())
        .collect()
}
fn response(frames: &[Value], id: u64) -> &Value {
    frames.iter().find(|v| v["id"] == id).unwrap()
}
fn load(key: &str, pinned: bool) -> Value {
    rpc(
        "backend/load",
        json!({"key":key,"model":{"source":"test"},"pinned":pinned}),
    )
}
fn request(stream: bool) -> Value {
    json!({"key":"k","messages":[{"role":"user","content":"hello"}],"params":{"temperature":0.2},"stream":stream})
}
fn resident(base: String) -> Resident {
    Resident {
        process: None,
        model: json::from_value(json!({"source":"test"})).unwrap(),
        api_model: "LiquidAI/LFM3-1.7B".into(),
        base,
        token: Some("test-secret".into()),
        port: None,
        pinned: false,
        used: Instant::now(),
    }
}
// Actual TCP HTTP, checking every route and bearer header. Each connection is
// closed to exercise request parsing independently of keep-alive behavior.
fn server(replies: Vec<(&'static str, u16, String)>) -> (String, thread::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let mut bodies = Vec::new();
        for (path, status, body) in replies {
            let start = Instant::now();
            let mut socket = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            start.elapsed() < Duration::from_secs(5),
                            "no request for {path}"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line.split_whitespace().nth(1), Some(path));
            let mut length = 0;
            let mut bearer = None;
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (key, value) = line.split_once(':').unwrap();
                if key.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
                if key.eq_ignore_ascii_case("authorization") {
                    bearer = Some(value.trim().to_owned());
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            bodies.push(json!({"body":if bytes.is_empty() { Value::Null } else {json::from_slice::<Value>(&bytes).unwrap()},"bearer":bearer}));
            write!(
                socket,
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        bodies
    });
    (base, worker)
}
#[test]
fn manifest_is_disabled_by_default() {
    let manifest: toml::Value = toml::from_str(include_str!("../plugin.toml")).unwrap();
    assert_eq!(manifest["plugin"]["enabled"].as_bool(), Some(false));
    assert_eq!(
        manifest["run"]["command"].as_str(),
        Some("lh-backend-freetoken")
    );
}

#[test]
fn missing_token_environment_is_a_load_error() {
    let frames = run(
        json!({"models":{"test":{"url":"http://127.0.0.1:1","token_env":"LH_FT_TEST_DELIBERATELY_MISSING_TOKEN_9DB4C980"}}}),
        vec![load("k", false)],
    );
    assert_eq!(response(&frames, 2)["error"]["code"], -32602);
}

#[test]
fn text_only_rejects_images_and_audio_before_http() {
    for part in [
        json!({"type":"image_url","image_url":{"url":"http://unused"}}),
        json!({"type":"input_audio","input_audio":{"data":"AA==","format":"wav"}}),
    ] {
        let params = json!({"key":"k","messages":[{"role":"user","content":[part]}]});
        assert_eq!(
            chat(
                &agent(1),
                &resident("http://127.0.0.1:1".into()),
                &params,
                |_| Ok(())
            )
            .unwrap_err()
            .code,
            -32003
        );
    }
}

#[test]
fn base_url_alias_and_api_model_override_reach_http() {
    let body =
        json!({"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]})
            .to_string();
    let (base, worker) = server(vec![
        ("/v1/cache/status", 200, "{\"state\":\"serving\"}".into()),
        ("/v1/chat/completions", 200, body),
    ]);
    let frames = run(
        json!({"models":{"test":{"base_url":base,"model":"LFM3-1.7B"}}}),
        vec![load("k", false), rpc("backend/chat", request(false))],
    );
    assert_eq!(response(&frames, 3)["result"]["message"]["content"], "ok");
    assert_eq!(worker.join().unwrap()[1]["body"]["model"], "LFM3-1.7B");
}

#[test]
fn initialize_capabilities_decide_and_validation() {
    let frames = run(json!({}), vec![rpc("backend/decide", json!({}))]);
    assert_eq!(
        response(&frames, 1)["result"]["capabilities"],
        json!(["chat"])
    );
    assert_eq!(response(&frames, 2)["error"]["code"], -32003);
    for config in [
        json!({"bad":1}),
        json!({"max_resident":0}),
        json!({"models":{"test":{}}}),
        json!({"models":{"test":{"url":"http://localhost","spawn":{"model_path":"id"}}}}),
        json!({"models":{"test":{"url":"ftp://host"}}}),
        json!({"models":{"test":{"spawn":{"model_path":"id","args":["--port=99"]}}}}),
    ] {
        assert_eq!(response(&run(config, vec![]), 1)["error"]["code"], -32602);
    }
}
#[test]
fn url_readiness_requires_serving_not_http_success() {
    let (base, worker) = server(vec![
        (
            "/v1/cache/status",
            200,
            json!({"state":"loading"}).to_string(),
        ),
        (
            "/v1/cache/status",
            200,
            json!({"state":"serving"}).to_string(),
        ),
    ]);
    let mut r = resident(base);
    let mut notices = 0;
    ready(&mut r, Duration::from_secs(2), || {
        notices += 1;
        Ok(())
    })
    .unwrap();
    assert!(notices >= 1);
    let bodies = worker.join().unwrap();
    assert!(bodies.iter().all(|v| v["bearer"] == "Bearer test-secret"));
}
#[test]
fn url_load_idempotence_pinning_lru_and_unload() {
    let (base, worker) = server(vec![
        ("/v1/cache/status", 200, "{\"state\":\"serving\"}".into()),
        ("/v1/cache/status", 200, "{\"state\":\"serving\"}".into()),
    ]);
    let frames = run(
        json!({"max_resident":1,"models":{"test":{"url":base}}}),
        vec![
            load("k", false),
            load("k", false),
            load("second", true),
            load("third", false),
            rpc("backend/status", json!({})),
            rpc("backend/unload", json!({"key":"second"})),
            rpc("backend/status", json!({})),
        ],
    );
    assert_eq!(response(&frames, 2)["result"]["ready"], true);
    assert_eq!(response(&frames, 3)["result"]["ready"], true);
    assert_eq!(response(&frames, 5)["error"]["code"], -32002);
    assert_eq!(response(&frames, 6)["result"]["loaded"][0]["key"], "second");
    assert_eq!(response(&frames, 8)["result"]["loaded"], json!([]));
    assert!(frames.iter().any(|v| v["method"] == "$/progress"));
    worker.join().unwrap();
}
#[test]
fn http_chat_preserves_payload_tools_and_authorization() {
    let body = json!({"choices":[{"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"total_tokens":3}}).to_string();
    let (base, worker) = server(vec![("/v1/chat/completions", 200, body)]);
    let mut params = request(false);
    params["tools"] =
        json!([{"type":"function","function":{"name":"search","parameters":{"type":"object"}}}]);
    let value = chat(&agent(2), &resident(base), &params, |_| {
        panic!("unexpected stream")
    })
    .unwrap();
    assert_eq!(value["message"]["content"], "hello");
    let bodies = worker.join().unwrap();
    assert_eq!(bodies[0]["bearer"], "Bearer test-secret");
    assert_eq!(bodies[0]["body"]["tools"], params["tools"]);
    assert_eq!(bodies[0]["body"]["messages"], params["messages"]);
    assert_eq!(bodies[0]["body"]["temperature"], 0.2);
    assert_eq!(bodies[0]["body"]["stream"], false);
    assert_eq!(bodies[0]["body"]["model"], "LiquidAI/LFM3-1.7B");
}
fn stream_body() -> String {
    let chunks = [
        json!({"choices":[{"delta":{"content":"hé","tool_calls":[{"index":0,"id":"c","type":"function","function":{"name":"search","arguments":"{"}}]}}]}),
        json!({"choices":[{"delta":{"content":"llo","tool_calls":[{"index":0,"function":{"arguments":"}"}}]},"finish_reason":"tool_calls"}]}),
        json!({"choices":[],"usage":{"total_tokens":3}}),
    ];
    chunks
        .iter()
        .map(|v| format!("data: {v}\r\n\r\n"))
        .collect::<String>()
        + "data: [DONE]\r\n\r\n"
}
#[test]
fn http_sse_emits_aggregates_and_rejects_truncation() {
    let (base, worker) = server(vec![("/v1/chat/completions", 200, stream_body())]);
    let mut deltas = Vec::new();
    let value = chat(&agent(2), &resident(base), &request(true), |v| {
        deltas.push(v);
        Ok(())
    })
    .unwrap();
    assert_eq!(value["message"]["content"], "héllo");
    assert_eq!(
        value["message"]["tool_calls"][0]["function"]["arguments"],
        "{}"
    );
    assert_eq!(value["usage"]["total_tokens"], 3);
    assert_eq!(deltas.len(), 2);
    assert_eq!(
        worker.join().unwrap()[0]["body"]["stream_options"]["include_usage"],
        true
    );
    for body in [
        "data: [DONE]\n\n",
        "data: not-json\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
    ] {
        let (base, worker) = server(vec![("/v1/chat/completions", 200, body.into())]);
        assert_eq!(
            chat(&agent(2), &resident(base), &request(true), |_| Ok(()))
                .unwrap_err()
                .code,
            -32001
        );
        worker.join().unwrap();
    }
}
#[test]
fn http_errors_are_mapped_and_do_not_leak_tokens() {
    for (status, code) in [(401, -32001), (429, -32002), (503, -32002), (500, -32001)] {
        let (base, worker) = server(vec![("/v1/chat/completions", status, "test-secret".into())]);
        let error = chat(&agent(2), &resident(base), &request(false), |_| Ok(())).unwrap_err();
        assert_eq!(error.code, code);
        assert!(!error.message.contains("test-secret"));
        worker.join().unwrap();
    }
    let (base, worker) = server(vec![("/v1/cache/status", 403, String::new())]);
    assert!(ready(&mut resident(base), Duration::from_secs(2), || Ok(())).is_err());
    worker.join().unwrap();
}
#[test]
fn token_env_is_resolved_for_readiness_and_chat() {
    let body =
        json!({"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]})
            .to_string();
    let (base, worker) = server(vec![
        ("/v1/cache/status", 200, "{\"state\":\"serving\"}".into()),
        ("/v1/chat/completions", 200, body),
    ]);
    // Use an existing non-secret variable: no process-global environment mutation
    // in a concurrently running Rust test suite.
    let expected = format!("Bearer {}", std::env::var("HOME").unwrap());
    let frames = run(
        json!({"models":{"test":{"url":base,"token_env":"HOME"}}}),
        vec![load("k", false), rpc("backend/chat", request(false))],
    );
    assert_eq!(response(&frames, 3)["result"]["message"]["content"], "ok");
    assert!(
        worker
            .join()
            .unwrap()
            .iter()
            .all(|v| v["bearer"] == expected)
    );
}

#[test]
fn idle_worker_reaps_only_unpinned() {
    let backend = Backend::default();
    {
        let mut state = backend.state.0.lock().unwrap();
        state.ttl = Duration::from_millis(1);
        let mut pinned = resident("http://unused".into());
        pinned.pinned = true;
        state.models.insert("pinned".into(), pinned);
        state
            .models
            .insert("idle".into(), resident("http://unused".into()));
    }
    let start = Instant::now();
    loop {
        let state = backend.state.0.lock().unwrap();
        if !state.models.contains_key("idle") {
            assert!(state.models.contains_key("pinned"));
            break;
        }
        drop(state);
        assert!(start.elapsed() < Duration::from_secs(3));
        thread::sleep(Duration::from_millis(25));
    }
}

// Interpreter imports and scheduling are setup, not the readiness behavior under test.
const FIXTURE_STARTUP_SECS: u64 = 5;
const READINESS_TIMEOUT: Duration = Duration::from_millis(500);
const TIMEOUT_MESSAGE: &str = "FreeToken readiness timeout (cache state is not serving)";

struct Fixture {
    dir: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lh-ft-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let script = dir.join("bin/ft");
        std::fs::write(&script, include_str!("fake_ft.py")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }
    fn config(&self, state: &str) -> Value {
        json!({"load_timeout_secs":FIXTURE_STARTUP_SECS,"models":{"test":{"spawn":{"venv":self.dir,"model_path":"org/model","args":["--test-extra","value"],"env":{"FT_TEST_REPORT":self.dir.join("report.json"),"FT_TEST_STATE":state,"FT_TEST_VALUE":"configured"}}}}})
    }
    fn report(&self) -> Value {
        json::from_slice(&std::fs::read(self.dir.join("report.json")).unwrap()).unwrap()
    }
    fn loading_responses(&self) -> usize {
        std::fs::read_to_string(self.dir.join("report.json.responses"))
            .unwrap()
            .lines()
            .filter(|line| *line == "loading")
            .count()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn assert_reaped(report: &Value) {
    let pid = report["pid"].as_i64().unwrap() as i32;
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "direct child remains alive"
    );
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
        -1,
        "child was not reaped"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    let child = report["child"].as_i64().unwrap();
    let start = Instant::now();
    loop {
        let stat = std::fs::read_to_string(format!("/proc/{child}/stat"));
        if stat
            .as_ref()
            .map_or(true, |s| s.split_whitespace().nth(2) == Some("Z"))
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "descendant survived process group kill"
        );
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn fake_ft_spawn_arguments_env_readiness_and_shutdown_cleanup() {
    let fixture = Fixture::new();
    let frames = run(
        fixture.config("serving"),
        vec![load("k", true), rpc("backend/status", json!({}))],
    );
    assert_eq!(response(&frames, 2)["result"]["ready"], true, "{frames:?}");
    let report = fixture.report();
    let args = report["args"].as_array().unwrap();
    assert_eq!(
        &args[..3],
        &[json!("serve"), json!("--model-path"), json!("org/model")]
    );
    assert_eq!(args[3], "--port");
    assert!(args[4].as_str().unwrap().parse::<u16>().unwrap() > 0);
    assert_eq!(report["value"], "configured");
    assert_eq!(report["venv"], json!(fixture.dir));
    assert_reaped(&report);
}
#[test]
fn fake_ft_unload_and_failed_load_cleanup() {
    for state in ["serving", "loading"] {
        let fixture = Fixture::new();
        let frames = run(
            fixture.config(state),
            vec![
                load("k", false),
                rpc("backend/status", json!({})),
                rpc("backend/unload", json!({"key":"k"})),
                rpc("backend/status", json!({})),
            ],
        );
        if state == "loading" {
            assert_eq!(response(&frames, 2)["error"]["code"], -32001);
            assert_eq!(response(&frames, 2)["error"]["message"], TIMEOUT_MESSAGE);
            assert!(fixture.loading_responses() > 0);
            // Check before unload: failed load itself must leave no registration.
            assert_eq!(response(&frames, 3)["result"]["loaded"], json!([]));
        } else {
            assert_eq!(response(&frames, 2)["result"]["ready"], true, "{frames:?}");
        }
        assert_eq!(response(&frames, 5)["result"]["loaded"], json!([]));
        assert_reaped(&fixture.report());
    }
}

#[test]
fn fake_ft_slow_start_exceeds_old_budget_and_still_cleans_up() {
    let fixture = Fixture::new();
    let mut config = fixture.config("serving");
    config["models"]["test"]["spawn"]["env"]["FT_TEST_STARTUP_DELAY"] = json!("1.2");
    let start = Instant::now();
    let frames = run(config, vec![load("k", false)]);
    assert_eq!(response(&frames, 2)["result"]["ready"], true, "{frames:?}");
    assert!(start.elapsed() >= Duration::from_millis(1200));
    assert_reaped(&fixture.report());
}

#[test]
fn fake_ft_already_listening_loading_times_out_on_short_deadline() {
    let fixture = Fixture::new();
    let config: Config = json::from_value(fixture.config("loading")).unwrap();
    // Port zero lets this fixture retain its own ephemeral listening socket.
    let process = spawn(config.models["test"].spawn.as_ref().unwrap(), 0).unwrap();
    let mut r = resident(String::new());
    r.process = Some(process); // RAII cleanup also covers assertion failures.
    let setup = Instant::now();
    while !fixture.dir.join("report.json").exists() {
        assert!(setup.elapsed() < Duration::from_secs(FIXTURE_STARTUP_SECS));
        thread::sleep(Duration::from_millis(10));
    }
    let report = fixture.report();
    r.base = format!("http://127.0.0.1:{}", report["port"]);
    // Confirm a real loading response before starting the short deadline.
    let value: Value = agent(2)
        .get(&format!("{}/v1/cache/status", r.base))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    assert_eq!(value["state"], "loading");
    let start = Instant::now();
    let error = ready(&mut r, READINESS_TIMEOUT, || Ok(())).unwrap_err();
    let elapsed = start.elapsed();
    assert_eq!(error.code, -32001);
    assert_eq!(error.message, TIMEOUT_MESSAGE);
    assert!(elapsed >= READINESS_TIMEOUT);
    assert!(elapsed < Duration::from_secs(2), "elapsed: {elapsed:?}");
    // One setup probe plus at least one request from ready, all still loading.
    assert!(fixture.loading_responses() >= 2);
    drop(r);
    assert_reaped(&report);
}
