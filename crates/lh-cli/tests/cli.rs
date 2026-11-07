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
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins"),
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
