//! Per-response generation statistics (`lh_stats`) for clients such as the web UI.
//!
//! Token counts come only from the backend (`usage`, or llama.cpp `timings`);
//! nothing is estimated from characters, words or stream chunks. Every field is
//! `null` when the backend or the harness did not measure it.
//!
//! `tokens_per_second` is the decode rate, chosen in this order:
//! 1. `backend_timings`: llama.cpp `timings.predicted_n * 1000 / timings.predicted_ms`.
//!    Window: the backend's own generation phase, excluding prompt processing,
//!    model load, routing and transport.
//! 2. `harness_stream`: `(completion_tokens - 1) * 1000 / (last_delta_ms - first_delta_ms)`,
//!    measured where the harness receives `$/stream` deltas. The first token
//!    closes prompt processing, so the window covers the remaining tokens. Needs
//!    at least two backend-counted tokens and a window of `MIN_STREAM_WINDOW_MS`,
//!    so a burst of buffered chunks is reported as unavailable instead of an
//!    absurd rate.
//!
//! Otherwise the rate is `null`. No division by zero or non-finite value is emitted.
use crate::json::{Value, json};

/// Shortest harness-measured delta window accepted for a rate.
pub const MIN_STREAM_WINDOW_MS: f64 = 50.0;

/// Harness wall-clock measurements for one model call, in milliseconds since
/// the call was dispatched to the backend.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CallTiming {
    /// Dispatch until the backend returned the complete result.
    pub elapsed_ms: Option<f64>,
    /// Dispatch until the first streamed delta (time to first output).
    pub first_delta_ms: Option<f64>,
    /// Dispatch until the last streamed delta.
    pub last_delta_ms: Option<f64>,
}

fn finite(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite() && *v >= 0.0)
}

fn count(value: &Value) -> Option<u64> {
    value.as_u64()
}

