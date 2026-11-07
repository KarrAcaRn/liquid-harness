use super::*;
use std::io::{BufRead, BufReader, Read, Write};

fn fake_http(response: String) -> (String, thread::JoinHandle<Value>) {
    fake_http_at(response, "/v1/chat/completions")
}
fn fake_http_at(response: String, path: &'static str) -> (String, thread::JoinHandle<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut line = String::new();
        let mut length = 0;
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with(&format!("POST {path} ")));
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':')
                && key.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(),
            response
        )
        .unwrap();
        json::from_slice(&body).unwrap()
    });
    (url, worker)
}
fn spec(source: &str) -> ModelSpec {
    json::from_value(json!({"source":source})).unwrap()
}

#[test]
fn model_arguments_and_validation() {
    let model: ModelSpec = json::from_value(
        json!({"source":"hf:Org/repo:Q8_0","ctx":4096,"batch":512,"threads":8,"mmproj":"auto"}),
    )
    .unwrap();
    assert_eq!(
        arguments(&model, 1234).unwrap(),
        [
            "--host",
            "127.0.0.1",
            "--port",
            "1234",
            "-hf",
            "Org/repo:Q8_0",
            "-c",
            "4096",
            "-b",
            "512",
            "-t",
            "8"
        ]
    );
    assert!(arguments(&spec("http:bad"), 1).is_err());
    let mut model = spec("file:/a b/model.gguf");
    model.mmproj = Some("file:/projector.gguf".into());
    assert!(
        arguments(&model, 1)
            .unwrap()
            .ends_with(&["--mmproj".into(), "/projector.gguf".into()])
    );
    model.mmproj = Some("auto".into());
    assert!(arguments(&model, 1).is_err());
    model.mmproj = None;
    model.threads = Some(0);
    assert!(arguments(&model, 1).is_err());
}
#[test]
fn http_chat_nonstream_preserves_multimodal_payload() {
    let response = json!({"choices":[{"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"total_tokens":7}}).to_string();
    let (url, worker) = fake_http(response);
    let params = json!({"key":"k","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AA"}}]}],"params":{"temperature":0.3,"stream":true},"stream":false});
    let result = chat(&agent(3), &url, &params, |_| panic!("unexpected stream")).unwrap();
    assert_eq!(result["message"]["content"], "hello");
    let body = worker.join().unwrap();
    assert_eq!(body["messages"], params["messages"]);
    assert_eq!(body["stream"], false);
    assert_eq!(body["temperature"], 0.3);
}
fn streaming_body() -> String {
    let chunks = [
        json!({"choices":[{"delta":{"role":"assistant","content":"hé","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"search","arguments":"{\"q\":"}}]}}]}),
        json!({"choices":[{"delta":{"content":"llo","tool_calls":[{"index":0,"function":{"arguments":"\"x\"}"}}]},"finish_reason":"tool_calls"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}}),
    ];
    format!(
        ": heartbeat\r\n\r\n{}data: [DONE]\r\n\r\n",
        chunks
            .into_iter()
            .map(|v| format!("data: {v}\r\n\r\n"))
            .collect::<String>()
    )
}
#[test]
fn http_sse_aggregates_text_tools_usage_and_emits() {
    let (url, worker) = fake_http(streaming_body());
    let params = json!({"key":"k","messages":[],"stream":true});
    let mut deltas = Vec::new();
    let result = chat(&agent(3), &url, &params, |delta| {
        deltas.push(delta);
        Ok(())
    })
    .unwrap();
    assert_eq!(deltas.len(), 2);
    assert_eq!(result["message"]["content"], "héllo");
    assert_eq!(
        result["message"]["tool_calls"][0]["function"]["arguments"],
        "{\"q\":\"x\"}"
    );
    assert_eq!(result["finish_reason"], "tool_calls");
    assert_eq!(result["usage"]["total_tokens"], 5);
    assert_eq!(
        worker.join().unwrap()["stream_options"]["include_usage"],
        true
    );
}
#[test]
fn broken_streams_are_errors() {
    for text in [
        "",
        "data: not-json\n\n",
        "data: [DONE]\n\n",
        "data: {\"error\":\"bad\"}\n\n",
    ] {
        assert!(sse(text.as_bytes(), |_| Ok(())).is_err());
    }
    assert!(
        sse(streaming_body().as_bytes(), |_| Err(internal(
            "closed output"
        )))
        .is_err()
    );
}

