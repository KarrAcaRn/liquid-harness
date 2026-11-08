//! Engine side of the model lifecycle: the single idempotent ensure-loaded
//! path used by every task (routing, chat, vision, transcribe, speak, agent),
//! memory-budget eviction, backend reconciliation and warm start.
use super::Engine;
use crate::{
    Error, Host, Result,
    lifecycle::{LoadGuard, ModelState, WarmTarget},
};
use lh_protocol::{
    ModelSpec, Router,
    json::{Value, json},
};
use tokio::sync::broadcast::error::RecvError;

impl Engine {
    /// Load (or confirm) `key` on `host`. The backend call is made on every
    /// use: it is idempotent and cheap for a resident model, and it restarts a
    /// crashed model server. Lifecycle state turns `loading` before a real
    /// load, or as soon as the backend reports progress for a model the
    /// tracker believed ready (crash restart).
    pub(crate) async fn ensure_loaded(
        &mut self,
        host: &Host,
        backend: &str,
        key: &str,
        model: &ModelSpec,
        pinned: bool,
    ) -> Result<Value> {
        let lifecycle = self.lifecycle.clone();
        let id = key.strip_prefix("router:").unwrap_or(key).to_string();
        let keep_warm = !pinned
            && self.config.lifecycle.keep_warm
            && (id == self.config.default_specialist
                || self.config.lifecycle.preload.contains(&id));
        let resident = lifecycle.state(key) == ModelState::Ready
            && lifecycle.backend(key).as_deref() == Some(backend);
        // Another backend of the fallback chain already serves this key: an
        // attempt on this backend must not clobber that state when it fails.
        let shadow = !resident
            && lifecycle.state(key) == ModelState::Ready
            && lifecycle.backend(key).is_some();
        if shadow {
            let loaded = host
                .call(
                    "backend/load",
                    json!({"key":key,"model":model,"pinned":pinned,"keep_warm":keep_warm}),
                )
                .await?;
            if loaded["ready"] != true {
                return Err(Error::new(-32001, "backend/load did not report ready"));
            }
            // A higher-preference backend recovered: release the old copy.
            if let Some(old) = lifecycle.backend(key)
                && let Ok(old_host) = self.host(&old)
            {
                let _ = old_host.call("backend/unload", json!({"key":key})).await;
            }
            lifecycle.begin(key, backend, pinned, keep_warm);
            lifecycle.ready(key);
            return Ok(loaded);
        }
        if !resident {
            if lifecycle.estimate(key).is_none() {
                lifecycle.set_estimate(key, estimate(host, key, model).await);
            }
            self.make_room(key).await?;
            lifecycle.begin(key, backend, pinned, keep_warm);
            eprintln!("lh: loading {id} ({backend})…");
        }
        let mut guard = LoadGuard {
            lifecycle: &lifecycle,
            key,
            armed: !resident,
        };
        let mut notifications = host.subscribe();
        let mut listening = true;
        let call = host.call(
            "backend/load",
            json!({"key":key,"model":model,"pinned":pinned,"keep_warm":keep_warm}),
        );
        tokio::pin!(call);
        let result = loop {
            tokio::select! {
                biased;
                notification = notifications.recv(), if listening => match notification {
                    // The engine is exclusively held: progress on this host belongs to this load.
                    Ok(n) if n.method == "$/progress" => {
                        if !guard.armed {
                            guard.armed = true;
                            lifecycle.begin(key, backend, pinned, keep_warm);
                            eprintln!("lh: restarting {id} ({backend})…");
                        }
                        lifecycle.progress(
                            key,
                            n.params["message"].as_str().map(str::to_owned),
                            n.params["fraction"].as_f64(),
                        );
                    }
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => listening = false,
                },
                result = &mut call => break result,
            }
        };
        let loaded = result.and_then(|loaded| {
            if loaded["ready"] == true {
                Ok(loaded)
            } else {
                Err(Error::new(-32001, "backend/load did not report ready"))
            }
        });
        match loaded {
            Ok(loaded) => {
                guard.armed = false;
                if let Some(took) = lifecycle.ready(key) {
                    eprintln!("lh: {id} ready ({:.1} s)", took.as_secs_f64());
                    // Files exist now: refine an estimate that was unknown before download.
                    if lifecycle.estimate(key).is_none() {
                        lifecycle.set_estimate(key, estimate(host, key, model).await);
                    }
                    self.make_room(key).await?;
                }
                Ok(loaded)
            }
            Err(error) => {
                guard.armed = false;
                lifecycle.failed(key, &error.message);
                Err(error)
            }
        }
    }