/// Build the `lh_stats` object for a backend chat result
/// (`{message, finish_reason, usage, timings?}`).
pub fn generation_stats(response: &Value, timing: CallTiming) -> Value {
    let usage = &response["usage"];
    let timings = &response["timings"];
    let completion_tokens =
        count(&usage["completion_tokens"]).or_else(|| count(&timings["predicted_n"]));
    let prompt_tokens = count(&usage["prompt_tokens"]).or_else(|| count(&timings["prompt_n"]));
    let elapsed_ms = finite(timing.elapsed_ms);
    let first_delta_ms = finite(timing.first_delta_ms);
    let backend = count(&timings["predicted_n"])
        .zip(finite(timings["predicted_ms"].as_f64()))
        .filter(|(n, ms)| *n > 0 && *ms > 0.0)
        .map(|(n, ms)| (n as f64 * 1000.0 / ms, ms, "backend_timings"));
    let stream = || {
        let n = completion_tokens.filter(|n| *n >= 2)?;
        let window = finite(timing.last_delta_ms)? - first_delta_ms?;
        (window >= MIN_STREAM_WINDOW_MS)
            .then(|| ((n - 1) as f64 * 1000.0 / window, window, "harness_stream"))
    };
    let rate = backend.or_else(stream).filter(|(r, _, _)| r.is_finite());
    json!({
        "completion_tokens": completion_tokens,
        "prompt_tokens": prompt_tokens,
        "tokens_per_second": rate.map(|r| r.0),
        "rate_source": rate.map(|r| r.2),
        "rate_window_ms": rate.map(|r| r.1),
        "elapsed_ms": elapsed_ms,
        "first_delta_ms": first_delta_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn timing(elapsed: f64, first: Option<f64>, last: Option<f64>) -> CallTiming {
        CallTiming {
            elapsed_ms: Some(elapsed),
            first_delta_ms: first,
            last_delta_ms: last,
        }
    }
    #[test]
    fn backend_timings_take_precedence() {
        let response = json!({"usage":{"prompt_tokens":12,"completion_tokens":40,"total_tokens":52},
            "timings":{"prompt_n":12,"prompt_ms":30.0,"predicted_n":40,"predicted_ms":2000.0}});
        let stats = generation_stats(&response, timing(2100.0, Some(80.0), Some(2090.0)));
        assert_eq!(stats["completion_tokens"], 40);
        assert_eq!(stats["prompt_tokens"], 12);
        assert_eq!(stats["tokens_per_second"], 20.0);
        assert_eq!(stats["rate_source"], "backend_timings");
        assert_eq!(stats["rate_window_ms"], 2000.0);
        assert_eq!(stats["elapsed_ms"], 2100.0);
        assert_eq!(stats["first_delta_ms"], 80.0);
    }
    #[test]
    fn timings_supply_counts_without_usage() {
        let response =
            json!({"usage":{},"timings":{"prompt_n":3,"predicted_n":5,"predicted_ms":250.0}});
        let stats = generation_stats(&response, CallTiming::default());
        assert_eq!(stats["completion_tokens"], 5);
        assert_eq!(stats["prompt_tokens"], 3);
        assert_eq!(stats["tokens_per_second"], 20.0);
        assert!(stats["elapsed_ms"].is_null());
    }
    #[test]
    fn harness_stream_window_excludes_first_token() {
        let response = json!({"usage":{"completion_tokens":11}});
        let stats = generation_stats(&response, timing(600.0, Some(100.0), Some(600.0)));
        assert_eq!(stats["tokens_per_second"], 20.0);
        assert_eq!(stats["rate_source"], "harness_stream");
        assert_eq!(stats["rate_window_ms"], 500.0);
    }
    #[test]
    fn missing_or_degenerate_measurements_are_null() {
        let cases = [
            // No usage and no timings: nothing is estimated from the text.
            (
                json!({"message":{"content":"many words here"},"usage":{}}),
                timing(10.0, Some(1.0), Some(9.0)),
            ),
            // Zero-duration and zero-token backend timings.
            (
                json!({"usage":{"completion_tokens":0},"timings":{"predicted_n":0,"predicted_ms":0.0}}),
                timing(5.0, None, None),
            ),
            (
                json!({"usage":{"completion_tokens":4},"timings":{"predicted_n":4,"predicted_ms":0.0}}),
                timing(5.0, Some(1.0), Some(1.0)),
            ),
            // One token cannot span a window; a buffered burst is too short.
            (
                json!({"usage":{"completion_tokens":1}}),
                timing(500.0, Some(100.0), Some(400.0)),
            ),
            (
                json!({"usage":{"completion_tokens":30}}),
                timing(500.0, Some(400.0), Some(410.0)),
            ),
            // Non-streaming: no delta window.
            (
                json!({"usage":{"completion_tokens":30}}),
                timing(500.0, None, None),
            ),
            // Clock values that went backwards or are invalid.
            (
                json!({"usage":{"completion_tokens":30},"timings":{"predicted_n":30,"predicted_ms":-5.0}}),
                timing(500.0, Some(400.0), Some(100.0)),
            ),
        ];
        for (response, timing) in cases {
            let stats = generation_stats(&response, timing);
            assert!(stats["tokens_per_second"].is_null(), "{response} {stats}");
            assert!(stats["rate_source"].is_null());
            assert!(stats["rate_window_ms"].is_null());
        }
        let stats = generation_stats(&json!({"usage":{}}), CallTiming::default());
        for field in [
            "completion_tokens",
            "prompt_tokens",
            "elapsed_ms",
            "first_delta_ms",
        ] {
            assert!(stats[field].is_null(), "{field}");
        }
        let invalid = CallTiming {
            elapsed_ms: Some(f64::NAN),
            first_delta_ms: Some(-1.0),
            last_delta_ms: Some(f64::INFINITY),
        };
        let stats = generation_stats(&json!({"usage":{"completion_tokens":0}}), invalid);
        assert_eq!(stats["completion_tokens"], 0);
        assert!(stats["elapsed_ms"].is_null() && stats["first_delta_ms"].is_null());
    }
}