// A real child process with no shell or external tools, used to exercise cleanup.
#[test]
fn child_fixture() {
    if std::env::var_os("LH_LLAMA_TEST_CHILD").is_none() {
        return;
    }
    thread::sleep(Duration::from_secs(30));
}
fn child() -> Process {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "tests::child_fixture"])
        .env("LH_LLAMA_TEST_CHILD", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Process(command.spawn().unwrap())
}
fn resident(pinned: bool, age: u64) -> Resident {
    Resident {
        process: child(),
        model: spec("file:/fake"),
        port: 1,
        pinned,
        used: Instant::now() - Duration::from_secs(age),
    }
}
fn dead(pid: u32) -> bool {
    unsafe {
        libc::kill(pid as i32, 0) == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
}
#[test]
fn lru_pins_and_process_reaping() {
    let mut state = State {
        models: BTreeMap::new(),
        ttl: Duration::from_secs(60),
        stop: false,
    };
    state.models.insert("pinned".into(), resident(true, 30));
    state.models.insert("old".into(), resident(false, 20));
    state.models.insert("recent".into(), resident(false, 1));
    let pid = state.models["old"].process.0.id();
    state.room(3).unwrap();
    assert!(dead(pid));
    assert!(state.models.contains_key("pinned"));
    assert!(state.models.contains_key("recent"));
    state.room(2).unwrap();
    assert_eq!(state.room(1).unwrap_err().code, -32002);
}
#[test]
fn background_ttl_and_drop_cleanup() {
    let backend = Backend::default();
    let (expired, pinned) = {
        let mut state = backend.state.0.lock().unwrap();
        state.ttl = Duration::from_millis(10);
        state.models.insert("expired".into(), resident(false, 1));
        state.models.insert("pinned".into(), resident(true, 1));
        (
            state.models["expired"].process.0.id(),
            state.models["pinned"].process.0.id(),
        )
    };
    let start = Instant::now();
    while !dead(expired) && start.elapsed() < Duration::from_secs(3) {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(dead(expired));
    assert!(!dead(pinned));
    drop(backend);
    assert!(dead(pinned));
}
#[test]
fn production_spawn_group_and_early_exit() {
    let executable = std::env::current_exe().unwrap();
    let mut process = spawn(
        executable.to_str().unwrap(),
        &["--exact".into(), "tests::child_fixture".into()],
        None,
    )
    .unwrap();
    let pid = process.0.id();
    assert_eq!(unsafe { libc::getpgid(pid as i32) }, pid as i32);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let error = ready(&mut process, port, Duration::from_secs(3), || Ok(())).unwrap_err();
    assert!(error.message.contains("exited"));
    drop(process);
    assert!(dead(pid));
}
#[test]
fn health_wait_and_failure_cleanup() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let worker = thread::spawn(move || {
        for status in ["503 Service Unavailable", "200 OK"] {
            let (mut socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            write!(
                socket,
                "HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .unwrap();
        }
    });
    let mut process = child();
    let mut notices = 0;
    ready(&mut process, port, Duration::from_secs(3), || {
        notices += 1;
        Ok(())
    })
    .unwrap();
    worker.join().unwrap();
    assert!(notices > 0);
    let pid = process.0.id();
    assert!(ready(&mut process, port, Duration::from_millis(20), || Ok(())).is_err());
    drop(process);
    assert!(dead(pid));
}
#[test]
fn decide_proxy_and_explicit_unload() {
    let (url, worker) = fake_http_at(
        json!({"answers":{"q":{"noul":0.9}}}).to_string(),
        "/v1/systemone",
    );
    let backend = Backend::default();
    let mut r = resident(true, 0);
    r.port = url.rsplit(':').next().unwrap().parse().unwrap();
    let pid = r.process.0.id();
    backend
        .state
        .0
        .lock()
        .unwrap()
        .models
        .insert("d1".into(), r);
    let request = json!({"key":"d1","state":"hi","images":["image.png"],"questions":{"q":{"type":"noul","instructions":"Ready?"}}});
    let input = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol":1,"config":{}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"backend/decide","params":request}),
        json!({"jsonrpc":"2.0","id":3,"method":"backend/unload","params":{"key":"d1"}}),
        json!({"jsonrpc":"2.0","id":4,"method":"backend/status"}),
    ]
    .into_iter()
    .map(|v| format!("{v}\n"))
    .collect::<String>();
    let mut output = Vec::new();
    lh_plugin_sdk::serve_io(backend, input.as_bytes(), &mut output).unwrap();
    let output: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|v| json::from_str(v).unwrap())
        .collect();
    assert_eq!(output[1]["result"]["answers"]["q"]["noul"], 0.9);
    assert_eq!(output[3]["result"]["loaded"], json!([]));
    let body = worker.join().unwrap();
    assert!(body.get("key").is_none());
    assert_eq!(body["images"], request["images"]);
    assert!(dead(pid));
}
#[test]
fn progress_notifications_are_request_scoped() {
    struct Progress;
    impl Handler for Progress {
        fn handle(&mut self, _: &str, _: Value, ctx: &mut Context<'_>) -> Result<Value, RpcError> {
            ctx.progress("loading", None).map_err(internal)?;
            ctx.progress("ready", Some(1.0)).map_err(internal)?;
            Ok(json!({}))
        }
    }
    let mut output = Vec::new();
    lh_plugin_sdk::serve_io(
        Progress,
        b"{\"jsonrpc\":\"2.0\",\"id\":42,\"method\":\"go\"}\n".as_slice(),
        &mut output,
    )
    .unwrap();
    let output: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|v| json::from_str(v).unwrap())
        .collect();
    assert_eq!(output[0]["method"], "$/progress");
    assert_eq!(output[0]["params"]["id"], 42);
    assert!(output[0]["params"].get("fraction").is_none());
    assert_eq!(output[1]["params"]["fraction"], 1.0);
}
#[test]
fn sdk_progress_initialize_errors_and_status() {
    let input = [json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol":1,"config":{}}}),json!({"jsonrpc":"2.0","id":2,"method":"backend/status"}),json!({"jsonrpc":"2.0","id":3,"method":"backend/load","params":{"key":"x","model":{"source":"bad"}}})].into_iter().map(|v| format!("{v}\n")).collect::<String>();
    let mut output = Vec::new();
    lh_plugin_sdk::serve_io(Backend::default(), input.as_bytes(), &mut output).unwrap();
    let values: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|l| json::from_str(l).unwrap())
        .collect();
    assert_eq!(values[1]["result"]["loaded"], json!([]));
    assert_eq!(values[2]["error"]["code"], -32602);
}

