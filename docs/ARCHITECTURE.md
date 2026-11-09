# liquid-harness – Architecture

Status: M2 through M4c implemented (llama.cpp and opt-in FreeToken backends,
capability-aware fallback, routing cascade, sessions, evaluation, audio ASR/TTS,
conversation REPL, OpenAI-compatible HTTP subset, bounded agent runs, MCP host,
approvals, skills, management APIs and embedded browser UI).
Later milestone sections remain the design contract.
Change it in the same commit when a decision changes.

## 1. Goals

- **Local-first harness** for small, specialised models. Instead of one big
  general model, every request is routed to the specialist that fits it.
- **Lightweight core.** The `lh` binary only hosts plugins, routes and
  orchestrates. It does not do inference, does not link llama.cpp, and has no
  HTTP client. Target: release binary < 5 MB, idle RSS < 15 MB, cold start < 50 ms.
- **Everything is a plugin.** Backends, specialists (models), routers and tools
  can be added without recompiling the core.
- **First model family: Liquid AI LFM3** (released 2026-10-27), all GGUF,
  served by llama.cpp ≥ b11960 (tested with b12189):

  | Model              | Role in harness                              | Input            | Output      |
  |--------------------|----------------------------------------------|------------------|-------------|
  | `d1-2B`            | **router** (decision model, `/v1/systemone`)  | text, images     | typed answers |
  | `d1-Omni-2B`       | router variant that also hears audio         | text, img, audio | typed answers |
  | `LFM3-1.7B`        | text agent: chat, tool use, coding, 256K ctx | text             | text, tool calls |
  | `LFM3-VL-2B`       | vision: describe, read, ground screenshots   | text, images     | text        |
  | `LFM3-Audio-2B`    | speech in/out (ASR, TTS, voice chat)         | text, audio      | text, audio |
  | `LFM3-Omni-2B`     | audio+vision combined, speech → tool calls   | text, img, audio | text        |

## 2. Process model

```
            ┌───────────── lh (core) ──────────────┐
 CLI / REPL │  config ─ registry ─ router engine   │
 HTTP API ──┤             │             │          │
            │       plugin host (JSON-RPC/stdio)   │
            └──────┬──────────┬──────────┬─────────┘
                   │          │          │
       lh-backend-llamacpp  router plugin  tool plugins (MCP)
          │      │            (optional)
   llama-server llama-server …   (one per resident model, localhost HTTP)
```

- Plugins are **separate executables** speaking **JSON-RPC 2.0, one JSON object
  per line, over stdin/stdout**. `stderr` is free-form log output that the core
  forwards to its own log with a `[plugin-name]` prefix.
- A crashing plugin never takes the core down. The host marks it failed, returns
  errors for in-flight calls, and restarts it on next use (max 3 restarts / 60 s).
- Plugins are started **lazily** on first use and stopped on core shutdown.
- Specialists are **declarative** (TOML only, no process). A specialist names a
  backend; the backend plugin does the work.

## 3. Plugin layout and manifest

A plugin is a directory with a `plugin.toml`. Search path, in order:
`$LH_PLUGIN_PATH` (platform-separated), configured `plugin_path` entries,
`<config dir>/plugins`, `./plugins`. Entries may name a plugin directory or its
parent pack directory. Repeated canonical manifest paths are read once; distinct
files declaring the same ID are a startup error (there is no first-ID-wins override).

```toml
# plugins/backend-llamacpp/plugin.toml
[plugin]
id = "llamacpp"
version = "0.1.0"
protocol = 1
description = "Runs GGUF models via llama.cpp llama-server"
provides = ["backend"]          # backend | router | tool

[run]                           # only for process plugins
command = "lh-backend-llamacpp" # plugin dir, running lh executable's dir, then $PATH
args = []

[config]                        # passed verbatim in `initialize`; user config may override
llama_server = "llama-server"
models_dir = "~/.local/share/liquid-harness/models"
# max_resident = 4              # optional hard cap; the harness memory budget decides (§5 "Model lifecycle")
idle_ttl_secs = 600
```

A specialist pack is a plugin with only `[[specialist]]` / `[router]` entries:

```toml
# plugins/lfm3/plugin.toml
[plugin]
id = "lfm3"
version = "0.1.0"
protocol = 1
description = "Liquid AI LFM3 specialists"
provides = ["specialist", "router"]

[[specialist]]
id = "lfm3-text"
# `route` is shown to the router as the choice criterion – write it for the router.
route = "General conversation, questions, writing, reasoning, coding, planning and tasks that need tools"
task = "chat"                       # chat | decide | transcribe | speak
inputs = ["text"]
outputs = ["text", "tool_calls"]
backend = "llamacpp"
model = { source = "hf:LiquidAI/LFM3-1.7B-GGUF:Q4_K_M", ctx = 32768 }
params = { temperature = 0.3, top_p = 0.9 }

[[specialist]]
id = "lfm3-vl"
route = "Questions about an attached picture, screenshot, photo, chart or scanned document"
task = "chat"
inputs = ["text", "image"]
outputs = ["text"]
backend = "llamacpp"
model = { source = "hf:LiquidAI/LFM3-VL-2B-GGUF:Q4_K_M", mmproj = "auto" }

[router]
id = "d1"
strategy = "decision-model"         # decision-model | plugin | rules
backend = "llamacpp"
model = { source = "hf:LiquidAI/d1-2B-GGUF:Q4_0", ctx = 4096, batch = 512, threads = 8 }
inputs = ["text"]
min_confidence = 0.55
```

Rules for manifests:
- `id`s are globally unique across all loaded plugins; duplicates are a startup error naming both files.
- Unknown manifest metadata keys are an error (`deny_unknown_fields`) so typos surface
  immediately. `[config]` is deliberately an arbitrary plugin-owned map, as are sampling
  `params`; backend-specific configuration validation belongs to that plugin.
- `model.source` schemes: `hf:<repo>:<quant>` (llama.cpp `-hf` semantics) and `file:<path>`.

M2b adds ordered backend choices. The existing `backend` plus `model` is valid
shorthand for one choice; use exactly one form (mixed/incomplete forms fail):

```toml
backends = [
  { backend = "freetoken", model = { source = "hf:LiquidAI/LFM3-1.7B-NVFP4" } },
  { backend = "llamacpp", model = { source = "hf:LiquidAI/LFM3-1.7B-GGUF:Q4_K_M", ctx = 4096, threads = 8 } },
]
```

FreeToken sources can use `hf:<repo>` without a GGUF quant suffix, `file:<path>`,
or a `freetoken:<alias>` that maps to a configured endpoint. Its plugin-owned
`config.models` map is keyed by the exact source and chooses URL or spawn mode.
Sources/options are backend-specific; the core never translates weights.

`[plugin] enabled` defaults to true; `[plugins.<id>] enabled` in user config
overrides it. The shipped FreeToken plugin defaults to false and appears only
in the text specialist's preference chain, so CPU defaults remain unchanged.
Manifests remain discoverable while disabled, but Engine refuses to start their
backends. `[plugin] capabilities` is an optional discovery hint displayed by
`lh plugins list` with `capabilities_source: "manifest"`; listing never starts
processes or loads models. Runtime capability checks use initialize, not hints.

`Engine::load_specialist` tries the enabled choices in manifest order, checks
the specialist task capability, and loads its corresponding model. Startup,
capability and load errors are logged and allow the next choice; exhaustion
reports all errors. Success sets `RouteDecision.backend` and logs the backend.
`lh run` and specialist `lh models pull` use this path. Inference errors are
not retried, preventing duplicate streamed output. `lh route` stays cheap and
route-only: its `backend` is null, never a prediction claimed as a loaded backend.

## 4. Protocol (version 1)

Transport: newline-delimited JSON-RPC 2.0. Requests carry integer `id`s.
Streaming results are sent as notifications before the final response.

