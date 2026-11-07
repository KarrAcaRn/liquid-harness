use lh_core::{Engine, Session};
use lh_protocol::{
    RouteInput,
    json::{self, Value, json},
};
use serde::Deserialize;
use std::{collections::BTreeMap, path::Path, time::Instant};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    prompt: String,
    expected: String,
    #[serde(default)]
    images: Option<Vec<String>>,
    #[serde(default)]
    audio: Option<Vec<String>>,
    #[serde(default)]
    specialist: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    session: Option<String>,
}

fn latency(samples: &mut [u64]) -> Value {
    samples.sort_unstable();
    let percentile = |percent: usize| {
        if samples.is_empty() {
            None
        } else {
            Some(samples[(samples.len() * percent).div_ceil(100) - 1])
        }
    };
    json!({"count":samples.len(),"p50_ms":percentile(50),"p95_ms":percentile(95)})
}
fn loadavg() -> Value {
    match std::fs::read_to_string("/proc/loadavg") {
        Ok(value) => json!(value.trim()),
        Err(e) => json!({"error":e.to_string()}),
    }
}

fn total_latency(samples: &mut [u64]) -> Value {
    let percentiles = latency(samples);
    json!({"count":samples.len(),
        "p50_ms":percentiles["p50_ms"].as_u64().map(|v| v as f64 / 1000.0),
        "p95_ms":percentiles["p95_ms"].as_u64().map(|v| v as f64 / 1000.0)})
}

pub async fn evaluate(engine: &mut Engine, file: &Path, router: Option<&str>) -> Result<Value> {
    let text = std::fs::read_to_string(file)?;
    let start_load = loadavg();
    let initial_calls = engine.model_calls();
    let mut matrix: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut decisions = Vec::new();
    let mut fastpaths = Vec::new();
    let mut errors = Vec::new();
    let mut results = Vec::new();
    let mut correct = 0;
    let mut total = 0;
    let mut sessions: BTreeMap<String, Session> = BTreeMap::new();
    let mut all_latencies = Vec::new();
    let mut stage_counts: BTreeMap<String, usize> = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        total += 1;
        let case: Case = match json::from_str(line) {
            Ok(case) => case,
            Err(e) => {
                errors.push(json!({"line":index+1,"error":e.to_string()}));
                continue;
            }
        };
        if !engine.registry.specialists.contains_key(&case.expected) {
            errors.push(json!({"line":index+1,"expected":case.expected,"error":"unknown expected specialist"}));
            continue;
        }
        let mut standalone = Session::default();
        let session = match &case.session {
            Some(id) => sessions.entry(id.clone()).or_default(),
            None => &mut standalone,
        };
        let previous = session.previous_input.clone().unwrap_or_default();
        let input = RouteInput {
            prompt: case.prompt,
            images: case.images.unwrap_or(previous.images),
            audio: case.audio.unwrap_or(previous.audio),
            specialist: case.specialist,
        };
        let start = Instant::now();
        let routed = engine.route_session(&input, router, session).await;
        all_latencies.push(start.elapsed().as_micros() as u64);
        match routed {
            Ok(decision) => {
                *stage_counts.entry(decision.reason.clone()).or_default() += 1;
                correct += usize::from(decision.specialist == case.expected);
                *matrix
                    .entry(case.expected.clone())
                    .or_default()
                    .entry(decision.specialist.clone())
                    .or_default() += 1;
                if decision.model_called {
                    decisions.push(decision.router_ms);
                } else {
                    fastpaths.push(decision.router_ms);
                }
                results.push(json!({"line":index+1,"language":case.language,"expected":case.expected,"decision":decision}));
            }
            Err(e) => {
                *matrix
                    .entry(case.expected.clone())
                    .or_default()
                    .entry("<error>".into())
                    .or_default() += 1;
                errors.push(json!({"line":index+1,"expected":case.expected,"error":e.to_string()}));
            }
        }
    }
    if total == 0 {
        return Err("routing evaluation file contains no cases".into());
    }
    Ok(json!({
        "cascade_enabled":engine.config.cascade.enabled,
        "model_calls":engine.model_calls() - initial_calls,"d1_call_rate":(engine.model_calls() - initial_calls) as f64 / total as f64,
        "stage_counts":stage_counts,
        "file":file,"router":router.unwrap_or(&engine.config.router),"total":total,"correct":correct,
        "accuracy":correct as f64 / total as f64,"error_count":errors.len(),"errors":errors,
        "confusion_matrix":matrix,"confusion_matrix_axes":{"rows":"expected","columns":"predicted"},
        "latency":{"total":total_latency(&mut all_latencies),"decision_model":latency(&mut decisions),"fastpaths":latency(&mut fastpaths),
            "note":"Nearest-rank milliseconds; total measured with microsecond precision; includes startup/load and all attempted routes."},
        "loadavg":{"before":start_load,"after":loadavg()},"results":results
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn heldout_is_bilingual_with_conversations() {
        let cases: Vec<Case> = include_str!("../../../evals/routing-heldout.jsonl")
            .lines()
            .map(|line| json::from_str(line).unwrap())
            .collect();
        assert_eq!(cases.len(), 40);
        for language in ["en", "de"] {
            assert_eq!(
                cases
                    .iter()
                    .filter(|c| c.language.as_deref() == Some(language))
                    .count(),
                20
            );
        }
        assert_eq!(cases.iter().filter(|c| c.session.is_some()).count(), 16);
        assert_eq!(total_latency(&mut [1001, 2500])["p50_ms"], 1.001);
    }
    #[test]
    fn nearest_rank_percentiles_and_empty_samples() {
        assert_eq!(
            latency(&mut [9, 1, 3, 7, 5]),
            json!({"count":5,"p50_ms":5,"p95_ms":9})
        );
        assert_eq!(
            latency(&mut []),
            json!({"count":0,"p50_ms":null,"p95_ms":null})
        );
        assert_eq!(latency(&mut [4]), json!({"count":1,"p50_ms":4,"p95_ms":4}));
    }
    #[test]
    fn bundled_cases_are_bilingual_and_at_least_thirty() {
        let cases: Vec<Case> = include_str!("../../../evals/routing-tuning.jsonl")
            .lines()
            .map(|line| json::from_str(line).unwrap())
            .collect();
        assert!(cases.len() >= 30);
        for language in ["en", "de"] {
            assert!(
                cases
                    .iter()
                    .filter(|c| c.language.as_deref() == Some(language))
                    .count()
                    >= 15
            );
        }
    }
}
