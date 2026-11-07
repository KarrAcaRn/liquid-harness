# liquid-harness – Architecture

Status: M2 and M2b implemented (llama.cpp and opt-in FreeToken backends,
capability-aware backend fallback, streaming one-shot CLI, routing evaluation).
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
max_resident = 2                # LRU-unload beyond this
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
| `backend/chat`          | `{key, messages, tools?, params, stream: bool}`             | `{message, finish_reason, usage}` |
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
llama.cpp/mock return `["chat", "decide", "stream"]`. The host also accepts the
legacy boolean map and normalizes true entries. Capabilities are refreshed on
every process generation. It rejects unsupported chat/decide calls before they
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

## 5. Routing (the core)

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
and usage, and bounds total input to 8 MiB.

The LFM3 pack enables text-only d1 routing: image/audio attachments select their
sole compatible specialist without loading d1 or a router projector. Audio is
manifest/routing-only; `lh run` rejects non-chat specialist tasks until M4.
LFM3 chat disables thinking by default to avoid an invisible reasoning-only
answer exhausting the short completion budget (256 tokens, overridable in pack).

Routing is the central feature and is built first.

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
- Sticky routing: within one conversation the previous specialist is kept unless
  the new turn changes modality or the router is ≥ 0.8 confident in another one.
  M1 exposes this as a pure function; conversation state and pre-call sticky shortcuts
  belong to M3. Explicit specialists still undergo modality validation.
- If the configured default is not a modality-compatible candidate, M1 uses the first
  compatible ID in lexical order. Low-confidence decisions preserve the winning model
  probability and full original score distribution, even when the specialist is replaced.
  The router load is idempotent and requested before each decision, with `pinned: true`,
  so a restarted backend can recover its state. Exactly one decide call follows the load.
  M1 routes a single user turn; summaries and optional extra questions remain future work.

## 6. Crates

```
crates/lh-protocol    JSON-RPC envelopes, manifest structs, DTOs (chat, decide, route).
                      Depends on serde/serde_json/toml only. No async.
crates/lh-plugin-sdk  Write Rust plugins in ~30 lines: blocking stdio loop, method
                      dispatch, stream/log helpers. std + lh-protocol only.
crates/lh-core        Plugin discovery + host (tokio process/io), registry, router
                      engine, orchestrator, config. No HTTP.
crates/lh-cli         Binary `lh`. M2 adds run, models pull and eval routing. HTTP server
                      and its feature flags are deferred to M3.
plugins/backend-llamacpp  Binary `lh-backend-llamacpp`: spawns/stops llama-server per
                      model on a free localhost port, health-waits, LRU/TTL unload,
                      proxies chat (SSE → `$/stream`) and decide (`/v1/systemone`).
plugins/backend-freetoken Binary `lh-backend-freetoken`: URL or managed ft serve,
                      readiness, LRU/TTL, chat/SSE only; explicitly opt-in.
plugins/mock-backend  Deterministic backend (Unicode echo/three stream chunks,
                      decide via case-insensitive word overlap, with normalized scores).
plugins/mock-pack     Offline text/vision/audio specialists and mock-router fixture.
plugins/lfm3          Manifest-only specialist + router pack.
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
lh chat  [--model ID|auto]           # REPL; /model, /explain, /reset
lh serve [--bind 127.0.0.1:8484]     # OpenAI-compatible API
lh models pull <specialist-id>       # pre-download a specialist's model
```

## 8. HTTP API (`lh serve`)

OpenAI-compatible subset so Hermes, Open WebUI, etc. can use the harness as a provider:
- `GET /v1/models` → `auto` + every specialist id.
- `POST /v1/chat/completions` (stream and non-stream). `model: "auto"` routes;
  a specialist id forces it. Images via `image_url` data URLs, audio via `input_audio`.
- `POST /v1/route` → RouteDecision (harness extension).
- `POST /v1/decide` → passthrough to the router/decision model (harness extension).
- Bind to localhost by default; optional bearer token from config.

## 9. Configuration

`$XDG_CONFIG_HOME/liquid-harness/config.toml` (overridable with `--config`/`LH_CONFIG`):

```toml
default_specialist = "lfm3-text"
router = "d1"                   # router id, or "rules"
plugin_path = []                # extra plugin dirs
[plugins.llamacpp.config]       # overrides a plugin's [config]
llama_server = "/opt/llama.cpp/llama-server"
threads = 8
```

M1 defaults to `router = "rules"` and `default_specialist = "mock-text"` so the
included fixture pack works without configuration. Precedence is `--config`, then
`LH_CONFIG`, then the XDG default (`$HOME/.config` if XDG_CONFIG_HOME is unset).
An explicitly named missing file is an error; a missing default file uses defaults.
Relative `plugin_path` entries are relative to the configuration file. When a config
file is overridden, its parent directory supplies the adjacent `plugins` search path.
User `[plugins.<id>.config]` settings shallowly override manifest `[config]` keys.

## 10. Testing

- Unit tests per crate; integration tests spawn the real `mock-backend` binary through the
  plugin host (no models needed). `cargo test --workspace` must pass offline.
- `scripts/smoke.sh`: real end-to-end run on CPU with d1-2B + LFM3-1.7B (Q4), a few minutes max.
  Skips cleanly with a message if `llama-server` or the models are missing.
  Python 3 prepares isolated file-source manifests and a deterministic red-square PNG.
  Also runs LFM3-VL and checks nonempty streamed answers. It never downloads;
  `LLAMA_CACHE`, `LLAMA_SERVER`, and `LH_BIN` override discovery. Each CLI process
  finishes before the next starts, including router-only then explicit text generation.
- `evals/routing.jsonl` + `lh eval routing`: labelled prompts → expected specialist; reports
  accuracy, confusion matrix, p50/p95 router latency. Measure alone (check `/proc/loadavg`).

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
- **M4 – Audio & tools:** LFM3-Audio (ASR/TTS), d1-Omni router, MCP tool plugins with the
  LFM3-1.7B agent loop.