| Method (core → plugin)  | Params                                                     | Result |
|-------------------------|------------------------------------------------------------|--------|
| `initialize`            | `{protocol: 1, plugin_id, config, core_version}`            | `{name, version, provides, capabilities}` |
| `shutdown` (notification)| –                                                         | – |
| `backend/load`          | `{key, model: ModelSpec, pinned?: bool}`                    | `{key, ready: true, info}` |
| `backend/unload`        | `{key}`                                                     | `{}` |
| `backend/chat`          | `{key, messages, tools?, params, stream: bool}`             | `{message, finish_reason, usage, timings?}` |
| `backend/transcribe`    | `{key, audio: local-path-or-data-URL, language?}`           | `{text, language?}` |
| `backend/speak`         | `{key, text, voice?, language?}`                           | `{audio: WAV-path-or-data-URL, sample_rate, duration_ms}` |
| `backend/decide`        | `{key, state, images?, files?, questions}`                  | `{answers}` (d1 `/v1/systemone` shape) |
| `backend/status`        | `{}`                                                        | `{loaded: [{key, pid, port, idle_secs}]}` |
| `router/route`          | `{request: RouteInput, candidates: [Candidate]}`            | `RouteDecision` |
| `$/cancel` (notification)| `{id}`                                                    | – |

| Notification (plugin → core) | Params |
|------------------------------|--------|
| `$/stream`                   | `{id, delta: {content?, tool_calls?}}` – chunk for request `id` |
| `$/log`                      | `{level, message}` |
| `$/progress`                 | `{id, message, fraction?}` – e.g. model download |

Initialize `capabilities` is an array of strings: FreeToken returns `["chat"]`,
llama.cpp/mock return `["chat", "decide", "stream", "transcribe", "speak"]`. The host also accepts the
legacy boolean map and normalizes true entries. Capabilities are refreshed on
every process generation. It rejects unsupported chat/decide/transcribe/speak calls before they
reach the plugin. A decision-model router checks `decide` before loading; its
error names both router and backend. FreeToken itself returns -32003 for decide.

M1 transport details:
- Frames are bounded to 8 MiB including the newline. Invalid envelopes, unknown
  response IDs, EOF, or a call/handshake timeout fail that process generation.
  The default is 30 seconds. Reserved `config.host_timeout_secs` overrides it
  (manifest or user override) and is removed before plugin initialization.
  The llama.cpp manifest uses 660 seconds, longer than its 600-second load/HTTP limits.
  In-flight requests fail together and are never replayed. The initial start is free;
  at most three restart attempts are allowed in a rolling 60-second window.
- The host correlates concurrent calls, accepts out-of-order responses, and exposes
  notifications via a bounded broadcast subscription (256 entries; lag is reported).
  A dedicated ordered writer avoids stdin/stdout backpressure deadlocks. At most
  64 calls are in flight per plugin; excess calls fail with `-32002` rather than
  blocking response processing. Oversized outgoing requests fail with `-32602`.
  Shutdown sends a notification, waits briefly, then kills/reaps an uncooperative child.
  Applications should await `Host::shutdown`/`Engine::shutdown` before dropping their runtime.
- The SDK is intentionally blocking and serial: cancellation cannot interrupt an
  active handler. `$/cancel` is ignored in M1; cancellable inference is future work.
- Decision DTOs follow llama.cpp b12189 `/v1/systemone`: choice criteria are an ID-to-text
  map, score criteria an ordered string array, and noul asks a yes/no question.
  Answers use `noul`, `choice` (with confidence/probabilities), or `score` (with
  confidence/probabilities/legend), not a generic value wrapper. M1 accepts text state
  and string attachment references; richer d1 state/criteria variants can be added later.

- `messages` use the OpenAI chat format (`role`, `content` as string or parts with
  `type: text | image_url | input_audio`, `tool_calls`, `tool_call_id`). The core
  never translates formats per model – backends do.
- Errors: JSON-RPC error codes; `-32001` model not available, `-32002` backend
  overloaded, `-32003` unsupported modality, `-32004` cancelled.
- Tool plugins are plain **MCP servers** (stdio). We do not invent a tool protocol.

M4a ships `lh-tools-fs`, `lh-tools-shell`, and `lh-tools-web` as independent
MCP **2025-03-26** servers using `lh-plugin-sdk::mcp`, not the backend handshake.
They implement initialize/initialized, ping, tools/list and tools/call with
JSON input schemas, text/isError results, and destructive annotations. Trusted
configuration comes from `LH_TOOL_CONFIG` JSON and optional initialize `config`
overrides. Workspace roots default to the inherited cwd, preserved by the MCP host.
Calls are serial; close stdin for shutdown. M4b reuses the process host with a
separate MCP handshake; failed calls are never replayed. Tool discovery is lazy
on first agent/tools-list request, names are qualified on collisions, and host
policy gates destructive calls before dispatch. Detailed contracts below.
Filesystem guards reject traversal, symlinks and common secret filenames but
are not an atomic filesystem sandbox. Shell runs in its own process group with
bounded output/deadlines, not permission isolation. Web uses ureq with validated
connection DNS addresses, no proxies/redirects, and public destinations by default.
Contracts, configuration, limitations and smoke evidence: [tools](tools.md).

## 5. Routing (the core)

### M2c cascade (current behavior)

`Engine::route` is stateless conversation-wise; `route_session(input, router,
&mut Session)` keeps the previous input and chosen specialist. Only successful
routes advance the caller-owned session; `Session::reset()` clears it. Both
use the same engine-local bounded decision cache. The cascade runs:

1. Explicit specialist, with input and (when enabled) requirement validation.
2. Input-modality filtering, then `requires` filtering. `inputs` says what a
   specialist accepts; `requires = ["image"]` or `["audio"]` says what must exist.
   Zero candidates is an error. One remaining candidate skips inference;
   reason is `requires` when that filter reduced the set, otherwise `single-candidate`.
3. Weighted literal phrase hints. `hints = [{pattern = "bitte vorlesen", weight = 1.0}]`
   matches contiguous case-insensitive Unicode words (punctuation separates words).
   This is not regex. Exactly one specialist with positive evidence and score
   at least `hint_threshold` wins; any competing evidence falls through.
4. Sticky: keep the previous compatible specialist on unchanged image/audio
   presence and no hint evidence for another specialist. This happens before
   the weak text-default shortcut, so a voice followup is not lost to text.
   A unique `default_for_text = true` wins only on text-only turns with no hints.
   It reports `hint`, with `default-text` in the trace. The shipped LFM3 pack
   deliberately does not enable this broad default: unrecognized speech requests
   still deserve a model decision.
5. In-process LRU (256 entries by default), with normalized whitespace (case
   preserved), full candidate metadata, router ID/model, config and session
   context in a hashed key, plus exact key comparison. Local attachment bytes
   are streamed into fingerprints; data URLs are content-addressed by their
   reference. Unreadable files and remote URLs bypass cache. Mutable local files
   are fingerprinted again on every eligible lookup. Cache only stores successful
   model/rules decisions, never errors. No disk persistence or cross-engine reuse.
6. The existing decision model, including low-confidence fallback. With a
   session, its state contains the previous user turn/specialist and current
   user turn (not full chat history or assistant outputs). Rules remains an
   explicit offline alternative, not a claimed d1 decision.

Every decision has `reason`, ordered `stages`, and `model_called`; `router_ms`
measures all routing stages. `lh route --explain` pretty-prints this trace.
Reasons: `explicit`, `requires`, `single-candidate`, `hint`, `sticky`, `cache`,
`decision-model`, `low-confidence` (plus `rules` for the offline strategy).

Configuration, all defaults shown:

```toml
[cascade]
enabled = true
requires = true
single_candidate = true
hints = true
sticky = true
cache = true
cache_capacity = 256
hint_threshold = 1.0
```

Disabling `enabled` skips every optional cascade stage; explicit selection and
modality safety remain intentional invariants. `lh eval routing --baseline`
sets this switch for a genuine model baseline, including sole candidates.
No new dependency: literal phrase matching avoids regex binary/dependency cost.

LFM3-VL requires images; `lfm3-audio` requires audio for transcription/listening.
`lfm3-speak` is a separate text-input TTS specialist using the same audio model.
Both audio tasks execute end-to-end in M3a. The mock specialist pack is now
`tests/fixtures/mock-pack`, discovered only by explicit test/demo paths, never
by default repository discovery. The default rules specialist is `lfm3-text`.

