# Routing evaluation

`lh eval routing --file evals/routing.jsonl --router d1` routes labelled cases
without generating specialist answers. Select your configuration with `--config`.
The bundled corpus has 42 cases (21 English, 21 German), using the specialist IDs
`lfm3-text`, `lfm3-vl`, and `lfm3-audio`. Use an isolated LFM3 plugin pack (without
mock specialists) for meaningful model measurements; otherwise mock specialists
also become candidates. The default CLI configuration deliberately remains offline
rules/mock, so configure the LFM3 default specialist and router explicitly.

Each JSONL object requires `prompt` and `expected`. Optional fields are `language`,
`images` (string references), `audio` (string references), and `specialist` (explicit
selection). Unknown fields are rejected. Most cases test semantic intent routing
without attachments, including requests describing future image/audio work. Two
image cases use a tiny embedded PNG; additional image/audio reference cases test
modality routing and deliberately ambiguous prompts. Two explicit cases
exercise the explicit-selection shortcut. This is a routing corpus, not a test of
image understanding or audio inference. No specialist models are loaded by eval.

The JSON report includes accuracy over **all nonblank cases** (errors count as
incorrect), a sparse expected-row/predicted-column confusion matrix, per-case
results, line-numbered errors, and `/proc/loadavg` before/after. Malformed rows and
unknown labels have no valid matrix row but remain in the total and error list.
A nonzero error count causes exit status 1 after printing the report; incorrect
predictions alone do not. Empty datasets fail.

Latency uses nearest-rank p50/p95 of the core's `router_ms`. Decision-model and
low-confidence fallback decisions share one bucket; fastpaths have a separate
bucket (core reports their latency as zero). Empty buckets report null, not zero.
Decision-model latency includes startup/load and routing overhead; it is not a
pure warmed inference measurement. Run alone and inspect load averages before
interpreting latency. See `docs/M2-RESULTS.md` for the measured development-set run.

## Other M2 commands

- `lh run --model auto "prompt"` routes, loads, and streams an answer. Explicit IDs
  skip model routing. `--router ID` overrides routing. Repeated `--image` accepts
  local PNG/JPEG/WebP/GIF paths (encoded as data URLs), HTTP(S) URLs, or image data
  URLs. Tool-call deltas go to stderr; execution of tools is not implemented.
- `lh models pull SPECIALIST_OR_ROUTER_ID` calls `backend/load`. Router keys are
  `router:ID` and pinned; specialist keys are their IDs and unpinned. This command
  verifies readiness, prints the backend result, and shuts down its plugin process;
  it leaves downloaded files cached, not a running server.

Offline verification: `cargo test --offline -p lh-cli -p lh-mock-backend` builds
and exercises the actual mock backend. The CLI-only `base64` dependency encodes
local image attachments; no HTTP/inference dependency is added to the core.
