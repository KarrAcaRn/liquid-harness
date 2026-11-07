//! Real plugin stdio -> fake ft HTTP -> stream notifications -> process cleanup.
use lh_protocol::json::{self, Value, json};
use std::{
    io::Write,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
};

#[test]
fn binary_spawn_stream_and_shutdown() {
    let dir = std::env::temp_dir().join(format!("lh-ft-process-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let script = dir.join("bin/ft");
    std::fs::write(&script, include_str!("../src/fake_ft.py")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let report = dir.join("report.json");
    let config = json!({"load_timeout_secs":3,"models":{"hf:LiquidAI/LFM3-1.7B":{"spawn":{"venv":dir,"model_path":"LiquidAI/LFM3-1.7B","env":{"FT_TEST_REPORT":report,"FT_TEST_VALUE":"test","FT_TEST_STATE":"serving"}}}}});
    let mut child = Command::new(env!("CARGO_BIN_EXE_lh-backend-freetoken"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for (id, method, params) in [
        (1, "initialize", json!({"protocol":1,"config":config})),
        (
            2,
            "backend/load",
            json!({"key":"text","model":{"source":"hf:LiquidAI/LFM3-1.7B"}}),
        ),
        (
            3,
            "backend/chat",
            json!({"key":"text","messages":[{"role":"user","content":"hello"}],"stream":true}),
        ),
    ] {
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
    }
    writeln!(input, "{}", json!({"jsonrpc":"2.0","method":"shutdown"})).unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let frames: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|s| json::from_str(s).unwrap())
        .collect();
    let result = frames.iter().find(|v| v["id"] == 3).unwrap();
    assert_eq!(
        result["result"]["message"]["content"], "Grüße",
        "{frames:?}"
    );
    assert!(frames.iter().any(|v| v["method"] == "$/stream"
        && v["params"]["id"] == 3
        && v["params"]["delta"]["content"] == "Grüße"));
    assert!(frames.iter().any(|v| v["method"] == "$/progress"));
    let info: Value = json::from_slice(&std::fs::read(report).unwrap()).unwrap();
    let pid = info["pid"].as_i64().unwrap() as i32;
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "server survived shutdown"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
