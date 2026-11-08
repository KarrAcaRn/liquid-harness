use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "lh-selfinfo-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A llama.cpp-style backend whose executable and llama-server are missing,
/// a fake model cache with one hub-layout and one flat download, and one
/// specialist per interesting case.
fn fixture(tmp: &Temp) -> (Registry, Config) {
    let cache = tmp.0.join("cache");
    tmp.write(
        "cache/models--Org--Here-GGUF/snapshots/abc/Here-Q4_K_M.gguf",
        &[0; 1000],
    );
    tmp.write("cache/org_vision-gguf_Vision-Q4_K_M.gguf", &[0; 300]);
    tmp.write("cache/org_vision-gguf_mmproj-Vision-F16.gguf", &[0; 50]);
    let manifest = format!(
        r#"[plugin]
id = "llamacpp"
version = "1"
provides = ["backend"]
[run]
command = "lh-definitely-missing-backend"
[config]
llama_server = "/nonexistent/llama-server"
models_dir = "{}"

[[specialist]]
id = "here"
route = "r"
task = "chat"
backend = "llamacpp"
model = {{ source = "hf:Org/Here-GGUF:Q4_K_M" }}

[[specialist]]
id = "vision"
route = "r"
task = "chat"
backend = "llamacpp"
model = {{ source = "hf:Org/Vision-GGUF:Q4_K_M", mmproj = "auto" }}

[[specialist]]
id = "absent"
route = "r"
task = "chat"
backend = "llamacpp"
model = {{ source = "hf:Org/Absent-GGUF:Q4_K_M" }}

[[specialist]]
id = "local"
route = "r"
task = "chat"
backend = "llamacpp"
model = {{ source = "file:{}/missing.gguf" }}
"#,
        cache.display(),
        tmp.0.display()
    );
    tmp.write("plugins/llamacpp/plugin.toml", manifest.as_bytes());
    let registry = Registry::from_paths(&[tmp.0.join("plugins")]).unwrap();
    let config = Config {
        directory: tmp.0.join("config"),
        default_specialist: "here".into(),
        ..Default::default()
    };
    (registry, config)
}
fn model<'a>(info: &'a SelfInfo, id: &str) -> &'a Value {
    info.models
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .unwrap()
}
fn codes(info: &SelfInfo) -> Vec<(&'static str, String)> {
    info.problems
        .iter()
        .map(|p| (p.code, p.subject.clone()))
        .collect()
}

#[test]
fn resolves_model_files_and_downloaded_flags_in_a_fake_cache() {
    let tmp = Temp::new();
    let (registry, config) = fixture(&tmp);
    let info = SelfInfo::gather(&Context {
        registry: &registry,
        config: &config,
        lifecycle: None,
        service: None,
    });
    let here = &model(&info, "here")["backends"][0]["local"];
    assert_eq!(here["downloaded"], true, "{here}");
    assert_eq!(here["model_bytes"], 1000);
    assert_eq!(here["layout"], "hf-cache");
    assert!(
        here["model_file"]
            .as_str()
            .unwrap()
            .ends_with("Here-Q4_K_M.gguf")
    );
    let vision = &model(&info, "vision")["backends"][0]["local"];
    assert_eq!(vision["downloaded"], true, "{vision}");
    assert_eq!(vision["layout"], "llama.cpp flat");
    assert_eq!(vision["projector_bytes"], 50);
    let absent = &model(&info, "absent")["backends"][0]["local"];
    assert_eq!(absent["downloaded"], false);
    assert!(absent["model_file"].is_null());
    assert_eq!(
        model(&info, "local")["backends"][0]["local"]["downloaded"],
        false
    );
    let backend = &info.backends[0];
    assert_eq!(backend["kind"], "llama.cpp");
    assert_eq!(backend["model_cache"]["source"], "models_dir");
    assert_eq!(backend["model_cache"]["exists"], true);
}

