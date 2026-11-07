# M4b agent-host results

## Verification

Linux host, Rust 1.99.1, llama.cpp b12189. No new Cargo dependency was added.
The MCP host shares the backend actor's crash isolation/restart accounting;
MCP uses its own handshake and EOF shutdown. Core has no HTTP client/inference.

Checks run:

- `cargo fmt --all --check`
- `cargo clippy --offline --workspace --all-targets -- -D warnings`
- `cargo test --offline --workspace`
- `cargo build --offline --workspace`
- `python3 scripts/agent-smoke.py --out <scratch-output>`
- `python3 scripts/agent-smoke.py --real --out docs/m4b-smoke`

Offline tests cover MCP negotiation/EOF/cwd/config, pagination/namespaces,
timeout/crash/restart limits; skills metadata/discovery failures; scripted full
agent loops with skill loading, approval allow/deny/timeout, cancellation,
malformed batches (no side effects), output/step budgets, tool timeout/reset;
HTTP management/control and auth; and real pseudo-terminal `lh run` y/n and
`lh chat` always prompts. HTTP uses actual filesystem MCP binaries. No offline
test requires a model or external network. Existing routing/audio/server suites
remain green. `always` is tested as an exact request, run-local grant.

## Model format, not a handwritten model parser

Sources inspected:

- https://huggingface.co/LiquidAI/LFM3-1.7B
- https://huggingface.co/LiquidAI/LFM3-1.7B/raw/main/chat_template.jinja

The checkpoint's template renders calls as XML-like `<tool_call>`,
`<function=NAME>`, `<parameter=NAME>` blocks. Its direct-template interface
requires argument mappings, not nonempty JSON-encoded argument strings.
llama-server b12189 performs that model-specific rendering/parsing. Its OpenAI
HTTP response, observed in both smokes, is the usual
`tool_calls:[{id,type:"function",function:{name,arguments:"{...}"}}]` shape.
The existing backend forwards/aggregates this; the core validates JSON argument
strings and correlates `role:"tool"` messages using `tool_call_id`. It does not
execute XML embedded in plain assistant content. The coding transcript includes
two calls in one assistant message and valid continuation after their results.

## Sequential real smoke

Cached LFM3-1.7B Q4_K_M, CPU, eight threads, ctx 32768. Smoke overrides:
temperature 0.1, top_k 50, repeat_penalty 1.1, max_tokens 512 per step,
max_steps 6, thinking disabled. Both prompts ran in separate scratch Git repos
with the same tiny initially failing Python test. Destructive operations were
approved through the HTTP endpoint; not bypassed via yolo. No other llama-server
was present at launch; cases ran sequentially. Logs record model PIDs 2166163
and 2166567, both gone after shutdown; a final `pgrep -x llama-server` was empty.
No latency/throughput benchmark is claimed.

| Case | Observed behavior | Result |
|---|---|---|
| Web fact → file | web_search, write_file, final answer; 3 model steps | `fact.txt` really contained October 7, 2024 and https://www.python.org/downloads/release/python-3130/ |
| Fix failing Python test | read_file twice (same step), run_command (exit 1), edit_file, run_command (exit 0), final answer; 5 model steps | Changed only `return a - b` to `return a + b`; independent post-run test printed PASS, exit 0 |

Web used the returned search snippets, not `web_fetch`; it did not independently
open the cited release page. Its final prose's additional-source corroboration
is not a separately verified claim. The unrelated failing Python fixture stays
failing in the web case, intentionally. Neither smoke called `skill_view`; the
index was present, but on-demand skill use is demonstrated by the offline
scripted test, not by these model runs.

Full events (including actual calls/results/final answers), file contents,
independent test exit codes, model memory/status, shutdown checks and backend log
excerpts:

- [Web transcript](m4b-smoke/web.json)
- [Coding transcript](m4b-smoke/code.json)

Load average (1/5/15 minutes):

- Web: before 2.70 / 2.88 / 2.87; after 9.06 / 5.33 / 3.77.
- Coding: before 8.50 / 5.27 / 3.76; after 9.15 / 6.66 / 4.49.

The second run starts with residual load average from the first, not overlapping
model processes. Functional success is the measurement; do not infer isolated
latency from this sequence. Full offline build/tests ran after model shutdown.

## Prompt and memory overhead

Both cases used eight MCP tools plus the two skill built-ins. llama-server's
`/apply-template` and `/tokenize` measured a one-user-message baseline (11 tokens)
versus the same message plus actual tool schemas, the compact four-skill index
and its fixed system instructions (993 tokens): **982 additional tokens**.
This includes the template's tool instructions, not merely JSON tokenization.
Measurements use the server's default generation-template settings and are an
approximation of fixed overhead, not per-run usage attribution. The backend's
actual prompt/completion counts are retained in each response transcript.

For comparison, serialized management responses were 3952 bytes for tools
(including provenance metadata and built-ins) and 648 bytes for skills (including
advisory tool names). These are not claimed as actual prompt byte counts.

Host had 17551 MiB available before the smoke. At ctx 32768, llama-server RSS
was 2373200 KiB (web) / 2377628 KiB (coding), sampled after inference;
VmSize was 4211108 / 4214296 KiB. This supported raising the shipped text pack
from 4096 to 32768 on this host. Not a peak-RSS or multi-model capacity test.

## Known limits

- Two tiny successes do not establish reliable autonomous coding. The official
  model card explicitly does not recommend agentic coding or complex multi-step
  reasoning for this small model. No broad benchmark or long soak was run.
- MCP annotations and local skills are trusted plugin/config data. Shell retains
  user permissions/environment; filesystem guards are not an atomic sandbox.
  Wildcard command allow patterns can include shell operators; prefer exact
  commands. Process termination cannot roll back writes or guarantee cleanup of
  escaped descendants (same underlying tool-plugin limitation as M4a).
- One engine execution at a time. Management reads/new agent runs return 409
  while busy. Approve/cancel remain available. Loading precedes run creation;
  active model inference drains before cancellation takes effect. No durable
  runs, reconnection/replay, or per-user policy isolation.
- OpenAI chat remains client-owned tool execution. Host agents use the explicit
  harness API, or CLI agent-enabled specialists. HTTP audio→agent composition
  is not implemented by the agent endpoint; existing audio endpoints remain.
- Only tool-result bodies are byte-trimmed. Long user/assistant histories and
  many schemas can still exceed model context; there is no token-aware summary.
  Truncated tool output is visible text and need not remain valid JSON.
- Skills accept the documented restricted YAML frontmatter subset, not arbitrary
  YAML or every Hermes metadata extension. Extra metadata/structures fail loudly.
- Tool discovery lists every enabled tool plugin before selection for stable
  namespaces, so an unrelated broken enabled server can prevent discovery.
  MCP resources/prompts, dynamic list changes, and in-protocol cancellation are
  outside this tools-only implementation.
- Real smokes were recorded before final history-index deduplication, conservative
  output-budget accounting, transport-timeout default and completion-limit guard
  cleanups; final offline tests exercise those changes. The model wire/dispatch
  path and successful smoke requests were unchanged.
