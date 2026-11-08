//! Host and process probes for [`super::SelfInfo`]: Linux `/proc`/`/sys`
//! reads, the running executable's identity, llama-server `--version` and
//! timestamp formatting. Every probe degrades to `None` instead of failing.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Identity of a file on disk; a different stamp means a different binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    pub modified_unix: u64,
    pub inode: u64,
}
impl Stamp {
    pub fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(&meta);
        #[cfg(not(unix))]
        let inode = 0;
        Some(Self {
            size: meta.len(),
            modified_unix: meta
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_secs(),
            inode,
        })
    }
}

/// Facts captured once, as early as possible in the process (`capture`).
#[derive(Debug)]
pub struct Runtime {
    pub pid: u32,
    pub started_unix: u64,
    pub started: Instant,
    /// The executable path at start (a later rebuild replaces the file there).
    pub exe: Option<PathBuf>,
    pub exe_stamp: Option<Stamp>,
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// In-place restarts so far (`LH_RESTART_COUNT`, set before re-exec).
    pub restart_count: u64,
    pub restart_reason: Option<String>,
}
static RUNTIME: OnceLock<Runtime> = OnceLock::new();
/// The process facts, captured on first use. `lh` calls this first thing in `main`.
pub fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        // On Linux a replaced binary reads as "<path> (deleted)": the path is
        // still where the new binary lives, which is what a restart executes.
        let exe = std::env::current_exe().ok().map(|p| {
            let text = p.to_string_lossy();
            match text.strip_suffix(" (deleted)") {
                Some(path) => PathBuf::from(path),
                None => p,
            }
        });
        Runtime {
            pid: std::process::id(),
            started_unix: unix_now(),
            started: Instant::now(),
            exe_stamp: exe.as_deref().and_then(Stamp::of),
            exe,
            argv: std::env::args().collect(),
            cwd: std::env::current_dir().ok(),
            restart_count: std::env::var("LH_RESTART_COUNT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            restart_reason: std::env::var("LH_RESTART_REASON")
                .ok()
                .filter(|v| !v.is_empty()),
        }
    })
}
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
/// `2026-11-08T16:38:00Z` without a date library (proleptic Gregorian, UTC).
pub fn rfc3339(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}
pub fn hostname() -> Option<String> {
    read("/proc/sys/kernel/hostname")
        .or_else(|| std::env::var("HOSTNAME").ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}
pub fn kernel() -> Option<String> {
    read("/proc/sys/kernel/osrelease").map(|s| s.trim().to_owned())
}
pub fn os_name() -> Option<String> {
    let text = read("/etc/os-release").or_else(|| read("/usr/lib/os-release"))?;
    text.lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim().trim_matches('"').to_owned())
}
pub fn cpu_model() -> Option<String> {
    read("/proc/cpuinfo")?
        .lines()
        .find_map(|l| {
            l.strip_prefix("model name")
                .or_else(|| l.strip_prefix("Model"))
        })
        .and_then(|rest| rest.split_once(':'))
        .map(|(_, v)| v.trim().to_owned())
}
/// MemTotal, lowered to a cgroup v2 `memory.max` of this process (or an ancestor).
pub fn memory_total() -> Option<(u64, &'static str)> {
    let total = read("/proc/meminfo")?
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|v| v.trim().strip_suffix("kB"))
        .and_then(|v| v.trim().parse::<u64>().ok())?
        * 1024;
    match cgroup_limit() {
        Some(limit) if limit < total => Some((limit, "cgroup memory.max")),
        _ => Some((total, "MemTotal")),
    }
}
fn cgroup_limit() -> Option<u64> {
    let cgroup = read("/proc/self/cgroup")?;
    let relative = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?.trim();
    let mut path = PathBuf::from("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
    let mut best: Option<u64> = None;
    loop {
        if let Some(max) = std::fs::read_to_string(path.join("memory.max"))
            .ok()
            .and_then(|m| m.trim().parse::<u64>().ok())
        {
            best = Some(best.map_or(max, |b| b.min(max)));
        }
        if path == Path::new("/sys/fs/cgroup") || !path.pop() {
            break;
        }
    }
    best
}
/// A best-effort hint, never proof: marker files and PID 1's cgroup.
pub fn container() -> Option<String> {
    if Path::new("/.dockerenv").exists() {
        return Some("docker (/.dockerenv)".into());
    }
    if Path::new("/run/.containerenv").exists() {
        return Some("podman (/run/.containerenv)".into());
    }
    if let Ok(kind) = std::env::var("container")
        && !kind.is_empty()
    {
        return Some(format!("{kind} ($container)"));
    }
    let cgroup = read("/proc/1/cgroup").unwrap_or_default();
    for marker in ["kubepods", "docker", "containerd", "lxc", "libpod"] {
        if cgroup.contains(marker) {
            return Some(format!("{marker} (cgroup of PID 1)"));
        }
    }
    None
}
/// Where stderr (the log) goes: a file path, `terminal`, `pipe` or `null`.
pub fn stderr_destination() -> Option<String> {
    let target = std::fs::read_link("/proc/self/fd/2").ok()?;
    let text = target.to_string_lossy().into_owned();
    Some(if text == "/dev/null" {
        "null".into()
    } else if text.starts_with("/dev/pts/") || text.starts_with("/dev/tty") {
        format!("terminal ({text})")
    } else if text.starts_with("pipe:") {
        "pipe (supervisor or shell redirection)".into()
    } else if text.starts_with("socket:") {
        "socket (journald or supervisor)".into()
    } else {
        text
    })
}

/// `<binary> --version`, first lines only, cached per path and file stamp.
/// Runs with a 5 s deadline; an error text explains a failure.
pub fn version_of(binary: &Path) -> Result<String, String> {
    type Cache = Mutex<BTreeMap<PathBuf, (Option<Stamp>, Result<String, String>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let stamp = Stamp::of(binary);
    let cache = CACHE.get_or_init(Default::default);
    if let Some((cached, result)) = cache.lock().unwrap().get(binary)
        && *cached == stamp
    {
        return result.clone();
    }
    let result = run_version(binary);
    cache
        .lock()
        .unwrap()
        .insert(binary.to_path_buf(), (stamp, result.clone()));
    result
}
fn run_version(binary: &Path) -> Result<String, String> {
    let mut child = std::process::Command::new(binary)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("--version timed out after 5 s".into());
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    // llama.cpp prints its version on stderr.
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(3)
        .collect();
    if output.status.success() {
        Ok(lines.join("; "))
    } else {
        Err(format!(
            "--version exited with {}: {}",
            output.status,
            lines.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats_utc_timestamps() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_794_155_953), "2026-11-08T16:39:13Z");
    }
    #[test]
    fn version_probe_reports_failures() {
        let missing = Path::new("/nonexistent/llama-server");
        assert!(version_of(missing).is_err());
        // Cached: a second call answers without spawning again.
        assert_eq!(version_of(missing), run_version(missing));
    }
}
