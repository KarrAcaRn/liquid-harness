# Audio backend operations

`backend/transcribe` accepts `{key, audio, language?}`. `audio` is a local path
(including `~/`) or base64 `data:audio/...` URL, not a remote URL. Valid PCM or
float RIFF WAV passes through; other formats run configured `ffmpeg` (default:
PATH lookup) to produce mono 16 kHz PCM16 WAV. Missing ffmpeg is an explicit
error. Input and returned WAV are limited to 5 MiB to fit the 8 MiB RPC frame.
Temporary files live in private directories under the platform temp directory
(`TMPDIR` on Unix) and are removed on normal success/error/timeout.

ASR posts nonstreaming OpenAI `/v1/chat/completions`, temperature zero, a system
message `Perform ASR.` (or `Perform ASR in English.` for `en`), and a user
`input_audio` part containing bare base64 and `format: "wav"`. Other language
hints are interpolated into `Perform ASR in <language>.` without claiming that
the model supports them. The returned language is the requested hint, not
automatic language detection.

`backend/speak` accepts `{key, text, voice?, language?}` and returns
`{audio: "data:audio/wav;base64,...", sample_rate, duration_ms}`. Metadata comes
from validated WAV chunks, not guesses. It runs `llama-tts` with the model's
`-m`/`-hf`, projector, context, batch and thread arguments, followed by
`-p TEXT --no-escape -o PRIVATE_OUTPUT`, and optional `--tts-voice` and
`--tts-lang`. These flags were verified against installed llama.cpp b12189
`llama-tts --help`; real bilingual smoke results are in `docs/M3A-RESULTS.md`.
HF `mmproj = "auto"` delegates projector discovery to llama.cpp.

Configure `llama_tts` explicitly, or it is inferred beside a path-valued
`llama_server`; for a bare server command it is `llama-tts` on PATH. Configure
`ffmpeg` explicitly or use PATH. No installation-specific path is compiled in.
Both subprocess operations use `request_timeout_secs` (default 600), process
groups, Linux parent-death protection, kill/reap on timeout and inherited
stderr. ASR HTTP has the same configured request timeout. Normal SDK
cancellation still cannot interrupt a serial handler.

## Residency tradeoff

Audio uses existing `backend/load` registrations and ModelSpec keys. Initial
load still starts llama-server, even for TTS-only use. TTS stops/reaps all
servers registered with the **same exact model source** before starting its
one-shot process; no ASR and TTS inference can overlap in this backend because
the existing state mutex is held throughout. Other model sources may remain
resident. If any registration of that source is pinned, TTS returns `-32002`
rather than violating physical pinning.

Suspended registrations retain their specs and count toward LRU capacity,
remain subject to idle TTL, and report null PID/port. Repeated loads are
idempotent; repeated TTS does not reload a server. The next ASR/chat/decide
starts and health-checks a server lazily. This saves simultaneous duplicate
weights but switching TTS to ASR has cold-start latency. Different source
spellings that resolve to the same weights are not deduplicated. Process-group
cleanup covers ordinary errors/timeouts; abrupt plugin SIGKILL may leave
temporary files, and daemonizing wrapper descendants have the existing
parent-death limitations.

Dependency: `base64` is a lightweight codec for audio RPC/HTTP payloads and was
already transitive through ureq; no core dependency was added.