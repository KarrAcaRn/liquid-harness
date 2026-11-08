//! Shared wire types. Manifest metadata is strict; `config` is explicitly a
//! plugin-owned extension map whose arbitrary keys are passed through unchanged.
use json::Value;
use serde::{Deserialize, Serialize};
pub use serde_json as json;
use std::collections::BTreeMap;
pub use toml;
pub mod chat;
mod sse;
pub mod stats;
pub use chat::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscribeRequest {
    pub key: String,
    /// Local path or audio data URL.
    pub audio: String,
    #[serde(default)]
    pub language: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscribeResponse {
    pub text: String,
    #[serde(default)]
    pub language: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakRequest {
    pub key: String,
    pub text: String,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakResponse {
    /// WAV data URL or local path; never raw samples.
    pub audio: String,
    pub sample_rate: u32,
    pub duration_ms: u64,
}

pub const PROTOCOL_VERSION: u32 = 1;
fn protocol_version() -> u32 {
    PROTOCOL_VERSION
}
fn object() -> Value {
    json::json!({})
}
fn enabled() -> bool {
    true
}
fn min_confidence() -> f64 {
    0.55
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub plugin: Plugin,
    #[serde(default)]
    pub run: Option<Run>,
    /// Arbitrary plugin-owned settings, intentionally exempt from unknown-key rejection.
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
    #[serde(default)]
    pub specialist: Vec<Specialist>,
    #[serde(default)]
    pub router: Option<Router>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    pub id: String,
    pub version: String,
    #[serde(default = "protocol_version")]
    pub protocol: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// Discovery hint; initialize is authoritative.
    #[serde(default)]
    pub capabilities: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Specialist {
    #[serde(default)]
    pub agent: bool,
    #[serde(default)]
    pub tools: Vec<String>,
    /// Required request modalities (text, image, audio), beyond supported inputs.
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub hints: Vec<Hint>,
    #[serde(default)]
    pub default_for_text: bool,
    pub id: String,
    pub route: String,
    pub task: String,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub model: Option<ModelSpec>,
    #[serde(default)]
    pub backends: Vec<BackendChoice>,
    #[serde(default = "object")]
    pub params: Value,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hint {
    /// Literal Unicode word phrase, case-insensitive; never a regular expression.
    pub pattern: String,
    pub weight: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendChoice {
    pub backend: String,
    pub model: ModelSpec,
}
impl Specialist {
    pub fn choices(&self) -> Result<Vec<BackendChoice>, &'static str> {
        match (&self.backend, &self.model, self.backends.is_empty()) {
            (Some(backend), Some(model), true) if !backend.is_empty() => Ok(vec![BackendChoice {
                backend: backend.clone(),
                model: model.clone(),
            }]),
            (None, None, false) if self.backends.iter().all(|b| !b.backend.is_empty()) => {
                Ok(self.backends.clone())
            }
            _ => Err("specialist needs either backend/model or a nonempty backends list"),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSpec {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mmproj: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threads: Option<u32>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Router {
    pub id: String,
    pub strategy: String,
    #[serde(default)]
    pub backend: Option<String>,
    #[serde(default)]
    pub model: Option<ModelSpec>,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default = "min_confidence")]
    pub min_confidence: f64,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteInput {
    pub prompt: String,
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default)]
    pub audio: Vec<String>,
    #[serde(default)]
    pub specialist: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub route: String,
    pub inputs: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteDecision {
    #[serde(default)]
    pub stages: Vec<String>,
    #[serde(default)]
    pub model_called: bool,
    pub specialist: String,
    /// None for route-only decisions; set after successful loading.
    #[serde(default)]
    pub backend: Option<String>,
    pub confidence: f64,
    #[serde(default)]
    pub scores: BTreeMap<String, f64>,
    pub reason: String,
    #[serde(default = "object")]
    pub extras: Value,
    #[serde(default)]
    pub router_ms: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecideRequest {
    pub key: String,
    pub state: String,
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    pub questions: BTreeMap<String, Question>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    Noul {
        instructions: String,
    },
    Choice {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecideResponse {
    pub answers: BTreeMap<String, Answer>,
}
/// d1's named answer fields, not a generic `value` wrapper. See
/// https://docs.liquid.ai/lfm/models/decision-models#typed-answers and
/// https://huggingface.co/LiquidAI/d1-2B . Only the three llama.cpp primitives
/// are represented here; backend-specific extra answer fields are tolerated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        confidence: f64,
        probabilities: BTreeMap<String, f64>,
    },
    Score {
        score: f64,
        confidence: f64,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        probabilities: BTreeMap<String, f64>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        legend: BTreeMap<String, String>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}
impl Request {
    pub fn new(id: u64, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            method: method.into(),
            params,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}
impl Notification {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_value"
    )]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}
// Preserve a successful JSON null as Some(Null), distinct from an absent result.
fn present_value<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}
impl Response {
    pub fn success(id: u64, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }
    pub fn failure(id: u64, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(error),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}
impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifest_defaults_and_extension_config() {
        let source = "[plugin]\nid='demo'\nversion='0.1'\n[config]\ncustom={nested=true}\n[router]\nid='rules'\nstrategy='rules'\n";
        let manifest: Manifest = toml::from_str(source).unwrap();
        assert_eq!(manifest.plugin.protocol, 1);
        assert_eq!(manifest.config["custom"]["nested"], true);
        assert_eq!(manifest.router.as_ref().unwrap().min_confidence, 0.55);
        let encoded = toml::to_string(&manifest).unwrap();
        assert_eq!(toml::from_str::<Manifest>(&encoded).unwrap(), manifest);
    }
    #[test]
    fn audio_wire_types_round_trip_and_optional_fields_default() {
        let transcribe: TranscribeRequest =
            json::from_value(json::json!({"key":"asr","audio":"recording.wav"})).unwrap();
        assert_eq!(transcribe.language, None);
        let speak: SpeakRequest =
            json::from_value(json::json!({"key":"tts","text":"Hello"})).unwrap();
        assert_eq!(speak.voice, None);
        assert_eq!(speak.language, None);
        assert_eq!(
            json::from_value::<SpeakRequest>(json::to_value(&speak).unwrap()).unwrap(),
            speak
        );
        assert_eq!(
            json::from_value::<TranscribeRequest>(json::to_value(&transcribe).unwrap()).unwrap(),
            transcribe
        );
        let response = SpeakResponse {
            audio: "data:audio/wav;base64,AA==".into(),
            sample_rate: 16000,
            duration_ms: 1,
        };
        assert_eq!(
            json::from_value::<SpeakResponse>(json::to_value(&response).unwrap()).unwrap(),
            response
        );
        let response = TranscribeResponse {
            text: "Hello".into(),
            language: Some("en".into()),
        };
        assert_eq!(
            json::from_value::<TranscribeResponse>(json::to_value(&response).unwrap()).unwrap(),
            response
        );
        assert!(json::from_value::<SpeakRequest>(json::json!({"key":"tts"})).is_err());
        assert!(
            json::from_value::<TranscribeRequest>(json::json!({"key":"asr","audio":1})).is_err()
        );
    }
    #[test]
    fn all_manifest_structs_reject_unknown_keys() {
        for (kind, value) in [
            (
                "manifest",
                json::json!({"plugin":{"id":"x","version":"1"},"typo":1}),
            ),
            ("plugin", json::json!({"id":"x","version":"1","typo":1})),
            ("run", json::json!({"command":"x","typo":1})),
            ("model", json::json!({"source":"file:x","typo":1})),
            (
                "router",
                json::json!({"id":"x","strategy":"rules","typo":1}),
            ),
            (
                "specialist",
                json::json!({"id":"x","route":"x","task":"chat","backend":"x","model":{"source":"file:x"},"typo":1}),
            ),
        ] {
            let rejected = match kind {
                "manifest" => json::from_value::<Manifest>(value).is_err(),
                "plugin" => json::from_value::<Plugin>(value).is_err(),
                "run" => json::from_value::<Run>(value).is_err(),
                "model" => json::from_value::<ModelSpec>(value).is_err(),
                "router" => json::from_value::<Router>(value).is_err(),
                _ => json::from_value::<Specialist>(value).is_err(),
            };
            assert!(rejected, "{kind}");
        }
    }
    #[test]
    fn question_and_d1_answer_shapes() {
        let q = Question::Noul {
            instructions: "Needed?".into(),
        };
        assert_eq!(
            json::to_value(q).unwrap(),
            json::json!({"type":"noul","instructions":"Needed?"})
        );
        let wire = json::json!({"answers":{
            "gate":{"type":"noul","noul":0.8},
            "route":{"type":"choice","choice":"text","confidence":0.9,"probabilities":{"text":0.9,"vision":0.1}},
            "rank":{"type":"score","score":1.5,"confidence":0.75,"legend":{"0":"low","2":"high"},"probabilities":{"0":0.25,"2":0.75}}
        }});
        let parsed: DecideResponse = json::from_value(wire.clone()).unwrap();
        assert_eq!(json::to_value(parsed).unwrap(), wire);
    }
    #[test]
    fn rpc_round_trips_and_omits_unused_fields() {
        let req = Request::new(2, "initialize", object());
        assert_eq!(
            json::from_str::<Request>(&json::to_string(&req).unwrap()).unwrap(),
            req
        );
        let success = Response::success(2, Value::Null);
        let wire = json::to_value(&success).unwrap();
        assert!(wire.get("error").is_none());
        assert_eq!(json::from_value::<Response>(wire).unwrap(), success);
        let failure = json::to_value(Response::failure(
            2,
            RpcError::new(-32601, "Unknown method"),
        ))
        .unwrap();
        assert!(failure.get("result").is_none());
        assert!(failure["error"].get("data").is_none());
    }

    #[test]
    fn routing_and_decision_request_round_trips() {
        let input = RouteInput {
            prompt: "Read this".into(),
            images: vec!["photo.png".into()],
            audio: vec![],
            specialist: None,
        };
        assert_eq!(
            json::from_value::<RouteInput>(json::to_value(&input).unwrap()).unwrap(),
            input
        );
        let candidate = Candidate {
            id: "vision".into(),
            route: "Pictures".into(),
            inputs: vec!["text".into(), "image".into()],
        };
        assert_eq!(
            json::from_value::<Candidate>(json::to_value(&candidate).unwrap()).unwrap(),
            candidate
        );
        let decision = RouteDecision {
            stages: vec![],
            model_called: false,
            specialist: "vision".into(),
            backend: None,
            confidence: 0.9,
            scores: BTreeMap::from([("vision".into(), 0.9)]),
            reason: "test".into(),
            extras: object(),
            router_ms: 1,
        };
        assert_eq!(
            json::from_value::<RouteDecision>(json::to_value(&decision).unwrap()).unwrap(),
            decision
        );
        let request = DecideRequest {
            key: "router:test".into(),
            state: "hello".into(),
            images: vec![],
            files: vec![],
            questions: BTreeMap::from([
                (
                    "choice".into(),
                    Question::Choice {
                        instructions: "pick".into(),
                        criteria: BTreeMap::from([("text".into(), "Chat".into())]),
                    },
                ),
                (
                    "score".into(),
                    Question::Score {
                        instructions: "rank".into(),
                        criteria: vec!["low".into(), "high".into()],
                    },
                ),
            ]),
        };
        assert_eq!(
            json::from_value::<DecideRequest>(json::to_value(&request).unwrap()).unwrap(),
            request
        );
    }
}
