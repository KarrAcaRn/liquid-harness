//! lh-core selection tests using actual stdio mock processes (no inference).
use lh_core::{Config, Engine, Registry};
use lh_protocol::{
    BackendChoice, RouteInput,
    json::{self, json},
};
use std::path::Path;

fn registry() -> Registry {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut r = Registry::from_paths(&[
        root.join("mock-backend"),
        root.join("../tests/fixtures/mock-pack"),
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
    let mut first = r.plugins["mock"].clone();
    first.manifest.plugin.id = "preferred".into();
    r.plugins.insert("preferred".into(), first);
    let specialist = r.specialists.get_mut("mock-text").unwrap();
    specialist.backend = None;
    specialist.model = None;
    specialist.backends = json::from_value::<Vec<BackendChoice>>(json!([
        {"backend":"preferred","model":{"source":"hf:LiquidAI/LFM3-1.7B-NVFP4"}},
        {"backend":"mock","model":{"source":"file:fallback"}}
    ]))
    .unwrap();
    r
}
async fn select(engine: &mut Engine) -> lh_protocol::RouteDecision {
    let mut decision = engine
        .route(
            &RouteInput {
                specialist: Some("mock-text".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(decision.backend, None); // Routing alone never starts inference.
    let host = engine.load_specialist(&mut decision).await.unwrap();
    let answer = host
        .call(
            "backend/chat",
            json!({"key":"mock-text","messages":[{"role":"user","content":"hello"}]}),
        )
        .await
        .unwrap();
    assert_eq!(answer["message"]["content"], "hello");
    let expected = if decision.backend.as_deref() == Some("preferred") {
        "hf:LiquidAI/LFM3-1.7B-NVFP4"
    } else {
        "file:fallback"
    };
    assert_eq!(
        host.call("backend/status", json!({})).await.unwrap()["models"]["mock-text"]["source"],
        expected
    );
    decision
}

#[tokio::test]
async fn load_failure_falls_back_and_records_actual_backend() {
    let mut r = registry();
    r.plugins
        .get_mut("preferred")
        .unwrap()
        .manifest
        .config
        .insert("fail_load".into(), json!(true));
    let mut e = Engine::new(r, Config::default());
    assert_eq!(select(&mut e).await.backend.as_deref(), Some("mock"));
    let status = e
        .host("preferred")
        .unwrap()
        .call("backend/status", json!({}))
        .await
        .unwrap();
    assert_eq!(status["loaded"], json!([]));
    e.shutdown().await;
}

#[tokio::test]
async fn successful_preference_and_user_opt_in_override() {
    let mut r = registry();
    r.plugins
        .get_mut("preferred")
        .unwrap()
        .manifest
        .plugin
        .enabled = false;
    let config = lh_protocol::toml::from_str("[plugins.preferred]\nenabled=true").unwrap();
    let mut e = Engine::new(r, config);
    assert_eq!(select(&mut e).await.backend.as_deref(), Some("preferred"));
    // Unused fallback has no resident model.
    assert_eq!(
        e.host("mock")
            .unwrap()
            .call("backend/status", json!({}))
            .await
            .unwrap()["loaded"],
        json!([])
    );
    e.shutdown().await;
}

#[tokio::test]
async fn disabled_backend_is_not_started_and_capability_mismatch_is_skipped() {
    for mode in ["disabled", "incapable", "missing-binary"] {
        let mut r = registry();
        let p = &mut r.plugins.get_mut("preferred").unwrap().manifest;
        if mode == "disabled" {
            p.plugin.enabled = false;
            p.run.as_mut().unwrap().command = "/does/not/exist".into();
        } else if mode == "missing-binary" {
            p.run.as_mut().unwrap().command = "/does/not/exist".into();
        } else {
            p.config.insert("capabilities".into(), json!(["decide"]));
        }
        let mut e = Engine::new(r, Config::default());
        assert_eq!(select(&mut e).await.backend.as_deref(), Some("mock"));
        if mode == "incapable" {
            assert_eq!(
                e.host("preferred")
                    .unwrap()
                    .call("backend/status", json!({}))
                    .await
                    .unwrap()["loaded"],
                json!([])
            );
        }
        e.shutdown().await;
    }
}

#[tokio::test]
async fn all_failures_are_reported_and_no_backend_is_claimed() {
    let mut r = registry();
    for id in ["preferred", "mock"] {
        r.plugins
            .get_mut(id)
            .unwrap()
            .manifest
            .config
            .insert("fail_load".into(), json!(true));
    }
    let mut e = Engine::new(r, Config::default());
    let mut d = e
        .route(
            &RouteInput {
                specialist: Some("mock-text".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    let error = e.load_specialist(&mut d).await.err().unwrap();
    assert!(error.message.contains("preferred") && error.message.contains("mock"));
    assert_eq!(d.backend, None);
    e.shutdown().await;
}

#[tokio::test]
async fn router_rejects_chat_only_backend_before_load_or_decide() {
    let mut r = registry();
    r.plugins
        .get_mut("mock")
        .unwrap()
        .manifest
        .config
        .insert("capabilities".into(), json!(["chat"]));
    let mut e = Engine::new(r, Config::default());
    let error = e
        .route(
            &RouteInput {
                prompt: "hello".into(),
                ..Default::default()
            },
            Some("mock-router"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, -32003);
    assert!(
        error.message.contains("mock")
            && error.message.contains("mock-router")
            && error.message.contains("decide")
    );
    let host = e.host("mock").unwrap();
    let status = host.call("backend/status", json!({})).await.unwrap();
    assert_eq!(status["loaded"], json!([]));
    assert_eq!(status["decide_calls"], 0);
    // Direct host calls cannot bypass the capability guard either.
    assert_eq!(
        host.call("backend/decide", json!({}))
            .await
            .unwrap_err()
            .code,
        -32003
    );
    e.shutdown().await;
}

#[test]
fn bundled_opt_in_and_manifest_forms_are_validated() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let r = Registry::from_paths(&[root.to_owned()]).unwrap();
    assert!(!r.plugins["freetoken"].manifest.plugin.enabled);
    let text = &r.specialists["lfm3-text"];
    assert_eq!(text.choices().unwrap()[0].backend, "freetoken");
    assert_eq!(text.choices().unwrap()[1].backend, "llamacpp");
    for id in ["lfm3-vl", "lfm3-audio"] {
        assert_eq!(r.specialists[id].choices().unwrap().len(), 1);
    }
    let mut invalid = text.clone();
    invalid.backend = Some("mock".into());
    assert!(invalid.choices().is_err());
    invalid.backends.clear();
    assert!(invalid.choices().is_err());
}
