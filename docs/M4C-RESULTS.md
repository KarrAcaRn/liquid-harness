# M4c verification: embedded browser UI

## Scope and release size

Same-origin HTML/CSS/ES modules embedded in `lh`, default-on `web` feature
(implies `server`), no added dependency or build step. Chat, Plugins including
Specialists/Router, Tools, Skills and Status reuse M4b management endpoints.
No new management endpoint was necessary. Full `lh` routing metadata was added
to ordinary chat JSON/SSE; agent runs already exposed it.

On this Linux host, stable Rust 1.99.1, the workspace release profile:

| Build | `target/release/lh` bytes |
|---|---:|
| Before M4c, parent 7532e2a, default features | 3,687,432 |
| M4c, default features | 3,731,008 |
| Increase | 43,576 |

Both measured using `cargo build --offline --release -p lh-cli` and `stat`.
The increase is below 300 KB. This measures the executable, not plugin/model
sizes. No latency, throughput, or idle-memory claim is made.

## Offline checks

- `cargo fmt --all --check`
- `cargo clippy --offline --workspace --all-targets -- -D warnings`
- `cargo test --offline --workspace`
- `cargo clippy --offline -p lh-cli --no-default-features --all-targets -- -D warnings`
- `cargo test --offline -p lh-cli --no-default-features --features server --test server`
- `cargo build --offline --workspace`
- `node scripts/test-web.mjs`

Rust HTTP tests cover all embedded asset paths, UTF-8 content types, CSP,
nosniff, public shell/authenticated APIs, wrong credentials, HEAD, disallowed
methods, no wildcard CORS, unknown paths, and absence of assets without `web`.
The Rust suite runs Node checks when installed (explicit skip otherwise). Node
checks module parsing, Markdown injection escaping/code blocks, SSE split at
all byte chunk sizes including Unicode and CR/LF boundaries, and request caps.

## Browser observations

Actual Chromium DOM checks against `scripts/web-smoke-server.py`, no models:

- All five pages loaded. Plugins showed IDs, versions, manifest capabilities,
  paths/process state/restarts; Specialists and Router sections loaded. Tools
  listed filesystem schemas/destructive flags and built-in skills. Read coding
  rendered its Markdown. Status showed resident mock model/process and routes.
- Initial parallel management reads produced spurious 409s from their shared
  nonblocking lock. Reads were serialized; repeat Plugins/Status checks showed
  all sections and no errors. Genuine busy errors still show Refresh guidance.
- Chat streamed the mock echo and displayed the full route with actual backend.
  Fenced code produced copy buttons. Literal `<img ... onerror=...>` remained
  text; there were no injected image elements. Saved conversation data survived
  reload and new conversation selection.
- Allow and Always produced tool-result success and two-step completion. Deny
  showed `approval denied or timed out` in the retained tool result. Cancel
  showed a cancelled partial-response warning and restored the composer.
- Speak replies produced a blob-backed WAV player; autoplay was blocked and
  the UI asked the user to press Play. Explicit mock-speak also produced an
  inline generated-speech player.
- Real MediaRecorder was exercised using a synthetic oscillator MediaStream
  substituted for microphone acquisition. A trusted click resumed AudioContext,
  recorded `recording.webm`, attached it and sent an audio-bearing mock turn.
  An earlier suspended AudioContext produced an empty recording and a visible
  error, not a false successful attachment. This is NOT a hardware microphone
  permission or acoustic-quality test; the mock does not test ffmpeg conversion.
- Review found stale cross-tab writes and attachment leakage across conversation
  changes. Saving now detects a changed storage snapshot and pauses with an
  export/reload warning; a browser probe confirmed the other snapshot remained
  untouched. Switching while a file was loading was rejected; New conversation
  after loading cleared pending media. Recording transitions use the same guard.
- Ctrl+Enter (later addition), via trusted CDP key events against the mock
  fixture: empty and whitespace-only prompts did not send (empty showed
  `valueMissing`); plain Enter inserted a newline; Ctrl+Enter sent
  `"line one\nline two"` without an extra newline and cleared the field; one
  press plus three auto-repeat keydowns produced exactly one message; Ctrl+Enter
  with text present but focus on the model select, Agent checkbox or page body
  sent nothing; the Send button and Enter on a focused Send button still sent.

## Real model smoke through the UI

One sequential run, CPU eight threads, llama.cpp b12189, cached GGUF only:
LFM3-1.7B Q4_K_M and LFM3-VL-2B Q4_K_M with a cached F16 projector, context 8192.
No actual llama-server existed at startup. Another worker was notified to avoid
concurrent inference. Load before: **1.78 / 6.42 / 6.50**; after:
**5.67 / 6.37 / 6.46**. This was functional verification, not a benchmark.

1. Text: “Say hello in one short sentence.” → “Hello! How can I help you today?”
   Route `web-text`, backend `llamacpp`, explicit, confidence 1.
2. Vision: browser-generated 96×96 red PNG attached through the file input;
   preview showed one image. “What color is this image? Answer briefly.” →
   “The image is predominantly red.” Route `web-vision`, single-candidate.
3. Agent: requested `browser-proof.txt` containing exactly `browser verified`.
   The UI showed the actual `write_file` arguments for approval. Allow executed
   the write and returned `Wrote 18 bytes`, followed by a two-step completion.
   Independent fixture shutdown read-back printed **`browser-verified.`** plus
   newline. The model changed the requested string and falsely claimed exactness.
   This verifies approval/tool execution/UI, NOT exact instruction compliance.

The fixture shut down normally; `pgrep -x llama-server` returned no process.
The temporary filesystem outputs were read before cleanup. No servers were left
running for this real check.

## Limits

- Small Markdown subset, not CommonMark; no remote Markdown images or HTML.
- Browser-local unencrypted history; no server sync, import, transactional
  cross-tab editing, automatic context trimming or storage quota expansion.
  Conflicting tabs pause saving and require export/reload; use one editing tab.
- Management is a manually refreshed snapshot, not live polling. Cascade switches
  are read-only. Responses can legitimately show 409 during inference.
- Existing agent semantics remain: load precedes cancellable run ID, active
  inference drains, approval expires, and already executed effects cannot be
  undone. Tool annotations and installed skills/plugins remain trusted inputs.
- Browser microphone permission/hardware, actual audible playback and real-model
  audio conversion/synthesis were not checked here. Existing backend audio tests
  cover conversion; browser testing here used synthetic input/mock speech.
- HTTP chat retains attachment history, so audio may be retranscribed on later
  turns. Large histories may hit browser quota, request cap or model context.
- No multi-browser, mobile-device or performance soak. Public immutable shell
  is intentional; API authentication is unchanged. Keep localhost or use a
  properly secured HTTPS reverse proxy; there is no multi-user isolation.

Start instructions and feature variants are in the README. For a repeatable
manual offline fixture run `python3 scripts/web-smoke-server.py`, select
`web-agent` and Agent mode for approvals. `--real --port 8486` selects host-local
cached model paths and refuses an already running llama-server. Stop with Ctrl-C.