    /// Load (idempotently) a decision-model router, pinned.
    pub async fn ensure_router(&mut self, router: &Router) -> Result<Host> {
        let backend = router
            .backend
            .as_deref()
            .ok_or_else(|| Error::new(-32602, "router needs backend"))?;
        let host = self.require(backend, "decide", &router.id).await?;
        let model = router
            .model
            .clone()
            .ok_or_else(|| Error::new(-32602, "router has no decision model"))?;
        let key = format!("router:{}", router.id);
        self.ensure_loaded(&host, backend, &key, &model, true)
            .await?;
        Ok(host)
    }

    /// Unload least-recently-used models until `key` fits the memory budget
    /// and the optional `max_resident` cap. The pinned router never counts as
    /// evictable; keep-warm models go last.
    async fn make_room(&mut self, key: &str) -> Result<()> {
        self.reconcile().await;
        while let Some(eviction) = self.lifecycle.next_eviction(key)? {
            let host = self.host(&eviction.backend)?;
            let reason = format!("evicted ({})", eviction.reason);
            if let Err(error) = host
                .call("backend/unload", json!({"key":eviction.key}))
                .await
            {
                eprintln!("lh: unloading {} failed: {error}", eviction.key);
            }
            eprintln!("lh: unloaded {} ({})", eviction.key, eviction.reason);
            self.lifecycle.stopped(&eviction.key, &reason);
        }
        Ok(())
    }

    /// Fold `backend/status` of every started backend into the tracker, so
    /// idle-TTL unloads and crashed model servers stop counting as resident.
    /// Callers must hold the engine (no other call may be active).
    pub async fn reconcile(&mut self) {
        for backend in self.lifecycle.backends() {
            let Some(host) = self.hosts.get(&backend).cloned() else {
                continue;
            };
            let running = host.status().await.is_ok_and(|s| s["state"] == "running");
            if !running {
                self.lifecycle.reconcile(&backend, None);
                continue;
            }
            if let Ok(status) = host.call("backend/status", json!({})).await
                && let Some(loaded) = status["loaded"].as_array()
            {
                self.lifecycle.reconcile(&backend, Some(loaded));
            }
        }
    }

    /// Background loads after startup: the decision-model router (when
    /// `warm_router`), then `preload` specialists in order. Lock per target.
    pub fn warm_targets(&self) -> Vec<WarmTarget> {
        let mut targets = Vec::new();
        let lifecycle = &self.config.lifecycle;
        if lifecycle.warm_router
            && self.config.router != "rules"
            && let Some(router) = self.registry.routers.get(&self.config.router)
            && router.strategy == "decision-model"
            && router.backend.is_some()
            && router.model.is_some()
        {
            targets.push(WarmTarget::Router(router.id.clone()));
        }
        for id in &lifecycle.preload {
            let target = WarmTarget::Specialist(id.clone());
            if !targets.contains(&target) {
                targets.push(target);
            }
        }
        targets
    }

    pub async fn warm(&mut self, target: &WarmTarget) -> Result<()> {
        match target {
            WarmTarget::Router(id) => {
                let router = self
                    .registry
                    .routers
                    .get(id)
                    .cloned()
                    .ok_or_else(|| Error::new(-32602, format!("unknown router '{id}'")))?;
                self.ensure_router(&router).await.map(|_| ())
            }
            WarmTarget::Specialist(id) => self.prepare(id).await.map(|_| ()),
        }
    }

    /// Warm every target in order (used by `lh chat`; `lh serve` locks per target).
    pub async fn warm_all(&mut self) {
        let targets = self.warm_targets();
        for (index, target) in targets.iter().enumerate() {
            self.lifecycle.set_warmup(
                "running",
                targets[index..].iter().map(WarmTarget::label).collect(),
            );
            if let Err(error) = self.warm(target).await {
                eprintln!("lh: warm-up of {} failed: {error}", target.label());
            }
        }
        if !targets.is_empty() {
            self.lifecycle.set_warmup("done", vec![]);
        }
    }
}

/// `backend/estimate` is optional: unknown methods or sizes mean "no estimate".
async fn estimate(host: &Host, key: &str, model: &ModelSpec) -> Option<Value> {
    host.call("backend/estimate", json!({"key":key,"model":model}))
        .await
        .ok()
        .filter(|v| v["ram_bytes"].is_u64())
}
