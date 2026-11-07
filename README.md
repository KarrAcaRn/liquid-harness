# liquid-harness

A lightweight, plugin-based harness for **local specialist models**, written in Rust.

Instead of sending everything to one large model, `lh` routes each request to the small
model that fits the task. A decision model (Liquid AI **d1**) picks the specialist in a single
forward pass; specialists like **LFM3-1.7B** (text/agent), **LFM3-VL-2B** (vision) and
**LFM3-Audio-2B** (speech) do the work, all running locally via llama.cpp.

Everything is a plugin: inference backends, specialist models, routers and tools (MCP).
Plugins are separate processes speaking JSON-RPC over stdio, so they can be written in any
language and a crashing plugin never takes the harness down.

Status: M1 offline routing core implemented. Real inference, chat and HTTP serving
are later milestones – see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Try the offline routing core

Requires Rust 1.99+ (edition 2024). From the repository root:

```sh
cargo build --workspace --release
export PATH="$PWD/target/release:$PATH"
lh plugins list
lh route --explain --router mock-router "coding programming tools"
lh route --image picture.png "describe this"
lh route --audio voice.wav "transcribe this"
```

The mock backend needs no models or network. `--image`/`--audio` attach references
for routing; M1 does not open or encode these files. Plain routing defaults to
deterministic rules; `--router mock-router` exercises the JSON-RPC decision backend.
The built-in pack exposes `mock-text`, `mock-vision`, and `mock-audio`.

Plugin search: `LH_PLUGIN_PATH`, configured `plugin_path`, the config directory's
`plugins`, and `./plugins`. Commands resolve beside their manifest first, then on
`PATH`. Configuration is read from `$XDG_CONFIG_HOME/liquid-harness/config.toml`
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

## License

Code: MIT OR Apache-2.0. Model weights are not part of this repository and are governed by
their own licenses (LFM3/d1: LFM Open License v1.0).
