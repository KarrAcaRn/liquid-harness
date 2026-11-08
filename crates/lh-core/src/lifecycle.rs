//! Model lifecycle bookkeeping: per-model load state, RAM estimates, the
//! memory budget, LRU eviction choice and warm-start targets.
//!
//! The tracker is shared (`Arc<Lifecycle>`) and never requires the engine
//! lock, so status endpoints can report `loading` while a request or warm-up
//! holds the engine for the whole load. Backends stay the source of truth for
//! residency; [`Lifecycle::reconcile`] folds their `backend/status` back in.
use crate::{Error, Registry, Result};
use lh_protocol::json::{Value, json};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::watch;

/// `"auto"` (60 % of available memory), `"N%"`, a byte count with an optional
/// unit (`"8GiB"`, `"512MB"`, `4000000000`) or `"unlimited"`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum MemoryBudget {
    Bytes(u64),
    Text(String),
}
impl Default for MemoryBudget {
    fn default() -> Self {
        Self::Text("auto".into())
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LifecycleConfig {
    /// Load the configured decision-model router right after `lh serve`/`lh chat` start.
    pub warm_router: bool,
    /// Specialists loaded in the background after the router, in order.
    pub preload: Vec<String>,
    pub memory_budget: MemoryBudget,
    /// Optional hard cap on resident models (router included) across all backends.
    pub max_resident: Option<usize>,
    /// Exempt preloaded specialists and the default specialist from the backend idle TTL.
    pub keep_warm: bool,
}
impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            warm_router: true,
            preload: vec![],
            memory_budget: MemoryBudget::default(),
            max_resident: None,
            keep_warm: false,
        }
    }
}
impl LifecycleConfig {
    pub fn validate(&self) -> Result<()> {
        if self.max_resident == Some(0) {
            return Err(Error::new(
                -32602,
                "lifecycle.max_resident must be positive",
            ));
        }
        parse_budget(&self.memory_budget).map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq)]
enum BudgetSpec {
    Unlimited,
    Bytes(u64),
    Fraction(f64),
}
fn parse_budget(value: &MemoryBudget) -> Result<BudgetSpec> {
    let invalid = |text: &str| {
        Error::new(
            -32602,
            format!(
                "lifecycle.memory_budget '{text}' must be auto, unlimited, N% or a size such as 8GiB"
            ),
        )
    };
    let text = match value {
        MemoryBudget::Bytes(0) => return Err(invalid("0")),
        MemoryBudget::Bytes(n) => return Ok(BudgetSpec::Bytes(*n)),
        MemoryBudget::Text(text) => text.trim(),
    };
    match text.to_ascii_lowercase().as_str() {
        "auto" => return Ok(BudgetSpec::Fraction(0.6)),
        "unlimited" | "none" => return Ok(BudgetSpec::Unlimited),
        _ => {}
    }
    if let Some(percent) = text.strip_suffix('%') {
        let p: f64 = percent.trim().parse().map_err(|_| invalid(text))?;
        if !(p > 0.0 && p <= 100.0) {
            return Err(invalid(text));
        }
        return Ok(BudgetSpec::Fraction(p / 100.0));
    }
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().map_err(|_| invalid(text))?;
    let factor: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1e3,
        "m" | "mb" => 1e6,
        "g" | "gb" => 1e9,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        _ => return Err(invalid(text)),
    };
    let bytes = number * factor;
    if !(bytes >= 1.0 && bytes.is_finite()) {
        return Err(invalid(text));
    }
    Ok(BudgetSpec::Bytes(bytes as u64))
}

/// The resolved budget, fixed at engine creation (startup).
#[derive(Debug, Clone, PartialEq)]
pub struct Budget {
    pub bytes: Option<u64>,
    pub source: String,
    pub available_bytes: Option<u64>,
}
impl Budget {
    pub fn resolve(config: &LifecycleConfig) -> Self {
        Self::resolve_with(config, available_memory())
    }
    /// `available`: usable bytes and a description of where the number came from.
    pub fn resolve_with(config: &LifecycleConfig, available: Option<(u64, String)>) -> Self {
        let spec = parse_budget(&config.memory_budget).unwrap_or(BudgetSpec::Fraction(0.6));
        let available_bytes = available.as_ref().map(|(bytes, _)| *bytes);
        match spec {
            BudgetSpec::Unlimited => Self {
                bytes: None,
                source: "unlimited (configured)".into(),
                available_bytes,
            },
            BudgetSpec::Bytes(bytes) => Self {
                bytes: Some(bytes),
                source: "configured".into(),
                available_bytes,
            },
            BudgetSpec::Fraction(fraction) => match available {
                Some((bytes, source)) => Self {
                    bytes: Some((bytes as f64 * fraction) as u64),
                    source: format!("{}% of {source} at startup", fraction * 100.0),
                    available_bytes,
                },
                None => Self {
                    bytes: None,
                    source: "unlimited (available memory unknown)".into(),
                    available_bytes,
                },
            },
        }
    }
}

