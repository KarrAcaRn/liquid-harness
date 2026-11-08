# liquid-harness

A lightweight, plugin-based harness for **local specialist models**, written in Rust.

Instead of sending everything to one large model, `lh` routes each request to the small
model that fits the task. A decision model (Liquid AI **d1**) picks the specialist in a single
forward pass; specialists like **LFM3-1.7B** (text/agent), **LFM3-VL-2B** (vision) and
**LFM3-Audio-2B** (speech) do the work, all running locally via llama.cpp.

Everything is a plugin: inference backends, specialist models, routers and tools (MCP).
Plugins are separate processes speaking JSON-RPC over stdio, so they can be written in any
language and a crashing plugin never takes the harness down.

Status: M4c implemented: embedded browser UI, llama.cpp backend, streamed text/image answers, ASR/TTS,
model pull, routing cascade/evaluation, conversation REPL, MCP tools, bounded agent
runs with approvals/skills, and HTTP serving/management –
see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Try the offline routing core

Requires Rust 1.99+ (edition 2024). From the repository root:

```sh
cargo build --workspace --release
export PATH="$PWD/target/release:$PATH"
repo="$PWD"
export LH_PLUGIN_PATH="$repo/plugins/mock-backend:$repo/tests/fixtures/mock-pack"
# Keep real ./plugins out of this explicitly offline demo.
cd "$(mktemp -d)"
lh plugins list
lh route --explain --router mock-router "coding programming tools"
lh route --image picture.png "describe this"
lh route --audio voice.wav "transcribe this"
```

The mock backend needs no models or network. `--image`/`--audio` attach references
for routing; M1 does not open or encode these files. Plain routing defaults to
deterministic rules; `--router mock-router` exercises the JSON-RPC decision backend.
The explicitly loaded fixture pack exposes `mock-text`, `mock-vision`, and
`mock-audio`; repository defaults contain only real LFM3 specialists.

Plugin search: `LH_PLUGIN_PATH`, configured `plugin_path`, the config directory's
`plugins`, and `./plugins`. Commands resolve beside their manifest first, then beside
the running `lh` executable, then on `PATH`. Spawn errors name the plugin, command
and searched locations. Configuration is read from `$XDG_CONFIG_HOME/liquid-harness/config.toml`
(or `$HOME/.config/liquid-harness/config.toml`); override with `--config` or `LH_CONFIG`.

```sh
# Fetch build dependencies once, then all tests/builds work offline.
cargo fetch
cargo fmt --all --check
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
```

Integration tests spawn the actual mock executable, including crash/restart,
timeouts, concurrent RPC correlation, streaming, routing and configuration tests.
No test launches llama-server or downloads models. The SDK is blocking and cannot
interrupt a handler for cancellation; external router plugins are deferred.

## Run real LFM3 models

Build the whole workspace, not only `lh`. Set `PATH`/`LD_LIBRARY_PATH` for your
llama.cpp installation. On the development host:

```sh
cargo build --workspace --release
repo="$PWD"
export PATH="$repo/target/release:/opt/data/tools/llama.cpp/llama-b12189:$PATH"
export LD_LIBRARY_PATH=/opt/data/tools/llama.cpp/llama-b12189
export LLAMA_CACHE=/opt/data/models/llama-cache
export LH_CONFIG="$repo/examples/lfm3.toml"
unset LH_PLUGIN_PATH # remove any explicit offline-demo fixture paths
lh models pull d1
lh models pull lfm3-text
lh run "Explain a Rust mutex in two sentences."
lh run --image /absolute/path/photo.png "Describe this photo."
lh eval routing --file "$repo/evals/routing-tuning.jsonl"
lh eval routing --file "$repo/evals/routing-heldout.jsonl"
# Add --baseline to either command to disable the routing cascade.
```

HF sources may download weights; `models pull` verifies model readiness then
shuts down. `lh run` supports chat, transcription and speech synthesis.
Text agent completions cap at 1024 tokens per step (vision at 256), thinking disabled;
edit specialist `params` for a larger budget. Explicit model selection skips d1.

### Model lifecycle

`lh serve` and `lh chat` load the router (d1) in the background right after
start, then the specialists listed in `[lifecycle] preload` (`lfm3-text` in
`examples/lfm3.toml`); everything else loads on first use. The server answers
immediately; a request that arrives during a load waits for it instead of starting
a second copy. Models stay resident while their estimated RAM (GGUF + projector +
KV cache) fits `memory_budget` (default 60 % of available memory at startup);
beyond that the least recently used specialist is unloaded, never the router.
A crashed llama-server is restarted by the next request.

```sh
curl -s localhost:8484/lh/v1/status | jq '.models[] | {id, state, progress, ram_estimate_bytes}'
```

