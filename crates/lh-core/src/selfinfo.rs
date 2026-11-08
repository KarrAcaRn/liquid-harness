//! Self-description: where the harness runs, what it loaded and found, and how
//! it restarts. One source ([`SelfInfo`]) for `lh doctor`, `GET /lh/v1/self`,
//! the web UI "About this harness" card and the agent (`harness_info` and the
//! compact "Harness environment" system block).
//!
//! Gathering only reads files and metadata; it never starts plugins or loads
//! models. The one subprocess is `llama-server --version`, cached per binary.
//! Secret values never appear: only whitelisted environment variables are
//! listed, and token/key variables are reported as `set`/`unset`.
use crate::{Config, Registry, lifecycle::Lifecycle, router::plugin_enabled, skills};
use lh_protocol::{
    ModelSpec,
    json::{Value, json},
    model_files,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};
pub mod probe;
pub use probe::{Runtime, runtime};

/// Called by the server's restart trigger with a reason; false when a restart
/// is already in progress (or the server is shutting down).
pub type Restarter = Arc<dyn Fn(String) -> bool + Send + Sync>;

/// What `lh serve` knows about itself; absent for CLI one-shots.
#[derive(Clone)]
pub struct Service {
    pub bind: String,
    pub log: Option<String>,
    /// `exec` (in place, same PID) or `command` (`[service] restart_command`).
    pub restart_method: String,
    pub restart: Option<Restarter>,
}

/// Borrowed inputs; `lh doctor` has no lifecycle or service.
pub struct Context<'a> {
    pub registry: &'a Registry,
    pub config: &'a Config,
    pub lifecycle: Option<&'a Lifecycle>,
    pub service: Option<&'a Service>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Problem {
    /// `error` (a feature cannot work) or `warning` (works, with a cost).
    pub severity: &'static str,
    pub code: &'static str,
    pub subject: String,
    pub message: String,
    pub hint: String,
}

/// Sections are JSON objects; see docs/ARCHITECTURE.md "Self-awareness".
#[derive(Debug, Clone, Serialize)]
pub struct SelfInfo {
    pub build: Value,
    pub host: Value,
    pub config: Value,
    pub discovery: Value,
    pub models: Value,
    pub backends: Value,
    pub service: Value,
    pub env: Value,
    pub problems: Vec<Problem>,
}
pub const SECTIONS: &[&str] = &[
    "build",
    "host",
    "config",
    "discovery",
    "models",
    "backends",
    "service",
    "env",
    "problems",
];

impl SelfInfo {
    pub fn to_value(&self) -> Value {
        json!(self)
    }
    pub fn section(&self, name: &str) -> Option<Value> {
        SECTIONS
            .contains(&name)
            .then(|| self.to_value()[name].clone())
    }
    pub fn has_errors(&self) -> bool {
        self.problems.iter().any(|p| p.severity == "error")
    }

    pub fn gather(ctx: &Context) -> Self {
        let mut problems = Vec::new();
        let runtime = runtime();
        let mut path_dirs = BTreeSet::new();
        let build = build(runtime);
        let host = host(ctx, runtime);
        let config = config_section(ctx, &mut problems);
        let discovery = discovery(ctx, &mut problems, &mut path_dirs);
        let backends = backends(ctx, &mut problems, &mut path_dirs);
        let models = models(ctx, &mut problems);
        let service = service(ctx);
        let env = env(ctx, &path_dirs);
        Self {
            build,
            host,
            config,
            discovery,
            models,
            backends,
            service,
            env,
            problems,
        }
    }
}