/// MemAvailable, lowered to the remaining cgroup v2 allowance when a memory
/// limit applies to this process or one of its ancestor cgroups.
pub fn available_memory() -> Option<(u64, String)> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut available = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))
        .and_then(|v| v.trim().strip_suffix("kB"))
        .and_then(|v| v.trim().parse::<u64>().ok())?
        * 1024;
    let mut source = "MemAvailable".to_string();
    if let Some(limit) = cgroup_allowance()
        && limit < available
    {
        available = limit;
        source = "the cgroup memory limit".into();
    }
    Some((available, source))
}
fn cgroup_allowance() -> Option<u64> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let relative = cgroup.lines().find_map(|l| l.strip_prefix("0::"))?.trim();
    let mut path =
        std::path::PathBuf::from("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
    let mut best: Option<u64> = None;
    loop {
        let read = |name: &str| std::fs::read_to_string(path.join(name)).ok();
        if let (Some(max), Some(current)) = (read("memory.max"), read("memory.current"))
            && let (Ok(max), Ok(current)) =
                (max.trim().parse::<u64>(), current.trim().parse::<u64>())
        {
            let left = max.saturating_sub(current);
            best = Some(best.map_or(left, |b| b.min(left)));
        }
        if path == std::path::Path::new("/sys/fs/cgroup") || !path.pop() {
            break;
        }
    }
    best
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelState {
    Stopped,
    Loading,
    Ready,
    Failed,
}
impl ModelState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    id: String,
    kind: &'static str,
    backend: Option<String>,
    state: ModelState,
    error: Option<String>,
    reason: Option<String>,
    message: Option<String>,
    fraction: Option<f64>,
    started: Option<Instant>,
    last_load: Option<Duration>,
    last_used: Option<Instant>,
    estimate: Option<Value>,
    pinned: bool,
    keep_warm: bool,
    preload: bool,
    loads: u64,
    pid: Option<u64>,
    rss_bytes: Option<u64>,
}
impl Entry {
    fn new(key: &str) -> Self {
        let (kind, id) = match key.strip_prefix("router:") {
            Some(id) => ("router", id.to_string()),
            None => ("specialist", key.to_string()),
        };
        Self {
            id,
            kind,
            backend: None,
            state: ModelState::Stopped,
            error: None,
            reason: None,
            message: None,
            fraction: None,
            started: None,
            last_load: None,
            last_used: None,
            estimate: None,
            pinned: kind == "router",
            keep_warm: false,
            preload: false,
            loads: 0,
            pid: None,
            rss_bytes: None,
        }
    }
    fn ram(&self) -> u64 {
        self.estimate
            .as_ref()
            .and_then(|e| e["ram_bytes"].as_u64())
            .or(self.rss_bytes)
            .unwrap_or(0)
    }
    /// Progress is the backend fraction when reported, else time relative to
    /// the previous load of the same model in this process (never above 95 %).
    fn progress(&self) -> (Option<f64>, Option<&'static str>, Option<u64>, Option<u64>) {
        let elapsed = self.started.map(|s| s.elapsed());
        let eta = match (elapsed, self.last_load) {
            (Some(e), Some(last)) => Some(last.saturating_sub(e).as_millis() as u64),
            _ => None,
        };
        let elapsed_ms = elapsed.map(|e| e.as_millis() as u64);
        if let Some(f) = self.fraction.filter(|f| f.is_finite()) {
            return (Some(f.clamp(0.0, 1.0)), Some("backend"), elapsed_ms, eta);
        }
        match (elapsed, self.last_load) {
            (Some(e), Some(last)) if !last.is_zero() => (
                Some((e.as_secs_f64() / last.as_secs_f64()).min(0.95)),
                Some("previous-load"),
                elapsed_ms,
                eta,
            ),
            _ => (None, None, elapsed_ms, eta),
        }
    }
}

