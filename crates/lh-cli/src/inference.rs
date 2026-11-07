use base64::{Engine as _, engine::general_purpose::STANDARD};
use lh_core::Engine;
use lh_protocol::{
    RouteInput,
    json::{Value, json},
};
use std::{
    io::{self, Read, Write},
    path::Path,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn image_url(reference: &str) -> Result<String> {
    if reference.starts_with("data:image/")
        || reference.starts_with("https://")
        || reference.starts_with("http://")
    {
        return Ok(reference.into());
    }
    let path = Path::new(reference.strip_prefix("file:").unwrap_or(reference));
    let mime = match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => return Err(format!("unsupported image extension: {}", path.display()).into()),
    };
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
}

pub async fn pull(engine: &mut Engine, id: &str) -> Result<Value> {
    if engine.registry.specialists.contains_key(id) {
        let decision = engine.prepare(id).await?;
        return Ok(json!({"key":id,"ready":true,"backend":decision.backend}));
    }
    let (backend, key, model, pinned) = if let Some(r) = engine.registry.routers.get(id) {
        (
            r.backend.clone().ok_or("router has no backend")?,
            format!("router:{}", r.id),
            json!(r.model.as_ref().ok_or("router has no model")?),
            true,
        )
    } else {
        return Err(format!("unknown specialist or router '{id}'").into());
    };
    let host = engine.require(&backend, "decide", id).await?;
    let result = host
        .call(
            "backend/load",
            json!({"key":key,"model":model,"pinned":pinned}),
        )
        .await?;
    if result["ready"] != true {
        return Err("backend/load did not report ready".into());
    }
    Ok(result)
}

pub(crate) fn audio_url(reference: &str) -> Result<String> {
    const MAX_AUDIO: usize = 5 * 1024 * 1024;
    if reference.starts_with("data:audio/") {
        let (header, data) = reference.split_once(',').ok_or("invalid audio data URL")?;
        if data.len() > MAX_AUDIO.div_ceil(3) * 4 {
            return Err("audio exceeds 5 MiB limit".into());
        }
        if !header.ends_with(";base64") || STANDARD.decode(data)?.is_empty() {
            return Err("audio must be nonempty base64".into());
        }
        return Ok(reference.into());
    }
    let path = Path::new(reference.strip_prefix("file:").unwrap_or(reference));
    let format = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // The backend probes bytes and converts non-WAV formats with ffmpeg.
    let format = if format.is_empty() {
        "octet-stream"
    } else {
        &format
    };
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_AUDIO as u64).read_to_end(&mut bytes))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() >= MAX_AUDIO {
        return Err("audio exceeds 5 MiB limit".into());
    }
    if bytes.is_empty() {
        return Err("empty audio file".into());
    }
    Ok(format!(
        "data:audio/{format};base64,{}",
        STANDARD.encode(bytes)
    ))
}