fn build(runtime: &Runtime) -> Value {
    let commit = env!("LH_BUILD_COMMIT");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source_dir = source
        .join("Cargo.toml")
        .is_file()
        .then(|| source.canonicalize().unwrap_or(source));
    let current = runtime.exe.as_deref().and_then(probe::Stamp::of);
    let changed = match (&runtime.exe_stamp, &current) {
        (Some(start), Some(now)) => Some(start != now),
        _ => None,
    };
    let built: u64 = env!("LH_BUILD_UNIX_TIME").parse().unwrap_or(0);
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "commit": commit,
        "commit_short": commit.get(..12).unwrap_or(commit),
        "dirty": match env!("LH_BUILD_DIRTY") { "true" => json!(true), "false" => json!(false), _ => Value::Null },
        "built_at": (built > 0).then(|| probe::rfc3339(built)),
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "source_dir": source_dir,
        "binary": runtime.exe,
        "binary_size": runtime.exe_stamp.as_ref().map(|s| s.size),
        "binary_modified_at": runtime.exe_stamp.as_ref().map(|s| probe::rfc3339(s.modified_unix)),
        "binary_changed": changed,
        "binary_on_disk_modified_at": current.as_ref().map(|s| probe::rfc3339(s.modified_unix)),
        "pid": runtime.pid,
        "started_at": probe::rfc3339(runtime.started_unix),
        "started_unix": runtime.started_unix,
        "uptime_secs": runtime.started.elapsed().as_secs(),
        "argv": runtime.argv,
        "restart_count": runtime.restart_count,
        "last_restart_reason": runtime.restart_reason,
    })
}

/// Effective plugin config: manifest `[config]`, shallowly overridden by the user's.
pub fn plugin_config(registry: &Registry, config: &Config, id: &str) -> BTreeMap<String, Value> {
    let mut merged = registry
        .plugins
        .get(id)
        .map(|p| p.manifest.config.clone())
        .unwrap_or_default();
    if let Some(user) = config.plugins.get(id) {
        merged.extend(user.config.clone());
    }
    merged
}

fn host(ctx: &Context, runtime: &Runtime) -> Value {
    let memory = probe::memory_total();
    let available = crate::lifecycle::available_memory();
    let cwd = std::env::current_dir().ok().or(runtime.cwd.clone());
    // The tools-fs root decides where the agent reads and writes files.
    let tools = plugin_config(ctx.registry, ctx.config, "tools-fs");
    let (workspace, workspace_source) = match tools.get("workspace_root").and_then(Value::as_str) {
        Some(root) if root != "." => (Some(PathBuf::from(root)), "tools-fs workspace_root"),
        _ => (cwd.clone(), "working directory"),
    };
    json!({
        "hostname": probe::hostname(),
        "os": probe::os_name().unwrap_or_else(|| std::env::consts::OS.into()),
        "kernel": probe::kernel(),
        "arch": std::env::consts::ARCH,
        "cpu_model": probe::cpu_model(),
        "logical_cpus": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "memory_total_bytes": memory.map(|m| m.0),
        "memory_total_source": memory.map(|m| m.1),
        "memory_available_bytes": available.as_ref().map(|a| a.0),
        "memory_available_source": available.map(|a| a.1),
        "container": probe::container(),
        "cwd": cwd,
        "workspace_root": workspace,
        "workspace_source": workspace_source,
    })
}

fn config_section(ctx: &Context, problems: &mut Vec<Problem>) -> Value {
    let config = ctx.config;
    let router_known =
        config.router == "rules" || ctx.registry.routers.contains_key(&config.router);
    if !router_known {
        problems.push(Problem {
            severity: "error",
            code: "router-missing",
            subject: config.router.clone(),
            message: format!("configured router '{}' is not provided by any discovered plugin", config.router),
            hint: "add the plugin pack that declares it to the plugin search path, or set router = \"rules\"".into(),
        });
    }
    if !ctx
        .registry
        .specialists
        .contains_key(&config.default_specialist)
    {
        problems.push(Problem {
            severity: "warning",
            code: "default-specialist-missing",
            subject: config.default_specialist.clone(),
            message: format!(
                "default_specialist '{}' is not a discovered specialist",
                config.default_specialist
            ),
            hint: "set default_specialist to one of the ids listed by `lh plugins list`".into(),
        });
    }
    let token = config.server_token_env.as_ref().map(|name| {
        let set = secret_set(name);
        if !set {
            problems.push(Problem {
                severity: "error",
                code: "token-unset",
                subject: name.clone(),
                message: format!("server_token_env names '{name}', which is unset or empty: lh serve refuses to start"),
                hint: format!("export {name}=<token> before starting lh serve"),
            });
        }
        json!({"variable": name, "value": if set { "set" } else { "unset" }})
    });
    json!({
        "path": config.source.path,
        "chosen_by": config.source.chosen_by,
        "loaded": config.source.loaded,
        "directory": config.directory,
        "router": config.router,
        "router_known": router_known,
        "default_specialist": config.default_specialist,
        "server_token": token,
        "agent_approval": config.agent.approval,
        "context_in_chat": config.self_info.context_in_chat,
        "lifecycle": {"warm_router": config.lifecycle.warm_router, "preload": config.lifecycle.preload,
            "keep_warm": config.lifecycle.keep_warm, "max_resident": config.lifecycle.max_resident},
        "service": {"restart_command": config.service.restart_command,
            "drain_timeout_secs": config.service.drain_timeout_secs, "log": config.service.log},
    })
}

