use crate::{Config, Error, Host, Registry, Result};
use lh_protocol::{
    Answer, DecideRequest, DecideResponse, Question, RouteDecision, RouteInput, Specialist,
    json::{self, json},
};
use std::{collections::BTreeMap, time::Instant};

pub struct Engine {
    pub registry: Registry,
    pub config: Config,
    hosts: BTreeMap<String, Host>,
}
impl Engine {
    pub fn new(registry: Registry, config: Config) -> Self {
        Self {
            registry,
            config,
            hosts: BTreeMap::new(),
        }
    }
    pub fn host(&mut self, id: &str) -> Result<Host> {
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
        let start = Instant::now();
        if let Some(id) = input.specialist.as_deref().filter(|s| *s != "auto") {
            let specialist = self
                .registry
                .specialists
                .get(id)
                .ok_or_else(|| Error::new(-32602, format!("unknown specialist '{id}'")))?;
            if !supports(specialist, input) {
                return Err(Error::new(
                    -32003,
                    "explicit specialist does not support the requested modality",
                ));
            }
            return Ok(decision(id, "explicit"));
        }
        let candidates: Vec<_> = self
            .registry
            .specialists
            .values()
            .filter(|s| supports(s, input))
            .cloned()
            .collect();
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
        if candidates.len() == 1 {
            return Ok(decision(&candidates[0].id, "single-candidate"));
        }
        let fallback = candidates
            .iter()
            .find(|s| s.id == self.config.default_specialist)
            .unwrap_or(&candidates[0])
            .id
            .clone();
        let Some(router) = router.filter(|r| r.strategy != "rules") else {
            return Ok(decision(&fallback, "rules"));
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
            state: input.prompt.clone(),
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
        let host = self.host(
            router
                .backend
                .as_deref()
                .ok_or_else(|| Error::new(-32602, "router needs backend"))?,
        )?;
        // Idempotent load on every use also recovers model state after a plugin restart.
        host.call(
            "backend/load",
            json!({"key":request.key,"model":router.model,"pinned":true}),
        )
        .await?;
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
        Ok(RouteDecision {
            specialist: if low { fallback } else { choice.clone() },
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
        })
    }
}
fn decision(id: &str, reason: &str) -> RouteDecision {
    RouteDecision {
        specialist: id.into(),
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
        proposed.reason = "sticky".into();
    }
    proposed
}
#[cfg(test)]
mod tests {
    use super::*;
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
