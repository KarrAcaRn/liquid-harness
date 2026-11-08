//! Harness-only APIs; authorization and request body limits live in the parent.
use super::*;
use lh_core::{
    agent::{self, Control},
    tools,
};
use std::collections::BTreeMap;
pub(super) type Runs = Arc<Mutex<BTreeMap<String, Arc<Control>>>>;

pub(super) async fn management(state: &State, path: &str) -> ApiResult<Reply> {
    // Status never waits for the engine: model states come from the shared
    // lifecycle tracker, process details only when the engine is free.
    if path == "/lh/v1/status" {
        let mut value = match state.engine.try_lock() {
            Ok(mut engine) => {
                engine.reconcile().await;
                json!({"engine":"idle","processes":engine.process_status().await,"resident_models":engine.resident_models().await,"cascade":engine.routing_stats()})
            }
            Err(_) => json!({"engine":"busy"}),
        };
        if let (Some(target), Value::Object(lifecycle)) =
            (value.as_object_mut(), state.lifecycle.snapshot())
        {
            target.extend(lifecycle);
        }
        return Ok(json_response(value));
    }
    // A running agent holds the engine lock. Fail promptly rather than deadlock
    // clients trying to inspect status while an approval is pending.
    let mut engine = state
        .engine
        .try_lock()
        .map_err(|_| (StatusCode::CONFLICT, "engine busy; retry after run".into()))?;
    let value = match path {
        "/lh/v1/plugins" => {
            let states = engine.process_status().await;
            json!({"plugins":engine.registry.plugins.iter().map(|(id,e)|json!({"id":id,"version":e.manifest.plugin.version,"description":e.manifest.plugin.description,"provides":e.manifest.plugin.provides,"capabilities":e.manifest.plugin.capabilities,"capabilities_source":"manifest","enabled":engine.enabled(id),"manifest_path":e.path,"process":states[id]})).collect::<Vec<_>>()})
        }
        "/lh/v1/specialists" => {
            json!({"specialists":engine.registry.specialists.values().collect::<Vec<_>>()})
        }
        "/lh/v1/router" => {
            json!({"selected":engine.config.router,"routers":engine.registry.routers.values().collect::<Vec<_>>(),"cascade":engine.config.cascade,"stats":engine.routing_stats()})
        }
        "/lh/v1/tools" => {
            json!({"tools":tools::discover(&mut engine,&[]).await.map_err(backend)?,"builtins":agent::builtin_schemas()})
        }
        "/lh/v1/skills" => {
            json!({"skills":agent::skill_registry(&engine).map_err(backend)?.values().map(|s|json!({"name":s.name,"description":s.description,"tools":s.tools})).collect::<Vec<_>>()})
        }
        _ if path.starts_with("/lh/v1/skills/") => {
            let name = path.trim_start_matches("/lh/v1/skills/");
            let skills = agent::skill_registry(&engine).map_err(backend)?;
            json!(
                skills
                    .get(name)
                    .ok_or((StatusCode::NOT_FOUND, "unknown skill".into()))?
            )
        }
        _ => return Err((StatusCode::NOT_FOUND, "unknown endpoint".into())),
    };
    Ok(json_response(value))
}
pub(super) async fn control(state: &State, path: &str, value: Value) -> ApiResult<Reply> {
    let rest = path
        .strip_prefix("/lh/v1/agent/runs/")
        .ok_or_else(|| bad("invalid run path"))?;
    let (id, action) = rest.split_once('/').ok_or_else(|| bad("action required"))?;
    let control = state
        .runs
        .lock()
        .await
        .get(id)
        .cloned()
        .ok_or((StatusCode::NOT_FOUND, "unknown or finished run".into()))?;
    match action {
        "approve" => {
            let call_id = value["call_id"]
                .as_str()
                .ok_or_else(|| bad("call_id required"))?;
            let allow = value["allow"]
                .as_bool()
                .ok_or_else(|| bad("allow must be boolean"))?;
            let always = value
                .get("always")
                .map(|v| v.as_bool().ok_or_else(|| bad("always must be boolean")))
                .transpose()?
                .unwrap_or(false);
            if !control.approve(call_id, allow, always).await {
                return Err((StatusCode::CONFLICT, "no pending approval".into()));
            }
        }
        "cancel" => control.cancel(),
        _ => return Err((StatusCode::NOT_FOUND, "unknown action".into())),
    }
    Ok(json_response(json!({"ok":true,"run_id":id})))
}
pub(super) async fn start(state: State, value: Value) -> ApiResult<Reply> {
    let messages = messages(&value)?;
    let model = value
        .get("model")
        .map(|v| v.as_str().ok_or_else(|| bad("model must be string")))
        .transpose()?
        .unwrap_or("auto")
        .to_owned();
    // No client-supplied approval overrides, paths, or tool definitions.
    if value
        .as_object()
        .unwrap()
        .keys()
        .any(|k| !matches!(k.as_str(), "model" | "messages"))
    {
        return Err(bad("agent request accepts only model and messages"));
    }
    // An agent (possibly awaiting approval) holding the engine is a conflict;
    // warm-up or another request's model load is a wait, reported as loading.
    let mut engine = match state.engine.clone().try_lock_owned() {
        Ok(engine) => engine,
        Err(_) if state.lifecycle.warming() || state.lifecycle.any_loading() => {
            state.engine.clone().lock_owned().await
        }
        Err(_) => return Err((StatusCode::CONFLICT, "engine busy".into())),
    };
    let input = crate::conversation::routing_input(&engine, &messages, &model);
    let mut route = engine.route(&input, None).await.map_err(bad)?;
    let specialist = &engine.registry.specialists[&route.specialist];
    if specialist.task != "chat" || !(specialist.agent || !specialist.tools.is_empty()) {
        return Err(bad("selected specialist is not an agent"));
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = format!(
        "run-{}-{}",
        created(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let control = Arc::new(Control::default());
    state.runs.lock().await.insert(id.clone(), control.clone());
    let (out, incoming) = mpsc::channel::<Bytes>(16);
    let response_id = id.clone();
    let worker = tokio::spawn(async move {
        // Load inside the stream so the client sees `model_loading` events.
        let mut feed = super::loading::Feed::new(state.lifecycle.clone());
        let style = super::loading::Style::Agent { run_id: &id };
        let loaded = {
            let load = engine.load_specialist(&mut route);
            tokio::pin!(load);
            loop {
                tokio::select! {
                    result = &mut load => break result,
                    _ = feed.tick() => for frame in feed.frames(style) {
                        let _ = out.send(frame).await;
                    },
                }
            }
        };
        if let Err(error) = loaded {
            let event = json!({"event":"error","data":{"message":error.to_string()},"run_id":id});
            let _ = out
                .send(Bytes::from(format!("event: error\ndata: {event}\n\n")))
                .await;
            let event = json!({"event":"done","data":{"error":error.to_string()},"run_id":id});
            let _ = out
                .send(Bytes::from(format!("event: done\ndata: {event}\n\n")))
                .await;
            state.runs.lock().await.remove(&id);
            return;
        }
        let (tx, mut rx) = mpsc::unbounded_channel();
        let run = agent::run(
            &mut engine,
            &route,
            messages,
            json!({}),
            tx,
            control.clone(),
        );
        tokio::pin!(run);
        let mut connected = true;
        loop {
            tokio::select! {biased;
                Some(mut event)=rx.recv()=>{
                    event["run_id"]=json!(id);
                    if connected {
                        let frame=Bytes::from(format!("event: {}\ndata: {}\n\n",event["event"].as_str().unwrap_or("error"),event));
                        connected=matches!(tokio::time::timeout(Duration::from_secs(5),out.send(frame)).await,Ok(Ok(())));
                        if !connected {control.cancel();}
                    }
                },
                _=&mut run=>break,
                _=out.closed(),if connected=>{connected=false;control.cancel();},
            }
        }
        while let Ok(mut event) = rx.try_recv() {
            event["run_id"] = json!(id);
            if connected {
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    out.send(Bytes::from(format!(
                        "event: {}\ndata: {}\n\n",
                        event["event"].as_str().unwrap_or("error"),
                        event
                    ))),
                )
                .await;
            }
        }
        state.runs.lock().await.remove(&id);
    });
    {
        let mut streams = state.streams.lock().unwrap();
        streams.retain(|w| !w.is_finished());
        streams.push(worker.abort_handle());
    }
    let body = StreamBody::new(stream::unfold(incoming, |mut incoming| async move {
        incoming
            .recv()
            .await
            .map(|bytes| (Ok::<_, Infallible>(Frame::data(bytes)), incoming))
    }))
    .boxed_unsync();
    Ok(Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("x-lh-run-id", response_id)
        .body(body)
        .unwrap())
}
