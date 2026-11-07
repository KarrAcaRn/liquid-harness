use lh_core::Engine;
use lh_protocol::{
    RouteInput,
    json::{self, Value, json},
};
use serde::Deserialize;
use std::{collections::BTreeMap, path::Path};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    prompt: String,
    expected: String,
    #[serde(default)]
    images: Vec<String>,
    #[serde(default)]
    audio: Vec<String>,
    #[serde(default)]
    specialist: Option<String>,
    #[serde(default)]
    language: Option<String>,
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

pub async fn evaluate(engine: &mut Engine, file: &Path, router: Option<&str>) -> Result<Value> {
    let text = std::fs::read_to_string(file)?;
    let start_load = loadavg();
    let mut matrix: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut decisions = Vec::new();
    let mut fastpaths = Vec::new();
    let mut errors = Vec::new();
    let mut results = Vec::new();
    let mut correct = 0;
    let mut total = 0;
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
        let input = RouteInput {
            prompt: case.prompt,
            images: case.images,
            audio: case.audio,
            specialist: case.specialist,
        };
        match engine.route(&input, router).await {
            Ok(decision) => {
                correct += usize::from(decision.specialist == case.expected);
                *matrix
                    .entry(case.expected.clone())
                    .or_default()
                    .entry(decision.specialist.clone())
                    .or_default() += 1;
                if matches!(
                    decision.reason.as_str(),
                    "decision-model" | "low-confidence"
                ) {
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
        "file":file,"router":router.unwrap_or(&engine.config.router),"total":total,"correct":correct,
        "accuracy":correct as f64 / total as f64,"error_count":errors.len(),"errors":errors,
        "confusion_matrix":matrix,"confusion_matrix_axes":{"rows":"expected","columns":"predicted"},
        "latency":{"decision_model":latency(&mut decisions),"fastpaths":latency(&mut fastpaths),
            "note":"Nearest-rank percentiles of router_ms; decision-model includes load/startup, low-confidence fallback included. Fastpaths are reported separately and core reports them as zero."},
        "loadavg":{"before":start_load,"after":loadavg()},"results":results
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let cases: Vec<Case> = include_str!("../../../evals/routing.jsonl")
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