#[test]
fn doctor_flags_missing_binaries_models_and_bad_skills() {
    let tmp = Temp::new();
    let (registry, config) = fixture(&tmp);
    tmp.write(
        "config/skills/good/SKILL.md",
        b"---\nname: good\ndescription: Fine\n---\nBody\n",
    );
    tmp.write("config/skills/bad/SKILL.md", b"no frontmatter here\n");
    let info = SelfInfo::gather(&Context {
        registry: &registry,
        config: &config,
        lifecycle: None,
        service: None,
    });
    let found = codes(&info);
    for (code, subject) in [
        ("plugin-executable-missing", "llamacpp"),
        ("llama-server-missing", "/nonexistent/llama-server"),
        ("model-not-downloaded", "absent"),
        ("model-file-missing", "local"),
    ] {
        assert!(
            found.iter().any(|(c, s)| *c == code && s == subject),
            "missing {code} for {subject}: {found:?}"
        );
    }
    assert!(
        found
            .iter()
            .any(|(c, s)| *c == "skill-unreadable" && s.ends_with("bad/SKILL.md"))
    );
    assert!(
        !found.iter().any(|(_, s)| s == "here" || s == "vision"),
        "{found:?}"
    );
    assert!(info.has_errors());
    for problem in &info.problems {
        assert!(!problem.hint.is_empty(), "{problem:?}");
    }
    let skills = info.discovery["skills"].as_array().unwrap();
    assert!(
        skills
            .iter()
            .any(|s| s["name"] == "good" && s["error"].is_null())
    );
    assert!(
        skills
            .iter()
            .any(|s| s["name"] == "harness-self" && s["builtin"] == true)
    );
    let plugin = &info.discovery["plugins"][0];
    assert_eq!(plugin["resolved_by"], "missing");
    assert!(plugin["executable"].is_null());
    let sources: Vec<&str> = info.discovery["plugin_search_path"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["source"].as_str().unwrap())
        .collect();
    assert!(sources.ends_with(&["config-dir", "cwd"]), "{sources:?}");
}

#[test]
fn secret_values_are_never_reported() {
    let tmp = Temp::new();
    let (registry, mut config) = fixture(&tmp);
    config.server_token_env = Some("MY_SERVER_TOKEN".into());
    let vars = [
        ("LH_PLUGIN_PATH", "/visible/plugins"),
        ("LH_API_TOKEN", "supersecret-1"),
        ("LH_TOOL_CONFIG", "{\"api_key\":\"supersecret-2\"}"),
        ("MY_SERVER_TOKEN", "supersecret-3"),
        ("HF_TOKEN", "supersecret-4"),
        ("UNRELATED", "not-whitelisted"),
        ("LLAMA_CACHE", "/visible/cache"),
    ];
    let ctx = Context {
        registry: &registry,
        config: &config,
        lifecycle: None,
        service: None,
    };
    let env = env_from(
        &ctx,
        &BTreeSet::new(),
        vars.iter().map(|(k, v)| (k.into(), v.into())),
    );
    let text = env.to_string();
    for secret in ["supersecret", "not-whitelisted"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    assert_eq!(env["variables"]["LH_PLUGIN_PATH"], "/visible/plugins");
    assert_eq!(env["variables"]["LLAMA_CACHE"], "/visible/cache");
    assert_eq!(env["secrets"]["LH_API_TOKEN"], "set");
    assert_eq!(env["secrets"]["LH_TOOL_CONFIG"], "set");
    assert_eq!(env["secrets"]["MY_SERVER_TOKEN"], "set");
    assert_eq!(env["secrets"]["HF_TOKEN"], "set");
    // The whole report never carries the token value either.
    let info = SelfInfo::gather(&ctx).to_value().to_string();
    assert!(!info.contains("supersecret"));
}

#[test]
fn sections_and_context_block_stay_small() {
    let tmp = Temp::new();
    let (registry, config) = fixture(&tmp);
    let info = SelfInfo::gather(&Context {
        registry: &registry,
        config: &config,
        lifecycle: None,
        service: Some(&Service {
            bind: "127.0.0.1:18484".into(),
            log: Some("/tmp/lh.log".into()),
            restart_method: "exec".into(),
            restart: None,
        }),
    });
    assert!(info.section("models").unwrap().is_array());
    assert!(info.section("problems").unwrap().is_array());
    assert!(info.section("secrets").is_none());
    assert_eq!(info.service["url"], "http://127.0.0.1:18484");
    assert_eq!(info.service["restart"]["method"], "exec");
    assert_eq!(info.build["pid"], std::process::id());
    assert!(info.build["started_at"].as_str().unwrap().ends_with('Z'));
    let block = context_block(&info);
    assert!(block.starts_with("Harness environment"), "{block}");
    assert!(block.contains("harness_restart"), "{block}");
    assert!(block.contains("here [specialist]"), "{block}");
    assert!(block.len() <= 1400, "{} chars", block.len());
}
