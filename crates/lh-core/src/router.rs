use crate::{
    Config, Error, Host, Registry, Result, Session,
    cascade::{RouteCache, hint_score, requirements},
};
use lh_protocol::{
    Answer, DecideRequest, DecideResponse, Question, RouteDecision, RouteInput, Specialist,
    json::{self, json},
};
use std::{collections::BTreeMap, time::Instant};

/// Explicit task mapping; arbitrary task names cannot become RPC methods.
pub fn task_method(task: &str) -> Result<&'static str> {
    match task {
        "chat" => Ok("backend/chat"),
        "decide" => Ok("backend/decide"),
        "transcribe" => Ok("backend/transcribe"),
        "speak" => Ok("backend/speak"),
        _ => Err(Error::new(
            -32602,
            format!("unsupported specialist task '{task}'"),
        )),
    }
}
pub struct Engine {
    pub registry: Registry,
    pub config: Config,
    hosts: BTreeMap<String, Host>,
    cache: RouteCache,
    model_calls: u64,
}
impl Engine {
    /// Load without routing an artificial request (for example, models pull).
    pub async fn prepare(&mut self, id: &str) -> Result<RouteDecision> {
        let mut selected = decision(id, "explicit-load");
        self.load_specialist(&mut selected).await?;
        Ok(selected)
    }
    /// Dispatch to an already loaded specialist; override the key with its ID.
    pub async fn dispatch(
        &mut self,
        selected: &RouteDecision,
        mut params: json::Value,
    ) -> Result<json::Value> {
        let specialist = self
            .registry
            .specialists
            .get(&selected.specialist)
            .ok_or_else(|| Error::new(-32602, "unknown specialist"))?;
        let method = task_method(&specialist.task)?;
        let object = params
            .as_object_mut()
            .ok_or_else(|| Error::new(-32602, "task params must be an object"))?;
        object.insert("key".into(), json!(specialist.id));
        let valid = match specialist.task.as_str() {
            "chat" => json::from_value::<lh_protocol::ChatRequest>(params.clone())
                .map(|r| !r.messages.is_empty()),
            "decide" => {
                json::from_value::<DecideRequest>(params.clone()).map(|r| !r.questions.is_empty())
            }
            "transcribe" => json::from_value::<lh_protocol::TranscribeRequest>(params.clone())
                .map(|r| !r.audio.trim().is_empty()),
            "speak" => json::from_value::<lh_protocol::SpeakRequest>(params.clone())
                .map(|r| !r.text.trim().is_empty()),
            _ => unreachable!(),
        }
        .map_err(|e| Error::new(-32602, e.to_string()))?;
        if !valid {
            return Err(Error::new(-32602, "empty task input"));
        }
        let backend = selected
            .backend
            .as_deref()
            .ok_or_else(|| Error::new(-32602, "specialist is not loaded"))?;
        if !specialist
            .choices()
            .map_err(|e| Error::new(-32602, e))?
            .iter()
            .any(|c| c.backend == backend)
        {
            return Err(Error::new(-32602, "backend is not a specialist choice"));
        }
        self.host(backend)?.call(method, params).await
    }
    pub fn new(registry: Registry, config: Config) -> Self {
        Self {
            registry,
            config,
            hosts: BTreeMap::new(),
            cache: RouteCache::default(),
            model_calls: 0,
        }
    }
    /// Number of backend/decide attempts, including failed requests.
    pub fn model_calls(&self) -> u64 {
        self.model_calls
    }
    pub fn host(&mut self, id: &str) -> Result<Host> {
        if !self.enabled(id) {
            return Err(Error::new(-32001, format!("backend '{id}' is disabled")));
        }
        if let Some(host) = self.hosts.get(id) {
            return Ok(host.clone());
        }
        let entry = self.registry.backend(id)?.clone();
        let config = self
            .config
            .plugins
            .get(id)
            .map(|c| c.config.clone())
            .unwrap_or_default();
        let host = Host::new(entry, config);
        self.hosts.insert(id.into(), host.clone());
        Ok(host)
    }
    pub fn enabled(&self, id: &str) -> bool {
        self.config
            .plugins
            .get(id)
            .and_then(|c| c.enabled)
            .unwrap_or_else(|| {
                self.registry
                    .plugins
                    .get(id)
                    .is_some_and(|p| p.manifest.plugin.enabled)
            })
    }
    pub async fn require(&mut self, backend: &str, capability: &str, owner: &str) -> Result<Host> {
        let host = self.host(backend)?;
        if !host
            .capabilities()
            .await?
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == capability))
        {
            return Err(Error::new(
                -32003,
                format!("backend '{backend}' does not support {capability} required by '{owner}'"),
            ));
        }
        Ok(host)
    }
    /// Resolve preference order at load time, not route-only time. Never retry inference.
    pub async fn load_specialist(&mut self, decision: &mut RouteDecision) -> Result<Host> {
        decision.backend = None;
        let specialist = self
            .registry
            .specialists
            .get(&decision.specialist)
            .cloned()
            .ok_or_else(|| Error::new(-32602, "unknown specialist"))?;
        let mut errors = Vec::new();
        task_method(&specialist.task)?;
        for choice in specialist.choices().map_err(|e| Error::new(-32602, e))? {
            let result = async {
                let host = self
                    .require(&choice.backend, &specialist.task, &specialist.id)
                    .await?;
                let loaded = host
                    .call(
                        "backend/load",
                        json!({"key":specialist.id,"model":choice.model,"pinned":false}),
                    )
                    .await?;
                if loaded["ready"] != true {
                    return Err(Error::new(-32001, "backend/load did not report ready"));
                }
                Ok(host)
            }
            .await;
            match result {
                Ok(host) => {
                    eprintln!(
                        "[route] specialist={} backend={}",
                        specialist.id, choice.backend
                    );
                    decision.backend = Some(choice.backend);
                    return Ok(host);
                }
                Err(error) => {
                    eprintln!(
                        "[route] skipping backend={} for {}: {error}",
                        choice.backend, specialist.id
                    );
                    errors.push(format!("{}: {error}", choice.backend));
                }
            }
        }
        Err(Error::new(
            -32001,
            format!(
                "no backend loaded for '{}': {}",
                specialist.id,
                errors.join("; ")
            ),
        ))
    }
    pub async fn shutdown(&self) {
        for host in self.hosts.values() {
            host.shutdown().await;
        }
    }
    pub async fn route(
        &mut self,
        input: &RouteInput,
        router_override: Option<&str>,
    ) -> Result<RouteDecision> {
        self.route_session(input, router_override, &mut Session::default())
            .await
    }
    /// Route one turn and advance caller-owned state only on success.
    pub async fn route_session(
        &mut self,
        input: &RouteInput,
        router_override: Option<&str>,
        session: &mut Session,
    ) -> Result<RouteDecision> {
        let start = Instant::now();
        let mut result = self.route_inner(input, router_override, session).await?;
        result.router_ms = start.elapsed().as_millis() as u64;
        session.remember(input, &result);
        Ok(result)
    }
    async fn route_inner(
        &mut self,
        input: &RouteInput,
        router_override: Option<&str>,
        session: &Session,
    ) -> Result<RouteDecision> {
        let start = Instant::now();
        let config = self.config.cascade.clone();
        if !config.hint_threshold.is_finite() || config.hint_threshold <= 0.0 {
            return Err(Error::new(
                -32602,
                "cascade hint_threshold must be finite and positive",
            ));
        }
        self.cache.resize(if config.enabled && config.cache {
            config.cache_capacity
        } else {
            0
        });
        let mut stages = vec!["explicit".into()];
        if let Some(id) = input.specialist.as_deref().filter(|s| *s != "auto") {
            let specialist = self
                .registry
                .specialists
                .get(id)
                .ok_or_else(|| Error::new(-32602, format!("unknown specialist '{id}'")))?;
            if !supports(specialist, input)
                || (config.enabled && config.requires && !requirements(specialist, input))
            {
                return Err(Error::new(
                    -32003,
                    "explicit specialist does not support the requested modality",
                ));
            }
            return Ok(decision(id, "explicit"));
        }
        stages.push("modality".into());
        let mut candidates: Vec<_> = self
            .registry
            .specialists
            .values()
            .filter(|s| supports(s, input))
            .cloned()
            .collect();
        let before_requires = candidates.len();
        if config.enabled && config.requires {
            stages.push("requires".into());
            candidates.retain(|s| requirements(s, input));
        }
        if candidates.is_empty() {
            return Err(Error::new(
                -32003,
                "no specialist supports the requested modalities",
            ));
        }
        let selected_router = router_override.unwrap_or(&self.config.router);
        // Validate an explicit router even when a fast path can skip inference.
        let router = if selected_router == "rules" {
            None
        } else {
            Some(
                self.registry
                    .routers
                    .get(selected_router)
                    .cloned()
                    .ok_or_else(|| {
                        Error::new(-32602, format!("unknown router '{selected_router}'"))
                    })?,
            )
        };
        if config.enabled && config.single_candidate {
            stages.push("single-candidate".into());
            if candidates.len() == 1 {
                let reason = if before_requires > candidates.len() {
                    "requires"
                } else {
                    "single-candidate"
                };
                return Ok(traced(&candidates[0].id, reason, stages));
            }
        }
        let mut hinted = Vec::new();
        if config.enabled && config.hints {
            stages.push("hints".into());
            for s in &candidates {
                let score = hint_score(s, &input.prompt);
                if score > 0.0 {
                    hinted.push((s, score));
                }
            }
            // Any positive evidence for multiple specialists is ambiguous, even
            // when one has a higher score. Let the model resolve the conflict.
            if hinted.len() == 1 && hinted[0].1 >= config.hint_threshold {
                return Ok(traced(&hinted[0].0.id, "hint", stages));
            }
        }
        if config.enabled && config.sticky {
            stages.push("sticky".into());
            if let Some(previous) = &session.previous_specialist
                && session.same_modality(input)
                && candidates.iter().any(|s| &s.id == previous)
                && hinted.iter().all(|(s, _)| &s.id == previous)
            {
                return Ok(traced(previous, "sticky", stages));
            }
        }
        // A text default is weak evidence: never displace a valid sticky route,
        // nor conceal conflicting or below-threshold phrase evidence.
        if config.enabled
            && config.hints
            && hinted.is_empty()
            && input.images.is_empty()
            && input.audio.is_empty()
        {
            let defaults: Vec<_> = candidates.iter().filter(|s| s.default_for_text).collect();
            if defaults.len() == 1 {
                stages.push("default-text".into());
                return Ok(traced(&defaults[0].id, "hint", stages));
            }
        }
        // JSON retains full key content for collision-safe equality. Whitespace
        // normalization preserves case, punctuation, attachment references and
        // all candidate/router metadata. Session context prevents cross-talk.
        let mut normalized = input.clone();
        normalized.prompt = input
            .prompt
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let fingerprints = crate::cascade::attachment_fingerprints(input);
        let previous_fingerprints = session
            .previous_input
            .as_ref()
            .map(crate::cascade::attachment_fingerprints);
        let cacheable = config.enabled
            && config.cache
            && fingerprints.is_some()
            && !matches!(previous_fingerprints, Some(None));
        let cache_key = json::to_string(&json!({
            "fingerprints":fingerprints,"previous_fingerprints":previous_fingerprints,
            "input": normalized, "candidates": candidates, "router_id": selected_router,
            "router": router, "cascade": config, "default": self.config.default_specialist,
            "plugins": format!("{:?}", self.config.plugins), "session": session,
        }))?;
        if cacheable {
            stages.push("cache".into());
            if let Some(mut cached) = self.cache.get(&cache_key) {
                cached.reason = "cache".into();
                cached.model_called = false;
                cached.backend = None;
                cached.stages = stages;
                return Ok(cached);
            }
        }
        let fallback = candidates
            .iter()
            .find(|s| s.id == self.config.default_specialist)
            .unwrap_or(&candidates[0])
            .id
            .clone();
        let Some(router) = router.filter(|r| r.strategy != "rules") else {
            stages.push("rules".into());
            let result = traced(&fallback, "rules", stages);
            if cacheable {
                self.cache
                    .insert(cache_key, result.clone(), config.cache_capacity);
            }
            return Ok(result);
        };
        if router.strategy != "decision-model" {
            return Err(Error::new(
                -32601,
                "external router strategy is not implemented in M1",
            ));
        }
        let criteria = candidates
            .iter()
            .map(|s| (s.id.clone(), s.route.clone()))
            .collect();
        let request = DecideRequest {
            key: format!("router:{}", router.id),
            state: match &session.previous_input {
                Some(previous) => format!(
                    "Previous user turn: {}\nPrevious specialist: {}\nCurrent user turn: {}",
                    previous.prompt,
                    session.previous_specialist.as_deref().unwrap_or("unknown"),
                    input.prompt
                ),
                None => input.prompt.clone(),
            },
            images: if router.inputs.iter().any(|s| s == "image") {
                input.images.clone()
            } else {
                vec![]
            },
            files: if router.inputs.iter().any(|s| s == "audio") {
                input.audio.clone()
            } else {
                vec![]
            },
            questions: BTreeMap::from([(
                "specialist".into(),
                Question::Choice {
                    instructions: "Which specialist should handle this request?".into(),
                    criteria,
                },
            )]),
        };
        let host = self
            .require(
                router
                    .backend
                    .as_deref()
                    .ok_or_else(|| Error::new(-32602, "router needs backend"))?,
                "decide",
                &router.id,
            )
            .await?;
        // Idempotent load on every use also recovers model state after a plugin restart.
        host.call(
            "backend/load",
            json!({"key":request.key,"model":router.model,"pinned":true}),
        )
        .await?;
        self.model_calls += 1;
        let response: DecideResponse = json::from_value(
            host.call("backend/decide", json::to_value(request)?)
                .await?,
        )?;
        let Some(Answer::Choice {
            choice,
            probabilities,
            ..
        }) = response.answers.get("specialist")
        else {
            return Err(Error::new(-32603, "router did not return a choice answer"));
        };
        if !candidates.iter().any(|s| &s.id == choice)
            || probabilities.len() != candidates.len()
            || candidates
                .iter()
                .any(|s| !probabilities.contains_key(&s.id))
            || probabilities
                .values()
                .any(|p| !p.is_finite() || !(0.0..=1.0).contains(p))
        {
            return Err(Error::new(
                -32603,
                "router returned invalid candidate probabilities",
            ));
        }
        let confidence = probabilities[choice];
        if (probabilities.values().sum::<f64>() - 1.0).abs() > 0.01
            || probabilities.values().any(|p| *p > confidence + 1e-9)
        {
            return Err(Error::new(
                -32603,
                "router probabilities are not a normalized winning choice",
            ));
        }
        let low = confidence < router.min_confidence;
        let extras: BTreeMap<_, _> = response
            .answers
            .iter()
            .filter(|(name, _)| name.as_str() != "specialist")
            .collect();
        stages.push("model".into());
        let result = RouteDecision {
            stages,
            model_called: true,
            specialist: if low { fallback } else { choice.clone() },
            backend: None,
            confidence,
            scores: probabilities.clone(),
            reason: if low {
                "low-confidence"
            } else {
                "decision-model"
            }
            .into(),
            extras: json::to_value(extras)?,
            router_ms: start.elapsed().as_millis() as u64,
        };
        if cacheable {
            self.cache
                .insert(cache_key, result.clone(), config.cache_capacity);
        }
        Ok(result)
    }
}
fn traced(id: &str, reason: &str, stages: Vec<String>) -> RouteDecision {
    let mut result = decision(id, reason);
    result.stages = stages;
    result
}
fn decision(id: &str, reason: &str) -> RouteDecision {
    RouteDecision {
        stages: vec![reason.into()],
        model_called: false,
        specialist: id.into(),
        backend: None,
        confidence: 1.0,
        scores: BTreeMap::from([(id.into(), 1.0)]),
        reason: reason.into(),
        extras: json!({}),
        router_ms: 0,
    }
}
fn supports(s: &Specialist, input: &RouteInput) -> bool {
    (input.prompt.is_empty() || s.inputs.iter().any(|m| m == "text"))
        && (input.images.is_empty() || s.inputs.iter().any(|m| m == "image"))
        && (input.audio.is_empty() || s.inputs.iter().any(|m| m == "audio"))
}
/// Apply only after routing; callers may skip routing entirely if their policy
/// already knows the previous specialist should stay. A modality change resets it.
pub fn sticky(
    previous: &str,
    modality_changed: bool,
    mut proposed: RouteDecision,
) -> RouteDecision {
    if !modality_changed && proposed.specialist != previous && proposed.confidence < 0.8 {
        proposed.specialist = previous.into();
        proposed.backend = None;
        proposed.reason = "sticky".into();
    }
    proposed
}
#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Engine {
        let mut registry = Registry::default();
        for value in [
            json!({"id":"text", "route":"general", "task":"chat", "inputs":["text"], "default_for_text":true}),
            json!({"id":"voice", "route":"speech", "task":"chat", "inputs":["text"], "hints":[{"pattern":"read aloud","weight":1.0},{"pattern":"laut vorlesen","weight":1.0}]}),
            json!({"id":"vision", "route":"image", "task":"chat", "inputs":["text","image"], "requires":["image"]}),
        ] {
            let s: Specialist = json::from_value(value).unwrap();
            registry.specialists.insert(s.id.clone(), s);
        }
        Engine::new(
            registry,
            Config {
                default_specialist: "text".into(),
                ..Config::default()
            },
        )
    }
    fn input(prompt: &str) -> RouteInput {
        RouteInput {
            prompt: prompt.into(),
            ..RouteInput::default()
        }
    }
    #[tokio::test]
    async fn cascade_hints_defaults_sticky_and_modality() {
        let mut e = engine();
        let mut session = Session::default();
        let d = e
            .route_session(&input("Bitte laut vorlesen"), None, &mut session)
            .await
            .unwrap();
        assert_eq!(
            (d.specialist.as_str(), d.reason.as_str(), d.model_called),
            ("voice", "hint", false)
        );
        let d = e
            .route_session(&input("continue"), None, &mut session)
            .await
            .unwrap();
        assert_eq!(
            (d.specialist.as_str(), d.reason.as_str()),
            ("voice", "sticky")
        );
        let mut image = input("look");
        image.images.push("photo.png".into());
        let d = e.route_session(&image, None, &mut session).await.unwrap();
        assert_eq!(d.specialist, "vision");
        assert_eq!(d.reason, "single-candidate");
        assert!(!d.stages.iter().any(|s| s == "model"));
        let d = e
            .route_session(&input("new topic"), None, &mut session)
            .await
            .unwrap();
        assert_eq!(d.specialist, "text");
        assert_eq!(d.reason, "hint");
        let mut invalid = image;
        invalid.specialist = Some("text".into());
        let before = session.clone();
        assert!(e.route_session(&invalid, None, &mut session).await.is_err());
        assert_eq!(session, before);
    }
    #[tokio::test]
    async fn conflicting_hints_bypass_sticky_and_default() {
        let mut e = engine();
        e.registry
            .specialists
            .get_mut("text")
            .unwrap()
            .hints
            .push(lh_protocol::Hint {
                pattern: "read aloud".into(),
                weight: 2.0,
            });
        let mut session = Session {
            previous_specialist: Some("voice".into()),
            previous_input: Some(input("old")),
        };
        let d = e
            .route_session(&input("read aloud"), None, &mut session)
            .await
            .unwrap();
        assert_eq!(d.reason, "rules");
        assert!(d.stages.iter().any(|s| s == "cache"));
    }
    #[tokio::test]
    async fn cache_normalization_context_and_invalidation() {
        let mut e = engine();
        e.config.cascade.hints = false;
        let d = e.route(&input("Hello world"), None).await.unwrap();
        assert_eq!(d.reason, "rules");
        assert_eq!(
            e.route(&input(" Hello   world "), None)
                .await
                .unwrap()
                .reason,
            "cache"
        );
        assert_eq!(
            e.route(&input("hello world"), None).await.unwrap().reason,
            "rules"
        );
        e.registry.specialists.get_mut("text").unwrap().route = "changed".into();
        assert_eq!(
            e.route(&input("Hello world"), None).await.unwrap().reason,
            "rules"
        );
        e.config.cascade.sticky = false;
        let mut session = Session {
            previous_specialist: Some("voice".into()),
            previous_input: Some(input("old")),
        };
        assert_eq!(
            e.route_session(&input("Hello world"), None, &mut session)
                .await
                .unwrap()
                .reason,
            "rules"
        );
        e.config.cascade.cache_capacity = 0;
        assert_eq!(
            e.route(&input("Hello world"), None).await.unwrap().reason,
            "rules"
        );
        assert_eq!(
            e.route(&input("Hello world"), None).await.unwrap().reason,
            "rules"
        );
    }
    #[tokio::test]
    async fn disabled_cascade_keeps_safety_and_reaches_model_with_one_candidate() {
        let mut e = engine();
        e.config.cascade.enabled = false;
        e.registry.routers.insert(
            "d1".into(),
            json::from_value(json!({"id":"d1","strategy":"decision-model"})).unwrap(),
        );
        let mut image = input("look");
        image.images.push("photo".into());
        // Missing backend proves no single/default/sticky shortcut hid the model path.
        assert_eq!(
            e.route(&image, Some("d1")).await.unwrap_err().message,
            "router needs backend"
        );
        image.specialist = Some("text".into());
        assert_eq!(e.route(&image, Some("d1")).await.unwrap_err().code, -32003);
        image.specialist = Some("vision".into());
        assert_eq!(
            e.route(&image, Some("d1")).await.unwrap().reason,
            "explicit"
        );
        let d = e.route(&input("plain"), None).await.unwrap();
        assert_eq!(d.stages, vec!["explicit", "modality", "rules"]);
    }
    #[tokio::test]
    async fn individual_flags_and_required_inputs() {
        let mut e = engine();
        e.config.cascade.hints = false;
        e.config.cascade.single_candidate = false;
        e.config.cascade.cache = false;
        e.config.cascade.sticky = false;
        let d = e.route(&input("read aloud"), None).await.unwrap();
        assert_eq!(d.stages, vec!["explicit", "modality", "requires", "rules"]);
        let mut image = input("look");
        image.images.push("photo".into());
        assert_eq!(e.route(&image, None).await.unwrap().reason, "rules");
        e.registry.specialists.retain(|id, _| id == "vision");
        assert_eq!(
            e.route(&input("plain"), None).await.unwrap_err().code,
            -32003
        );
        e.config.cascade.requires = false;
        assert_eq!(
            e.route(&input("plain"), None).await.unwrap().specialist,
            "vision"
        );
    }
    #[test]
    fn sticky_threshold_and_modality_change() {
        let mut d = decision("new", "decision-model");
        d.confidence = 0.79;
        assert_eq!(sticky("old", false, d.clone()).specialist, "old");
        assert_eq!(sticky("old", true, d.clone()).specialist, "new");
        d.confidence = 0.8;
        assert_eq!(sticky("old", false, d).specialist, "new");
    }
}