fn exists(path: &Path) -> bool {
    path.exists()
}

/// Canonical when it exists (no `..`), else absolute.
fn display_path(path: &Path) -> PathBuf {
    path.canonicalize()
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}
fn discovery(
    ctx: &Context,
    problems: &mut Vec<Problem>,
    path_dirs: &mut BTreeSet<PathBuf>,
) -> Value {
    let plugin_path: Vec<Value> = Registry::search_path(ctx.config, &ctx.config.directory)
        .into_iter()
        .map(|(path, source)| {
            json!({"path": display_path(&path), "source": source, "exists": exists(&path)})
        })
        .collect();
    let mut plugins = Vec::new();
    let mut skill_dirs = Vec::new();
    for (id, entry) in &ctx.registry.plugins {
        let enabled = plugin_enabled(ctx.registry, ctx.config, id);
        let dir = entry.path.parent().map(Path::to_path_buf);
        if enabled && let Some(dir) = &dir {
            skill_dirs.push(dir.clone());
        }
        let resolved = crate::host::resolve_command(entry);
        let (executable, resolved_by) = match &resolved {
            Some((path, "PATH", Some(found))) => {
                if let Some(parent) = found.parent() {
                    path_dirs.insert(parent.to_path_buf());
                }
                let _ = path;
                (Some(found.clone()), "PATH")
            }
            Some((path, how, _)) if *how != "missing" => (Some(path.clone()), *how),
            Some((_, how, _)) => (None, *how),
            None => (None, "declarative"),
        };
        if enabled && resolved_by == "missing" {
            let command = entry
                .manifest
                .run
                .as_ref()
                .map(|r| r.command.clone())
                .unwrap_or_default();
            problems.push(Problem {
                severity: "error",
                code: "plugin-executable-missing",
                subject: id.clone(),
                message: format!("plugin '{id}' runs '{command}', which is neither in {}, next to lh, nor on PATH",
                    dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default()),
                hint: "build it (`cargo build --workspace --release`) and put the binary next to lh or on PATH, or disable the plugin with [plugins.<id>] enabled = false".into(),
            });
        }
        plugins.push(json!({
            "id": id,
            "version": entry.manifest.plugin.version,
            "enabled": enabled,
            "provides": entry.manifest.plugin.provides,
            "manifest": entry.path,
            "dir": dir,
            "command": entry.manifest.run.as_ref().map(|r| &r.command),
            "executable": executable,
            "resolved_by": resolved_by,
        }));
    }
    let skill_paths = skills::labelled_search_paths(&ctx.config.directory, &skill_dirs);
    let paths: Vec<PathBuf> = skill_paths.iter().map(|(p, _)| p.clone()).collect();
    let mut skills_out = Vec::new();
    let mut names = BTreeSet::new();
    for file in skills::inventory(&paths) {
        if let Some(error) = &file.error {
            problems.push(Problem {
                severity: "error",
                code: "skill-unreadable",
                subject: file.path.display().to_string(),
                message: format!("skill {}: {error}", file.path.display()),
                hint: "fix the SKILL.md frontmatter (docs/skills.md) or remove the file; while it fails, agent runs cannot load any skill".into(),
            });
        }
        if let Some(name) = &file.name {
            names.insert(name.clone());
        }
        skills_out.push(json!({"name": file.name, "path": file.path, "error": file.error}));
    }
    for (name, _) in skills::BUILTIN {
        skills_out.push(json!({"name": name, "path": Value::Null, "builtin": true,
            "overridden": names.contains(*name)}));
    }
    json!({
        "plugin_search_path": plugin_path,
        "plugins": plugins,
        "skill_search_path": skill_paths.iter().map(|(path, source)| json!({
            "path": display_path(path), "source": source, "exists": exists(path)})).collect::<Vec<_>>(),
        "skills": skills_out,
    })
}

