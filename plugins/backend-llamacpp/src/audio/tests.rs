use super::*;
use crate::tests::{dead, fake_http, resident};
use std::os::unix::fs::PermissionsExt;

fn wave() -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend(356u32.to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(16000u32.to_le_bytes());
    bytes.extend(32000u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(320u32.to_le_bytes());
    bytes.extend([0u8; 320]);
    bytes
}
fn script(temp: &Temp, name: &str, body: &str) -> String {
    let path = temp.0.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path.to_string_lossy().into_owned()
}
fn state() -> State {
    let mut state = State {
        models: BTreeMap::new(),
        ttl: Duration::from_secs(60),
        stop: false,
    };
    state.models.insert("audio".into(), resident(false, 0));
    state
}
#[test]
fn wav_validation_and_normalization() {
    let backend = Backend::default();
    let bytes = wave();
    assert_eq!(wav(&bytes).unwrap(), (16000, 10));
    let url = format!("data:audio/wav;base64,{}", STANDARD.encode(&bytes));
    assert_eq!(normalize(&backend, &url).unwrap(), bytes);
    let temp = Temp::new().unwrap();
    let path = temp.0.join("input.wav");
    fs::write(&path, &bytes).unwrap();
    assert_eq!(normalize(&backend, path.to_str().unwrap()).unwrap(), bytes);
    for end in 0..bytes.len() {
        assert!(wav(&bytes[..end]).is_err());
    }
    assert!(normalize(&backend, "data:audio/wav;base64,!!").is_err());
    assert!(normalize(&backend, "https://example.com/audio.wav").is_err());
}
#[test]
fn asr_fake_http_payload_and_errors() {
    let backend = Backend::default();
    let params = json!({"key":"audio","audio":format!("data:audio/wav;base64,{}",STANDARD.encode(wave())),"language":"en"});
    let (url, worker) =
        fake_http(json!({"choices":[{"message":{"content":"Hello world"}}]}).to_string());
    let result = transcribe(&backend, &url, &params).unwrap();
    assert_eq!(result["text"], "Hello world");
    assert_eq!(result["language"], "en");
    let body = worker.join().unwrap();
    assert_eq!(body["temperature"], 0);
    assert_eq!(body["stream"], false);
    assert_eq!(body["messages"][0]["content"], "Perform ASR in English.");
    assert_eq!(
        body["messages"][1]["content"][0]["input_audio"]["format"],
        "wav"
    );
    assert_eq!(
        STANDARD
            .decode(
                body["messages"][1]["content"][0]["input_audio"]["data"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
        wave()
    );
    let (url, worker) = fake_http("{}".into());
    assert!(
        transcribe(&backend, &url, &params)
            .unwrap_err()
            .message
            .contains("ASR text")
    );
    worker.join().unwrap();
}

#[test]
fn asr_http_timeout_is_bounded() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let (_socket, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_millis(200));
    });
    let mut backend = Backend::default();
    backend.agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(30))
        .build();
    let start = Instant::now();
    assert!(transcribe(&backend,&url,&json!({"key":"audio","audio":format!("data:audio/wav;base64,{}",STANDARD.encode(wave()))})).is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
    worker.join().unwrap();
}
#[test]
fn tts_fake_success_arguments_metadata_and_cleanup() {
    let temp = Temp::new().unwrap();
    fs::write(temp.0.join("fixture.wav"), wave()).unwrap();
    let body = format!(
        "printf '%s\\n' \"$@\" > '{0}/args'\nwhile [ \"$1\" != '-o' ]; do shift; done\nshift\nprintf '%s' \"$1\" > '{0}/output-path'\ncp '{0}/fixture.wav' \"$1\"",
        temp.0.display()
    );
    let mut backend = Backend::default();
    backend.tts_executable = Some(script(&temp, "tts", &body));
    let mut state = state();
    let pid = state.models["audio"].process.as_ref().unwrap().0.id();
    let result = speak(
        &backend,
        &mut state,
        &json!({"key":"audio","text":"hello 'world'","voice":"male","language":"en"}),
    )
    .unwrap();
    assert!(dead(pid));
    assert!(state.models["audio"].process.is_none());
    assert_eq!(result["sample_rate"], 16000);
    assert_eq!(result["duration_ms"], 10);
    assert_eq!(
        normalize(&backend, result["audio"].as_str().unwrap()).unwrap(),
        wave()
    );
    let args = fs::read_to_string(temp.0.join("args")).unwrap();
    assert!(args.contains("-p\nhello 'world'\n--no-escape\n"));
    assert!(args.contains("--tts-voice\nmale\n--tts-lang\nen\n"));
    assert!(!args.contains("--port"));
    let output = fs::read_to_string(temp.0.join("output-path")).unwrap();
    assert!(!Path::new(&output).parent().unwrap().exists());
    state.models.get_mut("audio").unwrap().pinned = true;
    assert_eq!(
        speak(&backend, &mut state, &json!({"key":"audio","text":"hello"}))
            .unwrap_err()
            .code,
        -32002
    );
}
#[test]
fn tts_timeout_exit_invalid_output_and_missing_binary() {
    let temp = Temp::new().unwrap();
    for (body, expected) in [
        (
            format!(
                "echo $$ > '{0}/pid'\nwhile [ \"$1\" != '-o' ]; do shift; done; shift; printf '%s' \"$1\" > '{0}/output-path'\nwhile :; do :; done",
                temp.0.display()
            ),
            "timeout",
        ),
        ("exit 7".into(), "exited"),
        (
            "while [ \"$1\" != '-o' ]; do shift; done; shift; printf bad > \"$1\"".into(),
            "WAV",
        ),
    ] {
        let mut backend = Backend::default();
        backend.tts_executable = Some(script(&temp, "tts", &body));
        backend.request_timeout = Duration::from_millis(100);
        let mut state = state();
        let error =
            speak(&backend, &mut state, &json!({"key":"audio","text":"hello"})).unwrap_err();
        assert!(error.message.contains(expected), "{}", error.message);
        assert!(state.models["audio"].process.is_none());
        if expected == "timeout" {
            let pid = fs::read_to_string(temp.0.join("pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert!(dead(pid));
            let output = fs::read_to_string(temp.0.join("output-path")).unwrap();
            assert!(!Path::new(&output).parent().unwrap().exists());
        }
    }
    let mut backend = Backend::default();
    backend.tts_executable = Some(temp.0.join("missing").to_string_lossy().into_owned());
    assert!(
        speak(
            &backend,
            &mut state(),
            &json!({"key":"audio","text":"hello"})
        )
        .unwrap_err()
        .message
        .contains("llama-tts unavailable")
    );
}
#[test]
fn ffmpeg_conversion_missing_and_failure() {
    let temp = Temp::new().unwrap();
    fs::write(temp.0.join("fixture.wav"), wave()).unwrap();
    let body = format!(
        "for arg do output=\"$arg\"; done\ncp '{}/fixture.wav' \"$output\"",
        temp.0.display()
    );
    let mut backend = Backend::default();
    backend.ffmpeg = script(&temp, "ffmpeg", &body);
    assert_eq!(
        normalize(&backend, "data:audio/mp3;base64,YWJj").unwrap(),
        wave()
    );
    backend.ffmpeg = script(&temp, "ffmpeg-fail", "exit 2");
    assert!(
        normalize(&backend, "data:audio/mp3;base64,YWJj")
            .unwrap_err()
            .message
            .contains("exited")
    );
    backend.ffmpeg = temp.0.join("missing").to_string_lossy().into_owned();
    assert!(
        normalize(&backend, "data:audio/mp3;base64,YWJj")
            .unwrap_err()
            .message
            .contains("ffmpeg required")
    );
    backend.executable = "/custom/bin/llama-server".into();
    assert_eq!(tts_binary(&backend), "/custom/bin/llama-tts");
}

#[test]
fn suspended_registration_ttl_and_alias_pinning() {
    let temp = Temp::new().unwrap();
    let mut backend = Backend::default();
    backend.tts_executable = Some(script(&temp, "fail", "exit 2"));
    let mut state = state();
    state.models.insert("alias".into(), resident(true, 0));
    let params = json!({"key":"audio","text":"hello"});
    assert_eq!(
        speak(&backend, &mut state, &params).unwrap_err().code,
        -32002
    );
    state.models.get_mut("alias").unwrap().pinned = false;
    let pids: Vec<_> = state
        .models
        .values()
        .map(|r| r.process.as_ref().unwrap().0.id())
        .collect();
    assert!(speak(&backend, &mut state, &params).is_err());
    assert!(pids.into_iter().all(dead));
    assert!(state.models.values().all(|r| r.process.is_none()));
    state.reap();
    assert_eq!(state.models.len(), 2);
    state.models.get_mut("alias").unwrap().used = Instant::now() - Duration::from_secs(61);
    state.reap();
    assert_eq!(state.models.len(), 1);
    state.room(1).unwrap();
    assert!(state.models.is_empty());
}

#[test]
fn sdk_speak_status_and_lazy_http_restart() {
    let temp = Temp::new().unwrap();
    fs::write(temp.0.join("fixture.wav"), wave()).unwrap();
    let tts = script(
        &temp,
        "tts",
        &format!(
            "while [ \"$1\" != '-o' ]; do shift; done; shift; cp '{}/fixture.wav' \"$1\"",
            temp.0.display()
        ),
    );
    let executable = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .replace('\'', "'\\''");
    let server = script(
        &temp,
        "server",
        &format!(
            "export LH_FAKE_PORT=\"$4\"\nexec '{executable}' --exact tests::server_fixture --nocapture"
        ),
    );
    let backend = Backend::default();
    backend
        .state
        .0
        .lock()
        .unwrap()
        .models
        .insert("audio".into(), resident(false, 0));
    let frames = [
        (
            "initialize",
            json!({"protocol":1,"config":{"llama_server":server,"llama_tts":tts,"request_timeout_secs":2,"load_timeout_secs":2}}),
        ),
        ("backend/speak", json!({"key":"audio","text":"hello"})),
        ("backend/status", json!({})),
        (
            "backend/decide",
            json!({"key":"audio","state":"hi","questions":{}}),
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
    lh_plugin_sdk::serve_io(backend, input.as_bytes(), &mut output).unwrap();
    let responses: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| json::from_str::<Value>(line).unwrap())
        .filter(|v| v.get("id").is_some())
        .collect();
    assert_eq!(responses[1]["result"]["sample_rate"], 16000);
    assert!(responses[2]["result"]["loaded"][0]["pid"].is_null());
    assert_eq!(responses[3]["result"], json!({}));
    let pid = responses[4]["result"]["loaded"][0]["pid"].as_u64().unwrap() as u32;
    assert!(dead(pid));
    assert!(
        responses[0]["result"]["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("transcribe"))
    );
}
