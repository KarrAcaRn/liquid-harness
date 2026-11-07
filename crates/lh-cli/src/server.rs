//! Small HTTP/1 adapter. The engine lock spans each complete inference, keeping
//! backend broadcast notifications isolated between concurrent HTTP clients.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use futures_util::stream;
use http_body_util::{BodyExt, Full, Limited, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    Request, Response, StatusCode,
    body::{Frame, Incoming},
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use lh_core::Engine;
use lh_protocol::{
    RouteDecision, RouteInput,
    json::{self, Value, json},
};
use std::{
    convert::Infallible,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, mpsc},
    task::JoinSet,
};

type Body = UnsyncBoxBody<Bytes, Infallible>;
type Reply = Response<Body>;
type ApiResult<T> = Result<T, (StatusCode, String)>;
const MAX_BODY: usize = 6 * 1024 * 1024;
#[derive(Clone)]
struct State {
    engine: Arc<Mutex<Engine>>,
    token: Option<String>,
    streams: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
}
fn bad(message: impl ToString) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, message.to_string())
}
fn backend(message: impl ToString) -> (StatusCode, String) {
    (StatusCode::BAD_GATEWAY, message.to_string())
}
fn bytes_response(bytes: impl Into<Bytes>, mime: &str) -> Reply {
    Response::builder()
        .header("content-type", mime)
        .body(Full::new(bytes.into()).boxed_unsync())
        .unwrap()
}
fn json_response(value: Value) -> Reply {
    bytes_response(value.to_string(), "application/json")
}
fn failure(status: StatusCode, message: &str) -> Reply {
    let mut reply = json_response(
        json!({"error":{"message":message,"type":"invalid_request_error","code":status.as_u16()}}),
    );
    *reply.status_mut() = status;
    if status == StatusCode::UNAUTHORIZED {
        reply
            .headers_mut()
            .insert("www-authenticate", "Bearer".parse().unwrap());
    }
    reply
}
fn routed(mut reply: Reply, route: &RouteDecision) -> Reply {
    let header = format!(
        "{};p={};stage={}",
        route.specialist, route.confidence, route.reason
    );
    if let Ok(header) = header.parse() {
        reply.headers_mut().insert("x-lh-route", header);
    }
    reply
}
fn data_url(value: &str, kind: &str) -> ApiResult<Vec<u8>> {
    let (header, encoded) = value
        .split_once(',')
        .ok_or_else(|| bad("media must be a base64 data URL"))?;
    if !header.starts_with(&format!("data:{kind}/")) || !header.ends_with(";base64") {
        return Err(bad(
            "remote URLs and filesystem media references are forbidden",
        ));
    }
    let bytes = STANDARD.decode(encoded).map_err(bad)?;
    if bytes.is_empty() {
        return Err(bad("empty media"));
    }
    Ok(bytes)
}
fn messages(value: &Value) -> ApiResult<Vec<Value>> {
    let messages = value["messages"]
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| bad("messages must be a nonempty array"))?;
    for message in messages {
        if !matches!(
            message["role"].as_str(),
            Some("system" | "developer" | "user" | "assistant" | "tool")
        ) {
            return Err(bad("invalid message role"));
        }
        match &message["content"] {
            Value::String(_) => {}
            Value::Null if message.get("tool_calls").is_some() => {}
            Value::Array(parts) => {
                for part in parts {
                    match part["type"].as_str() {
                        Some("text") if part["text"].is_string() => {}
                        Some("image_url") => {
                            data_url(
                                part["image_url"]["url"]
                                    .as_str()
                                    .ok_or_else(|| bad("image_url.url required"))?,
                                "image",
                            )?;
                        }
                        Some("input_audio") => {
                            let audio = &part["input_audio"];
                            let format = audio["format"]
                                .as_str()
                                .ok_or_else(|| bad("audio format required"))?;
                            if !matches!(
                                format,
                                "wav" | "mp3" | "flac" | "ogg" | "m4a" | "webm" | "mp4"
                            ) {
                                return Err(bad("unsupported audio format"));
                            }
                            let encoded = audio["data"]
                                .as_str()
                                .ok_or_else(|| bad("audio data required"))?;
                            if STANDARD.decode(encoded).map_err(bad)?.is_empty() {
                                return Err(bad("empty audio"));
                            }
                        }
                        _ => return Err(bad("unsupported content part")),
                    }
                }
            }
            _ => return Err(bad("invalid message content")),
        }
    }
    Ok(messages.clone())
}

