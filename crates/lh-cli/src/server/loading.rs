//! Model-loading visibility for streamed responses. The lifecycle tracker is
//! readable without the engine lock, so a request waiting for the engine (or
//! for its own model load) reports what is loading while it waits.
use bytes::Bytes;
use lh_core::lifecycle::Lifecycle;
use lh_protocol::json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::time::{Interval, MissedTickBehavior};

pub(super) const TICK: Duration = Duration::from_millis(500);

#[derive(Clone, Copy)]
pub(super) enum Style<'a> {
    /// OpenAI-compatible stream: SSE comments always (ignored by clients,
    /// keep proxies from timing out), `event: model_loading` frames only when
    /// the client opted in with `lh_events: true`.
    Chat { events: bool },
    /// Agent run stream: harness-native named events.
    Agent { run_id: &'a str },
}

pub(super) struct Feed {
    lifecycle: Arc<Lifecycle>,
    interval: Interval,
    last: String,
    quiet_ticks: u32,
}
impl Feed {
    pub(super) fn new(lifecycle: Arc<Lifecycle>) -> Self {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Self {
            lifecycle,
            interval,
            last: String::new(),
            quiet_ticks: 0,
        }
    }
    pub(super) async fn tick(&mut self) {
        self.interval.tick().await;
    }
    /// Frames for the current tick: progress changes are sent at once, an
    /// unchanged state is repeated every few seconds as a keep-alive.
    pub(super) fn frames(&mut self, style: Style<'_>) -> Vec<Bytes> {
        let loading = self.lifecycle.loading();
        if loading.is_empty() {
            self.last.clear();
            return vec![];
        }
        let fingerprint: String = loading
            .iter()
            .map(|m| format!("{}:{}:{} ", m["key"], m["progress"], m["message"]))
            .collect();
        self.quiet_ticks += 1;
        if fingerprint == self.last && self.quiet_ticks < 4 {
            return vec![];
        }
        self.quiet_ticks = 0;
        self.last = fingerprint;
        loading
            .into_iter()
            .flat_map(|model| frames(&model, style))
            .collect()
    }
}

fn frames(model: &Value, style: Style<'_>) -> Vec<Bytes> {
    let percent = model["progress"]
        .as_f64()
        .map(|p| format!(" {:.0}%", p * 100.0))
        .unwrap_or_default();
    match style {
        Style::Chat { events } => {
            let mut out = vec![Bytes::from(format!(
                ": lh model_loading {}{percent}\n\n",
                model["specialist"].as_str().unwrap_or("model")
            ))];
            if events {
                out.push(Bytes::from(format!(
                    "event: model_loading\ndata: {model}\n\n"
                )));
            }
            out
        }
        Style::Agent { run_id } => {
            let event = json!({"event":"model_loading","data":model,"run_id":run_id});
            vec![Bytes::from(format!(
                "event: model_loading\ndata: {event}\n\n"
            ))]
        }
    }
}
