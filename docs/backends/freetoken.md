# FreeToken backend

`lh-backend-freetoken` adapts FreeToken's OpenAI-compatible chat endpoint. It
advertises only `capabilities = ["chat"]`; `backend/decide` always returns
`-32003`. It does not implement d1 `/v1/systemone`, a GGUF loader, or inference
inside the harness.

The bundled `plugins/backend-freetoken/plugin.toml` is **disabled by default**.
Enable it explicitly with `[plugins.freetoken] enabled = true` in the user config.
Install/build `lh-backend-freetoken` alongside `lh`, in the plugin directory, or
on `PATH`. URL mode needs no local FreeToken installation. Spawn mode requires
an independently installed `ft` executable and its own supported hardware and
model dependencies; the harness does not install Python packages or models.

## Configuration shape

The following is the plugin's `[config]` map (in its manifest, or the equivalent
backend configuration override). `models` is keyed by the exact specialist
`model.source` string; it is not keyed by the runtime load key. Arbitrary source
names using `hf:`, `file:` or `freetoken:` are accepted by core and must match exactly. Each entry must contain exactly
one of `url` or `spawn`.

```toml
[config]
max_resident = 2
idle_ttl_secs = 600
load_timeout_secs = 600
request_timeout_secs = 600
host_timeout_secs = 660 # consumed by the core, not sent to the backend

# Existing server: base URL, without /v1/chat/completions.
[config.models."freetoken:remote"]
url = "https://inference.example.org"
model = "LFM3-1.7B"              # optional API model ID (see /v1/models)
token_env = "FREETOKEN_API_TOKEN" # optional: name, never the secret itself

# Managed local process; model_path is passed literally to ft.
[config.models."freetoken:local".spawn]
command = "ft"                  # default; executable, not a shell command
venv = "~/.venvs/freetoken"      # optional
model_path = "organization/model-id"
args = []                       # extra ft serve flags, not shell fragments

[config.models."freetoken:local".spawn.env]
CUDA_VISIBLE_DEVICES = "0"      # example only; hardware selection is operator-owned
```

A specialist using the second entry specifies:

```toml
backend = "freetoken"
model = { source = "freetoken:local" }
```

Optional `token_env` belongs to the model entry (a sibling of `url` or `spawn`).
`base_url` is an alias for `url`. The optional `model` sets the required OpenAI
request model ID; otherwise it defaults to `spawn.model_path`, or the source
with its `hf:` prefix removed. Set it explicitly if the server uses an alias.
The token is resolved from the **plugin's environment at load time**, not `spawn.env`.
A missing, empty, or invalid bearer token is an explicit load error. The token
is used on both readiness and chat requests, never returned by status or
included in HTTP error messages. Use HTTPS for remote tokens; plain HTTP is
appropriate only on trusted local networks. HTTP redirects are disabled.
URLs must use HTTP(S), with no embedded credentials, query or fragment. A base
path is permitted and is preserved when appending API routes.

Configuration rejects unknown keys, zero limits/timeouts, missing endpoint
configuration, and ambiguous URL-plus-spawn entries. `ModelSpec`'s llama.cpp
`ctx`, `batch`, `threads`, and `mmproj` fields are rejected; configure supported
FreeToken options through `spawn.args` instead. Sampling parameters and tool
schemas are passed to the chat API without FreeToken-specific translation.

## Spawn and readiness

The backend chooses an ephemeral localhost port, releases the listener, then
executes the configured command without a shell:

```text
ft serve --model-path organization/model-id --port <ephemeral-port> <args...>
```

`spawn.args` may not override `--port` or `--model-path`. With `venv` and a bare
command name, the executable is `<venv>/bin/<command>`. An explicit command
path is used as-is (with `~/` expansion). `venv` also sets `VIRTUAL_ENV` and
prepends its `bin` directory to `PATH`; `spawn.env` is applied afterwards and
can override inherited environment settings. No shell activation is needed.
Child stdin/stdout are disconnected so server output cannot corrupt JSON-RPC;
child stderr is inherited for diagnostics. Configure the actual executable,
not a daemonizing wrapper. Configure the server's bind address appropriately
for your deployment; the adapter connects on `127.0.0.1` in spawn mode.

Both modes poll **`GET /v1/cache/status`** until its JSON body has
`"state": "serving"`. A successful HTTP response, `"loading"` state, or
`/health` availability is not readiness. The backend never probes `/health`.
It emits `$/progress` while waiting (no invented percentage), and fails when
`load_timeout_secs` elapses, the direct child exits, or authentication is
rejected. Individual readiness probes are capped at one second and at the
remaining load deadline. The ephemeral-port release/spawn race can still
produce a bind failure and therefore a load failure.

## Lifecycle and chat

- A load key is idempotent for the same `ModelSpec`; a different source under
  an already-loaded key requires unload first. A repeated load can pin a model.
- Least-recently-used unpinned residents are evicted at `max_resident`. If all
  slots are pinned, load fails with `-32002`. A 250 ms background worker evicts
  idle unpinned residents. Active loads/inference hold the state lock and cannot
  be evicted. Pins survive LRU/TTL, not explicit unload or shutdown.
- In **URL mode**, eviction/unload removes only the harness registration. The
  externally managed server is never stopped or remotely unloaded.