Evaluation reports actual model calls/rate, winning-stage counts and nearest-rank
p50/p95 total wall latency, measured at microsecond precision and reported in ms.
Optional JSONL `session` groups turns; omitted attachments inherit from the prior
successful turn, explicit empty arrays clear them. Session state is separate per
ID and identical in baseline/cascade. The core API itself takes explicit media
state; attachment inheritance is evaluator policy. See [M2c results](M2C-RESULTS.md)
for the frozen held-out methodology, tuning-set migration, load and limitations.

### M2b FreeToken lifecycle

The separate `lh-backend-freetoken` uses ureq with TLS for remote URL mode and
loopback HTTP for spawn mode. Bearer tokens come from a named environment
variable, never TOML. Spawn executes `ft serve --model-path ... --port ...`
with optional venv/command/environment/extra arguments. Both modes wait for
`GET /v1/cache/status` with `state=serving`; `/health` is not sufficient.
LRU/pinning/idle TTL and process-group cleanup follow the llama.cpp design
below. URL unload only forgets the registration, never stops the remote server.
The OpenAI SSE decoder is shared in lh-protocol (pure parsing, no HTTP).
FreeToken supports text chat only, not d1, VL/audio or LFM3 GGUF. Setup,
limits, CUDA/GPU prerequisites and both config modes: [FreeToken](backends/freetoken.md).
All FreeToken verification on this host uses fakes; there is no real GPU result.

### llama.cpp lifecycle details

The Linux backend uses `ureq` only for loopback HTTP (TLS features disabled);
llama.cpp owns Hugging Face downloads and honors `LLAMA_CACHE`. `models_dir`,
when set, overrides that cache variable. `file:` paths and explicit projector
paths expand `~/`. `mmproj = "auto"` enables HF discovery; absent `mmproj`
adds `--no-mmproj` to avoid downloading an unused projector for text routing.

ModelSpec ctx/batch/threads override backend config values (4096/512/8 by
default). The backend serializes inference and a 250 ms TTL worker with one
mutex, so active models cannot be evicted. Load is idempotent for the same key
and effective ModelSpec; reuse with a different specification is rejected.
Pinned models survive TTL/LRU but explicit unload and shutdown still remove them.
If all slots are pinned, load fails with -32002. Periodic load progress is sent
as `$/progress`; no download percentage is invented.

Every server has its own process group. RAII cleanup kills the group and reaps
the child, including health failures. Linux parent-death SIGKILL closes the
plugin-crash/host-timeout path for the direct llama-server. Arbitrary descendants
of third-party wrapper executables are not guaranteed cleanup after SIGKILL;
configure the actual llama-server binary, not a daemonizing wrapper. Port
selection releases an ephemeral listener before spawning; an intervening bind
race is possible and becomes a load failure. Cancellation remains serial SDK
behavior. SSE requires `[DONE]` and a finish reason, aggregates text/tool deltas
and usage, and bounds total input to 8 MiB. Usage is `{}` when the server sent
none (never placeholder zeros); llama.cpp's last `timings` object (stream or
non-stream) is passed through as `timings`.

The LFM3 pack enables text-only d1 routing: image/audio attachments select their
sole compatible specialist without loading d1 or a router projector. Audio
transcription uses the audio projector and a greedy ASR system prompt.
LFM3 chat disables thinking by default to avoid an invisible reasoning-only
answer exhausting the completion budget (text agent: 1024 tokens per step;
vision: 256 tokens, overridable in the pack).

### Model lifecycle (M5a)

`lh-core::lifecycle` tracks every router (`router:<id>`) and specialist in one
`Lifecycle` shared as `Arc` outside the engine mutex, so status and streams can
read it while a load or inference holds the engine. States: `stopped | loading
(progress, eta) | ready | failed (error)`. Backends stay authoritative for
residency: `Engine::reconcile` compares with `backend/status` and marks models
that vanished (crash, idle TTL, backend LRU) `stopped` with a reason; a
suspended llama-server is not counted as ready.

Every task (chat, vision, transcribe, speak, agent, explicit `/v1/decide`) loads
through one idempotent `Engine::ensure_loaded`/`ensure_router` path that calls
`backend/load` on every use, so a crashed llama-server is restarted by the next
request. A fallback chain never overwrites a ready model on another backend
with a failed attempt; a successful recovery unloads the old copy.

Configuration (`[lifecycle]`, all optional):

```toml
[lifecycle]
warm_router = true          # load the router in the background at lh serve/lh chat start
preload = ["lfm3-text"]     # then these specialists, in order (default: none)
memory_budget = "auto"      # 60 % of MemAvailable at startup, lowered to the remaining
                            # cgroup v2 allowance; or bytes, "8 GiB", "50%", "unlimited"
keep_warm = false           # preloaded + default specialist exempt from the backend idle TTL
# max_resident = 4          # optional hard cap on resident models, router included
```

Warm start is non-blocking: the HTTP listener accepts before the router load
starts. Warm-up takes the engine lock once per target; tokio's mutex is FIFO, so a
request arriving during the router load waits for that same load (no second
start) and runs before later preloads. Failures are logged and shown in status.

Residency: each backend may answer `backend/estimate` (llama.cpp: on-disk GGUF +
projector size, KV cache from GGUF block count/head dims/sliding window for the
effective ctx, plus a flat 256 MiB compute allowance; not-yet-downloaded models
have no estimate and count 0 until loaded, when the measured RSS is used). Before
a load, least-recently-used unpinned models are unloaded until the new estimate
fits the budget and `max_resident`; keep-warm models go last, the pinned router
never. The backend `max_resident` setting is now an optional cap (default
unlimited); the backend idle TTL (600 s) stays, except for keep-warm models.

Visibility: `/lh/v1/status` never waits for the engine. It always contains
`engine` (`idle|busy`), `warmup {state, pending}`, `budget {memory_budget_bytes,
source, available_bytes, resident_estimate_bytes, max_resident}` and `models[]`
(`id, kind, key, backend, state, progress, progress_source (backend |
previous-load), message, elapsed_ms, eta_ms, error, reason, ram_estimate_bytes,
rss_bytes, pid, pinned, keep_warm, preload, loads`); process and resident details
are added when the engine is free. Progress is the backend `$/progress` fraction
when reported, else elapsed time relative to the previous load of that model in
this process (capped at 95 %); otherwise only elapsed time.

While a request waits for a load, streams report it every 500 ms: OpenAI chat
streams always send SSE comments (`: lh model_loading <id> <pct>`, proxy
keepalive) and, with the harness extension `"lh_events": true`, a named
`event: model_loading` whose data is the model entry. Agent runs emit a native
`model_loading` event (`{event, data, run_id}`). A streamed chat arriving while
the engine is busy gets its headers at once (no `x-lh-route` header then; the
route is in the stream's model field as usual). Non-streaming chat and agent
runs wait instead of answering 409 while the engine is busy loading or warming;
an agent holding the engine (e.g. awaiting approval) still yields 409 for new
runs and for management endpoints other than status. `lh chat` warms up while
the user types and prints one-line `lh: loading <id> (<backend>)…` / `lh: <id>
ready (N s)` notes on stderr. The web UI requests `lh_events`, shows "Loading
<id>… N%" in the pending answer and the run status, and lists model states,
RAM estimates and the budget on the Status page.

Shutdown: SIGINT or SIGTERM stop accepting, abort warm-up and streams and shut
the engine down; backends kill each model's process group (verified with the
real binary and mock model processes in `crates/lh-cli/tests/lifecycle.rs`).

### M3a audio execution

`task_method` maps chat/decide/transcribe/speak to the corresponding backend method;
unknown tasks are rejected during registry validation. `Engine::prepare` loads a
specialist without routing (including `models pull` for specialists requiring media).
`Engine::dispatch` overwrites the request key with the selected specialist ID and
checks the selected backend belongs to its configured choices. It does not reroute
or retry inference. Clients must load first and own stream subscriptions.

