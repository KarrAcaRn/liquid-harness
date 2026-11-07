# liquid-harness

A lightweight, plugin-based harness for **local specialist models**, written in Rust.

Instead of sending everything to one large model, `lh` routes each request to the small
model that fits the task. A decision model (Liquid AI **d1**) picks the specialist in a single
forward pass; specialists like **LFM3-1.7B** (text/agent), **LFM3-VL-2B** (vision) and
**LFM3-Audio-2B** (speech) do the work, all running locally via llama.cpp.

Everything is a plugin: inference backends, specialist models, routers and tools (MCP).
Plugins are separate processes speaking JSON-RPC over stdio, so they can be written in any
language and a crashing plugin never takes the harness down.

Status: early design – see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## License

Code: MIT OR Apache-2.0. Model weights are not part of this repository and are governed by
their own licenses (LFM3/d1: LFM Open License v1.0).
