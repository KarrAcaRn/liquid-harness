#![cfg(feature = "server")]
//! Cross-process smoke with actual MCP plugins, HTTP SSE, and scripted backend.
use std::{path::PathBuf, process::Command};

struct OutputDir(PathBuf);

impl OutputDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("lh-agent-{label}-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for OutputDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn agent_http_management_and_approval_lifecycle() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bin_dir = std::path::Path::new(env!("CARGO_BIN_EXE_lh"))
        .parent()
        .unwrap();
    let profile = bin_dir.file_name().unwrap().to_str().unwrap();
    // Cargo does not build other packages' standalone binaries for this test.
    // Derive the output location from lh, not cwd or an assumed target/debug,
    // so a fresh custom CARGO_TARGET_DIR (and release profiles) works too.
    let build = Command::new(env!("CARGO"))
        .current_dir(&root)
        .args(["build", "--offline", "--bins", "--profile"])
        .arg(if profile == "debug" { "dev" } else { profile })
        .arg("--target-dir")
        .arg(bin_dir.parent().unwrap())
        .args([
            "-p",
            "lh-mock-backend",
            "-p",
            "lh-tools-fs",
            "-p",
            "lh-tools-shell",
            "-p",
            "lh-tools-web",
        ])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "fixture build failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let output = OutputDir::new("http");
    let result = Command::new("python3")
        .arg(root.join("scripts/agent-smoke.py"))
        .arg("--out")
        .arg(&output.0)
        .env("LH_BIN_DIR", bin_dir)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn output_directory_is_removed_on_unwind() {
    let output = OutputDir::new("cleanup");
    let path = output.0.clone();
    std::fs::write(path.join("partial.json"), "partial smoke output").unwrap();
    let result = std::panic::catch_unwind(move || {
        let _output = output;
        panic!("simulate smoke assertion failure");
    });
    assert!(result.is_err());
    assert!(!path.exists());
}
