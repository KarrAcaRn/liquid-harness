//! `lh doctor` text rendering and the `lh restart` client. The client speaks
//! plain HTTP/1.1 over `std::net` (local servers; no TLS, no new dependency).
use lh_core::selfinfo::SelfInfo;
use lh_protocol::json::{self, Value, json};
use std::{
    fmt::Write as _,
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    time::{Duration, Instant},
};

fn text(value: &Value) -> String {
    match value {
        Value::Null => "-".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
fn bytes(value: &Value) -> String {
    match value.as_u64() {
        Some(n) if n >= 1 << 30 => format!("{:.2} GiB", n as f64 / (1u64 << 30) as f64),
        Some(n) if n >= 1 << 20 => format!("{:.1} MiB", n as f64 / (1u64 << 20) as f64),
        Some(n) => format!("{n} B"),
        None => "-".into(),
    }
}

/// The human-readable report: one section per topic, problems last.
pub fn render(info: &SelfInfo) -> String {
    let mut out = String::new();
    let b = &info.build;
    let h = &info.host;
    let c = &info.config;
    let _ = writeln!(
        out,
        "liquid-harness {} ({}{}, {} build, built {})",
        text(&b["version"]),
        text(&b["commit_short"]),
        if b["dirty"] == true { ", dirty" } else { "" },
        text(&b["profile"]),
        text(&b["built_at"])
    );
    let _ = writeln!(out, "  binary   {}", text(&b["binary"]));
    if b["source_dir"].is_string() {
        let _ = writeln!(out, "  source   {}", text(&b["source_dir"]));
    }
    let _ = writeln!(out, "\nHost");
    let _ = writeln!(
        out,
        "  {} - {} (kernel {}, {})",
        text(&h["hostname"]),
        text(&h["os"]),
        text(&h["kernel"]),
        text(&h["arch"])
    );
    let _ = writeln!(
        out,
        "  CPU      {} x {}",
        text(&h["logical_cpus"]),
        text(&h["cpu_model"])
    );
    let _ = writeln!(
        out,
        "  memory   {} total ({}), {} available",
        bytes(&h["memory_total_bytes"]),
        text(&h["memory_total_source"]),
        bytes(&h["memory_available_bytes"])
    );
    if h["container"].is_string() {
        let _ = writeln!(out, "  container {}", text(&h["container"]));
    }
    let _ = writeln!(out, "  cwd      {}", text(&h["cwd"]));
    let _ = writeln!(
        out,
        "  workspace {} ({})",
        text(&h["workspace_root"]),
        text(&h["workspace_source"])
    );
    let _ = writeln!(out, "\nConfig");
    let _ = writeln!(
        out,
        "  file     {} (chosen by {}{})",
        text(&c["path"]),
        text(&c["chosen_by"]),
        if c["loaded"] == true {
            ""
        } else {
            ", not present: built-in defaults"
        }
    );
    let _ = writeln!(
        out,
        "  router   {}   default specialist {}",
        text(&c["router"]),
        text(&c["default_specialist"])
    );
    let d = &info.discovery;
    let _ = writeln!(out, "\nPlugin search path");
    for entry in d["plugin_search_path"].as_array().into_iter().flatten() {
        let _ = writeln!(
            out,
            "  {} {} [{}]",
            if entry["exists"] == true { "+" } else { "-" },
            text(&entry["path"]),
            text(&entry["source"])
        );
    }
    let _ = writeln!(out, "\nPlugins");
    for plugin in d["plugins"].as_array().into_iter().flatten() {
        let _ = writeln!(
            out,
            "  {:<16} {:<8} {}{}",
            text(&plugin["id"]),
            if plugin["enabled"] == true {
                "enabled"
            } else {
                "disabled"
            },
            text(&plugin["dir"]),
            match plugin["resolved_by"].as_str() {
                Some("declarative") => "  (declarative)".to_string(),
                Some("missing") => format!("  run: {} NOT FOUND", text(&plugin["command"])),
                Some(how) => format!("  run: {} [{how}]", text(&plugin["executable"])),
                None => String::new(),
            }
        );
    }
    let _ = writeln!(out, "\nSkill search path");
    for entry in d["skill_search_path"].as_array().into_iter().flatten() {
        if entry["exists"] == true {
            let _ = writeln!(
                out,
                "  + {} [{}]",
                text(&entry["path"]),
                text(&entry["source"])
            );
        }
    }
    let skills: Vec<String> = d["skills"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            let name = s["name"].as_str().unwrap_or("?");
            if s["builtin"] == true {
                format!(
                    "{name} (built-in{})",
                    if s["overridden"] == true {
                        ", overridden"
                    } else {
                        ""
                    }
                )
            } else if s["error"].is_string() {
                format!("{name} (ERROR)")
            } else {
                name.to_owned()
            }
        })
        .collect();
    let _ = writeln!(out, "  skills: {}", skills.join(", "));
    let _ = writeln!(out, "\nModels");
    for model in info.models.as_array().into_iter().flatten() {
        let _ = writeln!(
            out,
            "  {} {} [{}]{}",
            if model["active"] == true { "*" } else { " " },
            text(&model["id"]),
            text(&model["kind"]),
            model["state"]
                .as_str()
                .map(|s| format!(" state {s}"))
                .unwrap_or_default()
        );
        for choice in model["backends"].as_array().into_iter().flatten() {
            let local = &choice["local"];
            let disk = if local.is_null() {
                "files managed by the backend".to_string()
            } else if local["downloaded"] == true {
                format!(
                    "on disk {} {}{}",
                    bytes(&local["model_bytes"]),
                    text(&local["model_file"]),
                    if local["projector_file"].is_string() {
                        format!(" + mmproj {}", bytes(&local["projector_bytes"]))
                    } else {
                        String::new()
                    }
                )
            } else {
                "NOT DOWNLOADED".to_string()
            };
            let _ = writeln!(
                out,
                "      {}{}: {}  {disk}",
                text(&choice["backend"]),
                if choice["enabled"] == true {
                    ""
                } else {
                    " (disabled)"
                },
                text(&choice["source"])
            );
        }
    }
    let _ = writeln!(out, "\nBackends");
    for backend in info.backends.as_array().into_iter().flatten() {
        let mut line = format!(
            "  {:<16} {:<8}",
            text(&backend["id"]),
            if backend["enabled"] == true {
                "enabled"
            } else {
                "disabled"
            }
        );
        match backend["kind"].as_str() {
            Some("llama.cpp") => {
                let server = &backend["llama_server"];
                let _ = write!(
                    line,
                    " llama-server {} ({})  cache {}",
                    server["path"].as_str().unwrap_or("NOT FOUND"),
                    server["version"]
                        .as_str()
                        .or(server["version_error"].as_str())
                        .unwrap_or("-"),
                    text(&backend["model_cache"]["path"])
                );
            }
            Some("freetoken") => {
                for model in backend["models"].as_array().into_iter().flatten() {
                    let _ = write!(
                        line,
                        " {} {} {}",
                        text(&model["source"]),
                        text(&model["mode"]),
                        text(&model["url"])
                    );
                }
            }
            _ => {}
        }
        let _ = writeln!(out, "{line}");
    }
    let env = &info.env;
    let _ = writeln!(out, "\nEnvironment (no secret values)");
    for (name, value) in env["variables"].as_object().into_iter().flatten() {
        let _ = writeln!(out, "  {name}={}", text(value));
    }
    for (name, value) in env["secrets"].as_object().into_iter().flatten() {
        let _ = writeln!(out, "  {name}: {}", text(value));
    }
    let _ = writeln!(out, "\nRestart");
    let _ = writeln!(
        out,
        "  lh restart --url http://<bind>   (or POST /lh/v1/admin/restart, SIGHUP, the web UI Status page)"
    );
    let _ = writeln!(out, "\nProblems");
    if info.problems.is_empty() {
        let _ = writeln!(out, "  none");
    }
    for problem in &info.problems {
        let _ = writeln!(
            out,
            "  [{}] {}: {}\n      hint: {}",
            problem.severity, problem.code, problem.message, problem.hint
        );
    }
    out
}

struct Target {
    host: String,
    address: std::net::SocketAddr,
}
fn target(url: &str) -> Result<Target, String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or("only http:// URLs are supported")?;
    let host = rest.split('/').next().unwrap_or(rest).to_owned();
    let address = host
        .to_socket_addrs()
        .map_err(|e| format!("{host}: {e}"))?
        .next()
        .ok_or_else(|| format!("{host}: no address"))?;
    Ok(Target { host, address })
}
/// One request on a fresh connection; returns (status, body).
fn request(
    target: &Target,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&Value>,
) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect_timeout(&target.address, Duration::from_secs(3))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\n",
        target.host,
        body.len()
    );
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Ok((status, body.to_owned()))
}
fn self_info(target: &Target, token: Option<&str>) -> Option<Value> {
    match request(target, "GET", "/lh/v1/self", token, None) {
        Ok((200, body)) => json::from_str(&body).ok(),
        _ => None,
    }
}

