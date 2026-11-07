use lh_plugin_sdk::mcp::{ToolServer, serve, tool};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fs,
    io::{self, Read},
    os::{
        fd::OwnedFd,
        unix::{net::UnixStream, process::CommandExt},
    },
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    workspace_root: PathBuf,
    max_timeout_secs: u64,
    max_output_bytes: usize,
    allow: Vec<String>,
    deny: Vec<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            workspace_root: PathBuf::from("."),
            max_timeout_secs: 120,
            max_output_bytes: 65536,
            allow: vec![],
            deny: vec![],
        }
    }
}
#[derive(Default)]
struct Server {
    config: Config,
    allow: Vec<regex::Regex>,
    deny: Vec<regex::Regex>,
}
struct Group(Child);
impl Group {
    fn kill(&mut self) {
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        self.kill();
        let _ = self.0.wait();
    }
}

struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    cap: usize,
    total: usize,
}
impl Capture {
    fn new(cap: usize) -> Self {
        Self {
            head: vec![],
            tail: VecDeque::new(),
            cap,
            total: 0,
        }
    }
    fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.total = self.total.saturating_add(1);
            if self.head.len() < self.cap / 2 {
                self.head.push(b);
            } else {
                if self.tail.len() >= self.cap - self.cap / 2 {
                    self.tail.pop_front();
                }
                self.tail.push_back(b);
            }
        }
    }
    fn text(&self) -> String {
        let tail: Vec<_> = self.tail.iter().copied().collect();
        if self.total <= self.cap {
            let mut all = self.head.clone();
            all.extend(tail);
            String::from_utf8_lossy(&all).into_owned()
        } else {
            format!(
                "{}\n[truncated: {} bytes omitted]\n{}",
                String::from_utf8_lossy(&self.head),
                self.total - self.cap,
                String::from_utf8_lossy(&tail)
            )
        }
    }
}
impl Server {
    fn cwd(&self, input: &str) -> Result<PathBuf, String> {
        let root = fs::canonicalize(&self.config.workspace_root).map_err(|e| e.to_string())?;
        let mut cwd = root;
        for c in Path::new(input).components() {
            match c {
                Component::CurDir => (),
                Component::Normal(n) => {
                    cwd.push(n);
                    if fs::symlink_metadata(&cwd)
                        .map_err(|e| e.to_string())?
                        .file_type()
                        .is_symlink()
                    {
                        return Err("cwd symlinks are forbidden".into());
                    }
                }
                _ => return Err("cwd must be relative without parent components".into()),
            }
        }
        if !cwd.is_dir() {
            return Err("cwd must be a directory".into());
        }
        Ok(cwd)
    }
    fn run(&self, args: Value) -> Result<String, String> {
        let command = args["command"].as_str().ok_or("command must be a string")?;
        if command.is_empty() {
            return Err("command must not be empty".into());
        }
        if self.deny.iter().any(|r| r.is_match(command))
            || (!self.allow.is_empty() && !self.allow.iter().any(|r| r.is_match(command)))
        {
            return Err("command denied by policy".into());
        }
        let cwd = self.cwd(match args.get("cwd") {
            None => ".",
            Some(v) => v.as_str().ok_or("cwd must be string")?,
        })?;
        let seconds = match args.get("timeout_secs") {
            None => 120.min(self.config.max_timeout_secs),
            Some(v) => v
                .as_u64()
                .filter(|n| *n > 0 && *n <= self.config.max_timeout_secs)
                .ok_or("timeout_secs out of range")?,
        };
        let (mut reader, writer) = UnixStream::pair().map_err(|e| e.to_string())?;
        reader.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stderr = writer.try_clone().map_err(|e| e.to_string())?;
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(OwnedFd::from(writer)))
            .stderr(Stdio::from(OwnedFd::from(stderr)))
            .process_group(0);
        let mut child = Group(cmd.spawn().map_err(|e| e.to_string())?);
        drop(cmd);
        let start = Instant::now();
        let mut capture = Capture::new(self.config.max_output_bytes);
        let mut timed_out = false;
        let mut status = None;
        let mut eof = false;
        loop {
            // Bound each drain pass so an infinite writer cannot starve the deadline.
            for _ in 0..16 {
                let mut buf = [0u8; 8192];
                match reader.read(&mut buf) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(n) => capture.push(&buf[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.to_string()),
                }
            }
            if status.is_none() {
                status = child.0.try_wait().map_err(|e| e.to_string())?;
            }
            if eof && status.is_some() {
                break;
            }
            if start.elapsed() >= Duration::from_secs(seconds) {
                timed_out = true;
                child.kill();
                status = Some(child.0.wait().map_err(|e| e.to_string())?);
                // No blocking drain: escaped descendants must never hold us hostage.
                for _ in 0..16 {
                    let mut buf = [0u8; 8192];
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => capture.push(&buf[..n]),
                    }
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(json!({"output":capture.text(),"exit_code":status.and_then(|s|s.code()),"timed_out":timed_out,"truncated":capture.total>capture.cap}).to_string())
    }
}
impl ToolServer for Server {
    fn name(&self) -> &str {
        "tools-shell"
    }
    fn configure(&mut self, value: Value) -> Result<(), String> {
        let mut config: Config = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if config.max_timeout_secs == 0
            || config.max_timeout_secs > 86400
            || !(128..=1048576).contains(&config.max_output_bytes)
        {
            return Err("max_timeout_secs must be 1..86400; max_output_bytes 128..1048576".into());
        }
        config.workspace_root =
            fs::canonicalize(&config.workspace_root).map_err(|e| e.to_string())?;
        if !config.workspace_root.is_dir() {
            return Err("workspace_root must be directory".into());
        }
        let compile = |patterns: &Vec<String>| {
            patterns
                .iter()
                .map(|s| regex::Regex::new(s).map_err(|e| e.to_string()))
                .collect::<Result<Vec<_>, _>>()
        };
        let allow = compile(&config.allow)?;
        let deny = compile(&config.deny)?;
        self.config = config;
        self.allow = allow;
        self.deny = deny;
        Ok(())
    }
    fn tools(&self) -> Value {
        json!([tool(
            "run_command",
            "Execute sh -c with combined bounded output. Destructive: host approval required; NOT a sandbox.",
            json!({"command":{"type":"string"},"cwd":{"type":"string"},"timeout_secs":{"type":"integer","minimum":1,"maximum":self.config.max_timeout_secs}}),
            &["command"],
            true
        )])
    }
    fn call(&mut self, name: &str, args: Value) -> Result<String, String> {
        if name == "run_command" {
            self.run(args)
        } else {
            Err("unknown tool".into())
        }
    }
}
fn main() -> io::Result<()> {
    serve(Server::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_and_policy() {
        let mut s = Server::default();
        s.configure(json!({"allow":["^printf"],"deny":["forbidden"]}))
            .unwrap();
        let r: Value = serde_json::from_str(
            &s.run(json!({"command":"printf out; printf err >&2; exit 7"}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(r["output"], "outerr");
        assert_eq!(r["exit_code"], 7);
        assert!(s.run(json!({"command":"true"})).is_err());
        assert!(s.run(json!({"command":"printf forbidden"})).is_err());
        assert!(s.cwd("../").is_err());
        assert!(s.cwd("/").is_err());
    }
    #[test]
    fn capture_and_flood_timeout() {
        let mut c = Capture::new(128);
        c.push("ä".repeat(1000).as_bytes());
        assert!(c.text().contains("truncated"));
        let mut s = Server::default();
        s.configure(json!({"max_output_bytes":128})).unwrap();
        let start = Instant::now();
        let r:Value=serde_json::from_str(&s.run(json!({"command":"while :; do printf abcdefghijklmnopqrstuvwxyz; done","timeout_secs":1})).unwrap()).unwrap();
        assert_eq!(r["timed_out"], true);
        assert_eq!(r["truncated"], true);
        assert!(start.elapsed() < Duration::from_secs(5));
    }
    #[test]
    fn cwd_symlink_and_timeout_validation() {
        let root = std::env::temp_dir().join(format!("lh-shell-cwd-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        std::os::unix::fs::symlink("/", root.join("escape")).unwrap();
        let mut server = Server::default();
        server
            .configure(json!({"workspace_root":root,"max_timeout_secs":1}))
            .unwrap();
        assert_eq!(server.cwd("sub").unwrap(), root.join("sub"));
        assert!(server.cwd("escape").is_err());
        assert!(
            server
                .run(json!({"command":"true","timeout_secs":2}))
                .is_err()
        );
        assert!(
            server
                .run(json!({"command":"true","timeout_secs":0}))
                .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn timeout_kills_descendants() {
        let s = Server::default();
        let r: Value = serde_json::from_str(
            &s.run(json!({"command":"sleep 30 & echo $!; wait","timeout_secs":1}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(r["timed_out"], true);
        let pid = r["output"].as_str().unwrap().trim().parse::<u32>().unwrap();
        // SIGKILL delivery to the descendant is asynchronous, even after the
        // direct shell has been reaped. Wait for observed exit, not a fixed sleep.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let stat = fs::read_to_string(format!("/proc/{pid}/stat"));
            if stat.is_err() || stat.unwrap().split(") ").nth(1).unwrap().starts_with('Z') {
                break;
            }
            assert!(Instant::now() < deadline, "descendant survived group kill");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn exited_parent_open_pipe_is_bounded() {
        let s = Server::default();
        let start = Instant::now();
        let r: Value = serde_json::from_str(
            &s.run(json!({"command":"sleep 30 & exit 0","timeout_secs":1}))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(r["timed_out"], true);
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
