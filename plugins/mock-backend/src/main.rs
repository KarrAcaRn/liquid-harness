use base64::{Engine as _, engine::general_purpose::STANDARD};
use lh_plugin_sdk::{Context, Handler};
use lh_protocol::{
    Answer, DecideRequest, DecideResponse, Question, RpcError,
    json::{self, Value, json},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// Optional stand-in for a llama-server (`model_processes = true`): `sleep`
/// in its own process group that dies with the plugin, killed on unload.
struct ModelProcess(Child);
impl Drop for ModelProcess {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl ModelProcess {
    fn spawn() -> Result<Self, RpcError> {
        let mut command = Command::new("sleep");
        command
            .arg("3600")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                #[cfg(target_os = "linux")]
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
            .spawn()
            .map(Self)
            .map_err(|e| RpcError::new(-32001, format!("mock model process: {e}")))
    }
}

struct Resident {
    model: Value,
    process: Option<ModelProcess>,
    pinned: bool,
    keep_warm: bool,
    used: Instant,
}

#[derive(Default)]
struct Mock {
    loaded: BTreeMap<String, Resident>,
    /// Fresh (non-idempotent) loads per key.
    loads: BTreeMap<String, u64>,
    config: Value,
    decide_calls: u64,
}
impl Mock {
    /// Crashed model processes and idle (TTL) models are no longer resident.
    fn reap(&mut self) {
        let ttl = self.config["idle_ttl_ms"]
            .as_u64()
            .map(Duration::from_millis);
        self.loaded.retain(|_, r| {
            let alive = r
                .process
                .as_mut()
                .is_none_or(|p| matches!(p.0.try_wait(), Ok(None)));
            let idle = ttl.is_some_and(|ttl| r.used.elapsed() >= ttl);
            alive && (r.pinned || r.keep_warm || !idle)
        });
    }
    fn estimate(&self, key: &str) -> Option<u64> {
        let ram = &self.config["ram_bytes"];
        ram.as_u64()
            .or_else(|| ram[key].as_u64())
            .or_else(|| ram["default"].as_u64())
    }
}
fn error(message: impl Into<String>) -> RpcError {
    RpcError::new(-32602, message)
}
fn words(s: &str) -> BTreeSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}
impl Handler for Mock {
    fn handle(
        &mut self,
        method: &str,
        params: Value,
        ctx: &mut Context<'_>,
    ) -> Result<Value, RpcError> {
        if method == "initialize" {
            if params["protocol"] != 1 {
                return Err(error("unsupported protocol"));
            }
            self.config = params["config"].clone();
            ctx.log("info", "mock backend initialized")
                .map_err(|e| error(e.to_string()))?;
            return Ok(
                json!({"name":"mock","version":env!("CARGO_PKG_VERSION"),"provides":["backend"],"capabilities":self.config.get("capabilities").cloned().unwrap_or(json!(["chat","decide","stream","transcribe","speak"]))}),
            );
        }
        self.reap();
        match method {
            "backend/load" => {
                if self.config["fail_load"] == true {
                    return Err(RpcError::new(-32001, "configured load failure"));
                }
                let key = params["key"].as_str().ok_or_else(|| error("missing key"))?;
                if !params["model"]["source"].is_string() {
                    return Err(error("missing model.source"));
                }
                if self.config["fail_load_keys"]
                    .as_array()
                    .is_some_and(|keys| keys.iter().any(|k| k == key))
                {
                    return Err(RpcError::new(
                        -32001,
                        format!("configured load failure for {key}"),
                    ));
                }
                let pinned = params["pinned"] == true;
                let keep_warm = params["keep_warm"] == true;
                if let Some(r) = self.loaded.get_mut(key) {
                    r.model = params["model"].clone();
                    r.pinned |= pinned;
                    r.keep_warm |= keep_warm;
                    r.used = Instant::now();
                    return Ok(json!({"key":key,"ready":true,"info":{"mock":true}}));
                }
                // Test hook: a slow, observable load with backend progress.
                if let Some(ms) = self.config["load_delay_ms"].as_u64() {
                    for step in 0..4u32 {
                        ctx.progress("mock loading", Some(f64::from(step) / 4.0))
                            .map_err(|e| error(e.to_string()))?;
                        std::thread::sleep(Duration::from_millis(ms / 4));
                    }
                }
                let process = if self.config["model_processes"] == true {
                    Some(ModelProcess::spawn()?)
                } else {
                    None
                };
                self.loaded.insert(
                    key.into(),
                    Resident {
                        model: params["model"].clone(),
                        process,
                        pinned,
                        keep_warm,
                        used: Instant::now(),
                    },
                );
                *self.loads.entry(key.into()).or_default() += 1;
                Ok(json!({"key":key,"ready":true,"info":{"mock":true}}))
            }
            "backend/estimate" => {
                let key = params["key"].as_str().ok_or_else(|| error("missing key"))?;
                match self.estimate(key) {
                    Some(ram) => Ok(json!({"ram_bytes":ram,"basis":"configured"})),
                    None => Err(RpcError::new(-32601, "Method not found")),
                }
            }
            "backend/unload" => {
                let key = params["key"].as_str().ok_or_else(|| error("missing key"))?;
                self.loaded.remove(key);
                Ok(json!({}))
            }
            "backend/status" => {
                let loaded: Vec<Value> = self
                    .loaded
                    .iter()
                    .map(|(key, r)| {
                        let pid = r.process.as_ref().map_or(std::process::id(), |p| p.0.id());
                        json!({"key":key,"pid":pid,"port":null,"idle_secs":r.used.elapsed().as_secs(),
                            "pinned":r.pinned,"keep_warm":r.keep_warm,"rss_bytes":self.estimate(key),"loads":self.loads.get(key)})
                    })
                    .collect();
                let models: BTreeMap<&String, &Value> =
                    self.loaded.iter().map(|(k, r)| (k, &r.model)).collect();
                Ok(
                    json!({"loaded":loaded,"models":models,"loads":self.loads,"pid":std::process::id(),"decide_calls":self.decide_calls,"config":self.config}),
                )
            }
            "backend/chat" | "backend/decide" | "backend/transcribe" | "backend/speak" => {
                let key = params["key"].as_str().ok_or_else(|| error("missing key"))?;
                match self.loaded.get_mut(key) {
                    Some(r) => r.used = Instant::now(),
                    None => return Err(RpcError::new(-32001, "model not loaded")),
                }
                if method == "backend/transcribe" {
                    let request: lh_protocol::TranscribeRequest =
                        json::from_value(params).map_err(|e| error(e.to_string()))?;
                    if request.audio.trim().is_empty() {
                        return Err(error("empty audio"));
                    }
                    return Ok(
                        json!({"text":"Mock transcript (deterministic fixture).","language":request.language}),
                    );
                }
                if method == "backend/speak" {
                    let request: lh_protocol::SpeakRequest =
                        json::from_value(params).map_err(|e| error(e.to_string()))?;
                    if request.text.trim().is_empty() {
                        return Err(error("empty text"));
                    }
                    // One millisecond of PCM silence, not synthesized speech.
                    let mut wav = Vec::new();
                    wav.extend_from_slice(b"RIFF");
                    wav.extend_from_slice(&68_u32.to_le_bytes());
                    wav.extend_from_slice(b"WAVEfmt ");
                    wav.extend_from_slice(&16_u32.to_le_bytes());
                    wav.extend_from_slice(&1_u16.to_le_bytes());
                    wav.extend_from_slice(&1_u16.to_le_bytes());
                    wav.extend_from_slice(&16000_u32.to_le_bytes());
                    wav.extend_from_slice(&32000_u32.to_le_bytes());
                    wav.extend_from_slice(&2_u16.to_le_bytes());
                    wav.extend_from_slice(&16_u16.to_le_bytes());
                    wav.extend_from_slice(b"data");
                    wav.extend_from_slice(&32_u32.to_le_bytes());
                    wav.extend_from_slice(&[0; 32]);
                    return Ok(
                        json!({"audio":format!("data:audio/wav;base64,{}",STANDARD.encode(wav)),"sample_rate":16000,"duration_ms":1}),
                    );
                }
                if method == "backend/chat" {
                    let messages = params["messages"]
                        .as_array()
                        .ok_or_else(|| error("missing messages"))?;
                    if let Some(script) = self.config["scripted_chat"].as_array() {
                        let step = messages.iter().filter(|m| m["role"] == "assistant").count();
                        let message = script
                            .get(step)
                            .cloned()
                            .unwrap_or(json!({"role":"assistant","content":"Script complete"}));
                        return Ok(
                            json!({"finish_reason":if message["tool_calls"].is_array() {"tool_calls"} else {"stop"},"message":message,"usage":{}}),
                        );
                    }
                    let content = messages
                        .iter()
                        .rev()
                        .find(|m| m["role"] == "user")
                        .map(|m| &m["content"]);
                    let text = match content {
                        Some(Value::String(s)) => s.clone(),
                        Some(Value::Array(parts)) => parts
                            .iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<Vec<_>>()
                            .join(" "),
                        _ => String::new(),
                    };
                    if params["stream"] == true {
                        let chars: Vec<char> = text.chars().collect();
                        // Test hook: spacing deltas exercises harness-measured rates.
                        let delay = params["params"]["mock_delta_delay_ms"].as_u64();
                        for i in 0..3 {
                            if let Some(ms) = delay.filter(|_| i > 0) {
                                std::thread::sleep(std::time::Duration::from_millis(ms));
                            }
                            let chunk: String = chars
                                [chars.len() * i / 3..chars.len() * (i + 1) / 3]
                                .iter()
                                .collect();
                            ctx.stream(json!({"content":chunk}))
                                .map_err(|e| error(e.to_string()))?;
                        }
                    }
                    // The mock counts no tokens. Tests may inject backend-reported
                    // usage/timings; otherwise usage is empty (not reported).
                    let mut result = json!({"message":{"role":"assistant","content":text},"finish_reason":"stop","usage":params["params"].get("mock_usage").cloned().unwrap_or(json!({}))});
                    if let Some(timings) = params["params"].get("mock_timings") {
                        result["timings"] = timings.clone();
                    }
                    return Ok(result);
                }
                self.decide_calls += 1;
                let request: DecideRequest =
                    json::from_value(params).map_err(|e| error(e.to_string()))?;
                let state = words(&request.state);
                let mut answers = BTreeMap::new();
                for (name, q) in request.questions {
                    let answer = match q {
                        Question::Choice { criteria, .. } => {
                            if criteria.is_empty() {
                                return Err(error("choice requires criteria"));
                            }
                            let weights: BTreeMap<_, _> = criteria
                                .iter()
                                .map(|(id, text)| {
                                    (
                                        id.clone(),
                                        1.0 + 4.0 * words(text).intersection(&state).count() as f64,
                                    )
                                })
                                .collect();
                            let sum: f64 = weights.values().sum();
                            let probabilities: BTreeMap<_, _> =
                                weights.into_iter().map(|(id, w)| (id, w / sum)).collect();
                            let (choice, confidence) = probabilities
                                .iter()
                                .max_by(|(a, x), (b, y)| x.total_cmp(y).then_with(|| b.cmp(a)))
                                .unwrap();
                            Answer::Choice {
                                choice: choice.clone(),
                                confidence: *confidence,
                                probabilities,
                            }
                        }
                        Question::Noul { instructions } => Answer::Noul {
                            noul: if words(&instructions).intersection(&state).next().is_some() {
                                0.9
                            } else {
                                0.1
                            },
                        },
                        Question::Score { criteria, .. } => {
                            if criteria.len() < 2 {
                                return Err(error("score requires at least two criteria"));
                            }
                            let probability = 1.0 / criteria.len() as f64;
                            Answer::Score {
                                score: (criteria.len() - 1) as f64 / 2.0,
                                confidence: probability,
                                probabilities: (0..criteria.len())
                                    .map(|i| (i.to_string(), probability))
                                    .collect(),
                                legend: criteria
                                    .into_iter()
                                    .enumerate()
                                    .map(|(i, label)| (i.to_string(), label))
                                    .collect(),
                            }
                        }
                    };
                    answers.insert(name, answer);
                }
                json::to_value(DecideResponse { answers }).map_err(|e| error(e.to_string()))
            }
            // Fault injection is disabled unless explicitly enabled at initialization.
            "mock/crash" if self.config["enable_test_methods"] == true => std::process::exit(17),
            // Simulates a crashed model server (llama-server dying under the plugin).
            "mock/kill_model" if self.config["enable_test_methods"] == true => {
                let key = params["key"].as_str().ok_or_else(|| error("missing key"))?;
                let pid = self
                    .loaded
                    .get(key)
                    .and_then(|r| r.process.as_ref())
                    .map(|p| p.0.id())
                    .ok_or_else(|| error("no model process"))?;
                unsafe {
                    libc::kill(pid as i32, libc::SIGKILL);
                }
                Ok(json!({"killed":pid}))
            }
            "mock/flood" if self.config["enable_test_methods"] == true => {
                for _ in 0..512 {
                    ctx.stream(json!({"content":"x".repeat(4096)}))
                        .map_err(|e| error(e.to_string()))?;
                }
                Ok(json!({"done":true}))
            }
            "mock/hang" if self.config["enable_test_methods"] == true => {
                std::thread::sleep(std::time::Duration::from_secs(60));
                Ok(json!({}))
            }
            _ => Err(RpcError::new(-32601, "Method not found")),
        }
    }
}
fn main() -> std::io::Result<()> {
    lh_plugin_sdk::serve(Mock::default())
}