/// Requests the restart, then (unless `wait` is false) polls `/lh/v1/self`
/// until the server answers with a new start time.
pub fn restart(
    url: &str,
    reason: &str,
    token: Option<&str>,
    wait: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let target = target(url)?;
    let before = self_info(&target, token);
    let (status, body) = request(
        &target,
        "POST",
        "/lh/v1/admin/restart",
        token,
        Some(&json!({"reason": reason})),
    )
    .map_err(|e| format!("{url}: {e}"))?;
    if status != 200 {
        return Err(format!("restart refused ({status}): {}", body.trim()).into());
    }
    let reply: Value = json::from_str(&body).unwrap_or(Value::Null);
    println!(
        "restart requested: pid {} method {}",
        text(&reply["pid"]),
        text(&reply["method"])
    );
    if !wait {
        return Ok(());
    }
    let started_before = before.as_ref().map(|b| b["build"]["started_unix"].clone());
    let deadline = Instant::now() + Duration::from_secs(60);
    let begun = Instant::now();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        if let Some(after) = self_info(&target, token)
            && Some(after["build"]["started_unix"].clone()) != started_before
        {
            println!(
                "back after {:.1} s: pid {}, restart #{}, commit {}",
                begun.elapsed().as_secs_f64(),
                text(&after["build"]["pid"]),
                text(&after["build"]["restart_count"]),
                text(&after["build"]["commit_short"])
            );
            return Ok(());
        }
    }
    Err(format!("{url} did not come back within 60 s").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_http_targets() {
        let parsed = target("http://127.0.0.1:8484/").unwrap();
        assert_eq!(parsed.host, "127.0.0.1:8484");
        assert_eq!(parsed.address.port(), 8484);
        assert!(target("https://example.com").is_err());
    }
}