/// llama.cpp semantics apply to a backend whose effective config names a
/// `llama_server` (the shipped `llamacpp` plugin); other backends own their files.
fn is_llamacpp(ctx: &Context, backend: &str) -> bool {
    backend == "llamacpp"
        || plugin_config(ctx.registry, ctx.config, backend).contains_key("llama_server")
}
fn used_backends(ctx: &Context) -> BTreeSet<String> {
    let mut used = BTreeSet::new();
    for specialist in ctx.registry.specialists.values() {
        for choice in specialist.choices().unwrap_or_default() {
            used.insert(choice.backend);
        }
    }
    if let Some(router) = ctx.registry.routers.get(&ctx.config.router)
        && let Some(backend) = &router.backend
    {
        used.insert(backend.clone());
    }
    used
}

fn backends(
    ctx: &Context,
    problems: &mut Vec<Problem>,
    path_dirs: &mut BTreeSet<PathBuf>,
) -> Value {
    let used = used_backends(ctx);
    let mut out = Vec::new();
    for (id, entry) in &ctx.registry.plugins {
        if !entry
            .manifest
            .plugin
            .provides
            .iter()
            .any(|p| p == "backend")
        {
            continue;
        }
        let enabled = plugin_enabled(ctx.registry, ctx.config, id);
        let config = plugin_config(ctx.registry, ctx.config, id);
        let mut value = json!({"id": id, "enabled": enabled, "used": used.contains(id)});
        if is_llamacpp(ctx, id) {
            let configured = config
                .get("llama_server")
                .and_then(Value::as_str)
                .unwrap_or("llama-server")
                .to_owned();
            let path = crate::host::which(&model_files::expand(&configured));
            if let Some(dir) = path.as_ref().and_then(|p| p.parent()) {
                path_dirs.insert(dir.to_path_buf());
            }
            let version = path.as_deref().map(probe::version_of);
            let active = enabled && used.contains(id);
            match (&path, &version) {
                (None, _) if active => problems.push(Problem {
                    severity: "error",
                    code: "llama-server-missing",
                    subject: configured.clone(),
                    message: format!("llama-server '{configured}' was not found (llama.cpp models cannot load)"),
                    hint: "install llama.cpp (b11960 or newer) and put llama-server on PATH, or set [plugins.llamacpp.config] llama_server = \"/path/to/llama-server\"".into(),
                }),
                (Some(path), Some(Err(error))) if active => problems.push(Problem {
                    severity: "warning",
                    code: "llama-server-version",
                    subject: path.display().to_string(),
                    message: format!("`{} --version` failed: {error}", path.display()),
                    hint: "check that its shared libraries are found (LD_LIBRARY_PATH) and that it runs on this CPU".into(),
                }),
                _ => {}
            }
            let models_dir = config.get("models_dir").and_then(Value::as_str);
            let cache = model_files::cache_dir(models_dir);
            value["kind"] = json!("llama.cpp");
            value["llama_server"] = json!({"configured": configured, "path": path,
                "version": version.as_ref().map(|v| v.as_ref().ok()), "version_error": version.as_ref().and_then(|v| v.as_ref().err())});
            value["model_cache"] = json!({"path": cache, "source": if models_dir.is_some() { "models_dir" } else if std::env::var_os("LLAMA_CACHE").is_some_and(|v| !v.is_empty()) { "LLAMA_CACHE" } else { "default (~/.cache/llama.cpp)" },
                "exists": cache.as_deref().is_some_and(Path::is_dir)});
        } else if id == "freetoken" {
            let models: Vec<Value> = config
                .get("models")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .map(|(source, endpoint)| {
                            let token = endpoint["token_env"].as_str();
                            json!({"source": source,
                                "mode": if endpoint.get("spawn").is_some() { "spawn" } else { "url" },
                                "url": endpoint.get("url").or_else(|| endpoint.get("base_url")),
                                "token_env": token.map(|t| json!({"variable": t, "value": if secret_set(t) { "set" } else { "unset" }}))})
                        })
                        .collect()
                })
                .unwrap_or_default();
            value["kind"] = json!("freetoken");
            value["models"] = json!(models);
        } else {
            value["kind"] = json!("other");
        }
        out.push(value);
    }
    json!(out)
}