#[derive(Debug)]
struct Inner {
    models: BTreeMap<String, Entry>,
    budget: Budget,
    max_resident: Option<usize>,
    warmup: &'static str,
    warmup_pending: Vec<String>,
}

/// An eviction the engine must perform before (or after) loading a model.
#[derive(Debug, Clone, PartialEq)]
pub struct Eviction {
    pub key: String,
    pub backend: String,
    pub reason: String,
}

#[derive(Debug)]
pub struct Lifecycle {
    inner: Mutex<Inner>,
    changes: watch::Sender<u64>,
}
impl Lifecycle {
    pub fn new(registry: &Registry, config: &LifecycleConfig, budget: Budget) -> Self {
        let mut models = BTreeMap::new();
        for id in registry.specialists.keys() {
            let mut entry = Entry::new(id);
            entry.preload = config.preload.contains(id);
            models.insert(id.clone(), entry);
        }
        for id in registry.routers.keys() {
            let key = format!("router:{id}");
            models.insert(key.clone(), Entry::new(&key));
        }
        Self {
            inner: Mutex::new(Inner {
                models,
                budget,
                max_resident: config.max_resident,
                warmup: "idle",
                warmup_pending: vec![],
            }),
            changes: watch::channel(0).0,
        }
    }
    /// Receivers see a new version after every state, progress or budget change.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }
    fn update<T>(&self, key: &str, f: impl FnOnce(&mut Entry) -> T) -> T {
        let result = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let entry = inner
                .models
                .entry(key.to_string())
                .or_insert_with(|| Entry::new(key));
            f(entry)
        };
        self.changes.send_modify(|v| *v += 1);
        result
    }
    fn read<T>(&self, f: impl FnOnce(&Inner) -> T) -> T {
        f(&self.inner.lock().unwrap_or_else(|e| e.into_inner()))
    }
    pub fn state(&self, key: &str) -> ModelState {
        self.read(|i| i.models.get(key).map_or(ModelState::Stopped, |e| e.state))
    }
    pub fn backend(&self, key: &str) -> Option<String> {
        self.read(|i| i.models.get(key).and_then(|e| e.backend.clone()))
    }
    pub fn estimate(&self, key: &str) -> Option<Value> {
        self.read(|i| i.models.get(key).and_then(|e| e.estimate.clone()))
    }
    pub fn set_estimate(&self, key: &str, estimate: Option<Value>) {
        if estimate.is_some() {
            self.update(key, |e| e.estimate = estimate);
        }
    }
    pub fn begin(&self, key: &str, backend: &str, pinned: bool, keep_warm: bool) {
        self.update(key, |e| {
            e.state = ModelState::Loading;
            e.backend = Some(backend.into());
            e.error = None;
            e.reason = None;
            e.message = None;
            e.fraction = None;
            e.started = Some(Instant::now());
            e.pinned = pinned;
            e.keep_warm = keep_warm;
            e.pid = None;
            e.rss_bytes = None;
        });
    }
    pub fn progress(&self, key: &str, message: Option<String>, fraction: Option<f64>) {
        self.update(key, |e| {
            if message.is_some() {
                e.message = message;
            }
            if fraction.is_some() {
                e.fraction = fraction;
            }
        });
    }
    /// Marks a load complete; returns the load duration when it was a real load.
    pub fn ready(&self, key: &str) -> Option<Duration> {
        self.update(key, |e| {
            let took = (e.state == ModelState::Loading)
                .then(|| e.started.map(|s| s.elapsed()))
                .flatten();
            if let Some(took) = took {
                e.last_load = Some(took);
                e.loads += 1;
            }
            e.state = ModelState::Ready;
            e.started = None;
            e.message = None;
            e.fraction = None;
            e.error = None;
            e.last_used = Some(Instant::now());
            took
        })
    }
    pub fn failed(&self, key: &str, error: &str) {
        self.update(key, |e| {
            e.state = ModelState::Failed;
            e.error = Some(error.into());
            e.started = None;
            e.message = None;
            e.fraction = None;
        });
    }
    pub fn stopped(&self, key: &str, reason: &str) {
        self.update(key, |e| {
            e.state = ModelState::Stopped;
            e.reason = Some(reason.into());
            e.started = None;
            e.message = None;
            e.fraction = None;
            e.pid = None;
            e.rss_bytes = None;
        });
    }
    pub fn touch(&self, key: &str) {
        self.update(key, |e| e.last_used = Some(Instant::now()));
    }
    pub fn set_warmup(&self, state: &'static str, pending: Vec<String>) {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.warmup = state;
            inner.warmup_pending = pending;
        }
        self.changes.send_modify(|v| *v += 1);
    }
    pub fn warming(&self) -> bool {
        self.read(|i| i.warmup == "running")
    }
    pub fn any_loading(&self) -> bool {
        self.read(|i| i.models.values().any(|e| e.state == ModelState::Loading))
    }
    /// Backend `backend/status` truth: ready/loading entries of that backend
    /// that are no longer resident become stopped; pid/RSS details are kept.
    /// `loaded` is `None` when the backend process itself is not running.
    pub fn reconcile(&self, backend: &str, loaded: Option<&[Value]>) {
        let present: BTreeMap<String, &Value> = loaded
            .unwrap_or(&[])
            .iter()
            .filter_map(|m| Some((m["key"].as_str()?.to_string(), m)))
            .collect();
        let mut changed = false;
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            for (key, entry) in &mut inner.models {
                if entry.backend.as_deref() != Some(backend) || entry.state != ModelState::Ready {
                    continue;
                }
                match present.get(key).filter(|m| m["suspended"] != true) {
                    Some(model) => {
                        entry.pid = model["pid"].as_u64();
                        entry.rss_bytes = model["rss_bytes"].as_u64();
                    }
                    None => {
                        entry.state = ModelState::Stopped;
                        entry.reason = Some(
                            if loaded.is_some() {
                                "no longer resident in the backend (idle TTL, eviction or exit)"
                            } else {
                                "backend process is not running"
                            }
                            .into(),
                        );
                        entry.pid = None;
                        entry.rss_bytes = None;
                    }
                }
                changed = true;
            }
        }
        if changed {
            self.changes.send_modify(|v| *v += 1);
        }
    }
    /// The next model to unload so that `key` fits, or `None` if it fits (or
    /// nothing evictable is left and only the soft memory budget is exceeded).
    /// Pinned models (the router) are never chosen; keep-warm models go last.
    pub fn next_eviction(&self, key: &str) -> Result<Option<Eviction>> {
        self.read(|inner| {
            let need = inner.models.get(key).map_or(0, Entry::ram);
            let resident: Vec<(&String, &Entry)> = inner
                .models
                .iter()
                .filter(|(k, e)| {
                    k.as_str() != key && matches!(e.state, ModelState::Ready | ModelState::Loading)
                })
                .collect();
            let total: u64 = resident.iter().map(|(_, e)| e.ram()).sum::<u64>() + need;
            let over_budget = inner.budget.bytes.is_some_and(|b| total > b);
            let over_cap = inner
                .max_resident
                .is_some_and(|m| resident.len() + 1 > m);
            if !over_budget && !over_cap {
                return Ok(None);
            }
            let victim = resident
                .iter()
                .filter(|(_, e)| e.state == ModelState::Ready && !e.pinned && e.backend.is_some())
                .min_by_key(|(_, e)| (e.keep_warm, e.last_used))
                .map(|(k, e)| Eviction {
                    key: (*k).clone(),
                    backend: e.backend.clone().unwrap(),
                    reason: if over_cap {
                        format!("max_resident {}", inner.max_resident.unwrap())
                    } else {
                        "memory budget".into()
                    },
                });
            match victim {
                Some(v) => Ok(Some(v)),
                None if over_cap => Err(Error::new(
                    -32002,
                    format!(
                        "lifecycle.max_resident {} reached and every resident model is pinned or busy",
                        inner.max_resident.unwrap()
                    ),
                )),
                None => Ok(None),
            }
        })
    }
    /// Only models that are currently loading, for streamed `model_loading` events.
    pub fn loading(&self) -> Vec<Value> {
        self.read(|inner| {
            inner
                .models
                .iter()
                .filter(|(_, e)| e.state == ModelState::Loading)
                .map(|(key, e)| {
                    let (progress, source, elapsed_ms, eta_ms) = e.progress();
                    json!({"specialist":e.id,"kind":e.kind,"key":key,"backend":e.backend,
                        "progress":progress,"progress_source":source,"message":e.message,
                        "elapsed_ms":elapsed_ms,"eta_ms":eta_ms})
                })
                .collect()
        })
    }
    pub fn snapshot(&self) -> Value {
        self.read(|inner| {
            let resident: u64 = inner
                .models
                .values()
                .filter(|e| matches!(e.state, ModelState::Ready | ModelState::Loading))
                .map(Entry::ram)
                .sum();
            let models: Vec<Value> = inner
                .models
                .iter()
                .map(|(key, e)| {
                    let (progress, source, elapsed_ms, eta_ms) = if e.state == ModelState::Loading {
                        e.progress()
                    } else {
                        (None, None, None, None)
                    };
                    json!({"id":e.id,"kind":e.kind,"key":key,"backend":e.backend,
                        "state":e.state.as_str(),"progress":progress,"progress_source":source,
                        "message":e.message,"elapsed_ms":elapsed_ms,"eta_ms":eta_ms,
                        "error":e.error,"reason":e.reason,
                        "last_load_ms":e.last_load.map(|d| d.as_millis() as u64),
                        "idle_secs":e.last_used.map(|t| t.elapsed().as_secs()),
                        "ram_estimate_bytes":e.estimate.as_ref().and_then(|v| v["ram_bytes"].as_u64()),
                        "ram_estimate":e.estimate,"rss_bytes":e.rss_bytes,"pid":e.pid,
                        "pinned":e.pinned,"keep_warm":e.keep_warm,"preload":e.preload,"loads":e.loads})
                })
                .collect();
            json!({
                "budget":{"memory_budget_bytes":inner.budget.bytes,"source":inner.budget.source,
                    "available_bytes":inner.budget.available_bytes,"resident_estimate_bytes":resident,
                    "max_resident":inner.max_resident},
                "warmup":{"state":inner.warmup,"pending":inner.warmup_pending},
                "models":models,
            })
        })
    }
    /// Every key the tracker knows (for reconciliation of unseen backends).
    pub fn backends(&self) -> BTreeSet<String> {
        self.read(|i| {
            i.models
                .values()
                .filter_map(|e| e.backend.clone())
                .collect()
        })
    }
}

