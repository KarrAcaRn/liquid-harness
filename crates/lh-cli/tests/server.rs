#![cfg(feature = "server")]
use lh_protocol::json::{self, Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
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
    fn start(auth: bool) -> Self {
        Self::start_on(auth, "127.0.0.1:0")
    }
    fn start_on(auth: bool, bind: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "lh-server-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(directory.join("liquid-harness")).unwrap();
        std::fs::write(
            directory.join("liquid-harness/config.toml"),
            format!(
                "default_specialist='mock-text'\n{}",
                if auth {
                    "server_token_env='LH_TEST_SERVER_TOKEN'\n"
                } else {
                    ""
                }
            ),
        )
        .unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let extra = directory.join("decide");
        std::fs::create_dir_all(&extra).unwrap();
        std::fs::write(extra.join("plugin.toml"), "[plugin]\nid='decide-pack'\nversion='0.1.0'\nprovides=['specialist']\n[[specialist]]\nid='mock-decide'\nroute='Decide'\ntask='decide'\ninputs=['text']\noutputs=['text']\nbackend='mock'\nmodel={source='file:mock'}\n").unwrap();
        // The child owns port allocation; never release/rebind a discovered port.
        // A file also avoids filling an undrained stderr pipe during a test.
        let stderr = std::fs::File::create(directory.join("server.stderr")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_lh"))
            .current_dir(&directory)
            .env_remove("LH_CONFIG")
            .env("XDG_CONFIG_HOME", &directory)
            .env(
                "LH_PLUGIN_PATH",
                std::env::join_paths([
                    root.join("plugins/mock-backend"),
                    root.join("tests/fixtures/mock-pack"),
                    root.join("tests/fixtures/audio-pack"),
                    extra,
                ])
                .unwrap(),
            )
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
            .env("LH_TEST_SERVER_TOKEN", "test-secret")
            .args(["serve", "--bind", bind])
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
                let error = server.stderr();
                panic!("server exited {status}: {error}");
            }
            // Only a complete line from this child's private stderr proves bind.
            if let Some(address) = error.split_inclusive('\n').find_map(|line| {
                line.strip_suffix('\n')?
                    .strip_prefix("lh server listening on ")?
                    .parse::<std::net::SocketAddr>()
                    .ok()
            }) {
                assert!(address.ip().is_loopback());
                assert_ne!(address.port(), 0);
                server.address = address.to_string();
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "server startup timed out: {error}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        server
    }
    fn stderr(&self) -> String {
        std::fs::read_to_string(self.directory.join("server.stderr")).unwrap()
    }
    fn request(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        mime: &str,
        auth: Option<&str>,
    ) -> (u16, String, Vec<u8>) {
        let mut socket = TcpStream::connect(&self.address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let authorization = auth
            .map(|s| format!("Authorization: Bearer {s}\r\n"))
            .unwrap_or_default();
        write!(socket, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: {mime}\r\nContent-Length: {}\r\n{authorization}\r\n", body.len()).unwrap();
        socket.write_all(body).unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).unwrap_or_else(|error| {
            panic!(
                "{method} {path}: {error}; child={} address={} received={} bytes; stderr={}",
                self.child.id(),
                self.address,
                response.len(),
                self.stderr()
            )
        });
        let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let headers = String::from_utf8(response[..split].to_vec()).unwrap();
        let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut body = response[split + 4..].to_vec();
        if headers
            .to_lowercase()
            .contains("transfer-encoding: chunked")
        {
            let mut decoded = Vec::new();
            let mut rest = body.as_slice();
            loop {
                let end = rest.windows(2).position(|w| w == b"\r\n").unwrap();
                let size = usize::from_str_radix(
                    std::str::from_utf8(&rest[..end])
                        .unwrap()
                        .split(';')
                        .next()
                        .unwrap(),
                    16,
                )
                .unwrap();
                if size == 0 {
                    break;
                }
                rest = &rest[end + 2..];
                decoded.extend_from_slice(&rest[..size]);
                rest = &rest[size + 2..];
            }
            body = decoded;
        }
        (status, headers.to_lowercase(), body)
    }
    fn post(&self, path: &str, value: Value) -> (u16, String, Vec<u8>) {
        self.request(
            "POST",
            path,
            value.to_string().as_bytes(),
            "application/json",
            None,
        )
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
#[test]
fn occupied_listener_cannot_satisfy_child_readiness() {
    let decoy = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = decoy.local_addr().unwrap().to_string();
    let failure = std::panic::catch_unwind(|| Server::start_on(false, &address))
        .err()
        .expect("an unrelated listener must not make startup succeed");
    let message = failure.downcast_ref::<String>().unwrap();
    assert!(message.contains("server exited"), "{message}");
    assert!(message.contains("Address already in use"), "{message}");
    decoy.set_nonblocking(true).unwrap();
    assert_eq!(
        decoy.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn child_owned_listeners_survive_sibling_teardown_and_split_get() {
    let server = Server::start(false);
    let sibling = Server::start(true);
    assert_ne!(server.address, sibling.address);
    drop(sibling);
    // An empty readiness-style connection cannot affect the next connection.
    drop(TcpStream::connect(&server.address).unwrap());
    let mut socket = TcpStream::connect(&server.address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    socket.set_nodelay(true).unwrap();
    for part in [
        &b"GET /v1/models HTTP/1.1\r\n"[..],
        &b"Host: localhost\r\nConnection: close\r\n"[..],
        &b"Content-Length: 0\r\n"[..],
        &b"\r\n"[..],
    ] {
        socket.write_all(part).unwrap();
    }
    let mut response = Vec::new();
    socket.read_to_end(&mut response).unwrap();
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let headers = std::str::from_utf8(&response[..split])
        .unwrap()
        .to_lowercase();
    assert!(headers.starts_with("http/1.1 200 "));
    let length: usize = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(response[split + 4..].len(), length);
    let models: Value = json::from_slice(&response[split + 4..]).unwrap();
    assert!(
        models["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model["id"] == "auto")
    );
}

#[test]
fn endpoints_stream_multipart_and_media_validation() {
    let server = Server::start(false);
    let (status, _, body) = server.request("GET", "/v1/models", b"", "application/json", None);
    assert_eq!(status, 200);
    let models: Value = json::from_slice(&body).unwrap();
    assert!(
        models["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == "auto")
    );
    let chat = json!({"model":"mock-text","messages":[{"role":"user","content":"hello stream"}]});
    let (status, headers, body) = server.post("/v1/chat/completions", chat.clone());
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert!(headers.contains("x-lh-route: mock-text;p="));
    assert!(headers.contains(";stage="));
    let response: Value = json::from_slice(&body).unwrap();
    assert_eq!(response["choices"][0]["message"]["content"], "hello stream");
    let mut streaming = chat;
    streaming["stream"] = json!(true);
    let (status, headers, body) = server.post("/v1/chat/completions", streaming);
    assert_eq!(status, 200);
    assert!(headers.contains("text/event-stream"));
    let events = String::from_utf8(body).unwrap();
    assert!(events.ends_with("data: [DONE]\n\n"));
    let chunks: Vec<Value> = events
        .lines()
        .filter_map(|s| s.strip_prefix("data: "))
        .filter(|s| *s != "[DONE]")
        .map(|s| json::from_str(s).unwrap())
        .collect();
    assert!(chunks.len() > 1);
    assert!(
        chunks
            .iter()
            .all(|c| c["object"] == "chat.completion.chunk")
    );
    assert_eq!(
        chunks
            .iter()
            .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
            .collect::<String>(),
        "hello stream"
    );
    let (status, _, body) = server.post(
        "/v1/route",
        json!({"prompt":"hello","specialist":"mock-text"}),
    );
    assert_eq!(status, 200);
    assert_eq!(
        json::from_slice::<Value>(&body).unwrap()["specialist"],
        "mock-text"
    );
    let (status, _, body) = server.post("/v1/decide", json!({"model":"mock-decide","state":"hello","questions":{"ok":{"type":"noul","instructions":"Is this okay?"}}}));
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert!(json::from_slice::<Value>(&body).unwrap()["answers"].is_object());
    let (status, _, body) = server.post("/v1/decide", json!({"model":"mock-router","state":"hello","questions":{"ok":{"type":"noul","instructions":"Is this okay?"}}}));
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert!(json::from_slice::<Value>(&body).unwrap()["answers"].is_object());
    let form = b"--boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"sample.wav\"\r\nContent-Type: audio/wav\r\n\r\nfixture audio\r\n--boundary--\r\n";
    let (status, _, body) = server.request(
        "POST",
        "/v1/audio/transcriptions",
        form,
        "multipart/form-data; boundary=boundary",
        None,
    );
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert!(
        json::from_slice::<Value>(&body).unwrap()["text"]
            .as_str()
            .unwrap()
            .contains("Mock transcript")
    );
    let (status, headers, body) = server.post(
        "/v1/audio/speech",
        json!({"input":"hello","response_format":"wav"}),
    );
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert!(headers.contains("audio/wav"));
    assert_eq!(&body[..4], b"RIFF");
    assert_eq!(&body[8..12], b"WAVE");
    let (status, _, body) = server.post(
        "/v1/chat/completions",
        json!({"model":"mock-speak","stream":true,"messages":[{"role":"user","content":"hello"}]}),
    );
    assert_eq!(status, 200);
    assert!(
        String::from_utf8(body)
            .unwrap()
            .contains("data:audio/wav;base64,")
    );
    for streaming in [false, true] {
        let (status, headers, body) = server.post("/v1/chat/completions", json!({"model":"mock-transcribe","stream":streaming,"messages":[{"role":"user","content":[{"type":"text","text":"Summarize"},{"type":"input_audio","input_audio":{"format":"wav","data":"AA=="}}]}]}));
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        assert!(headers.contains("x-lh-route: mock-transcribe"));
        let text = String::from_utf8(body).unwrap();
        if streaming {
            let joined = text
                .lines()
                .filter_map(|s| s.strip_prefix("data: "))
                .filter_map(|s| json::from_str::<Value>(s).ok())
                .filter_map(|v| {
                    v["choices"][0]["delta"]["content"]
                        .as_str()
                        .map(str::to_owned)
                })
                .collect::<String>();
            assert!(joined.contains("Mock transcript"), "{joined}");
        } else {
            assert!(text.contains("Mock transcript"));
        }
    }
    let (status, headers, body) = server.post("/v1/chat/completions", json!({"model":"auto","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AA=="}},{"type":"text","text":"picture"}]},{"role":"assistant","content":"A picture"},{"role":"user","content":"describe it again"}]}));
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert!(headers.contains("x-lh-route: mock-vision"));
    for url in [
        "https://example.test/a.png",
        "file:/etc/passwd",
        "/etc/passwd",
        "data:image/png;base64,???",
        "data:image/png;base64,",
    ] {
        assert_eq!(server.post("/v1/chat/completions", json!({"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":url}}]}]})).0, 400);
        assert_eq!(
            server
                .post("/v1/route", json!({"prompt":"image","images":[url]}))
                .0,
            400
        );
    }
    assert_eq!(
        server
            .post("/v1/chat/completions", json!({"messages":[]}))
            .0,
        400
    );
    assert_eq!(server.post("/v1/chat/completions", json!({"messages":[{"role":"user","content":[{"type":"input_audio","input_audio":{"format":"wav","data":"file:/etc/passwd"}}]}]})).0, 400);
    assert_eq!(
        server
            .post("/v1/decide", json!({"state":"x","files":["/etc/passwd"]}))
            .0,
        400
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/v1/audio/transcriptions",
                b"broken",
                "multipart/form-data",
                None
            )
            .0,
        400
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/v1/chat/completions",
                b"{",
                "application/json",
                None
            )
            .0,
        400
    );
    assert_eq!(
        server
            .post(
                "/v1/audio/speech",
                json!({"input":"hi","response_format":"mp3"})
            )
            .0,
        400
    );
    assert_eq!(
        server
            .request("GET", "/missing", b"", "application/json", None)
            .0,
        404
    );
    assert_eq!(
        server
            .request("GET", "/v1/chat/completions", b"", "application/json", None)
            .0,
        405
    );
    let oversized = vec![b' '; 6 * 1024 * 1024 + 1];
    assert_eq!(
        server
            .request(
                "POST",
                "/v1/chat/completions",
                &oversized,
                "application/json",
                None
            )
            .0,
        413
    );
}
#[test]
fn bearer_auth_protects_every_endpoint() {
    let server = Server::start(true);
    for path in [
        "/v1/models",
        "/v1/chat/completions",
        "/v1/audio/transcriptions",
        "/v1/audio/speech",
        "/v1/route",
        "/v1/decide",
    ] {
        for token in [None, Some("incorrect")] {
            assert_eq!(
                server
                    .request("GET", path, b"", "application/json", token)
                    .0,
                401
            );
        }
    }
    assert_eq!(
        server
            .request(
                "GET",
                "/v1/models",
                b"",
                "application/json",
                Some("test-secret")
            )
            .0,
        200
    );
}
#[test]
fn ctrl_c_drains_and_exits_cleanly() {
    let mut server = Server::start(false);
    assert_eq!(server.post("/v1/chat/completions", json!({"model":"mock-text","messages":[{"role":"user","content":"initialize backend"}]})).0, 200);
    assert!(
        Command::new("kill")
            .args(["-INT", &server.child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "Ctrl-C shutdown timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn concurrent_streams_do_not_cross_contaminate() {
    let server = Server::start(false);
    std::thread::scope(|scope| {
        for text in [
            "first isolated request",
            "second isolated request",
            "third isolated request",
        ] {
            let server = &server;
            scope.spawn(move || {
                let (status, _, body) = server.post("/v1/chat/completions", json!({"model":"mock-text","stream":true,"messages":[{"role":"user","content":text}]}));
                assert_eq!(status, 200);
                let events = String::from_utf8(body).unwrap();
                let content: String = events.lines().filter_map(|s| s.strip_prefix("data: ")).filter(|s| *s != "[DONE]").map(|s| {
                    let value: Value = json::from_str(s).unwrap();
                    value["choices"][0]["delta"]["content"].as_str().unwrap_or("").to_owned()
                }).collect();
                assert_eq!(content, text);
            });
        }
    });
}

#[test]
fn missing_token_fails_closed() {
    let server = Server::start(true);
    let output = Command::new(env!("CARGO_BIN_EXE_lh"))
        .current_dir(&server.directory)
        .env_remove("LH_CONFIG")
        .env("XDG_CONFIG_HOME", &server.directory)
        .env_remove("LH_TEST_SERVER_TOKEN")
        .args(["serve", "--bind", "127.0.0.1:0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing or empty"));
}