fn chunk(id: &str, model: &str, delta: Value, finish: Value) -> Value {
    json!({"id":id,"object":"chat.completion.chunk","created":created(),"model":model,"choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
}
fn created() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
async fn chat(state: State, value: Value) -> ApiResult<Reply> {
    let messages = messages(&value)?;
    let model = value
        .get("model")
        .map(|v| v.as_str().ok_or_else(|| bad("model must be a string")))
        .transpose()?
        .unwrap_or("auto")
        .to_owned();
    let streaming = value
        .get("stream")
        .map(|v| v.as_bool().ok_or_else(|| bad("stream must be boolean")))
        .transpose()?
        .unwrap_or(false);
    let mut params = value
        .as_object()
        .cloned()
        .ok_or_else(|| bad("object required"))?;
    for name in ["messages", "model", "stream"] {
        params.remove(name);
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = format!(
        "chatcmpl-{}-{}",
        created(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let mut engine = state.engine.clone().lock_owned().await;
    if !streaming {
        let (route, response) =
            crate::conversation::complete(&mut engine, messages, &model, None, json!(params), None)
                .await
                .map_err(backend)?;
        return Ok(routed(
            json_response(
                json!({"id":id,"object":"chat.completion","created":created(),"model":route.specialist,"choices":[{"index":0,"message":response["message"],"finish_reason":response["finish_reason"]}],"usage":response["usage"]}),
            ),
            &route,
        ));
    }
    // Resolve before sending headers. Pin the chosen specialist for complete so
    // inference cannot disagree with the route advertised on the response.
    let input = crate::conversation::routing_input(&engine, &messages, &model);
    let route = engine.route(&input, None).await.map_err(bad)?;
    let selected = route.specialist.clone();
    let (out, incoming) = mpsc::channel::<Bytes>(16);
    let worker = tokio::spawn(async move {
        let (deltas, mut rx) = mpsc::unbounded_channel();
        let inference = crate::conversation::complete(
            &mut engine,
            messages,
            &selected,
            None,
            json!(params),
            Some(deltas),
        );
        tokio::pin!(inference);
        let result = loop {
            tokio::select! {
                biased;
                delta = rx.recv(), if !rx.is_closed() || !rx.is_empty() => if let Some(delta) = delta {
                    let frame = format!("data: {}\n\n", chunk(&id, &selected, delta, Value::Null));
                    // A disconnected client must not release the engine while
                    // its backend is still emitting broadcast notifications.
                    let _ = out.send(Bytes::from(frame)).await;
                },
                result = &mut inference => break result,
            }
        };
        while let Ok(delta) = rx.try_recv() {
            let frame = format!("data: {}\n\n", chunk(&id, &selected, delta, Value::Null));
            let _ = out.send(Bytes::from(frame)).await;
        }
        let final_event = match result {
            Ok((_, response)) => {
                chunk(&id, &selected, json!({}), response["finish_reason"].clone())
            }
            Err(error) => json!({"error":{"message":error.to_string(),"type":"server_error"}}),
        };
        let _ = out
            .send(Bytes::from(format!(
                "data: {final_event}\n\ndata: [DONE]\n\n"
            )))
            .await;
    });
    {
        let mut streams = state.streams.lock().unwrap();
        streams.retain(|worker| !worker.is_finished());
        streams.push(worker.abort_handle());
    }
    let frames = stream::unfold(incoming, |mut rx| async {
        rx.recv()
            .await
            .map(|bytes| (Ok::<_, Infallible>(Frame::data(bytes)), rx))
    });
    let reply = Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("x-accel-buffering", "no")
        .body(StreamBody::new(frames).boxed_unsync())
        .unwrap();
    Ok(routed(reply, &route))
}
async fn task(
    engine: &mut Engine,
    value: &Value,
    kind: &str,
    params: Value,
) -> ApiResult<(RouteDecision, Value)> {
    let model = value
        .get("model")
        .map(|v| v.as_str().ok_or_else(|| bad("model must be string")))
        .transpose()?
        .unwrap_or("auto");
    let id = if model == "auto" {
        crate::inference::task_specialist(engine, kind).map_err(bad)?
    } else {
        model.into()
    };
    if engine
        .registry
        .specialists
        .get(&id)
        .is_none_or(|s| s.task != kind)
    {
        return Err(bad(format!("model must name a {kind} specialist")));
    }
    let route = engine.prepare(&id).await.map_err(backend)?;
    let result = engine.dispatch(&route, params).await.map_err(backend)?;
    Ok((route, result))
}
async fn multipart(content_type: &str, bytes: Bytes) -> ApiResult<Value> {
    let boundary = multer::parse_boundary(content_type).map_err(bad)?;
    let source = stream::once(async move { Ok::<_, Infallible>(bytes) });
    let mut form = multer::Multipart::new(source, boundary);
    let mut value = json!({});
    while let Some(field) = form.next_field().await.map_err(bad)? {
        let name = field
            .name()
            .ok_or_else(|| bad("unnamed multipart field"))?
            .to_owned();
        if value
            .get(if name == "file" { "audio" } else { &name })
            .is_some()
        {
            return Err(bad("duplicate multipart field"));
        }
        if name == "file" {
            let bytes = field.bytes().await.map_err(bad)?;
            if bytes.is_empty() {
                return Err(bad("empty audio file"));
            }
            // Leave room for base64 expansion and JSON under the plugin's 8 MiB frame limit.
            if bytes.len() > 5 * 1024 * 1024 {
                return Err((StatusCode::PAYLOAD_TOO_LARGE, "audio exceeds 5 MiB".into()));
            }
            value["audio"] = json!(format!("data:audio/wav;base64,{}", STANDARD.encode(bytes)));
        } else if matches!(name.as_str(), "model" | "language" | "response_format") {
            value[&name] = json!(field.text().await.map_err(bad)?);
        } else {
            return Err(bad("unsupported multipart field"));
        }
    }
    if value.get("audio").is_none() {
        return Err(bad("multipart file required"));
    }
    if value.get("response_format").is_some_and(|v| v != "json") {
        return Err(bad("only json transcription format is supported"));
    }
    Ok(value)
}
async fn handle(request: Request<Incoming>, state: State) -> ApiResult<Reply> {
    if let Some(token) = &state.token {
        let expected = format!("Bearer {token}");
        if request
            .headers()
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            != Some(expected.as_str())
        {
            return Err((StatusCode::UNAUTHORIZED, "invalid bearer token".into()));
        }
    }
    let path = request.uri().path().to_owned();
    if request.method() == hyper::Method::GET && path == "/v1/models" {
        let engine = state.engine.lock().await;
        let mut data =
            vec![json!({"id":"auto","object":"model","created":0,"owned_by":"liquid-harness"})];
        data.extend(
            engine.registry.specialists.keys().map(
                |id| json!({"id":id,"object":"model","created":0,"owned_by":"liquid-harness"}),
            ),
        );
        return Ok(json_response(json!({"object":"list","data":data})));
    }
    if !matches!(
        path.as_str(),
        "/v1/chat/completions"
            | "/v1/audio/transcriptions"
            | "/v1/audio/speech"
            | "/v1/route"
            | "/v1/decide"
            | "/route"
            | "/decide"
    ) {
        return Err((StatusCode::NOT_FOUND, "unknown endpoint".into()));
    }
    if request.method() != hyper::Method::POST {
        return Err((StatusCode::METHOD_NOT_ALLOWED, "POST required".into()));
    }
    let content_type = request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = tokio::time::timeout(
        Duration::from_secs(30),
        Limited::new(request.into_body(), MAX_BODY).collect(),
    )
    .await
    .map_err(|_| (StatusCode::REQUEST_TIMEOUT, "request body timeout".into()))?
    .map_err(|_| {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            "body exceeds limit or is incomplete".into(),
        )
    })?
    .to_bytes();
    let value: Value = if path == "/v1/audio/transcriptions" {
        multipart(&content_type, bytes).await?
    } else {
        if content_type.split(';').next() != Some("application/json") {
            return Err((
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "application/json required".into(),
            ));
        }
        json::from_slice(&bytes).map_err(bad)?
    };
    if !value.is_object() {
        return Err(bad("JSON object required"));
    }
    if path == "/v1/chat/completions" {
        return chat(state, value).await;
    }
    let mut engine = state.engine.lock().await;
    match path.as_str() {
        "/v1/route" | "/route" => {
            let input: RouteInput = json::from_value(value.clone()).map_err(bad)?;
            for image in &input.images {
                data_url(image, "image")?;
            }
            for audio in &input.audio {
                data_url(audio, "audio")?;
            }
            let route = engine
                .route(&input, value["router"].as_str())
                .await
                .map_err(bad)?;
            Ok(routed(json_response(json!(route)), &route))
        }
        "/v1/decide" | "/decide" => {
            if value
                .get("files")
                .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
            {
                return Err(bad("file attachments forbidden"));
            }
            if let Some(images) = value.get("images") {
                for image in images
                    .as_array()
                    .ok_or_else(|| bad("images must be array"))?
                {
                    data_url(
                        image
                            .as_str()
                            .ok_or_else(|| bad("image must be data URL"))?,
                        "image",
                    )?;
                }
            }
            let mut params = value.clone();
            params["key"] = json!("");
            let request: lh_protocol::DecideRequest =
                json::from_value(params.clone()).map_err(bad)?;
            if request.questions.is_empty() {
                return Err(bad("questions required"));
            }
            let id = value["model"]
                .as_str()
                .filter(|s| *s != "auto")
                .unwrap_or(&engine.config.router)
                .to_owned();
            if let Some(router) = engine.registry.routers.get(&id).cloned() {
                let backend_id = router
                    .backend
                    .as_deref()
                    .ok_or_else(|| bad("router has no decision backend"))?;
                let model = router
                    .model
                    .ok_or_else(|| bad("router has no decision model"))?;
                let key = format!("router:{id}");
                let host = engine
                    .require(backend_id, "decide", &id)
                    .await
                    .map_err(backend)?;
                host.call(
                    "backend/load",
                    json!({"key":key,"model":model,"pinned":true}),
                )
                .await
                .map_err(backend)?;
                params["key"] = json!(key);
                let response = host.call("backend/decide", params).await.map_err(backend)?;
                let mut reply = json_response(response);
                if let Ok(header) = format!("{id};p=1;stage=explicit").parse() {
                    reply.headers_mut().insert("x-lh-route", header);
                }
                Ok(reply)
            } else {
                let (route, response) = task(&mut engine, &value, "decide", params).await?;
                Ok(routed(json_response(response), &route))
            }
        }
        "/v1/audio/transcriptions" => {
            let (route, response) = task(&mut engine, &value, "transcribe", value.clone()).await?;
            Ok(routed(json_response(response), &route))
        }
        "/v1/audio/speech" => {
            let text = value["input"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| bad("nonempty input required"))?;
            if value.get("response_format").is_some_and(|v| v != "wav") {
                return Err(bad("only WAV speech output supported"));
            }
            let (route, response) = task(
                &mut engine,
                &value,
                "speak",
                json!({"text":text,"voice":value["voice"],"language":value["language"]}),
            )
            .await?;
            let audio = response["audio"]
                .as_str()
                .ok_or_else(|| backend("missing speech audio"))?;
            let bytes = if audio.starts_with("data:") {
                data_url(audio, "audio").map_err(|(_, e)| backend(e))?
            } else {
                std::fs::read(audio.strip_prefix("file:").unwrap_or(audio)).map_err(backend)?
            };
            if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
                return Err(backend("backend returned invalid WAV"));
            }
            Ok(routed(bytes_response(bytes, "audio/wav"), &route))
        }
        _ => unreachable!(),
    }
}
/// Serve until Ctrl-C, then stop accepting requests, drain connections briefly,
/// cancel remaining work, and explicitly shut down every backend process.
pub async fn serve(engine: Engine, bind: &str) -> Result<(), Box<dyn std::error::Error>> {
    let token = engine
        .config
        .server_token_env
        .as_ref()
        .map(|name| {
            std::env::var(name)
                .ok()
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| {
                    format!("server token environment variable '{name}' is missing or empty")
                })
        })
        .transpose()?;
    let listener = TcpListener::bind(bind).await?;
    eprintln!("lh server listening on {}", listener.local_addr()?);
    let state = State {
        engine: Arc::new(Mutex::new(engine)),
        token,
        streams: Arc::new(std::sync::Mutex::new(Vec::new())),
    };
    let mut connections = JoinSet::new();
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    let outcome = loop {
        tokio::select! {
            signal = &mut shutdown => break signal,
            accept = listener.accept() => match accept {
                Ok((socket, _)) => { let state = state.clone(); connections.spawn(async move {
                    let service = service_fn(move |request| { let state = state.clone(); async move {
                        Ok::<_, Infallible>(match handle(request, state).await { Ok(reply) => reply, Err((status, message)) => failure(status, &message) })
                    }});
                    let _ = hyper::server::conn::http1::Builder::new().max_buf_size(32 * 1024).serve_connection(TokioIo::new(socket), service).await;
                }); },
                Err(error) => break Err(error),
            },
            _ = connections.join_next(), if !connections.is_empty() => {},
        }
    };
    drop(listener);
    if tokio::time::timeout(Duration::from_secs(2), async {
        while connections.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
    // No new requests can subscribe now; cancellation is safe during shutdown.
    for worker in state.streams.lock().unwrap().drain(..) {
        worker.abort();
    }
    state.engine.lock().await.shutdown().await;
    outcome?;
    Ok(())
}
