//! Bounded OpenAI SSE decoding shared by HTTP backend adapters.
use crate::{
    RpcError,
    json::{self, Value, json},
};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read},
};
fn failed(e: impl ToString) -> RpcError {
    RpcError::new(-32001, e.to_string())
}
fn append(value: &mut Value, text: &str) {
    if let Value::String(s) = value {
        s.push_str(text);
    } else {
        *value = json!(text);
    }
}
/// Decode a complete SSE stream, emitting deltas before returning the aggregate.
pub fn sse(
    reader: impl Read,
    mut emit: impl FnMut(Value) -> Result<(), RpcError>,
) -> Result<Value, RpcError> {
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let mut event = String::new();
    let mut message = json!({"role":"assistant","content":null});
    let mut tools: BTreeMap<u64, Value> = BTreeMap::new();
    let mut finish = Value::Null;
    // Empty means the backend reported no usage; zeros would look like real counts.
    let mut usage = json!({});
    let mut timings = Value::Null;
    let mut bytes = 0;
    const MAX: usize = 8 * 1024 * 1024;
    loop {
        line.clear();
        let n = reader
            .by_ref()
            .take((MAX + 1) as u64)
            .read_line(&mut line)
            .map_err(failed)?;
        bytes += n;
        if bytes > MAX {
            return Err(failed("SSE response exceeds size limit"));
        }
        if n == 0 {
            return Err(failed("SSE ended before [DONE]"));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if let Some(data) = line.strip_prefix("data:") {
            if !event.is_empty() {
                event.push('\n');
            }
            event.push_str(data.strip_prefix(' ').unwrap_or(data));
        }
        if !line.is_empty() || event.is_empty() {
            continue;
        }
        if event == "[DONE]" {
            break;
        }
        let chunk: Value = json::from_str(&event).map_err(failed)?;
        event.clear();
        if !chunk["error"].is_null() {
            return Err(failed(chunk["error"].to_string()));
        }
        if chunk["usage"].is_object() {
            usage = chunk["usage"].clone();
        }
        // llama.cpp reports generation timings on its final chunk.
        if chunk["timings"].is_object() {
            timings = chunk["timings"].clone();
        }
        if let Some(choice) = chunk["choices"].as_array().and_then(|a| a.first()) {
            let delta = &choice["delta"];
            if delta.is_object() {
                emit(delta.clone())?;
                if let Some(role) = delta["role"].as_str() {
                    message["role"] = json!(role);
                }
                for field in ["content", "reasoning_content", "refusal"] {
                    if let Some(text) = delta[field].as_str() {
                        append(&mut message[field], text);
                    }
                }
                if let Some(calls) = delta["tool_calls"].as_array() {
                    for call in calls {
                        let index = call["index"]
                            .as_u64()
                            .ok_or_else(|| failed("tool delta missing index"))?;
                        let tool = tools.entry(index).or_insert_with(|| json!({"id":"","type":"function","function":{"name":"","arguments":""}}));
                        for field in ["id", "type"] {
                            if let Some(s) = call[field].as_str() {
                                tool[field] = json!(s);
                            }
                        }
                        for field in ["name", "arguments"] {
                            if let Some(s) = call["function"][field].as_str() {
                                append(&mut tool["function"][field], s);
                            }
                        }
                    }
                }
            }
            if !choice["finish_reason"].is_null() {
                finish = choice["finish_reason"].clone();
            }
        }
    }
    if finish.is_null() {
        return Err(failed("SSE missing finish reason"));
    }
    if !tools.is_empty() {
        message["tool_calls"] = json!(tools.into_values().collect::<Vec<_>>());
    }
    let mut result = json!({"message":message,"finish_reason":finish,"usage":usage});
    if timings.is_object() {
        result["timings"] = timings;
    }
    Ok(result)
}