pub(crate) fn save_audio(reference: &str, target: Option<&Path>) -> Result<std::path::PathBuf> {
    let bytes = if let Some(data) = reference.strip_prefix("data:audio/wav;base64,") {
        STANDARD.decode(data)?
    } else if reference.starts_with("data:") {
        return Err("speech output must be a WAV data URL or local path".into());
    } else {
        std::fs::read(reference.strip_prefix("file:").unwrap_or(reference))?
    };
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("speech output is not a WAV file".into());
    }
    let path = target.map(Path::to_path_buf).unwrap_or_else(|| {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::path::PathBuf::from(format!(
            "lh-speech-{}-{time}-{}.wav",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    });
    // Never silently overwrite an existing recording.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(&bytes)?;
    Ok(path)
}

#[derive(Default)]
pub struct AudioOptions {
    pub audio: Option<String>,
    pub audio_question: bool,
    pub speak_out: Option<std::path::PathBuf>,
    pub language: Option<String>,
    pub voice: Option<String>,
}

pub(crate) fn task_specialist(engine: &Engine, task: &str) -> Result<String> {
    let candidates: Vec<_> = engine
        .registry
        .specialists
        .values()
        .filter(|s| s.task == task)
        .collect();
    match candidates.as_slice() {
        [one] => Ok(one.id.clone()),
        [] => Err(format!("no specialist for task '{task}'").into()),
        _ => {
            Err(format!("multiple specialists for task '{task}'; select --model explicitly").into())
        }
    }
}

pub async fn run(
    engine: &mut Engine,
    mut model: String,
    images: Vec<String>,
    router: Option<&str>,
    mut prompt: String,
    options: AudioOptions,
) -> Result<()> {
    if prompt.trim().is_empty() && (options.audio.is_none() || options.audio_question) {
        return Err("a nonempty prompt is required without audio or with --audio-question".into());
    }
    let mut audio = options.audio.as_deref().map(audio_url).transpose()?;
    if model == "auto" {
        if options.speak_out.is_some() {
            model = task_specialist(engine, "speak")?;
        } else if options.audio_question {
            model = task_specialist(engine, "transcribe")?;
        }
    }
    let images = images
        .iter()
        .map(|s| image_url(s))
        .collect::<Result<Vec<_>>>()?;
    let mut decision = engine
        .route(
            &RouteInput {
                prompt: prompt.clone(),
                images: images.clone(),
                audio: audio.iter().cloned().collect(),
                specialist: Some(model),
            },
            router,
        )
        .await?;
    let mut specialist = engine
        .registry
        .specialists
        .get(&decision.specialist)
        .ok_or("router selected an unknown specialist")?
        .clone();
    if options.speak_out.is_some() && specialist.task != "speak" {
        return Err("--speak-out requires a speak specialist".into());
    }
    if options.audio_question && specialist.task != "transcribe" {
        return Err("--audio-question requires a transcribe specialist".into());
    }
    if options.voice.is_some() && specialist.task != "speak" {
        return Err("--voice requires a speak specialist".into());
    }
    let mut host = engine.load_specialist(&mut decision).await?;
    eprintln!("[route] {}", lh_protocol::json::to_string(&decision)?);
    if specialist.task == "transcribe" {
        let source = audio.as_ref().ok_or("transcribe requires --audio")?;
        let result = engine
            .dispatch(
                &decision,
                json!({"audio":source,"language":options.language}),
            )
            .await?;
        let response: lh_protocol::TranscribeResponse = lh_protocol::json::from_value(result)?;
        if !options.audio_question {
            println!("{}", response.text);
            return Ok(());
        }
        prompt = format!(
            "Audio transcript:\n{}\n\nQuestion:\n{}",
            response.text, prompt
        );
        let id = engine.config.default_specialist.clone();
        decision = engine
            .route(
                &RouteInput {
                    prompt: prompt.clone(),
                    specialist: Some(id),
                    ..Default::default()
                },
                None,
            )
            .await?;
        specialist = engine.registry.specialists[&decision.specialist].clone();
        if specialist.task != "chat" {
            return Err("default text specialist must use chat".into());
        }
        host = engine.load_specialist(&mut decision).await?;
        eprintln!("[route] {}", lh_protocol::json::to_string(&decision)?);
        audio = None;
    }
    if specialist.task == "speak" {
        let response: lh_protocol::SpeakResponse = lh_protocol::json::from_value(
            engine
                .dispatch(
                    &decision,
                    json!({"text":prompt,"language":options.language,"voice":options.voice}),
                )
                .await?,
        )?;
        let path = save_audio(&response.audio, options.speak_out.as_deref())?;
        println!("{}", path.display());
        return Ok(());
    }
    if specialist.task != "chat" {
        return Err(
            "lh run expects chat, transcribe or speak; use core dispatch for typed decide requests"
                .into(),
        );
    }
    let content = if images.is_empty() && audio.is_none() {
        json!(prompt)
    } else {
        let mut parts = vec![json!({"type":"text","text":prompt})];
        parts.extend(
            images
                .into_iter()
                .map(|url| json!({"type":"image_url","image_url":{"url":url}})),
        );
        if let Some(url) = audio {
            let (header, data) = url.split_once(',').ok_or("invalid audio data URL")?;
            let format = header
                .strip_prefix("data:audio/")
                .and_then(|s| s.strip_suffix(";base64"))
                .ok_or("invalid audio data URL")?;
            parts.push(json!({"type":"input_audio","input_audio":{"data":data,"format":format}}));
        }
        json!(parts)
    };
    if specialist.agent || !specialist.tools.is_empty() {
        return crate::agent_ui::run(
            engine,
            &decision,
            vec![json!({"role":"user","content":content})],
        )
        .await;
    }
    // This one-shot CLI has exactly one active call on this host. Subscribe after
    // loading so only this chat's stream notifications are consumed.
    let mut notifications = host.subscribe();
    let call = engine.dispatch(&decision, json!({"messages":[{"role":"user","content":content}],"params":specialist.params,"stream":true}));
    tokio::pin!(call);
    let mut stdout = io::stdout().lock();
    let mut streamed = false;
    let response = loop {
        tokio::select! {
            // Drain queued chunks before accepting the final response.
            biased;
            notification = notifications.recv() => {
                let notification = notification.map_err(|e| format!("chat notification stream lost: {e}"))?;
                if notification.method == "$/stream" {
                    if let Some(text) = notification.params["delta"]["content"].as_str() {
                        stdout.write_all(text.as_bytes())?;
                        stdout.flush()?;
                        streamed |= !text.is_empty();
                    }
                    if let Some(tools) = notification.params["delta"].get("tool_calls") {
                        eprintln!("tool_calls: {tools}");
                    }
                }
            }
            result = &mut call => break result?,
        }
    };
    if !streamed && let Some(text) = response["message"]["content"].as_str() {
        stdout.write_all(text.as_bytes())?;
    }
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_helpers_accept_paths_validate_data_and_preserve_output_bytes() {
        let dir = std::env::temp_dir().join(format!("lh-audio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("input.WAV");
        let mut wav = vec![0; 44];
        wav[..4].copy_from_slice(b"RIFF");
        wav[8..12].copy_from_slice(b"WAVE");
        std::fs::write(&source, &wav).unwrap();
        let data = audio_url(source.to_str().unwrap()).unwrap();
        assert_eq!(
            data,
            format!("data:audio/wav;base64,{}", STANDARD.encode(&wav))
        );
        assert_eq!(audio_url(&data).unwrap(), data);
        let out = dir.join("data.wav");
        save_audio(&data, Some(&out)).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), wav);
        assert!(save_audio(&data, Some(&out)).is_err());
        let local = dir.join("local.wav");
        save_audio(source.to_str().unwrap(), Some(&local)).unwrap();
        assert_eq!(std::fs::read(local).unwrap(), wav);
        assert!(save_audio("data:audio/wav;base64,AA==", Some(&dir.join("invalid.wav"))).is_err());
        for bad in [
            "data:audio/wav;base64,",
            "data:audio/wav;base64,?",
            "data:audio/wav,AA==",
            "missing.txt",
        ] {
            assert!(audio_url(bad).is_err());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn encodes_local_images_as_base64_data_urls() {
        let path = std::env::temp_dir().join(format!("lh-image-{}.PNG", std::process::id()));
        std::fs::write(&path, [0, 1, 2, 253, 254, 255]).unwrap();
        let result = image_url(path.to_str().unwrap());
        std::fs::remove_file(path).unwrap();
        assert_eq!(result.unwrap(), "data:image/png;base64,AAEC/f7/");
    }
    #[test]
    fn preserves_remote_and_data_urls() {
        for url in [
            "https://example.test/image.png",
            "data:image/png;base64,AA==",
        ] {
            assert_eq!(image_url(url).unwrap(), url);
        }
        assert!(image_url("bad.txt").is_err());
        assert!(image_url("missing.png").is_err());
    }
}
