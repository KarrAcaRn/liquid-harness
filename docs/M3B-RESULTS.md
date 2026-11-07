# M3b acceptance — 2026-11-07

## Implementation and offline verification

`lh chat` is a plain line REPL using M2c sessions. Text/image/audio turns share
role-preserving history, with transcript adaptation and optional spoken replies.
`lh serve` exposes the documented OpenAI subset, true SSE, multipart ASR, WAV TTS,
routing/decision extensions and fail-closed optional bearer authentication.

Verified on this Linux host:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets --offline -- -D warnings`
- `cargo test --workspace --offline`: 112 passing tests, no models/network.
- `cargo test -p lh-cli --no-default-features --offline`: passing.
- Release builds with and without the default `server` feature.

New coverage includes command parsing, streamed REPL output, text sticky routing,
vision switching/history, explicit text switching and reset, audio→chat and
`/speak`, HTTP endpoints and route headers, data-URL validation, multipart, auth,
missing-token startup failure, concurrent SSE isolation, final-delta draining,
speech deltas, router decide passthrough and Ctrl-C shutdown.

## Binary size and HTTP choice

All sizes are stripped Linux release file bytes, Rust 1.99.1, `opt-level=s`,
`lto=thin`. No inference runtime is linked into `lh`.

| Build | Bytes |
|---|---:|
| M3a baseline `ad9f4f9`, workspace release | 2,347,288 |
| M3b Hyper, workspace release | 3,260,808 |
| M3b Hyper, CLI-only release | 3,261,080 |
| M3b CLI-only, no default features | 2,463,408 |
| Same-handler Axum 0.8.9 adapter probe | 3,343,624 |

Selected direct HTTP/1 Hyper: it stays below the 5 MB target and avoids the
additional Axum/Tower router/body adapter. The probe wrapped the same handlers in
an Axum fallback Router, kept the existing Hyper connection/shutdown loop, and
built successfully with HTTP/1/Tokio only (no Axum defaults). This is a bounded
adapter comparison, not proof that every possible Axum implementation is larger.
`python3 scripts/measure_server_size.py` reproduces the throwaway comparison
under `target/axum-size-probe`; it may download probe-only dependencies. Small
workspace-vs-single-package differences come from Cargo feature unification.

Production additions are confined to CLI: Hyper/Hyper-util for HTTP/1 + Tokio IO,
http-body-util/bytes for bounded bodies and frames, futures-util for body streams,
Multer for tested multipart parsing. Tokio net/signal enable listener/shutdown.
Core gains only an optional configuration field, no HTTP dependency.

## Real-model curl smoke

Run: `LH_BIN="$PWD/target/release/lh" python3 scripts/smoke_chat.py`.
Cached Q4 models, VL F16/audio Q8_0 projectors, llama.cpp b12189, at most eight
threads. The script used isolated file-source manifests: no downloads, no
concurrent inference run, every request completed before the next. Owned server
processes were stopped by PID via Ctrl-C/shutdown; final `pgrep -ax llama-server`
and `pgrep -ax llama-tts` returned no processes.

These are functional smoke wall times, not a throughput benchmark. Load includes
residual build/prior-smoke load; no competing llama inference was observed.
Raw curl commands, response bodies/headers, route traces and load snapshots:
[measurements/m3b-chat.json](measurements/m3b-chat.json).

| Request | Input route / reason | Wall seconds | Load before → after (1/5/15 min) |
|---|---|---:|---|
| Chat SSE: “Explain a Rust mutex in one short sentence.” | lfm3-text / decision-model, p=0.99565 | 13.065 | 7.83/7.77/6.39 → 7.81/7.76/6.41 |
| Chat image: describe generated red square | lfm3-vl / single-candidate | 8.260 | 7.81/7.76/6.41 → 7.56/7.70/6.40 |
| Speech: “Hello world. This is a short test.” | lfm3-speak / explicit-load | 17.515 | 7.56/7.70/6.40 → 7.56/7.71/6.43 |
| Multipart transcription of that WAV | lfm3-audio / explicit-load | 6.372 | 7.56/7.71/6.43 → 7.67/7.73/6.44 |
| Audio chat: repeat recording in one sentence | lfm3-audio / single-candidate, then text chat | 9.150 | 7.67/7.73/6.44 → 7.36/7.66/6.43 |

Every HTTP status was 200. SSE emitted real content chunks and `[DONE]`.
Image answer: “The colored shape is a square.” TTS returned a 24 kHz, 2,880 ms
WAV. ASR returned exactly “Hello world. This is a short test.” Audio chat returned
`The speaker says: "Hello world this is a short test."` This is a short self-TTS
round trip, not independent ASR quality evidence (see M3a for independent audio).

## Conversation transcript and routing

Required three-turn script (34.025 s total; load 7.36/7.66/6.43 → 7.87/7.75/6.51):

```text
> Explain a Rust mutex in one short sentence.
A Rust `Mutex` is a thread-safe lock that allows only one owner to access mutable
 data at a time, preventing race conditions and ensuring synchronized concurrent access.
route: lfm3-text, decision-model, p=0.99568, model_called=true, router_ms=5483
> /image square.png
> Describe the colored shape in one sentence.
The shape is a red square with equal width and height, centered on a light background.
route: lfm3-vl, single-candidate, p=1, model_called=false, router_ms=0
> What color was it? Answer briefly.
Red.
route: lfm3-vl, single-candidate, p=1, model_called=false, router_ms=0
```

The picture persists in history, so the third turn correctly remains vision.
M2c single-candidate selection precedes sticky; it would be inaccurate to label
that route `sticky`. A separate real two-text-turn script verified the actual
sticky stage (24.131 s total; load 3.52/6.72/6.66 → 5.11/6.80/6.69): first mutex
question chose lfm3-text via d1, “Give one short example.” then chose lfm3-text
with `reason=sticky`, `model_called=false`, `router_ms=0`. The raw response
contained a Rust example; its semantic/code correctness was not evaluated.

## Known gaps and observations

- An initial smoke used “Say hello in one short sentence.” d1 selected speech
  (p≈0.74) rather than text. This is an ambiguous text/speech routing limitation,
  not hidden as a successful text run. No routing hints or held-out labels were
  retuned here; final text smoke uses the unambiguous mutex question. Explicit
  `--model lfm3-text` / HTTP `model: lfm3-text` bypasses this ambiguity.
- That run also exposed dropped direct-speech SSE output; fixed by emitting the
  audio message delta, with offline regression coverage. Portable audio clients
  should use `/v1/audio/speech`; direct chat speech uses a documented extension.
- HTTP has no persistent conversation sessions; it serializes inference and
  cannot interrupt an SDK handler on client disconnect. No TLS, rate limit,
  context summarization, realtime audio, tool execution or full OpenAI surface.
- ASR pipeline headers identify the input specialist, and usage counts the final
  chat only. HTTP callers resending audio pay ASR again. Combined image+audio
  adaptation is implemented; this real smoke exercises modalities separately.
- Hermes/Open WebUI configuration examples are documented, not end-to-end client
  integration claims. No Hermes configuration was modified.