/// Local files of one backend choice; `None` when the backend owns its files.
fn local_files(ctx: &Context, backend: &str, model: &ModelSpec) -> Option<Value> {
    if !is_llamacpp(ctx, backend) {
        return None;
    }
    let config = plugin_config(ctx.registry, ctx.config, backend);
    let wants_projector = model.mmproj.is_some();
    if let Some(path) = model.source.strip_prefix("file:") {
        let path = PathBuf::from(model_files::expand(path));
        let size = std::fs::metadata(&path).ok().map(|m| m.len());
        let projector = model
            .mmproj
            .as_deref()
            .filter(|m| *m != "auto")
            .map(|m| PathBuf::from(model_files::expand(m.strip_prefix("file:").unwrap_or(m))));
        let projector_size = projector
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok().map(|m| m.len()));
        let downloaded = size.is_some() && (projector.is_none() || projector_size.is_some());
        return Some(
            json!({"model_file": path, "model_bytes": size, "projector_file": projector,
            "projector_bytes": projector_size, "downloaded": downloaded, "layout": "file"}),
        );
    }
    let source = model.source.strip_prefix("hf:")?;
    let cache = model_files::cache_dir(config.get("models_dir").and_then(Value::as_str))?;
    Some(match model_files::resolve_hf(source, &cache) {
        Some(((file, size), projector)) => {
            let projector = projector.filter(|_| wants_projector);
            let downloaded = !wants_projector || projector.is_some();
            json!({"model_file": file, "model_bytes": size,
                "projector_file": projector.as_ref().map(|p| &p.0), "projector_bytes": projector.as_ref().map(|p| p.1),
                "downloaded": downloaded, "layout": if file.to_string_lossy().contains("/models--") { "hf-cache" } else { "llama.cpp flat" },
                "cache": cache})
        }
        None => {
            json!({"model_file": null, "model_bytes": null, "downloaded": false, "cache": cache})
        }
    })
}

fn models(ctx: &Context, problems: &mut Vec<Problem>) -> Value {
    let states: BTreeMap<String, Value> = ctx
        .lifecycle
        .map(|l| l.snapshot()["models"].clone())
        .and_then(|m| m.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| Some((m["key"].as_str()?.to_owned(), m)))
        .collect();
    let mut out = Vec::new();
    let mut entry = |id: &str,
                     kind: &str,
                     key: String,
                     choices: Vec<(String, ModelSpec)>,
                     active: bool,
                     problems: &mut Vec<Problem>| {
        let mut first_enabled = true;
        let choices: Vec<Value> = choices
            .into_iter()
            .map(|(backend, model)| {
                let enabled = plugin_enabled(ctx.registry, ctx.config, &backend);
                let files = local_files(ctx, &backend, &model);
                if active
                    && enabled
                    && first_enabled
                    && let Some(files) = &files
                    && files["downloaded"] == false
                {
                    let (severity, code, hint) = if model.source.starts_with("file:") {
                        ("error", "model-file-missing", "fix the file: path in the manifest or copy the model there".to_string())
                    } else {
                        ("warning", "model-not-downloaded", format!("it downloads on first use; to fetch it now run `lh models pull {id}`"))
                    };
                    problems.push(Problem {
                        severity,
                        code,
                        subject: id.to_owned(),
                        message: format!("{kind} '{id}' model {} is not on disk", model.source),
                        hint,
                    });
                }
                if enabled {
                    first_enabled = false;
                }
                json!({"backend": backend, "enabled": enabled, "source": model.source, "mmproj": model.mmproj,
                    "ctx": model.ctx, "local": files})
            })
            .collect();
        let state = states.get(&key);
        out.push(
            json!({"id": id, "kind": kind, "key": key, "active": active, "backends": choices,
            "state": state.map(|s| s["state"].clone()).unwrap_or(Value::Null),
            "loaded_backend": state.map(|s| s["backend"].clone()).unwrap_or(Value::Null),
            "pid": state.map(|s| s["pid"].clone()).unwrap_or(Value::Null)}),
        );
    };
    for (id, router) in &ctx.registry.routers {
        let choices = match (&router.backend, &router.model) {
            (Some(backend), Some(model)) => vec![(backend.clone(), model.clone())],
            _ => vec![],
        };
        entry(
            id,
            "router",
            format!("router:{id}"),
            choices,
            *id == ctx.config.router,
            problems,
        );
    }
    for (id, specialist) in &ctx.registry.specialists {
        let choices = specialist
            .choices()
            .unwrap_or_default()
            .into_iter()
            .map(|c| (c.backend, c.model))
            .collect();
        entry(id, "specialist", id.clone(), choices, true, problems);
    }
    json!(out)
}

