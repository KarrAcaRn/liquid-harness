# M2c acceptance results

Measured 2026-11-07 on the Linux development host, Intel Core Ultra 7 155H,
llama.cpp b12189, d1-2B Q4_0, ctx 4096, batch 512, eight threads, CPU only.
`python3 scripts/eval_m2c.py` ran four batches sequentially. Before each batch
it checked existing llama-server processes and `/proc/loadavg`; after each batch
it verified no server remained. No other model job or compilation ran during
these final measurements. Load includes residual activity from preceding batches;
this was not a thermally controlled CPU benchmark. Do not interpret changes in
warm-model latency as speedups. The final pass supersedes an earlier pass whose
combined foreground tool timed out during the fourth batch (no leaked server).

## Baseline versus cascade

Total routing wall latency, nearest-rank percentiles, in milliseconds (includes
startup/model load). Load columns are 1/5/15-minute averages before → after.

| Set / mode | Accuracy | d1 calls | Call rate | p50 ms | p95 ms | Load before → after |
|---|---:|---:|---:|---:|---:|---|
| Tuning baseline | 42/42 | 40 | 95.24% | 2408.360 | 5415.907 | 1.96/3.98/4.91 → 8.59/5.77/5.43 |
| Tuning cascade | 42/42 | 19 | 45.24% | 0.009 | 4425.171 | 8.59/5.77/5.43 → 9.91/6.89/5.86 |
| Held-out baseline | 39/40 | 40 | 100% | 3334.878 | 7487.383 | 9.91/6.89/5.86 → 10.64/8.20/6.53 |
| Held-out cascade | 40/40 | 15 | 37.5% | 0.023 | 4356.411 | 10.64/8.20/6.53 → 8.76/8.02/6.56 |

All runs had zero routing errors. The baseline disables requirements, hints,
single-candidate, sticky and cache; explicit selection and modality safety remain.
Two tuning cases explicitly select their specialist, explaining the 95.24% baseline
call rate. Both accuracy targets are met without changing held-out data or tuning
hints against its errors. These small corpora are not a general accuracy guarantee.

Winning-stage counts:

| Set / mode | decision-model | explicit | single-candidate | hint | sticky | cache | requires | low-confidence |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Tuning baseline | 40 | 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| Tuning cascade | 19 | 2 | 21 | 0 | 0 | 0 | 0 | 0 |
| Held-out baseline | 40 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| Held-out cascade | 15 | 0 | 20 | 1 | 4 | 0 | 0 | 0 |

Requirements also narrow text candidate sets even when the final reason is
`decision-model`; winning-stage counts are not counts of all executed stages.
Cache has no hits in these unique-case workloads. Actual model-call savings from
cache, LRU eviction, candidate/config invalidation, file-content invalidation,
sticky behavior and disabled shortcuts are verified by offline tests instead.

Raw per-case reports (including scores, traces, confusion matrices and load):
- [Tuning baseline](measurements/m2c-tuning-baseline.json)
- [Tuning cascade](measurements/m2c-tuning-cascade.json)
- [Held-out baseline](measurements/m2c-heldout-baseline.json)
- [Held-out cascade](measurements/m2c-heldout-cascade.json)

## Corpus provenance

The original M2 `evals/routing.jsonl` is unchanged. M2c's `routing-tuning.jsonl`
preserves all 42 prompts exactly, adds missing attachment references for media
analysis and migrates four speech-output labels to `lfm3-speak`. Comparing the
new workload's latency directly against the old M2 numbers would be misleading:
old intent-only media prompts can no longer select a specialist requiring media.
Both M2c modes use the identical migrated set, not different per-mode fixtures.

A separate worker authored the 40-case held-out corpus before production hints
were written, without inspecting the hint lists, LFM3 pack or tuning prompts.
20 EN / 20 DE, five cases per language for each of the four specialists; eight
two-turn conversations. The file was frozen before any evaluation and never edited.
SHA-256: `d12f70a5fbaf7841d06f335733bbc861da3c493872871b9cc7cf1cec4565dfd2`.
See [eval instructions](../evals/README.md) for session and media inheritance rules.

## Implementation and verification

- `requires`, literal weighted multilingual phrase hints, optional text default,
  caller-owned Session, bounded content-fingerprinted LRU, per-stage switches and
  decision traces/model-call accounting. No new dependency; no regex engine.
- ASR/listening `lfm3-audio` requires audio. Text-only TTS uses `lfm3-speak` with
  the same model source. The broad text-default shortcut is supported but is not
  enabled in the shipped pack: unknown speech phrasing falls through to d1.
- Mock specialist pack moved to `tests/fixtures/mock-pack`. Default repo discovery
  exposes only the four real specialists. Regression tests inspect both CLI
  discovery and actual d1 criteria through a real stdio mock backend.
- Corrected the M2b example/smoke manifest discovery to include the disabled
  FreeToken backend referenced by the text specialist's preference chain.
- 90 offline workspace tests passed. Workspace fmt check, all-target clippy with
  `-D warnings`, workspace tests and release build passed. Release `lh` is
  2,189,296 bytes. No HTTP or inference dependency was added to lh-core.

## Known gaps

- Phrase matching is literal, not semantic: negated/quoted instructions can match.
  Sparse shipped hints reduce exposure but do not prove general correctness.
- Sticky policy can retain the wrong specialist on a same-modality topic change
  lacking contrary hints. Session stores only the prior user turn and specialist,
  not a conversation summary or assistant output. REPL/API persistence remains M3.
- Attachment content hashes are in-process noncryptographic fingerprints. Files
  should not mutate during a route; remote/unreadable attachments bypass cache.
- Media fixtures assess routing only, not comprehension. ASR/TTS inference remains
  M4; `lh run` still rejects non-chat tasks.
- CPU latency is load-sensitive and includes cold load; report the accompanying
  load values rather than treating these percentiles as portable performance.
