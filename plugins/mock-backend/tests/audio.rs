//! Offline core dispatch and capability tests, using actual stdio processes.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use lh_core::{Config, Engine, Registry};
use lh_protocol::{
    RouteInput, SpeakResponse, TranscribeResponse,
    json::{self, json},
};
use std::path::Path;

fn registry() -> Registry {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut r = Registry::from_paths(&[
        root.join("plugins/mock-backend"),
        root.join("tests/fixtures/mock-pack"),
        root.join("tests/fixtures/audio-pack"),
    ])
    .unwrap();
    r.plugins
        .get_mut("mock")
        .unwrap()
        .manifest
        .run
        .as_mut()
        .unwrap()
        .command = env!("CARGO_BIN_EXE_lh-mock-backend").into();
    r
}

#[tokio::test]
async fn dispatch_audio_validates_and_preserves_protocol() {
    let mut e = Engine::new(registry(), Config::default());
    let input = RouteInput {
        specialist: Some("mock-transcribe".into()),
        ..Default::default()
    };
    assert!(e.route(&input, None).await.is_err());
    // Pull/prepare bypasses request requirements, not capability checks.
    let selected = e.prepare("mock-transcribe").await.unwrap();
    let response: TranscribeResponse = json::from_value(
        e.dispatch(
            &selected,
            json!({"audio":"fixture.wav","language":"de","key":"wrong"}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(response.text, "Mock transcript (deterministic fixture).");
    assert_eq!(response.language.as_deref(), Some("de"));
    for params in [
        json!({}),
        json!({"audio":" "}),
        json!({"audio":42}),
        json!([]),
    ] {
        assert_eq!(
            e.dispatch(&selected, params).await.unwrap_err().code,
            -32602
        );
    }
    let selected = e.prepare("mock-speak").await.unwrap();
    let response: SpeakResponse = json::from_value(
        e.dispatch(
            &selected,
            json!({"text":"hello","voice":"fixture","language":"en"}),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let wav = STANDARD
        .decode(
            response
                .audio
                .strip_prefix("data:audio/wav;base64,")
                .unwrap(),
        )
        .unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    assert_eq!(
        u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize + 8,
        wav.len()
    );
    assert_eq!(response.sample_rate, 16000);
    assert_eq!(response.duration_ms, 1);
    assert!(e.dispatch(&selected, json!({"text":""})).await.is_err());
    let mut invalid = selected.clone();
    invalid.backend = None;
    assert!(e.dispatch(&invalid, json!({"text":"hello"})).await.is_err());
    for (task, method) in [
        ("chat", "backend/chat"),
        ("decide", "backend/decide"),
        ("transcribe", "backend/transcribe"),
        ("speak", "backend/speak"),
    ] {
        assert_eq!(lh_core::router::task_method(task).unwrap(), method);
    }
    assert!(lh_core::router::task_method("arbitrary").is_err());
    e.shutdown().await;
}

#[tokio::test]
async fn host_guards_audio_capabilities_before_sending() {
    let mut r = registry();
    r.plugins
        .get_mut("mock")
        .unwrap()
        .manifest
        .config
        .insert("capabilities".into(), json!(["chat"]));
    let mut e = Engine::new(r, Config::default());
    let host = e.host("mock").unwrap();
    for method in ["backend/transcribe", "backend/speak"] {
        // Without the host guard this invalid request would return -32602.
        assert_eq!(host.call(method, json!({})).await.unwrap_err().code, -32003);
    }
    assert!(
        e.prepare("mock-transcribe")
            .await
            .unwrap_err()
            .message
            .contains("does not support transcribe")
    );
    assert!(
        e.prepare("mock-speak")
            .await
            .unwrap_err()
            .message
            .contains("does not support speak")
    );
    assert_eq!(
        host.call("backend/status", json!({})).await.unwrap()["loaded"],
        json!([])
    );
    e.shutdown().await;
}