`lh run --audio F [prompt]` uses normal modality/cascade routing. The shipped pack
has one audio-input specialist, so it transcribes even if prompt contains a question.
For a question about a recording use `--audio-question`: explicitly transcribe,
then send the transcript and question to the configured default text/chat specialist.
This is an explicit two-step pipeline, not an unreliable question-mark heuristic.
Custom audio-capable chat specialists receive `input_audio` parts directly.
The pipeline does not interpret acoustic features, music or speaker identity.

`--speak-out out.wav` selects the unique speak specialist when model is auto;
with multiple speak specialists select `--model`. Otherwise phrase hints such as
`read this aloud`, `vorlesen`, or `lies das vor` select speech through the cascade.
Speech receives the complete prompt verbatim (including any instruction prefix).
Without `--speak-out`, speech writes a unique `lh-speech-*.wav` in the current
directory. Existing output paths are never overwritten. `--language` supplies an
ASR hint / TTS language code; `--voice` is passed to llama-tts (female/male).
CLI stdout is the transcript, streamed answer or WAV path; route traces use stderr.

The backend accepts local paths or base64 audio data URLs. PCM/float RIFF WAV is
validated; other encodings use `ffmpeg` from PATH or `[config] ffmpeg`, converted
to mono 16 kHz PCM16. Missing ffmpeg is a clear error. Audio is bounded below
5 MiB for the 8 MiB RPC framing limit. Returned audio is a WAV data URL with
metadata parsed from RIFF chunks. The requested language is echoed, not detected.

ASR uses `/v1/chat/completions`, `input_audio`, temperature zero, and `Perform ASR.`
or `Perform ASR in <language>.`. TTS uses the documented `llama-tts` one-shot
executable, rather than implementing the realtime WebSocket protocol. Configure
`llama_tts` or install beside path-valued `llama_server` (otherwise PATH lookup).
Both shipped audio ModelSpecs set `mmproj = "auto"`; file sources need an explicit
projector. See [backend audio details](../plugins/backend-llamacpp/AUDIO.md).

TTS and ASR cannot share one live inference process with this adapter: llama-tts
owns synthesis. Initial load starts a server; TTS stops same-source unpinned
servers before loading its one-shot process. Registrations remain in LRU/TTL;
the next ASR lazily starts its server again. Pinning a matching source forbids TTS.
Different specialist keys are not general-purpose resident aliases. Consequently
switching incurs cold-load overhead; repeated TTS avoids repeated server startup.
The backend mutex prevents inference overlap; other model sources may stay resident.
Temporary files are private and removed on success/error/timeout, and subprocesses
are killed/reaped on timeout. Serial SDK cancellation and crash limitations remain.
No HTTP/inference dependency was added to core; base64 was added to backend/mock.

The original M1/M2 model path is shown below for protocol context; M2c inserts
the cascade described above between modality filtering and this model path.

```
Request ─► 1. explicit? ──yes──► specialist by id/alias
              │no ("auto")
              ▼
           2. modality filter: drop specialists whose `inputs` don't cover the
              request's attachments (image/audio). 0 left → error -32003.
              1 left → done (no model call).
              ▼
           3. router: decision-model strategy builds ONE d1 call:
                state     = last user turn (+ short summary of earlier turns, ≤ 2K tokens)
                images    = request images if the router model accepts images
                questions = {
                  "specialist": {type: "choice", instructions: "Which specialist should handle this request?",
                                 criteria: {<id>: <route text> for each candidate}},
                  + extra questions declared by specialists/config (e.g. "needs_tools": noul)
                }
              ▼
           4. confidence = probability of the winning choice.
              < min_confidence → `default_specialist` (config), reason "low-confidence".
              ▼
           5. RouteDecision {specialist, confidence, scores{id: p}, reason, extras, router_ms}
```

- Every decision is logged and exposed: CLI `lh route --explain`, HTTP header
  `x-lh-route: <id>;p=<conf>` and an optional `lh` object in the response body.
- Strategies: `decision-model` (built-in, any backend with `decide`), `plugin`
  (external process implementing `router/route`), `rules` (built-in, deterministic:
  modality filter + default; used in tests and when no router model is installed).
- Router call budget: the router model stays resident (pinned, never LRU-unloaded).
  Baseline measured 2026-11-07 on the dev host (Core Ultra 7 155H, CPU only, llama.cpp b12189,
  `-t 8`, load < 2): d1-2B Q4_0 ≈ 1.9–2.2 s, Q8_0 ≈ 2.7–3.5 s per decision for a ~90-token prompt
  with 3 candidates; prompt processing only ≈ 48 tok/s (`llama-bench pp128`), which looks low for
  this CPU and is worth investigating (CPU backend variant, power state, threads). Routing quality
  in that probe: 7/7 correct (EN+DE), winning probability 0.89–0.997.
  Because a router call is not free, avoid it whenever possible: explicit model, single
  candidate after the modality filter, and sticky routing skip the model call.
- The legacy post-decision `sticky` helper retains its 0.8 confidence threshold;
  new conversations use the pre-call M2c session policy above instead.
- If the configured default is not a modality-compatible candidate, M1 uses the first
  compatible ID in lexical order. Low-confidence decisions preserve the winning model
  probability and full original score distribution, even when the specialist is replaced.
  The router load is idempotent and requested before each decision, with `pinned: true`,
  so a restarted backend can recover its state. Exactly one decide call follows the load.
  M1 routes a single user turn; summaries and optional extra questions remain future work.

## 6. Crates

```
crates/lh-protocol    JSON-RPC envelopes, manifest structs, DTOs (chat, decide, route),
                      `model_files` (llama.cpp cache lookup shared by the llama.cpp
                      backend and `lh doctor`). Depends on serde/serde_json/toml only. No async.
crates/lh-plugin-sdk  Write Rust plugins in ~30 lines: blocking stdio loop, method
                      dispatch, stream/log helpers; separate MCP tools transport.
                      std + lh-protocol only.
crates/lh-core        Plugin discovery + host (tokio process/io), registry, router
                      engine, orchestrator, config. No HTTP.
crates/lh-cli         Binary `lh`: run, models pull, eval routing, chat REPL and HTTP/1
                      server (default-on `server` feature). Shared conversation adapter.
plugins/backend-llamacpp  Binary `lh-backend-llamacpp`: spawns/stops llama-server per
                      model on a free localhost port, health-waits, LRU/TTL unload,
                      proxies chat (SSE → `$/stream`) and decide (`/v1/systemone`).
plugins/backend-freetoken Binary `lh-backend-freetoken`: URL or managed ft serve,
                      readiness, LRU/TTL, chat/SSE only; explicitly opt-in.
plugins/mock-backend  Deterministic backend (Unicode echo/three stream chunks,
                      decide via case-insensitive word overlap, with normalized scores).
tests/fixtures/mock-pack  Explicitly loaded offline specialists and mock-router fixture.
plugins/lfm3          Manifest-only specialist + router pack.
plugins/tools-fs      MCP confined-path UTF-8 read/write/edit/list/search tools.
plugins/tools-shell   MCP approved-by-host shell commands, group kill and output cap.
plugins/tools-web     MCP search providers and bounded public HTTP text fetching.
```

Dependency policy: every new dependency needs a one-line reason in the PR/commit.
Prefer std, `serde`, `tokio` (only `rt`, `process`, `io-util`, `sync`, `time`, `macros`),
`clap` (derive), `toml`. The HTTP server uses `hyper` 1.x directly or `axum` – whichever
keeps the binary smaller. HTTP client in the llama.cpp plugin: `ureq` (blocking, small).
`cargo build --release` with `lto = "thin"`, `strip = true`, `opt-level = "s"` for the core.

## 7. CLI (`lh`)

```
lh plugins list                      # discovered plugins, specialists, router, status
lh route [--explain] [--image F] [--audio F] "prompt"   # routing only, prints RouteDecision JSON
lh run   [--model ID|auto] [--image F] "prompt"         # one-shot, streams answer to stdout
lh run --audio recording.wav [--language en]           # transcription
lh run --audio recording.wav --audio-question "Summarize the recording"
lh run --speak-out out.wav --language de "Guten Morgen" # text verbatim to speech
lh chat  [--model ID|auto]           # REPL; /model, /explain, /reset
lh serve [--bind 127.0.0.1:8484]     # OpenAI-compatible API
lh models pull <specialist-id>       # pre-download a specialist's model
lh doctor [--json]                   # alias `lh where`: self-description + problems (exit 2 on errors)
lh restart [--url http://127.0.0.1:8484] [--reason R] [--no-wait]   # restart a running lh serve
```

