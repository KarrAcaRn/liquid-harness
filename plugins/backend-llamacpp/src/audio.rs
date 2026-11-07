//! Audio operations share the backend mutex and model registrations. TTS suspends
//! unpinned servers for the selected source; HTTP reloads them lazily.
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const MAX_AUDIO: usize = 5 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Result<Self, RpcError> {
        use std::os::unix::fs::DirBuilderExt;
        for _ in 0..100 {
            let path = std::env::temp_dir().join(format!(
                "lh-audio-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(failed(e)),
            }
        }
        Err(failed("cannot allocate private audio directory"))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn read_audio(path: &Path) -> Result<Vec<u8>, RpcError> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(failed)?
        .take((MAX_AUDIO + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(failed)?;
    if bytes.len() >= MAX_AUDIO {
        return Err(invalid("audio exceeds 5 MiB limit"));
    }
    Ok(bytes)
}

// Parse RIFF chunks rather than assuming a fixed 44-byte header. llama-tts
// emits PCM WAV; float WAV is accepted too. Truncation and inconsistent rates
// are rejected before publishing metadata or forwarding audio.
fn wav(bytes: &[u8]) -> Result<(u32, u64), RpcError> {
    let bad = || invalid("invalid or unsupported WAV (expected PCM/float RIFF WAVE)");
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad());
    }
    let end = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize + 8;
    if end > bytes.len() || end < 12 {
        return Err(bad());
    }
    let mut pos = 12;
    let mut fmt = None;
    let mut data = 0u64;
    while pos + 8 <= end {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        pos += 8;
        if len > end - pos {
            return Err(bad());
        }
        if id == b"fmt " {
            if len < 16 {
                return Err(bad());
            }
            let u16at = |i| u16::from_le_bytes(bytes[pos + i..pos + i + 2].try_into().unwrap());
            let u32at = |i| u32::from_le_bytes(bytes[pos + i..pos + i + 4].try_into().unwrap());
            let (kind, channels, rate, byte_rate, align, bits) =
                (u16at(0), u16at(2), u32at(4), u32at(8), u16at(12), u16at(14));
            if !matches!(kind, 1 | 3)
                || channels == 0
                || rate == 0
                || !matches!(bits, 8 | 16 | 24 | 32 | 64)
                || (kind == 3 && !matches!(bits, 32 | 64))
                || u32::from(align) != u32::from(channels) * u32::from(bits) / 8
                || u64::from(byte_rate) != u64::from(rate) * u64::from(align)
            {
                return Err(bad());
            }
            fmt = Some((rate, byte_rate, align));
        } else if id == b"data" {
            data += len as u64;
        }
        pos += len + len % 2;
    }
    let (rate, byte_rate, align) = fmt.ok_or_else(bad)?;
    if data == 0 || !data.is_multiple_of(u64::from(align)) {
        return Err(bad());
    }
    Ok((rate, data * 1000 / u64::from(byte_rate)))
}
fn bounded(mut process: Process, timeout: Duration, label: &str) -> Result<(), RpcError> {
    let start = Instant::now();
    loop {
        if let Some(status) = process.0.try_wait().map_err(failed)? {
            return if status.success() {
                Ok(())
            } else {
                Err(failed(format!("{label} exited: {status}")))
            };
        }
        if start.elapsed() >= timeout {
            return Err(failed(format!("{label} timeout")));
        }
        thread::sleep(Duration::from_millis(10));
    }
}
fn normalize(backend: &Backend, reference: &str) -> Result<Vec<u8>, RpcError> {
    let bytes = if reference.starts_with("data:") {
        let (header, data) = reference
            .split_once(',')
            .ok_or_else(|| invalid("invalid audio data URL"))?;
        if !header.starts_with("data:audio/") || !header.ends_with(";base64") {
            return Err(invalid("audio data URL must be base64 encoded"));
        }
        if data.len() > MAX_AUDIO.div_ceil(3) * 4 {
            return Err(invalid("audio exceeds 5 MiB limit"));
        }
        STANDARD.decode(data).map_err(invalid)?
    } else {
        if reference.contains("://") {
            return Err(invalid("audio must be a local path or data URL"));
        }
        read_audio(Path::new(&expand(reference)))?
    };
    if bytes.len() >= MAX_AUDIO {
        return Err(invalid("audio exceeds 5 MiB limit"));
    }
    if bytes.starts_with(b"RIFF") {
        wav(&bytes)?;
        return Ok(bytes);
    }
    let temp = Temp::new()?;
    let input = temp.0.join("input");
    let output = temp.0.join("output.wav");
    fs::write(&input, bytes).map_err(failed)?;
    let args = vec![
        "-nostdin".into(),
        "-v".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        input.to_string_lossy().into_owned(),
        "-vn".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        "16000".into(),
        "-c:a".into(),
        "pcm_s16le".into(),
        "-fs".into(),
        MAX_AUDIO.to_string(),
        output.to_string_lossy().into_owned(),
    ];
    let process = spawn(&backend.ffmpeg, &args, None).map_err(|e| {
        failed(format!(
            "ffmpeg required for non-WAV audio; configure ffmpeg or install it on PATH: {}",
            e.message
        ))
    })?;
    bounded(process, backend.request_timeout, "ffmpeg")?;
    let bytes = read_audio(&output)?;
    wav(&bytes)?;
    Ok(bytes)
}
fn required<'a>(params: &'a Value, key: &str) -> Result<&'a str, RpcError> {
    params[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| invalid(format!("{key} must be a nonempty string")))
}
fn optional<'a>(params: &'a Value, key: &str) -> Result<Option<&'a str>, RpcError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => required(params, key).map(Some),
    }
}
pub(super) fn transcribe(backend: &Backend, base: &str, params: &Value) -> Result<Value, RpcError> {
    required(params, "key")?;
    let language = optional(params, "language")?;
    let audio = normalize(backend, required(params, "audio")?)?;
    let prompt = match language {
        None => "Perform ASR.".to_owned(),
        Some("en" | "English" | "english") => "Perform ASR in English.".to_owned(),
        Some(language) if !language.trim().is_empty() => format!("Perform ASR in {language}."),
        Some(_) => return Err(invalid("language must not be empty")),
    };
    let body = json!({"stream":false,"temperature":0,"messages":[{"role":"system","content":prompt},{"role":"user","content":[{"type":"input_audio","input_audio":{"data":STANDARD.encode(audio),"format":"wav"}}]}]});
    let response: Value = backend
        .agent
        .post(&format!("{base}/v1/chat/completions"))
        .send_json(body)
        .map_err(http_error)?
        .into_json()
        .map_err(failed)?;
    let text = response["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| failed("missing ASR text"))?;
    Ok(json!({"text":text,"language":language}))
}
fn tts_binary(backend: &Backend) -> String {
    backend.tts_executable.clone().unwrap_or_else(|| {
        let server = Path::new(&backend.executable);
        if server.components().count() > 1 {
            server
                .with_file_name("llama-tts")
                .to_string_lossy()
                .into_owned()
        } else {
            "llama-tts".into()
        }
    })
}
pub(super) fn speak(
    backend: &Backend,
    state: &mut State,
    params: &Value,
) -> Result<Value, RpcError> {
    let key = required(params, "key")?;
    let text = required(params, "text")?;
    let voice = optional(params, "voice")?;
    let language = optional(params, "language")?;
    let resident = state
        .models
        .get(key)
        .ok_or_else(|| failed("model not loaded"))?;
    // Pinning guarantees physical residency: never silently stop a pinned server.
    let source = resident.model.source.clone();
    if state
        .models
        .values()
        .any(|r| r.model.source == source && r.pinned)
    {
        return Err(RpcError::new(
            -32002,
            "TTS requires unpinned registrations for this model source",
        ));
    }
    let temp = Temp::new()?;
    let output = temp.0.join("output.wav");
    let mut args = arguments(&resident.model, 0)?;
    args.drain(..4); // server-only --host/--port are not llama-tts flags.
    args.retain(|arg| arg != "--no-mmproj");
    args.extend([
        "-p".into(),
        text.into(),
        "--no-escape".into(),
        "-o".into(),
        output.to_string_lossy().into_owned(),
    ]);
    for (flag, value) in [("--tts-voice", voice), ("--tts-lang", language)] {
        if let Some(value) = value {
            if value.trim().is_empty() {
                return Err(invalid(format!("{flag} must not be empty")));
            }
            args.extend([flag.into(), value.into()]);
        }
    }
    // Destroy/reap before TTS starts, including on the error path. Keep the same
    // key/spec in LRU/TTL; next ASR/chat reloads, repeated TTS avoids server loads.
    for r in state
        .models
        .values_mut()
        .filter(|r| r.model.source == source)
    {
        drop(r.process.take());
    }
    let result = (|| {
        let process =
            spawn(&tts_binary(backend), &args, backend.models_dir.as_deref()).map_err(|e| {
                failed(format!(
                    "llama-tts unavailable; configure llama_tts or install beside llama_server: {}",
                    e.message
                ))
            })?;
        bounded(process, backend.request_timeout, "llama-tts")?;
        let bytes = read_audio(&output)?;
        let (sample_rate, duration_ms) = wav(&bytes)?;
        Ok(
            json!({"audio":format!("data:audio/wav;base64,{}", STANDARD.encode(bytes)),"sample_rate":sample_rate,"duration_ms":duration_ms}),
        )
    })();
    state.models.get_mut(key).unwrap().used = Instant::now();
    result
}

#[cfg(test)]
mod tests;
