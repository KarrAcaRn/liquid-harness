//! OpenAI-compatible chat payloads. Model-specific tool schemas and sampling
//! parameters stay JSON values; the harness does not rewrite them.
use crate::{json::Value, object};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub key: String,
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(default = "object")]
    pub params: Value,
    #[serde(default)]
    pub stream: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: Option<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
    InputAudio { input_audio: InputAudio },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputAudio {
    pub data: String,
    pub format: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    pub message: Message,
    pub finish_reason: String,
    pub usage: Usage,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{self, json};
    #[test]
    fn chat_multimodal_and_tool_round_trip() {
        let wire = json!({"key":"test","messages":[
            {"role":"system","content":"help"},
            {"role":"user","content":[{"type":"text","text":"hello"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAA"}},{"type":"input_audio","input_audio":{"data":"AAA","format":"wav"}}]},
            {"role":"assistant","content":null,"tool_calls":[{"id":"a","type":"function","function":{"name":"test","arguments":"{}"}}]},
            {"role":"tool","content":"ok","tool_call_id":"a"}
        ],"tools":[],"params":{"temperature":0.3},"stream":true});
        let request: ChatRequest = json::from_value(wire.clone()).unwrap();
        assert_eq!(json::to_value(request).unwrap(), wire);
        let result = json!({"message":{"role":"assistant","content":"hello"},"finish_reason":"stop","usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}});
        assert_eq!(
            json::to_value(json::from_value::<ChatResponse>(result.clone()).unwrap()).unwrap(),
            result
        );
    }
}