## 8. HTTP API (`lh serve`)

OpenAI-compatible subset so Hermes, Open WebUI, etc. can use the harness as a provider:
- `GET /v1/models` → `auto` + every specialist id. Each entry carries the harness
  extension `speak_ids` (see "Spoken replies" below); OpenAI clients ignore it.
- `POST /v1/chat/completions` (stream and non-stream). `model: "auto"` routes;
  a specialist id forces it. Images via `image_url` data URLs, audio via `input_audio`.
- `POST /v1/route` → RouteDecision (harness extension).
- `POST /v1/decide` → passthrough to the router/decision model (harness extension).
- `POST /v1/audio/transcriptions`: multipart `file`, `model` (auto by default),
  optional `language`; JSON transcription result. Other response formats rejected.
- `POST /v1/audio/speech`: `input`, `model`, optional `voice`/`language`,
  `response_format: wav`; binary WAV response.
- Bind to localhost by default; `server_token_env` names the bearer-token environment
  variable. Missing/empty configured tokens fail startup; auth covers every API endpoint.

M3b uses Hyper HTTP/1 directly, with Multer for multipart and CLI-only optional
HTTP dependencies. Core remains HTTP-free. The same-handler Axum adapter probe
was larger; binary measurements and scope are in [M3b results](M3B-RESULTS.md).
HTTP bodies are capped at 6 MiB with a 30-second body timeout. Only base64 data
URLs are accepted for HTTP images; input audio is base64 plus a validated format.
Local paths, remote URLs and decide file attachments are forbidden. Plugin RPC
frames remain bounded at 8 MiB. There is no TLS, cross-origin API access, rate limiting or HTTP/2.

