//! Linux parent-death cleanup without a model or external server.
use super::*;
use std::path::Path;

#[test]
fn parent_exit_fixture() {
    let Some(path) = std::env::var_os("LH_PARENT_EXIT_PID_FILE") else {
        return;
    };
    let executable = std::env::current_exe().unwrap();
    let process = spawn(
        executable.to_str().unwrap(),
        &["--exact".into(), "tests::child_fixture".into()],
        None,
    )
    .unwrap();
    std::fs::write(path, process.0.id().to_string()).unwrap();
    // Simulate an abrupt plugin exit: do not run Process::drop.
    std::process::exit(0);
}

#[test]
fn parent_death_kills_direct_server() {
    let path = std::env::temp_dir().join(format!("lh-parent-death-{}", std::process::id()));
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_tests::parent_exit_fixture"])
        .env("LH_PARENT_EXIT_PID_FILE", &path)
        .env("LH_LLAMA_TEST_CHILD", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let pid: i32 = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    std::fs::remove_file(path).unwrap();
    let start = Instant::now();
    let proc_path = format!("/proc/{pid}/stat");
    while start.elapsed() < Duration::from_secs(3) {
        // PID 1 may not yet have reaped the killed grandchild.
        if !Path::new(&proc_path).exists()
            || std::fs::read_to_string(&proc_path).is_ok_and(|s| {
                s.rsplit_once(')')
                    .is_some_and(|(_, rest)| rest.starts_with(" Z"))
            })
        {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    panic!("server survived plugin exit");
}
