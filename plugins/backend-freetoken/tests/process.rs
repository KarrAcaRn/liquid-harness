//! Real plugin stdio -> fake ft HTTP -> stream notifications -> process cleanup.
use lh_protocol::json::{self, Value, json};
use std::{
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
};

#[path = "../src/fixture_budget.rs"]
mod fixture_budget;
use fixture_budget::FIXTURE_STARTUP_SECS;

/// Owns the fixture directory. Removes it on every exit path; when the test
/// panics it first kills the recorded fake `ft` process group, which a killed
/// plugin can no longer clean up itself.
struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        if thread::panicking()
            && let Ok(bytes) = std::fs::read(self.0.join("report.json"))
            && let Ok(info) = json::from_slice::<Value>(&bytes)
            && let Some(pid) = info["pid"].as_i64()
        {
            // The fake is spawned with setpgid(0, 0), so its pid is the group id.
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Owns the plugin process. Kills and reaps it if it is still running when
/// the test unwinds before the normal shutdown/wait path completed.
struct Plugin(Child);
impl Drop for Plugin {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn read_all(mut reader: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = reader.read_to_end(&mut buffer);
        buffer
    })
}

#[test]
fn binary_spawn_stream_and_shutdown() {
    // Declared first so it drops last, after the plugin guard.
    let dir = TempDir(std::env::temp_dir().join(format!("lh-ft-process-{}", std::process::id())));
    std::fs::create_dir_all(dir.0.join("bin")).unwrap();
    let script = dir.0.join("bin/ft");
    std::fs::write(&script, include_str!("../src/fake_ft.py")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let report = dir.0.join("report.json");
    // The load budget covers fixture interpreter startup only; this test asserts
    // serving, streaming and shutdown cleanup, not timeout behavior.
    let config = json!({"load_timeout_secs":FIXTURE_STARTUP_SECS,"models":{"hf:LiquidAI/LFM3-1.7B":{"spawn":{"venv":dir.0,"model_path":"LiquidAI/LFM3-1.7B","env":{"FT_TEST_REPORT":report,"FT_TEST_VALUE":"test","FT_TEST_STATE":"serving"}}}}});
    let mut plugin = Plugin(
        Command::new(env!("CARGO_BIN_EXE_lh-backend-freetoken"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = read_all(plugin.0.stdout.take().unwrap());
    let stderr = read_all(plugin.0.stderr.take().unwrap());
    let mut input = plugin.0.stdin.take().unwrap();
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
    let status = plugin.0.wait().unwrap();
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    assert!(status.success(), "{}", String::from_utf8_lossy(&stderr));
    let frames: Vec<Value> = String::from_utf8(stdout)
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
}