HTTP conversations are stateless: clients supply history. One engine/cache is
shared, and a mutex serializes complete inference calls so backend broadcast
streams cannot contaminate other clients. SSE sends live delta frames followed
by a finish chunk and `[DONE]`, including draining queued final deltas. Routing
is resolved before streaming headers when the engine is free (see "Model
lifecycle" for the busy case), then the selected specialist is pinned
for execution (no second model decision). In-stream failures are error events.
A disconnected client does not cancel the serial backend handler. SIGINT/SIGTERM stop
accepting, drains connections briefly, aborts pending HTTP work and shuts down
the engine. SIGHUP and `POST /lh/v1/admin/restart` restart in place instead (see
"Self-awareness and restart"). No cancellation/replay promise is added to the SDK.

`x-lh-route: ID;p=CONFIDENCE;stage=REASON` identifies the input specialist.
Chat audio uses transcribe→chat (default text, or vision when images remain);
the header identifies ASR, while usage reflects the final chat stage only.
Directly routed speech returns a `message.audio` extension containing the backend
WAV data URL/sample-rate/duration (also emitted as an SSE delta); portable clients
should use the binary `/v1/audio/speech` endpoint instead.

The plain line REPL owns one M2c Session and a full role-preserving conversation.
`/image F` and `/audio F` attach to the next turn; `/speak` toggles speech of the
answer, `/model ID|auto` changes routing, `/explain` shows the last complete route,
`/reset` clears session/history/pending media, `/quit` exits. Images in history
remain relevant to auto routing; single-candidate selection precedes sticky as
in M2c. Explicit text switches filter old incompatible media from the model
request, never new unsupported media. Text/system/tool messages are preserved;
all-text part arrays collapse to strings for text models. ASR replaces audio parts
with transcript text in the REPL history. HTTP clients resending audio incur
transcription again. No automatic context truncation/summarization is performed.

### Embedded browser UI (M4c)

The default-on `web` feature implies `server`; `--no-default-features --features
server` keeps the API without the UI. `GET`/`HEAD` on `/`, `/index.html`,
`/app.mjs`, `/markdown.mjs`, `/stats.mjs`, `/settings.mjs`, `/style.css`, `/layout.css` serves compile-time embedded assets,
not arbitrary filesystem paths. These immutable assets are public to bootstrap
token entry; every API data/control request still requires the configured token.
The UI sends it as a bearer header and stores it only in sessionStorage.
Assets have explicit UTF-8 MIME types, no-cache, nosniff, no-referrer and CSP:
self-only scripts/styles/connect, self/data/blob images and media, no objects,
base URLs or framing. No inline scripts/styles, eval, CDN or runtime framework.
`style.css` holds design tokens and component visuals; `layout.css` (loaded
after it) holds placement only: the app shell, the chat view that fits the
viewport (`body[data-page=chat]`, set by `app.mjs`; only `#messages` scrolls),
composer rows with Send as the trailing control, and the management card grid.
Both share one breakpoint (760 px); see `docs/WEB-UI-PLAN.md`. A pending
approval card never shrinks (`#messages` yields space, then the document scrolls
on small screens). `cargo test` runs `scripts/test-web.mjs` (behaviour, markup and
layout contract) and `scripts/test-style.mjs` (tokens, contrast, states) when
`node` is installed; review results in `docs/WEB-UI-REVIEW-RESULTS.md`.

The chat "Model" `<select id="model">` has no separate refresh control: the list
is loaded at start, after a token change and every time the dropdown is opened
(`pointerdown` or an opening key, `opensSelect(event)`). Loads go through
`singleFlight`, so overlapping opens share one `GET /v1/models` request.
`modelOptions(catalog, previous)` dedupes the ids and keeps the current choice
while it is still offered (else the first entry); an unchanged list is not
rebuilt, so an already open popup stays put. While loading the select carries
`aria-busy`; a failed load leaves the previous options usable and is reported.

Hash navigation selects Chat/Plugins/Tools/Skills/Status/Options. Read-only management
uses existing M4b endpoints sequentially (parallel reads would contend for the
nonblocking engine lock); busy errors are visible with an explicit Refresh.
Router stage switches are displayed, not editable. Chat completion JSON and SSE
chunks add `lh: RouteDecision`; the final successful SSE chunk includes the
executed backend while retaining the original routing reason. Audio chat's route
still describes its input specialist, not a new routing call. Agent SSE already
carries the full route and retained tool history.

### Spoken replies

A speak ID is the id of a `task = "speak"` specialist. Nothing is hard-coded:
`Registry::speak_ids(model)` derives them from the discovered manifests and
`GET /v1/models` reports them per entry as `speak_ids` (sorted, unique):

- a specialist → the speak specialists declared in the same plugin manifest
  (`lfm3-text` → `["lfm3-speak"]`; a pack without one → `[]`);
- `auto` → every speak specialist (auto routing may pick any pack);
- unknown ids → `[]`. Clients treat a missing field (older server) as `[]`.

The web UI setting is one optional speak ID, `speakChoice` in `app.mjs`: `null`
means no automatic speech; a string means each completed, non-error, nonempty
answer is sent to `POST /v1/audio/speech` with `model` set to exactly that ID
(no `auto` speech routing). The scope and storage are unchanged from the former
checkbox: in-memory per tab, not persisted; every page load starts at `null`
(the `#speak` select has `autocomplete="off"`). Resolution is the pure exported
`resolveSpeakSelection(choice, available)`: a string is kept only if it is in
`available`; legacy boolean `true` becomes the single offered ID only when
exactly one is offered; `false`, stale/unknown IDs, several candidates or junk
become `null`.
`speakIdsFor(catalog, model)` reads (dedupes, filters) `speak_ids` from the
`/v1/models` data array.

The control is the "Speak replies" `<select id="speak">` (inside `boot()`):
`speakAvailable` holds the IDs of the selected model, `speakChoice` the current
selection; `syncSpeak()` runs after every `/v1/models` load and on `#model`
change, revalidates the choice (a dropped ID stays dropped when switching back)
and calls `renderSpeakControl()`, which only mirrors the state: it rebuilds all
options from `speakOptions(available, choice)` — first "`<none>`" (value `""`,
`SPEAK_NONE`), then each available ID once — and selects the mirrored value, so
model switches and catalog refreshes leave no stale or duplicate entries. A
model without speak IDs offers only "`<none>`". The change handler calls
`chooseSpeak(speakChoiceFromValue(value))` (`""` → `null`). Until the first
`/v1/models` answer only "`<none>`" exists and nothing is spoken. At ≤760 px
the dropdown takes its own composer row and the attachment-limits hint is
hidden so 390×844 does not scroll (the mobile nav is one internally scrolling
line for the same reason).
The REPL `/speak` toggle is unchanged (unique speak specialist).

### Generation statistics

Each chat answer carries a statistics object, computed by
`lh_protocol::stats::generation_stats` from backend data plus harness wall-clock
times of the model call (dispatch → result, excluding routing, model load and
ASR). It appears as `lh.stats` on the final successful SSE chunk (deltas and
error events carry none) and on non-streaming chat completions, and as
`response.lh_stats` in the agent `done` event, where it covers the final model
call only, not the whole tool loop. The final SSE chunk also carries the
OpenAI `usage` object when the backend reported one. Speech answers have
`stats: null`.

| Field | Meaning |
|-------|---------|
| `completion_tokens` | Backend `usage.completion_tokens`, else llama.cpp `timings.predicted_n` |
| `prompt_tokens` | Backend `usage.prompt_tokens`, else `timings.prompt_n` |
| `tokens_per_second` | Decode rate, see below |
| `rate_source` | `backend_timings`, `harness_stream`, or null |
| `rate_window_ms` | Time window the rate divides by |
| `elapsed_ms` | Harness time from dispatch to complete result |
| `first_delta_ms` | Harness time from dispatch to the first streamed delta |

`tokens_per_second` is `predicted_n * 1000 / predicted_ms` when llama.cpp
timings exist (window: the backend's generation phase, without prompt
processing). Otherwise, for streams with at least two backend-counted tokens, it
is `(completion_tokens - 1) * 1000 / (last_delta_ms - first_delta_ms)`: the first
delta ends prompt processing, so the window covers the remaining tokens; windows
below 50 ms (buffered bursts) give null. Every field is a finite non-negative
number or null meaning "not available"; nothing is estimated from characters,
words or chunk counts, and zero/negative windows never divide. FreeToken and
the mock report no timings; the mock has no usage unless a test injects it.

The browser keeps one stats object per assistant message (`web/stats.mjs`):
`startStats()` when an answer is created (phase `pending`, all values null),
`completeStats()` only after `[DONE]`/`done`, `endStats()` with `cancelled`,
`error` or `interrupted` (page reload) clearing every value. `latestStats(messages)`
returns the selected conversation's latest answer statistics or null, so a
conversation switch never shows another chat's values. Persisted stats are
re-validated by `restoreStats()`.

The chat shows them in `#chat-stats`, right of `#run-status` in one
`.chat-status` row between the messages and the composer (wraps on narrow
screens, never overlays). `formatStats(stats, locale)` builds the text: the
rate first (`24.3 tokens/s`, number format from the browser locale via `Intl`,
e.g. `24,3` for German), then `· N tokens` only when the server reported
completion tokens; source, window, prompt tokens and model-call time go into the
`title`. Missing values read `–`; pending answers show `– tokens/s` (the server
sends no live rate), cancelled/failed/interrupted ones append the phase. The line
is hidden while the shown conversation has no answer with stats. It is refreshed
from `renderMessages()`, i.e. on every delta, completion, error and conversation
switch, and keeps the final values until the next answer starts.

Ctrl+Enter in the focused message textarea calls `form.requestSubmit()`, so it
shares the Send button's constraint validation, submit handler and busy guard.
Plain Enter keeps its newline; auto-repeat and IME composition never submit. The
listener is bound to the textarea only, never to `document`/`window`.

Conversations/attachments are browser-local, unencrypted localStorage, with
explicit storage-failure warnings and export. Streaming is parsed incrementally
across UTF-8/chunk boundaries; incomplete answers are excluded from follow-up
context. HTML is escaped before the deliberately small Markdown renderer emits
markup; only HTTP(S) links without credentials are clickable, and remote Markdown
images are never fetched. Agent mode is explicit, with approval controls and
cancel; the existing server cancellation and approval boundaries are unchanged.
File/MediaRecorder inputs are bounded at 4 MiB per turn, complete JSON at 6 MiB.
WebM/Ogg/MP4 conversion remains the backend's ffmpeg responsibility. Speech uses
the binary WAV API and an audio player. Real hardware permissions, browser quota
and model context windows remain external limits. Verification: [M4c](M4C-RESULTS.md).
Target page structure, design rules and file ownership for the UI cleanup:
[WEB-UI-PLAN](WEB-UI-PLAN.md).

### Options

`#options` is a browser-local page (no server reads; `management()` only shows it
and refills the form). It holds one setting, "Default context for new chats", a
multi-line `#default-context` textarea (at most 8000 characters) with Save,
"Restore suggested text" (fills the field, still needs Save) and a hint that
changes apply only to conversations created afterwards.

Storage: localStorage key `lh.defaultContext`, value = the raw saved text, next
to `lh.conversations` and `lh.theme`; it survives reloads and browser/server
restarts and is per browser profile, like conversations. At every boot
`settings.init()` writes the suggested German text (`SUGGESTED_DEFAULT_CONTEXT`:
"Erfinde keine Zahlen und Fakten. …") only when the key is absent (new install
or an installation from before this setting); any stored value, including `''`,
is never overwritten. A deliberately empty (or whitespace-only) value disables
the default context. Unsaved edits block page unload like a running answer; a
save in another tab refreshes the field unless it has unsaved edits. If
localStorage is unavailable or a write fails, the value is kept in memory for
the tab and `#storage-warning` says so.

Interface (`web/settings.mjs`, pure, storage injected so tests use a fake):
`createSettings(storage)` returns `init()`, `defaultContext()` (raw text for the
form, re-read on each call), `saveDefaultContext(text)` (exact text; throws only
for non-strings or more than `MAX_DEFAULT_CONTEXT` characters, leaving the stored
value unchanged; returns `{value, persisted}`) and
`defaultContextForNewChat()`. `app.mjs` creates one instance, `settings`, in
`boot()` before the first conversation is created. Contract for chat creation:
call `settings.defaultContextForNewChat()` once when a conversation is created
and snapshot the result into it: the trimmed text, or `''` meaning add nothing;
a later save never touches existing conversations.

### Default context in conversations

Every conversation is created by `fresh()` (first boot with no conversations,
"New chat", deleting the last conversation) through the exported pure
`newConversation(id, settings.defaultContextForNewChat())`, which snapshots the
text as `defaultContext` on the conversation (`''` = none). It is persisted with
the conversation in `lh.conversations` (and exported), so reloads and restarts
keep it; nothing ever reassigns it, so later Options saves only affect
conversations created afterwards. Conversations from before this feature have
no field and get no context (`conversationContext(c)` treats a missing or
non-string value as `''`).

The snapshot is instruction text, not a chat turn: it is never stored in
`history`/`messages`. `send()` builds `messages` with
`requestMessages(context, history)`, which prepends one `{role:"system"}`
message and keeps the history unchanged, including any system or chat-specific
context messages already in it. The agent loop then adds its own tool/skill
system message before it, so both instructions reach the model. Agent runs
return `lh_messages`, which echo the request; `historyWithoutContext()` removes
the leading context message before the history is stored, and
`requestMessages()` does not prefix a history that already starts with it, so
the context is sent exactly once per request. The chat view shows the snapshot
as a quiet "Default context of this conversation" disclosure.

Scope: the option is browser-local, so only conversations created in the web UI
get it. `lh chat`, `lh run` and other API clients send whatever messages they
build; the server never injects the default context. Routing ignores system
messages (only user text and media select the specialist). The server contract
that `historyWithoutContext()` relies on (agent `lh_messages` starts with the
client's first message, without the agent's own system message) is tested in
`plugins/mock-backend/tests/agent.rs`; the client side in
`scripts/test-web.mjs`. The LFM3 chat template renders the two leading system
messages as two system turns (checked with llama-server `/apply-template`).
Manual browser checks: `docs/WEB-UI-DEFAULT-CONTEXT-RESULTS.md`.

The text only asks the model to behave in a certain way; it does not add tools
(no web search or file access unless the selected specialist offers them) and
does not guarantee tool use or factual correctness.

## 9. Configuration
### Self-awareness and restart (M5b)

One introspection source, `lh_core::selfinfo::SelfInfo`, describes where the
harness runs and what it found. It is gathered on request (cheap: file metadata,
`/proc`, no plugin start, no model load; the only subprocess is
`llama-server --version`, cached per binary path and file stamp) and serialized
as JSON sections:

- `build`: version, git commit + dirty flag + build time (from `crates/lh-core/build.rs`
  at compile time; git never runs at runtime; `unknown` without a checkout), profile,
  `source_dir` (the workspace the binary was built from, when it still exists),
  binary path/size/mtime at start, `binary_changed` (the file at that path now has a
  different size/mtime/inode: "new binary available"), pid, start time, uptime,
  argv, `restart_count` and `last_restart_reason`.
- `host`: hostname, OS (`/etc/os-release`), kernel, arch, CPU model, logical CPUs,
  memory total (MemTotal lowered to a cgroup v2 `memory.max`) and available
  (the M5a probe), a container hint (`/.dockerenv`, `/run/.containerenv`,
  `$container`, PID 1's cgroup; a hint, never proof), cwd and the agent workspace
  (`tools-fs` `workspace_root`, else the working directory).
- `config`: file path, `chosen_by` (`--config`, `LH_CONFIG`, `xdg-default`,
  `defaults` when no file exists), router, default specialist, token variable
  (set/unset), agent approval, lifecycle and service settings.
- `discovery`: plugin search path in order with each entry's source
  (`LH_PLUGIN_PATH`, `config`, `config-dir`, `cwd`) and existence; every plugin
  with manifest dir and resolved executable (`plugin-dir`, `lh-dir`, `PATH`,
  `missing`, `declarative`, the same order the host spawns with); the skill search
  path with sources and every `SKILL.md` with its name or error, plus built-ins.
- `models`: every router and specialist with its backend chain; for llama.cpp
  choices the resolved GGUF and projector in the cache (`models_dir`, else
  `LLAMA_CACHE`, else `~/.cache/llama.cpp`; Hugging Face hub layout and llama.cpp
  flat names) or the `file:` path, sizes, `downloaded`, and the M5a state/pid.
  Backends that manage their own files (FreeToken) report `local: null`.
- `backends`: llama-server path and `--version` (or the error), model cache;
  FreeToken models with mode (`url`/`spawn`), URL and token variable (set/unset).
- `service` (`lh serve` only): bind, URL, log destination (`[service] log`, else
  where fd 2 points: file, terminal, pipe, socket), restart method and triggers.
- `env`: whitelisted variables only (`LH_*`, `LLAMA_CACHE`, `HF_HOME`,
  `HF_HUB_CACHE`, `XDG_CONFIG_HOME`, `LD_LIBRARY_PATH`) and the PATH entries
  where `lh`, llama-server or plugin binaries were found. Variables whose name
  contains TOKEN/KEY/SECRET/PASSWORD/CREDENTIAL/AUTH, `LH_TOOL_CONFIG`, and every
  variable named by configuration (`server_token_env`, `*_env`, FreeToken
  `token_env`, `HF_TOKEN`) appear only as `set`/`unset`. No value of a secret is
  ever serialized.
- `problems`: `{severity, code, subject, message, hint}`; `error` means a feature
  cannot work: `router-missing`, `token-unset`, `plugin-executable-missing`,
  `llama-server-missing`, `model-file-missing`, `skill-unreadable` (agent runs
  fail while one exists); `warning`: `default-specialist-missing`,
  `llama-server-version`, `model-not-downloaded` (downloads on first use).
  Only enabled plugins and the active router/specialists are checked.

Surfaces: `lh doctor` (alias `lh where`) prints a text report or `--json`,
exit code 2 when an error-level problem exists (configuration that fails to
parse still fails before the report). `GET /lh/v1/self` returns the JSON with
the same bearer-token rule as every API endpoint; it answers from a copy of the
registry/config taken at start plus the shared lifecycle tracker, so it never
waits for the engine (an agent awaiting approval holds it). The web UI Status
page shows an "About this harness" card (build, host, paths, active models with
on-disk state, problems, a "new binary available – restart to apply" notice) and
a confirmed Restart button that polls `/lh/v1/self` until the start time changes.

Agent context: every agent run's system message gets a compact "Harness
environment" block (hard cap 1400 characters, about 300 tokens: version/pid,
host, workspace, config path, existing plugin/skill dirs, active models with
state, how to restart, problem count). Plain chats get it only with
`[self] context_in_chat = true` (default false), merged into a leading system
message. Built-in tools: `harness_info` (read-only, optional `section`) returns
the JSON; `harness_restart {reason}` exists only under `lh serve`. It is
destructive: it always asks for approval, even in `auto-edit` mode and with
`command_allow = ["*"]`; only `yolo` skips the prompt; "always allow" never
carries over (the approval key includes the call id). In runs without an
approval channel the prompt times out and denies. An approved restart ends the
run with `finish_reason: "restart"` before the server drains. The built-in skill
`harness-self` (compiled into `lh`, overridable by a local skill of the same
name) explains inspecting, rebuilding with `cargo build --workspace --release`
in `build.source_dir`, restarting, and when not to.

Restart (`lh serve`): triggers are `POST /lh/v1/admin/restart` (`{reason}`
optional; with a configured token the token is required, without one only
loopback peers are accepted), `lh restart [--url]` (plain HTTP/1.1 over
`std::net`, waits until `/lh/v1/self` reports a new start time), the web UI
button, the agent tool, and SIGHUP. SIGHUP performs the same full restart (there
is no partial reload: config, plugins and skills are re-read by the new process
and models reload through the M5a warm start). The HTTP reply is sent first.
Then: the listener closes (new connections are refused, new requests on kept-alive
connections get 503 "lh is restarting"), warm-up stops, in-flight requests and
agent streams get `[service] drain_timeout_secs` (default 10) to finish; after
that, waiting handlers answer 503 "request cancelled: lh is restarting (drain
timeout)" and agent runs are cancelled with an error event. The engine shuts
down every plugin process and the llama-servers it owns. Finally the process
re-executes its own binary (`exec`, same PID, same argv and environment plus
`LH_RESTART_COUNT`/`LH_RESTART_REASON`; a `--bind host:0` becomes the port that
was bound). With `[service] restart_command`, the command runs detached through
`/bin/sh -c` in its own process group and `lh` exits 0 (for systemd/supervisors).
A failed exec exits 1 with the error on stderr. A binary replaced on disk is what
the exec runs, so rebuild + restart applies a new version.

