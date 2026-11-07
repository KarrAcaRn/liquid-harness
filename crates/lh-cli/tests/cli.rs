use lh_protocol::json::{self, Value};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "lh-cli-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_lh"));
        c.current_dir(&self.0)
            .env_remove("LH_CONFIG")
            .env("XDG_CONFIG_HOME", &self.0)
            .env(
                "LH_PLUGIN_PATH",
                std::env::join_paths(["mock-backend", "mock-pack"].map(|name| {
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../plugins")
                        .join(name)
                }))
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
            );
        c
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn output(mut c: Command, args: &[&str]) -> Value {
    let o = c.args(args).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    json::from_slice(&o.stdout).unwrap()
}
fn failed(o: Output, expected: &str) {
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains(expected));
}
#[test]
fn list_shows_capabilities_and_opt_in_without_starting_backends() {
    let f = Fixture::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let mut command = f.command();
    command.env("LH_PLUGIN_PATH", &root);
    let listing = output(command, &["plugins", "list"]);
    let plugins = listing["plugins"].as_array().unwrap();
    let ft = plugins.iter().find(|p| p["id"] == "freetoken").unwrap();
    assert_eq!(ft["status"], "disabled");
    assert_eq!(ft["capabilities"], json::json!(["chat"]));
    let llama = plugins.iter().find(|p| p["id"] == "llamacpp").unwrap();
    assert_eq!(llama["status"], "not-started");
    assert_eq!(
        llama["capabilities"],
        json::json!(["chat", "decide", "stream"])
    );
    let config = f.0.join("enabled.toml");
    std::fs::write(&config, "[plugins.freetoken]\nenabled=true\n").unwrap();
    let mut command = f.command();
    command.env("LH_PLUGIN_PATH", root);
    let listing = output(
        command,
        &["--config", config.to_str().unwrap(), "plugins", "list"],
    );
    assert_eq!(
        listing["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "freetoken")
            .unwrap()["status"],
        "not-started"
    );
}

#[test]
fn run_streams_unicode_and_local_image_and_pull_loads_both_kinds() {
    let f = Fixture::new();
    let image = f.0.join("image.PNG");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\n").unwrap();
    for args in [
        vec!["run", "--model", "mock-text", "Grüße 🌍"],
        vec!["run", "--image", image.to_str().unwrap(), "Grüße 🌍"],
        vec!["run", "--router", "mock-router", "coding Grüße 🌍"],
    ] {
        let o = f.command().args(&args).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(
            String::from_utf8(o.stdout).unwrap(),
            format!("{}\n", args.last().unwrap())
        );
    }
    for (id, key) in [
        ("mock-text", "mock-text"),
        ("mock-router", "router:mock-router"),
    ] {
        let result = output(f.command(), &["models", "pull", id]);
        assert_eq!(result["ready"], true);
        assert_eq!(result["key"], key);
    }
    failed(
        f.command()
            .args(["models", "pull", "missing"])
            .output()
            .unwrap(),
        "unknown specialist or router",
    );
    failed(
        f.command()
            .args([
                "run",
                "--model",
                "mock-text",
                "--image",
                image.to_str().unwrap(),
                "hello",
            ])
            .output()
            .unwrap(),
        "-32003",
    );
    failed(
        f.command()
            .args(["run", "--image", "missing.png", "hello"])
            .output()
            .unwrap(),
        "missing.png",
    );
}
#[test]
fn eval_reports_separate_latencies_accuracy_and_errors() {
    let f = Fixture::new();
    let cases = f.0.join("cases.jsonl");
    std::fs::write(
        &cases,
        concat!(
            "{\"prompt\":\"coding programming\",\"expected\":\"mock-text\"}\n",
            "{\"prompt\":\"picture\",\"images\":[\"fixture.png\"],\"expected\":\"mock-vision\"}\n",
            "{\"prompt\":\"hello\",\"specialist\":\"mock-text\",\"expected\":\"mock-audio\"}\n"
        ),
    )
    .unwrap();
    let args = [
        "eval",
        "routing",
        "--file",
        cases.to_str().unwrap(),
        "--router",
        "mock-router",
    ];
    let report = output(f.command(), &args);
    assert_eq!(report["total"], 3);
    assert_eq!(report["correct"], 2);
    assert_eq!(report["error_count"], 0);
    assert_eq!(report["latency"]["decision_model"]["count"], 1);
    assert_eq!(report["latency"]["fastpaths"]["count"], 2);
    assert_eq!(report["confusion_matrix"]["mock-audio"]["mock-text"], 1);
    assert!(report["loadavg"]["before"].is_string());
    std::fs::write(&cases, "not json\n{\"prompt\":\"hello\",\"expected\":\"missing\"}\n{\"prompt\":\"hello\",\"expected\":\"mock-text\",\"specialist\":\"missing\"}\n").unwrap();
    let o = f.command().args(args).output().unwrap();
    assert!(!o.status.success());
    let report: Value = json::from_slice(&o.stdout).unwrap();
    assert_eq!(report["total"], 3);
    assert_eq!(report["error_count"], 3);
    assert_eq!(report["accuracy"], 0.0);
    assert!(report["latency"]["decision_model"]["p50_ms"].is_null());
    std::fs::write(&cases, "\n").unwrap();
    failed(
        f.command().args(args).output().unwrap(),
        "contains no cases",
    );
}
#[test]
fn plugins_list_and_all_route_flags() {
    let f = Fixture::new();
    let list = output(f.command(), &["plugins", "list"]);
    assert_eq!(list["plugins"].as_array().unwrap().len(), 2);
    assert_eq!(list["specialists"].as_array().unwrap().len(), 3);
    assert_eq!(list["routers"][0], "mock-router");
    for (args, id) in [
        (vec!["route", "--explain", "hello"], "mock-text"),
        (
            vec!["route", "--image", "photo.png", "hello"],
            "mock-vision",
        ),
        (vec!["route", "--audio", "voice.wav", "hello"], "mock-audio"),
        (vec!["route", "--router", "rules", "coding"], "mock-text"),
    ] {
        assert_eq!(output(f.command(), &args)["specialist"], id);
    }
    failed(
        f.command()
            .args(["route", "--image", "x", "--audio", "y", "hello"])
            .output()
            .unwrap(),
        "-32003",
    );
    failed(
        f.command()
            .args(["route", "--router", "missing", "hello"])
            .output()
            .unwrap(),
        "unknown router",
    );
}
#[test]
fn config_cli_precedes_environment_and_config_dir_discovery() {
    let f = Fixture::new();
    let env = f.0.join("env.toml");
    let explicit = f.0.join("explicit.toml");
    std::fs::write(&env, "default_specialist='mock-audio'").unwrap();
    std::fs::write(&explicit, "default_specialist='mock-vision'").unwrap();
    let mut c = f.command();
    c.env("LH_CONFIG", &env);
    assert_eq!(output(c, &["route", "hello"])["specialist"], "mock-audio");
    let mut c = f.command();
    c.env("LH_CONFIG", &env).arg("--config").arg(&explicit);
    assert_eq!(output(c, &["route", "hello"])["specialist"], "mock-vision");
    failed(
        f.command()
            .args(["--config", "missing.toml", "plugins", "list"])
            .output()
            .unwrap(),
        "missing.toml",
    );
    // Search the XDG config plugin directory without LH_PLUGIN_PATH.
    let config_plugins = f.0.join("liquid-harness/plugins/one");
    std::fs::create_dir_all(&config_plugins).unwrap();
    std::fs::write(
        config_plugins.join("plugin.toml"),
        "[plugin]\nid='local-pack'\nversion='1'",
    )
    .unwrap();
    let mut c = f.command();
    c.env_remove("LH_PLUGIN_PATH");
    assert_eq!(
        output(c, &["plugins", "list"])["plugins"][0]["id"],
        "local-pack"
    );
}

#[test]
fn plugin_resolution_uses_sibling_without_path_and_reports_search_locations() {
    let f = Fixture::new();
    let mut command = f.command();
    command.env("PATH", "");
    assert_eq!(
        output(
            command,
            &["route", "--router", "mock-router", "coding programming"]
        )["specialist"],
        "mock-text"
    );

    let pack = f.0.join("plugins/mock-backend");
    std::fs::create_dir_all(&pack).unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/mock-backend/plugin.toml");
    let manifest = std::fs::read_to_string(source)
        .unwrap()
        .replace("lh-mock-backend", "nonexistent-lh-plugin");
    std::fs::write(pack.join("plugin.toml"), manifest).unwrap();
    let mut command = f.command();
    command.env("PATH", "").env(
        "LH_PLUGIN_PATH",
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/mock-pack"),
    );
    let result = command
        .args(["route", "--router", "mock-router", "coding"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let error = String::from_utf8(result.stderr).unwrap();
    for expected in [
        "plugin 'mock'",
        "nonexistent-lh-plugin",
        "searched plugin path",
        "executable sibling",
        "PATH=",
    ] {
        assert!(error.contains(expected), "{error}");
    }
}