/// Marks a load as failed if its future is dropped before completion
/// (client disconnect, server shutdown, `lh chat` quitting during warm-up).
pub(crate) struct LoadGuard<'a> {
    pub lifecycle: &'a Lifecycle,
    pub key: &'a str,
    pub armed: bool,
}
impl Drop for LoadGuard<'_> {
    fn drop(&mut self) {
        if self.armed && self.lifecycle.state(self.key) == ModelState::Loading {
            self.lifecycle.failed(self.key, "load cancelled");
        }
    }
}

/// Something `lh serve`/`lh chat` loads in the background after startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarmTarget {
    Router(String),
    Specialist(String),
}
impl WarmTarget {
    pub fn label(&self) -> String {
        match self {
            Self::Router(id) => format!("router:{id}"),
            Self::Specialist(id) => id.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tracker(budget: Option<u64>, cap: Option<usize>) -> Lifecycle {
        let config = LifecycleConfig {
            max_resident: cap,
            ..Default::default()
        };
        Lifecycle::new(
            &Registry::default(),
            &config,
            Budget {
                bytes: budget,
                source: "test".into(),
                available_bytes: None,
            },
        )
    }
    fn load(t: &Lifecycle, key: &str, ram: u64, pinned: bool) {
        t.set_estimate(key, Some(json!({"ram_bytes":ram})));
        t.begin(key, "mock", pinned, false);
        t.ready(key);
    }
    #[test]
    fn budget_parsing_and_resolution() {
        let cfg = |v: MemoryBudget| LifecycleConfig {
            memory_budget: v,
            ..Default::default()
        };
        let t = |s: &str| MemoryBudget::Text(s.into());
        assert_eq!(
            parse_budget(&t("8GiB")).unwrap(),
            BudgetSpec::Bytes(8 << 30)
        );
        assert_eq!(
            parse_budget(&t("512 MB")).unwrap(),
            BudgetSpec::Bytes(512_000_000)
        );
        assert_eq!(parse_budget(&t("50%")).unwrap(), BudgetSpec::Fraction(0.5));
        assert_eq!(
            parse_budget(&MemoryBudget::Bytes(7)).unwrap(),
            BudgetSpec::Bytes(7)
        );
        for bad in ["", "0%", "101%", "12parsecs", "-1G"] {
            assert!(parse_budget(&t(bad)).is_err(), "{bad}");
        }
        assert!(parse_budget(&MemoryBudget::Bytes(0)).is_err());
        let auto = Budget::resolve_with(&cfg(t("auto")), Some((1000, "MemAvailable".into())));
        assert_eq!(auto.bytes, Some(600));
        assert!(auto.source.contains("60% of MemAvailable"));
        assert_eq!(Budget::resolve_with(&cfg(t("auto")), None).bytes, None);
        assert_eq!(
            Budget::resolve_with(&cfg(t("unlimited")), Some((1, "x".into()))).bytes,
            None
        );
        let config: LifecycleConfig =
            lh_protocol::toml::from_str("memory_budget = 4096\npreload=['a']").unwrap();
        assert_eq!(config.memory_budget, MemoryBudget::Bytes(4096));
        assert!(config.warm_router);
        assert!(lh_protocol::toml::from_str::<LifecycleConfig>("preload_typo=[]").is_err());
        if let Some((bytes, _)) = available_memory() {
            assert!(bytes > 0);
        }
    }
    #[test]
    fn eviction_is_lru_never_pinned_and_keep_warm_last() {
        let t = tracker(Some(300), None);
        load(&t, "router:d1", 100, true);
        load(&t, "a", 100, false);
        std::thread::sleep(Duration::from_millis(5));
        load(&t, "b", 100, false);
        t.set_estimate("c", Some(json!({"ram_bytes":100})));
        // a is least recently used: it goes first, never the pinned router.
        assert_eq!(t.next_eviction("c").unwrap().unwrap().key, "a");
        std::thread::sleep(Duration::from_millis(5));
        t.touch("a");
        assert_eq!(t.next_eviction("c").unwrap().unwrap().key, "b");
        // A keep-warm model is only evicted when nothing else is left.
        t.begin("b", "mock", false, true);
        t.ready("b");
        assert_eq!(t.next_eviction("c").unwrap().unwrap().key, "a");
        t.stopped("a", "test");
        t.stopped("b", "test");
        assert_eq!(t.next_eviction("c").unwrap(), None);
        // Only the router left and still too large: proceed (soft budget).
        t.set_estimate("c", Some(json!({"ram_bytes":10_000})));
        assert_eq!(t.next_eviction("c").unwrap(), None);
    }
    #[test]
    fn hard_cap_counts_router_and_fails_when_only_pinned_remain() {
        let t = tracker(None, Some(2));
        load(&t, "router:d1", 0, true);
        load(&t, "a", 0, false);
        let e = t.next_eviction("b").unwrap().unwrap();
        assert_eq!((e.key.as_str(), e.reason.as_str()), ("a", "max_resident 2"));
        let t = tracker(None, Some(1));
        load(&t, "router:d1", 0, true);
        assert_eq!(t.next_eviction("b").unwrap_err().code, -32002);
    }
    #[test]
    fn states_progress_eta_and_reconcile() {
        let t = tracker(None, None);
        let mut versions = t.subscribe();
        assert_eq!(t.state("x"), ModelState::Stopped);
        t.begin("x", "mock", false, false);
        assert!(versions.has_changed().unwrap());
        versions.mark_unchanged();
        t.progress("x", Some("downloading".into()), Some(0.25));
        assert!(versions.has_changed().unwrap());
        let loading = t.loading();
        assert_eq!(loading[0]["specialist"], "x");
        assert_eq!(loading[0]["progress"], 0.25);
        assert_eq!(loading[0]["progress_source"], "backend");
        assert!(t.ready("x").is_some());
        assert!(t.loading().is_empty());
        // A second load is timed relative to the first: progress/eta are estimates.
        t.begin("x", "mock", false, false);
        let snapshot = t.snapshot();
        let model = &snapshot["models"][0];
        assert_eq!(model["state"], "loading");
        assert_eq!(model["progress_source"], "previous-load");
        assert!(model["eta_ms"].is_u64());
        t.failed("x", "boom");
        let snapshot = t.snapshot();
        assert_eq!(snapshot["models"][0]["state"], "failed");
        assert_eq!(snapshot["models"][0]["error"], "boom");
        t.begin("x", "mock", false, false);
        t.ready("x");
        t.reconcile("mock", Some(&[json!({"key":"x","pid":7,"rss_bytes":42})]));
        assert_eq!(t.snapshot()["models"][0]["rss_bytes"], 42);
        t.reconcile("mock", Some(&[]));
        assert_eq!(t.state("x"), ModelState::Stopped);
        assert_eq!(t.snapshot()["models"][0]["loads"], 2);
        let guard = LoadGuard {
            lifecycle: &t,
            key: "x",
            armed: true,
        };
        t.begin("x", "mock", false, false);
        drop(guard);
        assert_eq!(t.state("x"), ModelState::Failed);
    }
}