### Agent execution and management (M4b)

Specialists opt in with `agent = true` or nonempty `tools = [...]`. Selectors
are plugin IDs, original tool names or collision-qualified `plugin__tool` names;
empty selection with `agent=true` means all enabled MCP tools. Discovery lists
all enabled tool plugins for a stable collision namespace; unrelated tools are
then filtered before exposure. Built-in `skills_list`/`skill_view`/`harness_info`/
`harness_restart` names are reserved (plugin tools with these names are qualified).
`lh run` and `lh chat` execute the loop. The OpenAI `/v1/chat/completions` adapter
remains client-owned tool execution, avoiding unexpected local side effects;
use the explicit `/lh/v1/agent/runs` endpoint for host execution.

Each run routes/loads once, adds a compact skill index, sends OpenAI tool schemas,
validates a complete assistant call batch, executes sequentially, appends
`role:tool` results with `tool_call_id`, and repeats. Text deltas stream live;
tool-call events are emitted only after the entire batch validates. Limits:
20 model steps, 32 calls per batch, 120 seconds per tool, 64 KiB cumulative
serialized tool output and 16 KiB retained tool context by default. UTF-8
truncation is visible; old tool bodies are trimmed newest-first without removing
call IDs. User/assistant history and schemas are not token-trimmed; very long
conversations can still exceed context. The shipped text pack uses 32K ctx.

