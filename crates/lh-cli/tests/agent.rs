#![cfg(feature = "server")]
//! Cross-process smoke with actual MCP plugins, HTTP SSE, and scripted backend.
#[test]
fn agent_http_management_and_approval_lifecycle() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::env::temp_dir().join(format!("lh-agent-http-{}", std::process::id()));
    let result = std::process::Command::new("python3")
        .arg(root.join("scripts/agent-smoke.py"))
        .arg("--out")
        .arg(&output)
        .env(
            "LH_BIN_DIR",
            std::path::Path::new(env!("CARGO_BIN_EXE_lh"))
                .parent()
                .unwrap(),
        )
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    std::fs::remove_dir_all(output).unwrap();
}