#[test]
fn server_fixture() {
    let Ok(port) = std::env::var("LH_FAKE_PORT") else {
        return;
    };
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if let Ok((mut socket, _)) = listener.accept() {
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut input = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                if input.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let _ = write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn full_load_defaults_idempotence_and_pinned_capacity() {
    use std::os::unix::fs::PermissionsExt;
    let script = std::env::temp_dir().join(format!("lh-fake-server-{}", std::process::id()));
    let executable = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .replace('\'', "'\\''");
    std::fs::write(&script, format!("#!/bin/sh\n[ \"$8\" = 2048 ] && [ \"${{10}}\" = 128 ] && [ \"${{12}}\" = 4 ] || exit 2\nexport LH_FAKE_PORT=\"$4\"\nexec '{executable}' --exact tests::server_fixture --nocapture\n")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let load = json!({"key":"router:d1","model":{"source":"file:/fake"},"pinned":true});
    let frames = [
        (
            "initialize",
            json!({"protocol":1,"config":{"llama_server":script,"max_resident":1,"threads":4,"ctx":2048,"batch":128}}),
        ),
        ("backend/load", load.clone()),
        ("backend/load", load),
        (
            "backend/load",
            json!({"key":"other","model":{"source":"file:/other"}}),
        ),
        (
            "backend/load",
            json!({"key":"router:d1","model":{"source":"file:/different"}}),
        ),
        ("backend/status", json!({})),
    ];
    let input = frames
        .into_iter()
        .enumerate()
        .map(|(id, (method, params))| {
            format!(
                "{}\n",
                json!({"jsonrpc":"2.0","id":id+1,"method":method,"params":params})
            )
        })
        .collect::<String>();
    let mut output = Vec::new();
    lh_plugin_sdk::serve_io(Backend::default(), input.as_bytes(), &mut output).unwrap();
    std::fs::remove_file(script).unwrap();
    let output: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|s| json::from_str::<Value>(s).unwrap())
        .filter(|v| v.get("id").is_some())
        .collect();
    assert_eq!(output[1]["result"]["ready"], true);
    assert_eq!(output[1]["result"], output[2]["result"]);
    assert_eq!(output[3]["error"]["code"], -32002);
    assert_eq!(output[4]["error"]["code"], -32602);
    let pid = output[5]["result"]["loaded"][0]["pid"].as_u64().unwrap() as u32;
    assert!(dead(pid), "EOF must stop and reap the resident server");
}
