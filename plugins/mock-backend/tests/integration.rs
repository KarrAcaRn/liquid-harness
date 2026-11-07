use lh_core::{Config, Engine, Host, Registry, registry::Entry};
use lh_protocol::{
    Manifest, RouteInput,
    json::{self, Value, json},
    toml,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

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
    r
}
fn host() -> Host {
    Host::new(
        registry().plugins["mock"].clone(),
        BTreeMap::from([("enable_test_methods".into(), json!(true))]),
    )
}
async fn load(host: &Host) {
    assert_eq!(
        host.call(
            "backend/load",
            json!({"key":"test","model":{"source":"file:mock"}})
        )
        .await
        .unwrap()["ready"],
        true
    );
}
fn input(prompt: &str) -> RouteInput {
    RouteInput {
        prompt: prompt.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn handshake_config_load_unicode_stream_and_clean_shutdown() {
    let host = host();
    let mut notifications = host.subscribe();
    let status = host.call("backend/status", json!({})).await.unwrap();
    let pid = status["pid"].as_u64().unwrap();
    assert_eq!(status["config"]["enable_test_methods"], true);
    assert_eq!(notifications.recv().await.unwrap().method, "$/log");
    assert_eq!(
        host.call("backend/chat", json!({"key":"test","messages":[]}))
            .await
            .unwrap_err()
            .code,
        -32001
    );
    load(&host).await;
    let text = "Hello Grüße 🦀!";
    let result = host.call("backend/chat", json!({"key":"test","messages":[{"role":"user","content":text}],"params":{},"stream":true})).await.unwrap();
    assert_eq!(result["message"]["content"], text);
    let mut chunks = Vec::new();
    let mut id = None;
    for _ in 0..3 {
        let n = notifications.recv().await.unwrap();
        assert_eq!(n.method, "$/stream");
        if let Some(previous) = id {
            assert_eq!(n.params["id"], previous);
        }
        id = Some(n.params["id"].clone());
        chunks.push(n.params["delta"]["content"].as_str().unwrap().to_owned());
    }
    assert_eq!(chunks.concat(), text);
    assert!(notifications.try_recv().is_err());
    host.call("backend/unload", json!({"key":"test"}))
        .await
        .unwrap();
    assert_eq!(
        host.call("backend/status", json!({})).await.unwrap()["loaded"],
        json!([])
    );
    host.shutdown().await;
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "child must be reaped before shutdown returns"
    );
    assert!(host.call("backend/status", json!({})).await.is_err());
}

#[tokio::test]
async fn concurrent_calls_are_correlated() {
    let h = host();
    load(&h).await;
    let mut calls = Vec::new();
    for i in 0..20 {
        let h = h.clone();
        calls.push(tokio::spawn(async move {
            let s = format!("message-{i}");
            let r = h
                .call(
                    "backend/chat",
                    json!({"key":"test","messages":[{"role":"user","content":s}]}),
                )
                .await
                .unwrap();
            assert_eq!(r["message"]["content"], s);
        }));
    }
    for call in calls {
        call.await.unwrap();
    }
    h.shutdown().await;
}

#[tokio::test]
async fn crash_restart_and_rate_limit() {
    let h = host();
    let first = h.call("backend/status", json!({})).await.unwrap()["pid"].clone();
    for attempt in 0..4 {
        assert!(h.call("mock/crash", json!({})).await.is_err());
        let next = h.call("backend/status", json!({})).await;
        if attempt < 3 {
            assert_ne!(next.unwrap()["pid"], first);
        } else {
            assert!(next.unwrap_err().message.contains("restart limit"));
        }
    }
    h.shutdown().await;
}

#[tokio::test]
async fn timeout_fails_all_inflight_and_next_use_restarts() {
    let h = Host::with_timeout(
        registry().plugins["mock"].clone(),
        BTreeMap::from([("enable_test_methods".into(), json!(true))]),
        Duration::from_millis(150),
    );
    let pid = h.call("backend/status", json!({})).await.unwrap()["pid"].clone();
    let (a, b) = tokio::join!(
        h.call("mock/hang", json!({})),
        h.call("backend/status", json!({}))
    );
    assert!(a.unwrap_err().message.contains("timed out"));
    assert!(b.unwrap_err().message.contains("timed out"));
    assert_ne!(
        h.call("backend/status", json!({})).await.unwrap()["pid"],
        pid
    );
    h.shutdown().await;
}

#[tokio::test]
async fn mock_supports_all_three_decision_primitives() {
    let h = host();
    load(&h).await;
    let r = h.call("backend/decide", json!({"key":"test","state":"refund billing","questions":{
        "route":{"type":"choice","instructions":"team","criteria":{"billing":"refund billing","technical":"broken software"}},
        "refund":{"type":"noul","instructions":"refund"},
        "urgency":{"type":"score","instructions":"priority","criteria":["low","medium","high"]}
    }})).await.unwrap();
    assert_eq!(r["answers"]["route"]["choice"], "billing");
    assert_eq!(r["answers"]["refund"]["noul"], 0.9);
    assert_eq!(r["answers"]["urgency"]["score"], 1.0);
    assert_eq!(r["answers"]["urgency"]["legend"]["2"], "high");
    let _: lh_protocol::DecideResponse = json::from_value(r).unwrap();
    h.shutdown().await;
}

#[tokio::test]
async fn rules_text_image_audio_and_unsupported_combination() {
    let mut e = Engine::new(
        registry(),
        Config {
            default_specialist: "mock-text".into(),
            ..Default::default()
        },
    );
    assert_eq!(
        e.route(&input("hello"), None).await.unwrap().specialist,
        "mock-text"
    );
    let mut i = input("describe");
    i.images.push("picture.png".into());
    let result = e.route(&i, None).await.unwrap();
    assert_eq!(result.specialist, "mock-vision");
    assert_eq!(result.reason, "single-candidate");
    i.images.clear();
    i.audio.push("sound.wav".into());
    assert_eq!(e.route(&i, None).await.unwrap().specialist, "mock-audio");
    i.images.push("picture.png".into());
    assert_eq!(e.route(&i, None).await.unwrap_err().code, -32003);
    e.shutdown().await;
}

#[tokio::test]
async fn decision_model_makes_exactly_one_decide_call_and_has_scores() {
    let mut e = Engine::new(
        registry(),
        Config {
            router: "mock-router".into(),
            ..Default::default()
        },
    );
    let d = e
        .route(&input("coding programming tools"), None)
        .await
        .unwrap();
    assert_eq!(d.specialist, "mock-text");
    assert_eq!(d.reason, "decision-model");
    assert!(d.confidence > 0.8);
    assert_eq!(d.scores.len(), 3);
    let h = e.host("mock").unwrap();
    assert_eq!(
        h.call("backend/status", json!({})).await.unwrap()["decide_calls"],
        1
    );
    assert_eq!(
        e.route(&input("picture screenshot"), None)
            .await
            .unwrap()
            .specialist,
        "mock-vision"
    );
    assert_eq!(
        e.route(&input("audio speech"), None)
            .await
            .unwrap()
            .specialist,
        "mock-audio"
    );
    e.shutdown().await;
}

#[tokio::test]
async fn low_confidence_falls_back_and_rules_override_skips_decide() {
    let mut e = Engine::new(
        registry(),
        Config {
            default_specialist: "mock-text".into(),
            router: "mock-router".into(),
            ..Default::default()
        },
    );
    let d = e.route(&input("xyzzy"), None).await.unwrap();
    assert_eq!(d.reason, "low-confidence");
    assert_eq!(d.specialist, "mock-text");
    assert!((d.confidence - 1.0 / 3.0).abs() < 1e-10);
    assert_eq!(d.scores.len(), 3);
    let d = e
        .route(&input("audio speech"), Some("rules"))
        .await
        .unwrap();
    assert_eq!(d.specialist, "mock-text");
    assert_eq!(d.reason, "rules");
    assert_eq!(
        e.host("mock")
            .unwrap()
            .call("backend/status", json!({}))
            .await
            .unwrap()["decide_calls"],
        1
    );
    e.shutdown().await;
}

#[tokio::test]
async fn explicit_and_single_candidate_bypass_backend() {
    let mut r = registry();
    r.plugins
        .get_mut("mock")
        .unwrap()
        .manifest
        .run
        .as_mut()
        .unwrap()
        .command = "/missing/lh-backend".into();
    let mut e = Engine::new(
        r,
        Config {
            router: "mock-router".into(),
            ..Default::default()
        },
    );
    let mut i = input("hello");
    i.specialist = Some("mock-audio".into());
    assert_eq!(e.route(&i, None).await.unwrap().reason, "explicit");
    i.images.push("picture.png".into());
    assert_eq!(e.route(&i, None).await.unwrap_err().code, -32003);
    i.specialist = None;
    assert_eq!(e.route(&i, None).await.unwrap().specialist, "mock-vision");
    assert!(e.route(&i, Some("missing")).await.is_err());
    e.shutdown().await;
}

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "lh-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn manifest(&self, name: &str, contents: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("plugin.toml"), contents).unwrap();
        p
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
const MINIMAL: &str = "[plugin]\nid='same'\nversion='1'\n";
#[test]
fn discovery_deduplicates_paths_but_rejects_duplicate_ids_with_both_files() {
    let temp = Temp::new();
    let a = temp.manifest("a", MINIMAL);
    let r = Registry::from_paths(&[a.clone(), a.clone()]).unwrap();
    assert_eq!(r.plugins.len(), 1);
    let b = temp.manifest("b", MINIMAL);
    let err = Registry::from_paths(std::slice::from_ref(&temp.0)).unwrap_err();
    assert!(
        err.message
            .contains(a.join("plugin.toml").to_str().unwrap())
    );
    assert!(
        err.message
            .contains(b.join("plugin.toml").to_str().unwrap())
    );
}
#[test]
fn discovery_rejects_cross_kind_duplicate_and_bad_protocol() {
    let temp = Temp::new();
    let a = temp.manifest(
        "a",
        "[plugin]\nid='same'\nversion='1'\n[router]\nid='same'\nstrategy='rules'",
    );
    assert!(
        Registry::from_paths(std::slice::from_ref(&a))
            .unwrap_err()
            .message
            .contains("duplicate id")
    );
    std::fs::write(
        a.join("plugin.toml"),
        "[plugin]\nid='same'\nversion='1'\nprotocol=99",
    )
    .unwrap();
    assert!(
        Registry::from_paths(&[a])
            .unwrap_err()
            .message
            .contains("unsupported protocol")
    );
}
#[test]
fn config_explicit_path_relative_plugins_and_strict_missing() {
    let temp = Temp::new();
    let path = temp.0.join("config.toml");
    std::fs::write(&path, "plugin_path=['extra']\ndefault_specialist='custom'").unwrap();
    let (c, dir) = Config::load(Some(&path)).unwrap();
    assert_eq!(dir, temp.0);
    assert_eq!(c.plugin_path, vec![temp.0.join("extra")]);
    assert_eq!(c.default_specialist, "custom");
    assert!(Config::load(Some(&temp.0.join("missing"))).is_err());
}

fn shell_entry(script: &str) -> Entry {
    let mut m: Manifest = toml::from_str(MINIMAL).unwrap();
    m.run = Some(lh_protocol::Run {
        command: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
    });
    Entry {
        path: PathBuf::from("/fixture/plugin.toml"),
        manifest: m,
    }
}
#[tokio::test]
async fn invalid_handshake_and_unknown_response_id_are_rejected() {
    for script in [
        "read line; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{}}'",
        "read line; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"name\":\"test\",\"version\":\"1\",\"provides\":[],\"capabilities\":{}}}'; read line; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":999,\"result\":{}}'",
    ] {
        let h = Host::with_timeout(shell_entry(script), BTreeMap::new(), Duration::from_secs(1));
        assert!(h.call("backend/status", json!({})).await.is_err());
        h.shutdown().await;
    }
}
#[tokio::test]
async fn responses_may_arrive_out_of_order() {
    let script = "read line; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"name\":\"test\",\"version\":\"1\",\"provides\":[],\"capabilities\":{}}}'; read one; read two; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":\"second\"}' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":\"first\"}'; read shutdown";
    let h = Host::new(shell_entry(script), BTreeMap::new());
    let (a, b) = tokio::join!(h.call("one", json!({})), h.call("two", json!({})));
    assert_eq!(a.unwrap(), "first");
    assert_eq!(b.unwrap(), "second");
    h.shutdown().await;
}
#[tokio::test]
async fn lazy_spawn_does_not_execute_until_called() {
    let temp = Temp::new();
    let marker = temp.0.join("started");
    let mut entry = shell_entry("printf started > \"$1\"");
    entry
        .manifest
        .run
        .as_mut()
        .unwrap()
        .args
        .extend(["fixture".into(), marker.to_string_lossy().into_owned()]);
    let h = Host::new(entry, BTreeMap::<String, Value>::new());
    tokio::task::yield_now().await;
    assert!(!marker.exists());
    assert!(h.call("backend/status", json!({})).await.is_err());
    assert!(marker.exists());
    h.shutdown().await;
}

#[tokio::test]
async fn crash_fails_every_inflight_call() {
    let script = "read line; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":0,\"result\":{\"name\":\"test\",\"version\":\"1\",\"provides\":[],\"capabilities\":{}}}'; read one; read two; exit 17";
    let h = Host::new(shell_entry(script), BTreeMap::new());
    let (a, b) = tokio::join!(h.call("one", json!({})), h.call("two", json!({})));
    assert!(a.is_err());
    assert!(b.is_err());
    h.shutdown().await;
}

#[tokio::test]
async fn bidirectional_backpressure_does_not_block_response_processing() {
    let h = Host::with_timeout(
        registry().plugins["mock"].clone(),
        BTreeMap::from([("enable_test_methods".into(), json!(true))]),
        Duration::from_secs(2),
    );
    load(&h).await;
    let content = "large".repeat(200_000);
    let (stream, chat) = tokio::join!(
        h.call("mock/flood", json!({})),
        h.call(
            "backend/chat",
            json!({"key":"test","messages":[{"role":"user","content":content}]})
        )
    );
    assert_eq!(stream.unwrap()["done"], true);
    assert_eq!(chat.unwrap()["message"]["content"], content);
    h.shutdown().await;
}
