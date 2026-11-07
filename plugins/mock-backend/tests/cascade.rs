//! Real stdio backend exercises model-call accounting without models or network.
use lh_core::{Config, Engine, Registry, Session};
use lh_protocol::{RouteInput, json::json};
use std::path::Path;

fn engine() -> Engine {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut registry = Registry::from_paths(&[
        root.join("plugins/mock-backend"),
        root.join("tests/fixtures/mock-pack"),
    ])
    .unwrap();
    registry
        .plugins
        .get_mut("mock")
        .unwrap()
        .manifest
        .run
        .as_mut()
        .unwrap()
        .command = env!("CARGO_BIN_EXE_lh-mock-backend").into();
    Engine::new(
        registry,
        Config {
            router: "mock-router".into(),
            default_specialist: "mock-text".into(),
            ..Default::default()
        },
    )
}
fn input(prompt: &str) -> RouteInput {
    RouteInput {
        prompt: prompt.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn repo_d1_criteria_never_contain_mock_specialists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let mut registry = Registry::from_paths(&[root]).unwrap();
    registry
        .plugins
        .get_mut("llamacpp")
        .unwrap()
        .manifest
        .run
        .as_mut()
        .unwrap()
        .command = env!("CARGO_BIN_EXE_lh-mock-backend").into();
    let mut e = Engine::new(registry, Config::default());
    e.config.cascade.enabled = false;
    let d = e
        .route(&input("coding programming"), Some("d1"))
        .await
        .unwrap();
    assert!(d.model_called);
    assert_eq!(d.scores.len(), 4);
    assert!(d.scores.keys().all(|id| id.starts_with("lfm3-")));
    e.shutdown().await;
}

#[tokio::test]
async fn cache_avoids_model_and_baseline_really_calls_it() {
    let mut e = engine();
    let first = e.route(&input("coding programming"), None).await.unwrap();
    assert!(first.model_called);
    let second = e.route(&input("coding   programming"), None).await.unwrap();
    assert_eq!(second.reason, "cache");
    assert!(!second.model_called);
    let host = e.host("mock").unwrap();
    assert_eq!(
        host.call("backend/status", json!({})).await.unwrap()["decide_calls"],
        1
    );
    e.config.cascade.enabled = false;
    assert!(
        e.route(&input("coding programming"), None)
            .await
            .unwrap()
            .model_called
    );
    let mut image = input("picture");
    image.images.push("fixture.png".into());
    assert!(e.route(&image, None).await.unwrap().model_called);
    assert_eq!(
        host.call("backend/status", json!({})).await.unwrap()["decide_calls"],
        3
    );
    e.shutdown().await;
}

#[tokio::test]
async fn file_content_changes_invalidate_cache_and_remote_media_bypass_it() {
    let mut e = engine();
    e.config.cascade.single_candidate = false;
    let file = std::env::temp_dir().join(format!("lh-cascade-image-{}", std::process::id()));
    std::fs::write(&file, b"first").unwrap();
    let mut i = input("picture");
    i.images.push(file.to_str().unwrap().into());
    assert!(e.route(&i, None).await.unwrap().model_called);
    assert_eq!(e.route(&i, None).await.unwrap().reason, "cache");
    std::fs::write(&file, b"other").unwrap();
    assert!(e.route(&i, None).await.unwrap().model_called);
    std::fs::remove_file(file).unwrap();
    i.images = vec!["https://example.invalid/image.png".into()];
    assert!(e.route(&i, None).await.unwrap().model_called);
    assert!(e.route(&i, None).await.unwrap().model_called);
    e.shutdown().await;
}

#[tokio::test]
async fn sticky_skips_model_but_modality_change_rechecks_candidates() {
    let mut e = engine();
    let mut session = Session::default();
    assert!(
        e.route_session(&input("coding programming"), None, &mut session)
            .await
            .unwrap()
            .model_called
    );
    assert_eq!(
        e.route_session(&input("continue"), None, &mut session)
            .await
            .unwrap()
            .reason,
        "sticky"
    );
    let mut image = input("picture");
    image.images.push("fixture.png".into());
    let d = e.route_session(&image, None, &mut session).await.unwrap();
    assert_eq!(d.specialist, "mock-vision");
    assert_eq!(d.reason, "single-candidate");
    session.reset();
    assert!(session.previous_input.is_none());
    e.shutdown().await;
}
