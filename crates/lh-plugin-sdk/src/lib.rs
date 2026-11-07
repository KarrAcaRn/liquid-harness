//! Blocking, single-request-at-a-time plugin transport. Stdout is reserved for
//! protocol traffic. Cancellation cannot interrupt a running blocking handler;
//! unknown notifications (including `$/cancel`) are ignored. Shutdown is honored
//! between calls, without a response.
use lh_protocol::{
    Notification, Request, Response, RpcError,
    json::{self, Value},
};
use std::io::{self, BufRead, Read, Write};

/// Maximum incoming or outgoing JSON frame size, including its newline.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

pub trait Handler {
    fn handle(
        &mut self,
        method: &str,
        params: Value,
        ctx: &mut Context<'_>,
    ) -> Result<Value, RpcError>;
}

pub struct Context<'a> {
    pub id: u64,
    output: &'a mut dyn Write,
}
impl Context<'_> {
    pub fn stream(&mut self, delta: Value) -> io::Result<()> {
        write_value(
            self.output,
            json::to_value(Notification::new(
                "$/stream",
                json::json!({"id":self.id,"delta":delta}),
            ))
            .map_err(io::Error::other)?,
        )
    }
    /// Report request-scoped progress; omit the fraction when it is unknown.
    pub fn progress(&mut self, message: &str, fraction: Option<f64>) -> io::Result<()> {
        let mut params = json::json!({"id": self.id, "message": message});
        if let Some(fraction) = fraction {
            params["fraction"] = json::json!(fraction);
        }
        write_value(
            self.output,
            json::to_value(Notification::new("$/progress", params)).map_err(io::Error::other)?,
        )
    }
    pub fn log(&mut self, level: &str, message: &str) -> io::Result<()> {
        write_value(
            self.output,
            json::to_value(Notification::new(
                "$/log",
                json::json!({"level":level,"message":message}),
            ))
            .map_err(io::Error::other)?,
        )
    }
}

pub fn serve(handler: impl Handler) -> io::Result<()> {
    serve_io(handler, io::stdin().lock(), io::stdout().lock())
}

/// Run the same transport with caller-owned streams, useful for embedding/tests.
/// Oversized or unterminated frames terminate the session with `InvalidData`.
/// Malformed JSON/envelopes receive JSON-RPC errors and do not stop later calls.
pub fn serve_io(
    mut handler: impl Handler,
    mut input: impl BufRead,
    mut output: impl Write,
) -> io::Result<()> {
    let mut frame = Vec::new();
    loop {
        frame.clear();
        let count = input
            .by_ref()
            .take((MAX_FRAME_BYTES + 1) as u64)
            .read_until(b'\n', &mut frame)?;
        if count == 0 {
            return Ok(());
        }
        if count > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "JSON-RPC frame exceeds size limit",
            ));
        }
        if frame.last() != Some(&b'\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "JSON-RPC frame missing newline",
            ));
        }
        let value: Value = match json::from_slice(&frame) {
            Ok(value) => value,
            Err(_) => {
                write_error(&mut output, Value::Null, -32700, "Parse error")?;
                continue;
            }
        };
        let valid_envelope = value.is_object()
            && value.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
            && value.get("method").and_then(Value::as_str).is_some();
        let id = value.get("id");
        if !valid_envelope || id.is_some_and(|id| id.as_u64().is_none()) {
            write_error(
                &mut output,
                id.and_then(Value::as_u64)
                    .map(Value::from)
                    .unwrap_or(Value::Null),
                -32600,
                "Invalid Request",
            )?;
            continue;
        }
        let params_valid = value
            .get("params")
            .is_none_or(|p| p.is_null() || p.is_object() || p.is_array());
        if id.is_none() {
            if params_valid && value["method"] == "shutdown" {
                return Ok(());
            }
            continue;
        }
        let request: Request = json::from_value(value).map_err(io::Error::other)?;
        if !params_valid {
            write_error(
                &mut output,
                Value::from(request.id),
                -32602,
                "Invalid params",
            )?;
            continue;
        }
        let mut ctx = Context {
            id: request.id,
            output: &mut output,
        };
        let response = match handler.handle(&request.method, request.params, &mut ctx) {
            Ok(result) => Response::success(request.id, result),
            Err(error) => Response::failure(request.id, error),
        };
        write_value(
            &mut output,
            json::to_value(response).map_err(io::Error::other)?,
        )?;
    }
}

fn write_error(output: &mut dyn Write, id: Value, code: i64, message: &str) -> io::Result<()> {
    // Null IDs are reserved for transport errors, outside the public u64 response DTO.
    write_value(
        output,
        json::json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}),
    )
}
fn write_value(output: &mut dyn Write, value: Value) -> io::Result<()> {
    let mut bytes = json::to_vec(&value).map_err(io::Error::other)?;
    if bytes.len() >= MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "JSON-RPC frame exceeds size limit",
        ));
    }
    bytes.push(b'\n');
    output.write_all(&bytes)?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Echo;
    impl Handler for Echo {
        fn handle(
            &mut self,
            method: &str,
            params: Value,
            ctx: &mut Context<'_>,
        ) -> Result<Value, RpcError> {
            match method {
                "echo" => {
                    ctx.stream(json::json!({"content":"chunk"}))
                        .map_err(|e| RpcError::new(-32603, e.to_string()))?;
                    ctx.log("info", "working")
                        .map_err(|e| RpcError::new(-32603, e.to_string()))?;
                    Ok(params)
                }
                _ => Err(RpcError::new(-32601, "Method not found")),
            }
        }
    }
    fn run(input: &str) -> Vec<Value> {
        let mut output = Vec::new();
        serve_io(Echo, input.as_bytes(), &mut output).unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| json::from_str(line).unwrap())
            .collect()
    }
    #[test]
    fn dispatch_stream_log_result_and_shutdown() {
        let messages = run(concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"echo\",\"params\":{\"text\":\"hello\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"shutdown\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"echo\"}\n"
        ));
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["method"], "$/stream");
        assert_eq!(messages[0]["params"]["id"], 7);
        assert_eq!(
            messages[1]["params"],
            json::json!({"level":"info","message":"working"})
        );
        assert_eq!(
            messages[2],
            json::json!({"jsonrpc":"2.0","id":7,"result":{"text":"hello"}})
        );
    }
    #[test]
    fn malformed_input_recovers_and_unknown_methods_fail() {
        let messages = run(concat!(
            "not json\n",
            "[]\n",
            "{\"jsonrpc\":\"1.0\",\"id\":4,\"method\":\"echo\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":-1,\"method\":\"echo\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"echo\",\"params\":true}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"$/cancel\",\"params\":{\"id\":5}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"missing\"}\n"
        ));
        let codes: Vec<i64> = messages
            .iter()
            .map(|m| m["error"]["code"].as_i64().unwrap())
            .collect();
        assert_eq!(codes, [-32700, -32600, -32600, -32600, -32602, -32601]);
        assert_eq!(messages[0]["id"], Value::Null);
        assert_eq!(messages[2]["id"], 4);
        assert_eq!(messages[3]["id"], Value::Null);
    }
    #[test]
    fn frames_are_bounded_and_must_be_terminated() {
        let large = vec![b'x'; MAX_FRAME_BYTES + 1];
        for input in [large.as_slice(), b"{}".as_slice()] {
            let err = serve_io(Echo, input, Vec::new()).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
        assert!(run("").is_empty());
    }
    #[test]
    fn output_errors_propagate() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let err = serve_io(Echo, b"bad\n".as_slice(), Broken).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }
}