fn service(ctx: &Context) -> Value {
    let Some(service) = ctx.service else {
        return json!({"running": false,
            "restart": {"cli": "lh restart --url http://127.0.0.1:8484", "api": "POST /lh/v1/admin/restart"}});
    };
    let url = format!("http://{}", service.bind);
    json!({
        "running": true,
        "bind": service.bind,
        "url": url,
        "log": ctx.config.service.log.as_ref().map(|p| p.display().to_string()).or(service.log.clone()),
        "restart": {
            "method": service.restart_method,
            "command": ctx.config.service.restart_command,
            "drain_timeout_secs": ctx.config.service.drain_timeout_secs,
            "api": "POST /lh/v1/admin/restart",
            "cli": format!("lh restart --url {url}"),
            "agent_tool": "harness_restart (always needs approval)",
            "sighup": "full in-place restart (same as the API)",
        },
    })
}

fn secret_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}
/// Names whose values are credentials: only reported as set/unset.
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "TOKEN",
        "KEY",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "AUTH",
    ]
    .iter()
    .any(|m| upper.contains(m))
}
const PLAIN_ENV: &[&str] = &[
    "LLAMA_CACHE",
    "HF_HOME",
    "HF_HUB_CACHE",
    "XDG_CONFIG_HOME",
    "LD_LIBRARY_PATH",
];

fn env(ctx: &Context, path_dirs: &BTreeSet<PathBuf>) -> Value {
    env_from(ctx, path_dirs, std::env::vars_os())
}
/// [`env`] over an explicit environment (tests).
pub fn env_from(
    ctx: &Context,
    path_dirs: &BTreeSet<PathBuf>,
    vars: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Value {
    let vars: BTreeMap<String, String> = vars
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.to_string_lossy().into_owned())))
        .collect();
    let mut shown = BTreeMap::new();
    let mut secrets = BTreeMap::new();
    for (name, value) in &vars {
        // LH_TOOL_CONFIG is trusted tool JSON that may embed credentials.
        let masked = is_secret_name(name) || name == "LH_TOOL_CONFIG";
        if masked && (name.starts_with("LH_") || name.starts_with("HF_")) {
            secrets.insert(name.clone(), if value.is_empty() { "unset" } else { "set" });
        } else if name.starts_with("LH_") || PLAIN_ENV.contains(&name.as_str()) {
            shown.insert(name.clone(), value.clone());
        }
    }
    // Variables named by configuration (bearer token, API keys): set/unset only.
    let mut named: BTreeSet<String> = ctx.config.server_token_env.iter().cloned().collect();
    named.insert("HF_TOKEN".into());
    for id in ctx.registry.plugins.keys() {
        let config = plugin_config(ctx.registry, ctx.config, id);
        for (key, value) in &config {
            if key.ends_with("_env")
                && let Some(name) = value.as_str()
            {
                named.insert(name.to_owned());
            }
        }
        if let Some(models) = config.get("models").and_then(Value::as_object) {
            for endpoint in models.values() {
                if let Some(name) = endpoint["token_env"].as_str() {
                    named.insert(name.to_owned());
                }
            }
        }
    }
    for name in named {
        let set = vars.get(&name).is_some_and(|v| !v.is_empty());
        secrets.insert(name, if set { "set" } else { "unset" });
    }
    // PATH entries that matter: where lh, llama-server and plugin binaries were found.
    let path: Vec<Value> = vars
        .get("PATH")
        .map(|p| std::env::split_paths(p).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|dir| {
            path_dirs.contains(dir)
                || runtime().exe.as_deref().and_then(Path::parent) == Some(dir.as_path())
        })
        .map(|dir| json!(dir))
        .collect();
    json!({"variables": shown, "secrets": secrets, "path_entries": path})
}

