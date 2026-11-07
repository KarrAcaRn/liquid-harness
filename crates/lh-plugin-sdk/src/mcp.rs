//! Serial MCP 2025-03-26 stdio server. No backend-protocol envelopes are used.
use lh_protocol::json::{self, Value, json};
use std::io::{self, BufRead, Read, Write};

pub trait ToolServer {
    fn name(&self) -> &str;
    fn configure(&mut self, config: Value) -> Result<(), String>;
    fn tools(&self) -> Value;
    fn call(&mut self, name: &str, args: Value) -> Result<String, String>;
}

/// MCP tool annotations are advisory, not an authorization mechanism.
pub fn tool(
    name: &str,
    description: &str,
    properties: Value,
    required: &[&str],
    destructive: bool,
) -> Value {
    json!({"name":name,"description":description,
        "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
        "annotations":{"readOnlyHint":!destructive,"destructiveHint":destructive,"openWorldHint":true}})
}

pub fn serve(server: impl ToolServer) -> io::Result<()> {
    let config = match std::env::var("LH_TOOL_CONFIG") {
        Ok(s) => json::from_str(&s).map_err(io::Error::other)?,
        Err(std::env::VarError::NotPresent) => json!({}),
        Err(e) => return Err(io::Error::other(e)),
    };
    run(server, io::stdin().lock(), io::stdout().lock(), config)
}

fn run(
    mut server: impl ToolServer,
    mut input: impl BufRead,
    mut output: impl Write,
    config: Value,
) -> io::Result<()> {
    let mut initialized = false;
    let mut ready = false;
    loop {
        let mut line = Vec::new();
        if input
            .by_ref()
            .take(crate::MAX_FRAME_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)?
            == 0
        {
            break;
        }
        if line.len() > crate::MAX_FRAME_BYTES {
            return Err(io::Error::other("MCP frame too large"));
        }
        let request: Value = match json::from_slice(&line) {
            Ok(v) => v,
            Err(_) => {
                reply(
                    &mut output,
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
                )?;
                continue;
            }
        };
        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str);
        if request.get("jsonrpc") != Some(&json!("2.0"))
            || method.is_none()
            || id
                .as_ref()
                .is_some_and(|i| !i.is_string() && !i.is_number())
        {
            reply(
                &mut output,
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Invalid request"}}),
            )?;
            continue;
        }
        let method = method.unwrap();
        if id.is_none() {
            if method == "notifications/initialized" && initialized {
                ready = true;
            }
            continue;
        }
        let params = request.get("params").cloned().unwrap_or(json!({}));
        let result: Result<Value, (i32, String)> = match method {
            "initialize" if !initialized => {
                if !params.get("protocolVersion").is_some_and(Value::is_string) {
                    Err((-32602, "protocolVersion is required".into()))
                } else {
                    let mut effective = config.clone();
                    if let Some(extra) = params.get("config") {
                        if let (Some(base), Some(extra)) =
                            (effective.as_object_mut(), extra.as_object())
                        {
                            base.extend(extra.clone());
                        } else {
                            effective = Value::Null;
                        }
                    }
                    match server.configure(effective) {
                        Ok(()) => {
                            initialized = true;
                            Ok(
                                json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":server.name(),"version":env!("CARGO_PKG_VERSION")}}),
                            )
                        }
                        Err(e) => Err((-32602, e)),
                    }
                }
            }
            "ping" => Ok(json!({})),
            _ if !ready => Err((
                -32600,
                "Initialize and send notifications/initialized first".into(),
            )),
            "tools/list" => Ok(json!({"tools":server.tools()})),
            "tools/call" => {
                if let Some(name) = params.get("name").and_then(Value::as_str) {
                    let args = params.get("arguments").cloned().unwrap_or(json!({}));
                    if !args.is_object() {
                        Err((-32602, "arguments must be an object".into()))
                    } else if !server
                        .tools()
                        .as_array()
                        .is_some_and(|ts| ts.iter().any(|t| t["name"] == name))
                    {
                        Err((-32602, "Unknown tool".into()))
                    } else {
                        let (text, error) = match server.call(name, args) {
                            Ok(s) => (s, false),
                            Err(s) => (s, true),
                        };
                        Ok(json!({"content":[{"type":"text","text":text}],"isError":error}))
                    }
                } else {
                    Err((-32602, "name is required".into()))
                }
            }
            _ => Err((-32601, "Method not found".into())),
        };
        let response = match result {
            Ok(r) => json!({"jsonrpc":"2.0","id":id,"result":r}),
            Err((code, message)) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
            }
        };
        reply(&mut output, response)?;
    }
    Ok(())
}
fn reply(out: &mut impl Write, value: Value) -> io::Result<()> {
    let bytes = json::to_vec(&value)?;
    if bytes.len() + 1 > crate::MAX_FRAME_BYTES {
        return Err(io::Error::other("MCP response too large"));
    }
    out.write_all(&bytes)?;
    out.write_all(b"\n")?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Test;
    impl ToolServer for Test {
        fn name(&self) -> &str {
            "test"
        }
        fn configure(&mut self, c: Value) -> Result<(), String> {
            if c.is_object() {
                Ok(())
            } else {
                Err("config must be object".into())
            }
        }
        fn tools(&self) -> Value {
            json!([tool("fail", "failure", json!({}), &[], false)])
        }
        fn call(&mut self, _: &str, _: Value) -> Result<String, String> {
            Err("expected".into())
        }
    }
    #[test]
    fn lifecycle_and_errors() {
        let requests = [
            json!({"jsonrpc":"2.0","id":0,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":"2024-11-05"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"fail"}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"missing"}}),
        ];
        let input = requests
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let mut out = Vec::new();
        run(Test, io::Cursor::new(input), &mut out, json!({})).unwrap();
        let responses: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|s| json::from_str(s).unwrap())
            .collect();
        assert_eq!(responses.len(), 5);
        assert!(responses[0]["error"].is_object());
        assert_eq!(responses[1]["id"], "init");
        assert_eq!(responses[2]["result"]["tools"][0]["name"], "fail");
        assert_eq!(responses[3]["result"]["isError"], true);
        assert_eq!(responses[4]["error"]["code"], -32602);
    }
}