`[agent] approval="ask"` is default; `auto-edit` only auto-approves filesystem
write/edit, `yolo` auto-approves everything. Anchored `command_allow` patterns
have only `*` wildcard syntax and are not shell isolation. Unanswered approvals
deny after 60 seconds. CLI prompts use a nonblocking controlling tty; piped stdin
does not approve. `always` remembers only identical tool/argument requests in
the current run. HTTP control state is separate from the engine mutex, so approve
and cancel work while inference is locked. Active inference drains before cancel
returns an error; tool cancellation/timeout resets its process generation and
retains restart accounting. Side effects already performed cannot be undone.

Skills live in trusted local search paths and have a deliberately limited YAML
frontmatter parser, with no new dependency. See [skills](skills.md).
The authenticated management JSON and agent SSE contracts, concurrency behavior,
configuration defaults and security limitations are in [tools](tools.md).
Real-model evidence and prompt overhead: [M4b results](M4B-RESULTS.md).


`$XDG_CONFIG_HOME/liquid-harness/config.toml` (overridable with `--config`/`LH_CONFIG`):

```toml
default_specialist = "lfm3-text"
router = "d1"                   # router id, or "rules"
plugin_path = []                # extra plugin dirs
[plugins.llamacpp.config]       # overrides a plugin's [config]
llama_server = "/opt/llama.cpp/llama-server"
threads = 8
```

Defaults are `router = "rules"` and `default_specialist = "lfm3-text"`.
Offline fixture users explicitly select mock paths and defaults. Precedence is `--config`, then
`LH_CONFIG`, then the XDG default (`$HOME/.config` if XDG_CONFIG_HOME is unset).
An explicitly named missing file is an error; a missing default file uses defaults.
Relative `plugin_path` entries are relative to the configuration file. When a config
file is overridden, its parent directory supplies the adjacent `plugins` search path.
User `[plugins.<id>.config]` settings shallowly override manifest `[config]` keys.

```toml
[self]
context_in_chat = false         # also give plain (non-agent) chats the "Harness environment" block
[service]
drain_timeout_secs = 10         # restart drain before in-flight requests are cancelled (1..=3600)
# restart_command = "systemctl --user restart lh"   # supervisor setups: run detached, then exit
# log = "/var/log/lh.log"       # reported log destination when stderr cannot be detected
```

## 10. Testing

- Unit tests per crate; integration tests spawn the real `mock-backend` binary through the
  plugin host (no models needed). `cargo test --workspace` must pass offline.
- The mock bidirectional-backpressure regression uses a test-only 15 s per-call
  deadlock watchdog, not a latency target: debug JSON/pipe I/O and parallel host
  scheduling need headroom. It still concurrently sends the full 1 MB echo request
  and the unchanged flood, requiring both flood completion and the exact echo.
  Production host timeouts are unchanged.
- The CLI agent HTTP smoke builds its mock backend and three MCP tool binaries
  with `$CARGO build --offline` before launching Python. It derives the target
  directory/profile from Cargo's `lh` executable, so a clean `CARGO_TARGET_DIR`
  needs no manual workspace prebuild. Smoke output has RAII cleanup, including
  assertion failures; a regression test checks cleanup during unwinding.
- FreeToken spawn fixtures (unit tests and `tests/process.rs`) share the test-only
  `FIXTURE_STARTUP_SECS` = 5 s budget for interpreter/server startup; the
  integration test owns its temp dir and plugin child with RAII guards.
  A separate already-listening, confirmed-loading fixture tests a 500 ms readiness
  deadline; startup delay (>1 s), failed-load registration and process-group cleanup
  remain covered. Production timeouts are unchanged. See
  [fixture hardening evidence](FREETOKEN-FIXTURE-HARDENING.md).
- CLI HTTP fixtures let the child bind port zero and use its private stderr
  bound-address announcement for readiness, rather than release/rebind plus a
  connect-only probe. Occupied-listener and sibling-teardown regressions cover
  ownership; historical resets are not conclusively attributed. See the
  [controlled reset investigation](CLI-RESET-INVESTIGATION.md).
- `scripts/smoke.sh`: real end-to-end run on CPU with d1-2B + LFM3-1.7B (Q4), a few minutes max.
  Skips cleanly with a message if `llama-server` or the models are missing.
  Python 3 prepares isolated file-source manifests and a deterministic red-square PNG.
  Also runs LFM3-VL and checks nonempty streamed answers. It never downloads;
  `LLAMA_CACHE`, `LLAMA_SERVER`, and `LH_BIN` override discovery. Each CLI process
  finishes before the next starts, including router-only then explicit text generation.
- `evals/routing.jsonl` + `lh eval routing`: labelled prompts → expected specialist; reports
  accuracy, confusion matrix, p50/p95 router latency. Measure alone (check `/proc/loadavg`).
- `scripts/smoke.sh --audio --sample /path/to/jfk.wav --out /new/output/dir`:
  sequential English/German TTS→ASR and independent JFK ASR, WAV durations,
  normalized word edit distance, wall latency and load before/after every call.
  No downloads; skips missing cached models. Output WAV/logs/JSON persist in `--out`.
  See [M3a results](M3A-RESULTS.md) for measured quality and limits.

## 11. Milestones

### M2 CPU investigation (2026-11-07, under 30 minutes)

On this Core Ultra 7 155H, b12189 selected `libggml-cpu-alderlake.so`, not a
generic CPU fallback. Short CPU-only `llama-bench -p 128 -n 0 -r 2` runs used
d1-2B Q4_0 with repacking on, no concurrent model process, at most eight threads.
Unpinned `-b 512 -t 4,6,8` yielded 80.041530 / 92.117897 / 100.476442 tok/s
(load average before 5.09/2.83/1.97, after 5.67/3.07/2.06; residual load from
the preceding sequential eval). Six physical P-cores, mask `54b` = logical CPUs
0,1,3,6,8,10 with `--cpu-strict 1`, yielded 129.312994 / 114.895230 / 111.322138
tok/s for batches 128/512/8192 (load before 3.23/2.77/2.00, after 3.74/2.89/2.05).
Topology was checked with `lscpu -e=CPU,CORE,MAXMHZ`; IDs are host-specific.

The old ~48 tok/s was not reproduced; these data do not establish its cause.
Hybrid-core placement and clock/power state are plausible contributors, not
proven diagnoses. Batch 8192 does not help a 128-token input. Two repetitions
are exploratory, not a sustained thermal benchmark. Keep portable defaults at
eight threads and batch 512, ctx 4096; on this host experiment with six physical
P-cores (e.g. launching lh under `taskset -c 0,1,3,6,8,10` and setting model
threads to six). Do not hardcode this affinity on other CPUs. Detailed routing
accuracy, smoke output, limitations and raw measurements: [M2-RESULTS.md](M2-RESULTS.md).

- **M1 – Routing core:** protocol, SDK, plugin host, mock backend, `rules` and `decision-model` routers,
  `lh plugins list`, `lh route`. Offline tests green.
- **M2 – d1 router on llama.cpp:** llama.cpp backend (load/unload/decide/chat), lfm3 pack,
  real-model `decision-model` integration, `lh run`, routing eval with ≥ 30 prompts, smoke script.
- **M3 – Specialists end-to-end:** text + vision chat streaming, `lh chat` REPL, sticky routing,
  `lh serve` OpenAI API.
- **M3a – Audio specialists:** LFM3-Audio ASR/TTS, explicit recording-question pipeline,
  audio protocol and CLI, fake/offline tests and bilingual real smoke.
- **M4 – Extended audio & tools:** realtime audio, d1-Omni router, MCP tool plugins with the
  LFM3-1.7B agent loop.
- **M4a – Developer tool plugins:** standalone filesystem, shell and web MCP
  servers; offline tests and stdio smoke.
- **M4b – Agent host:** MCP client, bounded tool loop, approvals, local skills,
  CLI prompts and HTTP agent/control/management APIs; offline and real-model smoke.
