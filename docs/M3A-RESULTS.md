# M3a audio acceptance results

Measured 2026-11-07 on the Linux development host, Intel Core Ultra 7 155H,
llama.cpp b12189, CPU only, eight model threads. LFM3-Audio-2B Q4_K_M with
Q8_0 audio projector, context 4096. Each CLI operation ran sequentially;
no competing llama-server/llama-tts job was present. Timings include cold
model loading and process/CLI overhead, not warm inference throughput.
No compilation ran during the measurements. Load includes residual activity.

Model card verified before implementation:
https://huggingface.co/LiquidAI/LFM3-Audio-2B-GGUF
Model revision reported by HF API: `02355c1a141e464a064f2ab2c276638dbcb7f38a`.
Downloaded Q4_K_M model and Q8_0 projector into the shared LLAMA_CACHE separately;
the smoke itself uses file sources and never downloads.

## Real smoke

Command: `scripts/smoke.sh --audio --sample target/m3a-fixtures/jfk.wav --out target/m3a-final`.
Raw result: [measurements/m3a-audio.json](measurements/m3a-audio.json).

| Operation | Wall seconds | Load 1/5/15 min before → after | WAV duration |
|---|---:|---|---:|
| English TTS | 13.174 | 2.93/2.27/2.09 → 4.00/2.54/2.18 | 3200 ms |
| English ASR | 5.232 | 4.00/2.54/2.18 → 3.75/2.51/2.17 | input above |
| German TTS | 13.941 | 3.75/2.51/2.17 → 3.77/2.57/2.20 | 3040 ms |
| German ASR | 5.568 | 3.77/2.57/2.20 → 4.11/2.66/2.23 | input above |
| Independent JFK ASR | 10.505 | 4.11/2.66/2.23 → 4.94/2.89/2.31 | original sample |

Both generated WAVs are 24 kHz. Files from this run:

- `/opt/data/home/projects/liquid-harness/.worktrees/t_1c65ad95/target/m3a-final/en.wav`
- `/opt/data/home/projects/liquid-harness/.worktrees/t_1c65ad95/target/m3a-final/de.wav`

English input and transcript: `Hello world. This is a short test.`

German input and transcript: `Guten Morgen. Das ist ein kurzer Test.`

Independent sample downloaded from
https://raw.githubusercontent.com/ggml-org/whisper.cpp/master/samples/jfk.wav
before the offline smoke. Transcript:

> And so, my fellow Americans, ask not what your country can do for you. Ask what you can do for your country.

Word edit distance after lowercasing and removing punctuation: English **0/7**,
German **0/7**, JFK **0/22** (WER 0 for each). These three short examples are not
a general ASR benchmark or perceptual TTS assessment. TTS sampling is stochastic;
repeated synthesis can have a different waveform/duration.

All operations returned exit 0. Servers and TTS processes were stopped after
runs; final process checks were empty. An initial run also transcribed both
languages correctly, but its post-ASR process check caught a transient defunct
server. The check now waits up to two seconds for reaping, still refuses any
remaining server, and the complete repeated run above passed.

## Verification

- 103 offline workspace tests passed; fmt check, all-target clippy with
  `-D warnings`, workspace tests and release build passed.
- Release `lh`: 2,347,288 bytes.
- Tests cover wire round trips, method/capability guards, mock transcription and
  WAV generation, output collision refusal, media-aware routing, explicit audio
  chat, question chaining, model pull with required media, fake ASR HTTP,
  conversion failures, WAV validation, TTS arguments/metadata, timeouts, cleanup,
  suspended registration TTL/LRU/pinning, and lazy server restart.
- No HTTP/inference dependency added to core. Existing `base64` crate now also
  serves the llama.cpp and mock backends; Cargo.lock gains no new package.

## Decisions and known gaps

- `--audio` transcribes with the shipped pack. Use `--audio-question` for an
  explicit transcript → default text-specialist pipeline. Custom audio chat
  specialists remain routable. The pipeline and arbitrary format conversion
  are covered offline; the real smoke focuses on WAV ASR/TTS.
- `--speak-out` forces speech selection; otherwise speech phrase hints use the
  existing cascade. The full prompt is spoken verbatim, not rewritten to strip
  prefixes such as “read this aloud”. Existing output files are never overwritten.
- TTS uses documented `llama-tts`, not `/v1/realtime`. No live inference process
  is shared between ASR and TTS. Same-source unpinned servers are suspended
  before TTS and lazily reloaded for subsequent ASR; initial TTS still loads a
  server first. Registrations count toward existing LRU/TTL capacity.
- Audio is limited below 5 MiB, no streaming audio output, no interruption of
  active serial SDK calls. Requested language is a hint, not detected metadata.
- Non-WAV formats require ffmpeg on PATH or configured explicitly. No host path
  is hardcoded in the backend. Direct audio chat forwards the format to its
  backend rather than promising conversion for every third-party chat backend.
- REPL, HTTP serving and realtime voice remain subsequent milestones.
