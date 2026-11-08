//! Self-description and restart for `lh serve`: `GET /lh/v1/self`,
//! `POST /lh/v1/admin/restart`, SIGHUP, the drain and the re-exec.
use super::*;
use lh_core::selfinfo::{self, SelfInfo};
use std::sync::atomic::{AtomicBool, Ordering};

/// One pending restart request; the serve loop waits on it.
#[derive(Default)]
pub(super) struct RestartSignal {
    requested: AtomicBool,
    reason: std::sync::Mutex<Option<String>>,
    wake: tokio::sync::Notify,
}
impl RestartSignal {
    /// False when a restart is already pending.
    pub(super) fn request(&self, reason: String) -> bool {
        if self.requested.swap(true, Ordering::SeqCst) {
            return false;
        }
        *self.reason.lock().unwrap() = Some(reason);
        self.wake.notify_one();
        true
    }
    pub(super) fn pending(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
    pub(super) async fn wait(&self) -> String {
        loop {
            if let Some(reason) = self.reason.lock().unwrap().clone() {
                return reason;
            }
            self.wake.notified().await;
        }
    }
}

/// Registry and config never change while serving, so `/lh/v1/self` answers
/// from this copy without the engine lock (an agent run may hold it).
pub(super) struct Shared {
    pub registry: lh_core::Registry,
    pub config: lh_core::Config,
    pub service: selfinfo::Service,
}
pub(super) fn info(state: &State) -> SelfInfo {
    SelfInfo::gather(&selfinfo::Context {
        registry: &state.shared.registry,
        config: &state.shared.config,
        lifecycle: Some(&state.lifecycle),
        service: Some(&state.shared.service),
    })
}

/// Without a token the admin endpoint only accepts loopback peers.
pub(super) async fn restart(
    state: &State,
    peer: std::net::SocketAddr,
    value: Value,
) -> ApiResult<Reply> {
    if state.token.is_none() && !peer.ip().is_loopback() {
        return Err((
            StatusCode::FORBIDDEN,
            "restart is only allowed from localhost unless server_token_env is configured".into(),
        ));
    }
    let reason = match value.get("reason") {
        None | Some(Value::Null) => "api request".to_owned(),
        Some(Value::String(text)) if !text.trim().is_empty() => text.chars().take(200).collect(),
        Some(_) => return Err(bad("reason must be a non-empty string")),
    };
    if !state.restart.request(format!("api: {reason}")) {
        return Err((
            StatusCode::CONFLICT,
            "a restart is already in progress".into(),
        ));
    }
    eprintln!("lh: restart requested ({reason})");
    Ok(json_response(json!({
        "restarting": true,
        "reason": reason,
        "pid": std::process::id(),
        "method": state.shared.service.restart_method,
        "drain_timeout_secs": state.shared.config.service.drain_timeout_secs,
    })))
}

/// After the drain: run the supervisor command (and return, so the process
/// exits) or replace this process with the same binary, argv and environment.
/// `bound` replaces a `--bind` port 0, so the restarted server keeps its port.
pub(super) fn relaunch(
    config: &lh_core::Config,
    reason: &str,
    bound: std::net::SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = selfinfo::runtime();
    let count = (runtime.restart_count + 1).to_string();
    if let Some(command) = &config.service.restart_command {
        eprintln!("lh: restart ({reason}): running restart_command and exiting");
        let mut child = std::process::Command::new("/bin/sh");
        child
            .arg("-c")
            .arg(command)
            .env("LH_RESTART_COUNT", &count)
            .env("LH_RESTART_REASON", reason)
            .stdin(std::process::Stdio::null());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut child, 0);
        child.spawn()?;
        return Ok(());
    }
    let exe = runtime
        .exe
        .clone()
        .ok_or("cannot restart: the path of the running binary is unknown")?;
    eprintln!("lh: restart ({reason}): re-executing {}", exe.display());
    #[cfg(unix)]
    {
        let error = std::os::unix::process::CommandExt::exec(
            std::process::Command::new(&exe)
                .args(keep_port(&runtime.argv[1..], bound))
                .env("LH_RESTART_COUNT", &count)
                .env("LH_RESTART_REASON", reason),
        );
        Err(format!("re-exec of {} failed: {error}", exe.display()).into())
    }
    #[cfg(not(unix))]
    Err("in-place restart needs a Unix host; set [service] restart_command".into())
}
/// `--bind host:0` (or `--bind=host:0`) becomes the port actually bound.
pub(super) fn keep_port(args: &[String], bound: std::net::SocketAddr) -> Vec<String> {
    let fix = |value: &str| match value.rsplit_once(':') {
        Some((host, "0")) => format!("{host}:{}", bound.port()),
        _ => value.to_owned(),
    };
    let mut out = Vec::with_capacity(args.len());
    let mut next_is_bind = false;
    for arg in args {
        if next_is_bind {
            out.push(fix(arg));
            next_is_bind = false;
        } else if let Some(value) = arg.strip_prefix("--bind=") {
            out.push(format!("--bind={}", fix(value)));
        } else {
            next_is_bind = arg == "--bind";
            out.push(arg.clone());
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restart_keeps_an_ephemeral_port() {
        let bound: std::net::SocketAddr = "127.0.0.1:41234".parse().unwrap();
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            keep_port(&args(&["serve", "--bind", "127.0.0.1:0"]), bound),
            args(&["serve", "--bind", "127.0.0.1:41234"])
        );
        assert_eq!(
            keep_port(
                &args(&["--config", "x:0", "serve", "--bind=0.0.0.0:0"]),
                bound
            ),
            args(&["--config", "x:0", "serve", "--bind=0.0.0.0:41234"])
        );
        assert_eq!(
            keep_port(&args(&["serve", "--bind", "127.0.0.1:8484"]), bound),
            args(&["serve", "--bind", "127.0.0.1:8484"])
        );
    }
    #[test]
    fn restart_signal_accepts_one_request() {
        let signal = RestartSignal::default();
        assert!(!signal.pending());
        assert!(signal.request("first".into()));
        assert!(!signal.request("second".into()));
        assert!(signal.pending());
        let reason = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(signal.wait());
        assert_eq!(reason, "first");
    }
}