- In **spawn mode**, unload, eviction, shutdown, EOF, and failed readiness kill
  the server's process group and reap the direct child. Linux parent-death
  `SIGKILL` also kills the direct server after a plugin crash. Descendants of
  wrappers are not guaranteed cleanup after the plugin itself receives
  `SIGKILL`; avoid wrappers that daemonize or escape the process group.
- `POST /v1/chat/completions` supports ordinary JSON replies and SSE. Streams
  emit `$/stream` notifications and aggregate content, tool calls, finish
  reason and usage using the shared protocol SSE parser. Truncated streams,
  missing `[DONE]`/finish reason, malformed events and oversized streams fail.
- HTTP 429/503 map to `-32002`; other HTTP/transport/readiness failures map to
  `-32001`; invalid configuration/request fields map to `-32602`.
- Status exposes load key, PID/port (null for URL mode), pin and idle age,
  never the bearer token. Calls are serial; SDK cancellation cannot interrupt
  an active blocking load or inference. Keep the core's host timeout longer
  than the backend load/request timeout.

## GPU deployment and LFM3 limitations

Use the [next-speed fork](https://github.com/KarrAcaRn/FreeToken-ByAI/tree/next-speed)
and follow its installation instructions for NVIDIA hardware, CUDA 13 and a
matching PyTorch build. This harness host has no NVIDIA GPU; no installation or
real GPU inference was attempted here. Check `nvidia-smi` and `ft --version` on
the GPU machine before configuring the adapter.

Supported LFM3 sources are safetensors checkpoints `LiquidAI/LFM3-1.7B` (bf16),
`LiquidAI/LFM3-1.7B-FP8` and `LiquidAI/LFM3-1.7B-NVFP4`. LFM3 GGUF, VL, Omni,
audio and d1 are not supported by this adapter. It rejects image/audio content
and cannot be used as a decision-model router. Keep d1 on llama.cpp or use rules.

For an existing server, run on the GPU machine (adjust paths/environment to the
installation; protect remote access with HTTPS and authentication):

```sh
ft serve --model-path LiquidAI/LFM3-1.7B-NVFP4 --port 19500 \
  --online-quant none --max-seq-len-override 16384 --max-running-requests 4
```

`--online-quant none` is especially important for bf16: the fork otherwise
defaults to online FP8. Wait for `/v1/cache/status` to report `state=serving`.
The shipped text specialist prefers FreeToken's NVFP4 source only when enabled.
For URL mode, a complete user-config addition is:

```toml
[plugins.freetoken]
enabled = true
[plugins.freetoken.config.models."hf:LiquidAI/LFM3-1.7B-NVFP4"]
base_url = "http://127.0.0.1:19500"
model = "LFM3-1.7B-NVFP4"
# token_env = "FREETOKEN_API_KEY" # if the server uses authentication
```

Alternatively replace that model entry with this managed-spawn configuration:

```toml
[plugins.freetoken]
enabled = true
[plugins.freetoken.config.models."hf:LiquidAI/LFM3-1.7B-NVFP4"]
model = "LFM3-1.7B-NVFP4"
# token_env = "FREETOKEN_API_KEY"
[plugins.freetoken.config.models."hf:LiquidAI/LFM3-1.7B-NVFP4".spawn]
command = "ft"
venv = "/srv/FreeToken-ByAI/.venv"
model_path = "LiquidAI/LFM3-1.7B-NVFP4"
args = ["--online-quant", "none", "--max-seq-len-override", "16384", "--max-running-requests", "4"]
[plugins.freetoken.config.models."hf:LiquidAI/LFM3-1.7B-NVFP4".spawn.env]
CUDA_HOME = "/usr/local/cuda"
PYTHONPATH = "/srv/FreeToken-ByAI/python"
# PATH can also be overridden here; otherwise venv/bin is prepended to inherited PATH.
```

Keep API keys in the environment, not `spawn.env` or args. FreeToken accepts
`FREETOKEN_API_KEY` from its inherited environment; `token_env` tells the client
to use the same value. For bf16, change both specialist source and config key
to `hf:LiquidAI/LFM3-1.7B`, plus the API model/model_path accordingly.

Run `lh run --model lfm3-text 'Hello'` after building the workspace. Load failures
fall back to the next enabled/capable backend (llama.cpp in the bundled pack),
and stderr records the actual backend. Inference errors are not retried on a
different backend, avoiding duplicate streamed output. `lh route` remains
route-only and reports `backend: null` until a model is actually loaded.

## Offline verification

```sh
cargo test -p lh-backend-freetoken --offline
cargo clippy -p lh-backend-freetoken --all-targets --offline -- -D warnings
```

Tests use actual loopback HTTP listeners and a Python 3 standard-library fake
`ft` executable. They require Linux/Python 3 but no FreeToken package, GPU,
models, Internet access, or inference. Tests cover readiness state transitions,
auth headers, JSON chat, SSE text/tools/usage, malformed/truncated streams,
HTTP error mapping, disabled manifest configuration, capacity/pinning/TTL,
spawn arguments/venv/environment and process-group cleanup with direct-child
reaping. These are adapter tests, not a claim of real GPU model compatibility.
