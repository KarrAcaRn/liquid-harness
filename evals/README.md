# Routing evaluation

Run each set twice, with the same LFM3 configuration:

```sh
lh --config examples/lfm3.toml eval routing --file evals/routing-tuning.jsonl --baseline
lh --config examples/lfm3.toml eval routing --file evals/routing-tuning.jsonl
lh --config examples/lfm3.toml eval routing --file evals/routing-heldout.jsonl --baseline
lh --config examples/lfm3.toml eval routing --file evals/routing-heldout.jsonl
```

`--baseline` disables optional cascade shortcuts, including requirements and the
single-candidate shortcut. Explicit selection and input-modality safety remain.
Default discovery now excludes mock specialists; fixtures require an explicit
`LH_PLUGIN_PATH`. Default routing remains offline `rules`; real evaluation must
configure `d1`, as above. No specialist inference is performed.

## Corpora and freezing

- `routing.jsonl`: original M2 tuning corpus, retained unchanged for provenance.
- `routing-tuning.jsonl`: the same 42 prompts, with missing image/audio references
  filled for media-analysis cases and four TTS labels changed to `lfm3-speak`.
  These are necessary schema/contract migrations, not new prompts or post-result
  label tuning. Both baseline and cascade use this identical migrated corpus.
- `routing-heldout.jsonl`: 40 independent cases, 20 EN and 20 DE, authored by a
  separate worker without reading the hint lists, specialist pack or tuning
  prompts. Includes eight two-turn sessions and ambiguous intent. Frozen before
  the production hints were written; no post-evaluation edits or hint tuning.
  SHA-256: `d12f70a5fbaf7841d06f335733bbc861da3c493872871b9cc7cf1cec4565dfd2`.

Each object requires `prompt`, `expected`. Optional: `language`, `images`, `audio`,
`specialist`, `session`. Unknown fields are rejected. Session IDs group successful
turns; omitted media fields inherit previous attachments, explicit `[]` clears
that modality. Independent rows have fresh session state. References test routing,
not media comprehension: dummy references need not exist for a single-candidate
shortcut, but are not cacheable. Baseline and cascade share conversation handling.

Reports include accuracy (errors count as incorrect), sparse confusion matrix,
model call count/rate, winning-stage counts, ordered traces, per-case decisions,
errors, and `/proc/loadavg` before/after. Nonzero error count exits 1 after printing
JSON; incorrect predictions alone do not. Empty datasets fail. Invalid rows
remain in the accuracy denominator but have no routing latency sample.

`latency.total` is nearest-rank p50/p95 wall routing latency in milliseconds,
measured at microsecond precision. It includes startup/load and attempted routes.
The legacy `decision_model` and `fastpaths` groups use integer `router_ms`.
Empty buckets report null. Run one model job at a time and report load with every
latency number; these are not pure warm-inference or controlled CPU benchmarks.

`python3 scripts/eval_m2c.py` reproduces the sequential measurements from cached
weights, verifies the frozen held-out hash and refuses existing llama-server
processes. `--only heldout-cascade` (or another set/mode pair) runs one batch.
All four batches can exceed an interactive tool timeout; use a tracked background
process. Each CLI shuts down its backend before the next batch. Results and
limitations: [M2C-RESULTS.md](../docs/M2C-RESULTS.md).

Audio ASR/TTS are routing-only until M4. `lh run` supports chat, with image
encoding in the CLI; `lh models pull` loads then shuts down. No inference or HTTP
dependency was added to the core.