Streamed chats send `: lh model_loading …` keepalive comments while waiting (and
`event: model_loading` with `"lh_events": true`); the web UI shows "Loading
lfm3-vl… 40%". Stopping `lh serve` (Ctrl-C or SIGTERM) stops every llama-server it
started. Details: [Model lifecycle](docs/ARCHITECTURE.md#model-lifecycle-m5a).

### Agent tools

The text specialist executes filesystem, shell and web tools in `lh run` and
`lh chat`, with a 20-step limit and approval prompts for writes/edits/commands.
Run from the intended workspace, not a directory containing sensitive files.
When outside this checkout, set `LH_PLUGIN_PATH="$repo/plugins"` and
`LH_SKILL_PATH="$repo/skills"`. The tools are not an OS sandbox.

```sh
lh run --model lfm3-text "Read the failing test, fix the code and run the test."
lh chat --model lfm3-text
```

Configuration `[agent] approval="auto-edit"` permits file edits but still asks
for commands; `"yolo"` permits all tools. Default `"ask"` denies unanswered and
noninteractive approvals. The HTTP host-execution endpoint is
`POST /lh/v1/agent/runs`; OpenAI chat retains client-owned tool execution.
See [tools/API contracts](docs/tools.md), [skills](docs/skills.md), and
[measured agent results and limitations](docs/M4B-RESULTS.md).

### Browser UI

With the backend/plugins configured as above, start `lh serve` and open
http://127.0.0.1:8484/ in your browser. Chat, Plugins (including specialists and
router configuration), Tools, Skills and Status are served on the same port.
No Node build, CDN, internet connection or frontend package installation is needed.

Chat stores conversations and attachments in this browser's localStorage. New,
Rename, Delete and Export manage that local history; it is not server-side storage
or encrypted storage. Select Auto route or a specialist. Agent mode explicitly
enables tool execution with Allow/Deny/Always approvals and cancellation. Always
means identical tool/arguments in this run only. Ordinary chat does not execute
local tools. Expand route and agent details to inspect decisions and tool results.
In the message field, Enter inserts a newline and Ctrl+Enter sends (same as Send).

Attach images/audio or record a short clip (localhost/HTTPS and microphone
permission required). Non-WAV recordings need backend ffmpeg support. The Speak
replies dropdown offers `<none>` (default, no automatic speech) followed by the
speak IDs of the selected model (same plugin pack; every speak specialist for Auto
route); with an ID chosen each answer is spoken with exactly that specialist. The
list follows model switches and a choice the new model does not offer falls back
to `<none>`. The choice is kept per tab in memory only; a reload starts at
`<none>`. If autoplay is blocked, press Play. Attachments are capped at 4 MiB per turn and
requests at 6 MiB including
history. Export and start a new conversation when storage or context limits arise.

When `server_token_env` is configured, expand Connection & authentication and
enter the bearer token. It stays in sessionStorage for this tab, not chat history.
The immutable UI shell is public so token entry can work; all API data/control
endpoints remain authenticated. Keep the default localhost binding: this server
has no TLS or multi-user isolation. UI responses carry a restrictive same-origin
CSP, and Markdown HTML is escaped. No wildcard CORS is enabled.

The default `web` Cargo feature includes `server`. API-only build:
`cargo build -p lh-cli --no-default-features --features server`; no HTTP/UI:
`cargo build -p lh-cli --no-default-features`.