/// The compact system-message block (target ≤ ~300 tokens, hard cap 1400 chars).
pub fn context_block(info: &SelfInfo) -> String {
    let s = |v: &Value| v.as_str().map(str::to_owned).unwrap_or_else(|| "?".into());
    let b = &info.build;
    let h = &info.host;
    let gib = |v: &Value| {
        v.as_u64()
            .map(|n| format!("{:.1} GiB", n as f64 / (1u64 << 30) as f64))
            .unwrap_or_else(|| "?".into())
    };
    let mut lines = vec![format!(
        "Harness environment (liquid-harness {} {}{}, pid {}):",
        s(&b["version"]),
        s(&b["commit_short"]),
        if b["dirty"] == true { "-dirty" } else { "" },
        b["pid"]
    )];
    lines.push(format!(
        "- Host: {}, {}, {} CPUs, {} RAM ({} free){}",
        s(&h["hostname"]),
        s(&h["os"]),
        h["logical_cpus"],
        gib(&h["memory_total_bytes"]),
        gib(&h["memory_available_bytes"]),
        h["container"]
            .as_str()
            .map(|c| format!(", container: {c}"))
            .unwrap_or_default()
    ));
    lines.push(format!("- Workspace: {}", s(&h["workspace_root"])));
    lines.push(format!(
        "- Config: {} ({})",
        info.config["path"].as_str().unwrap_or("none"),
        s(&info.config["chosen_by"])
    ));
    let existing = |list: &Value| {
        list.as_array()
            .map(|a| {
                a.iter()
                    .filter(|e| e["exists"] == true)
                    .filter_map(|e| e["path"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    lines.push(format!(
        "- Plugin dirs: {}",
        existing(&info.discovery["plugin_search_path"])
    ));
    lines.push(format!(
        "- Skill dirs: {}",
        existing(&info.discovery["skill_search_path"])
    ));
    let models: Vec<String> = info
        .models
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|m| m["active"] == true)
                .map(|m| {
                    let source = m["backends"]
                        .as_array()
                        .and_then(|c| c.iter().find(|c| c["enabled"] == true))
                        .and_then(|c| c["source"].as_str())
                        .map(|s| s.rsplit('/').next().unwrap_or(s).to_owned())
                        .unwrap_or_else(|| "-".into());
                    let state = m["state"].as_str().unwrap_or("stopped");
                    format!(
                        "{} [{}] {source} {state}",
                        m["id"].as_str().unwrap_or("?"),
                        if m["kind"] == "router" {
                            "router"
                        } else {
                            "specialist"
                        }
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    lines.push(format!("- Models: {}", models.join("; ")));
    lines.push(match info.service["url"].as_str() {
        Some(url) => format!(
            "- Server {url}; restart: tool harness_restart (needs approval) or `lh restart --url {url}`"
        ),
        None => "- Not running as lh serve; restart a server with `lh restart --url <url>`".into(),
    });
    let errors = info
        .problems
        .iter()
        .filter(|p| p.severity == "error")
        .count();
    if !info.problems.is_empty() {
        lines.push(format!(
            "- Problems: {} ({errors} errors), see harness_info section=problems",
            info.problems.len()
        ));
    }
    lines.push(
        "Details: tool harness_info; read skill harness-self before rebuilding or restarting."
            .into(),
    );
    let mut text = lines.join("\n");
    if text.len() > 1400 {
        let mut end = 1397;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests;