Offline browser fixture after `cargo build --workspace`:
`python3 scripts/web-smoke-server.py` (http://127.0.0.1:8485/; Ctrl-C to stop).
Use `web-agent` with Agent mode for a scripted approval. JS checks run from Rust
tests if Node is installed, or directly with `node scripts/test-web.mjs`.
See [M4c verification and limits](docs/M4C-RESULTS.md).

### Audio

```sh
lh models pull lfm3-audio
lh run --audio recording.wav --language en
lh run --audio recording.wav --audio-question "Summarize this recording."
lh run --speak-out greeting.wav --language de "Guten Morgen."
lh run "read this aloud: Hello world" # unique output WAV, path on stdout
```

The shipped audio specialist transcribes; asking a question requires the explicit
`--audio-question` pipeline to the default text specialist. TTS speaks the prompt
verbatim. `--voice female|male` selects the llama-tts voice; output files are never
overwritten. Both audio specialists use the audio projector. Install `llama-tts`
beside configured `llama_server`, or configure `plugins.llamacpp.config.llama_tts`.
WAV works directly; other formats require ffmpeg on PATH or the backend `ffmpeg`
setting. Short clips below 5 MiB are supported. See
[audio lifecycle/limits](plugins/backend-llamacpp/AUDIO.md).

`scripts/smoke.sh --audio --sample /path/to/jfk.wav --out /new/output/directory`
checks English/German TTS→ASR plus an independent recording, with word-error
scores, WAV durations, elapsed time and host load. It uses cached audio Q4_K_M
weights and a Q8_0 projector; never downloads. [Measured results](docs/M3A-RESULTS.md).

`scripts/smoke.sh` is offline-only: after `cargo build --workspace`, it uses cached
Q4 d1/text/VL weights plus the VL F16 projector, generates a small test PNG, and
checks routing and streamed answers. Missing binaries/models produce a clean
`SKIP`; existing llama-server processes produce an error rather than overlapping
measurements. Requires Python 3. See [M2 results](docs/M2-RESULTS.md) and
[evaluation details](evals/README.md).

## Conversations and OpenAI-compatible serving

With the real-model environment above:

```sh
lh chat --model auto
# Plain line REPL commands:
# /image /absolute/path/photo.png   attach to the next user turn
# /audio /absolute/path/clip.wav    attach to the next user turn
# /speak                           toggle WAV replies (paths printed, no playback)
# /model lfm3-text                 force a specialist; /model auto restores routing
# /explain                        last routing decision and ordered stages
# /reset                          clear history, attachments and routing session
# /quit                           shut down plugins and exit
lh serve                         # default 127.0.0.1:8484
```

History includes previous user/assistant messages. Images remain available for
follow-up questions; `/reset` clears them. Explicitly switching to a text model
omits old incompatible media from its request while preserving textual history.
New incompatible attachments are rejected, not silently dropped. Audio chat
transcribes then answers using the default text specialist (vision if pictures
are also present); the REPL keeps transcripts rather than retranscribing old clips.
`/speak` speaks the generated answer. Direct speech routing speaks the user text.

```sh
curl http://127.0.0.1:8484/v1/models
curl -N http://127.0.0.1:8484/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"auto","stream":true,"messages":[{"role":"user","content":"Explain a Rust mutex briefly."}]}'
curl http://127.0.0.1:8484/v1/audio/transcriptions \
  -F model=auto -F file=@recording.wav -F language=en
curl http://127.0.0.1:8484/v1/audio/speech \
  -H 'Content-Type: application/json' \
  -d '{"model":"auto","input":"Hello world","response_format":"wav"}' -o hello.wav
```

Chat accepts OpenAI `image_url` base64 data URLs and `input_audio` parts containing
base64 `data` and a `format` such as `wav` or `mp3`. Remote URLs and filesystem
references are rejected by HTTP endpoints. Send the complete history on every
HTTP request: HTTP has no server-side conversation sessions; the routing cache
still applies. Responses expose `x-lh-route: ID;p=CONFIDENCE;stage=REASON`.
The audio pipeline header identifies the input/ASR specialist, not the second
chat stage. `/v1/route` accepts `RouteInput`; `/v1/decide` accepts `state`,
`questions`, optional data-URL `images`, and optional `model` naming a router
(default configured router) or a decide specialist.

Optional authentication is enabled in the harness config, with the token supplied
only through the named environment variable:

```toml
server_token_env = "LH_API_TOKEN"
```

When configured, a missing/empty variable prevents startup and every endpoint
requires `Authorization: Bearer ...`. Keep the default localhost bind; nonlocal
binding has no built-in TLS. Ctrl-C shuts down owned backends. HTTP/1 support is
the default `server` Cargo feature; omit it with
`cargo build -p lh-cli --release --no-default-features`.

### Hermes provider example (documentation only)

Using Hermes' [custom-provider model aliases](https://hermes-agent.nousresearch.com/docs/integrations/providers),
an example configuration is:

```yaml
model_aliases:
  liquid:
    provider: custom
    model: auto
    base_url: http://127.0.0.1:8484/v1
    api_key: local  # non-secret placeholder when harness authentication is off
```

Select `/model liquid` in Hermes. With authentication enabled, replace the
placeholder with `key_env: LH_API_TOKEN` and supply the same token securely to
both processes. No Hermes configuration is changed by this repository or its
tests. For Open WebUI use the same OpenAI base URL/model; containers need an
explicit host networking/reachability arrangement rather than container localhost.
This is an OpenAI subset, not a claim of full Hermes/Open WebUI end-to-end testing.
Tool definitions/deltas pass through; tool execution remains the client's job.

Run `LH_BIN="$PWD/target/release/lh" python3 scripts/smoke_chat.py` for bounded,
cached-model curl and three-turn REPL acceptance. See [M3b results](docs/M3B-RESULTS.md)
for routes, timings, binary sizes and compatibility limitations.

## License

Code: MIT OR Apache-2.0. Model weights are not part of this repository and are governed by
their own licenses (LFM3/d1: LFM Open License v1.0).
