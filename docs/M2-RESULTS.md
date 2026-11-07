# M2 acceptance results

Measured on 2026-11-07, Linux, Intel Core Ultra 7 155H, llama.cpp b12189
(commit `80d2c0c2c`), CPU only, maximum eight inference threads. All real-model
operations ran sequentially; no other llama-server was running at their start.
Load averages include residual load from preceding sequential work, not simultaneous
model runs. Raw data: [measurements/m2.json](measurements/m2.json).

## Routing evaluation

42 bilingual cases: 21 English and 21 German, with text, image, audio, explicit
selection and ambiguous requests. Initial development run: 41/42 correct; the
German voicemail request was incorrectly classified as text. Adding voicemail,
voice-message and German speech-transcription terms to the audio route criterion
produced **42/42 correct**, zero errors. This is tuning-set accuracy, not a
held-out estimate of generalization.

Expected-row / predicted-column confusion matrix:

| Expected | text | vision | audio |
|---|---:|---:|---:|
| text | 17 | 0 | 0 |
| vision | 0 | 13 | 0 |
| audio | 0 | 0 | 12 |

35 decision-model calls: p50 **1840 ms**, p95 **2039 ms**, including startup/load
and core overhead (not pure warm inference). Seven fastpaths are reported
separately at zero core router_ms. Load before: **2.95 / 2.77 / 2.04**;
after: **4.60 / 3.29 / 2.27**. d1 Q4_0, ctx 4096, batch 512, threads 8.
Image/audio attachment cases test modality filtering, not media comprehension.

## Real smoke and HF source check

`scripts/smoke.sh` passed. It routes with d1, shuts down the router process,
then generates a text answer and a separate image answer with LFM3-VL. This
keeps only one real server at a time on the development host. An actual red
square PNG is generated, not a placeholder image.

Latest output:

- d1 selected `lfm3-text` via the low-confidence fallback (winning audio
  probability 0.5253878710038052, below 0.55); router_ms 2464.
- Text: `Hello! How can I help you today?`
- Image: `The colored shape is a square that is red in color.`
- `PASS: d1 routing, streamed text and image answers; servers shut down.`
- Load before **3.20 / 3.07 / 2.23**, after **3.33 / 3.10 / 2.25**.

The initial image attempt exhausted 128 tokens in reasoning without visible
content. The pack now disables thinking and uses a 256-token completion budget;
the repeated smoke passed. Assertions require nonempty successful answers, not
exact stochastic wording. Missing-server behavior was exercised with
`LLAMA_SERVER=/nonexistent`: exit 0 with an explicit `SKIP`.

`lh models pull d1` was separately run using the real `hf:` source and existing
HF cache: `ready: true`, followed by server shutdown. Load before and after
**3.20 / 3.07 / 2.23**. Text and VL weights were downloaded into LLAMA_CACHE;
the smoke uses `file:` sources and an explicit F16 VL projector to stay offline.
Fresh HF download/projector discovery is delegated to llama.cpp, not reimplemented.

## CPU investigation

See [Architecture](ARCHITECTURE.md#m2-cpu-investigation-2026-11-07-under-30-minutes)
for the measured thread/batch/affinity table and recommended defaults. The CPU
loader chose the optimized alderlake backend. The old ~48 tok/s was not
reproduced: unpinned eight threads achieved 100.476442 tok/s at pp128/b512,
load before 5.09/2.83/1.97 and after 5.67/3.07/2.06. Six physical P-cores
at b128 achieved 129.312994 tok/s, within the affinity sweep whose load was
3.23/2.77/2.00 before and 3.74/2.89/2.05 after. Two repetitions per point
are exploratory; neither power-state causation nor sustained performance is proven.

## Offline verification and implementation limits

- 54 offline tests passed, including fake HTTP chat/SSE/health/decide,
  load idempotence/config defaults, pinned capacity, LRU, TTL, normal cleanup,
  Linux parent-death cleanup, CLI streaming/eval, and executable lookup.
- `cargo fmt --all --check`, clippy for all workspace targets with `-D warnings`,
  workspace tests and release build passed.
- Release `lh`: 1,986,296 bytes; HTTP client stays outside lh-core.
- Operator-requested fix: plugin command resolution checks manifest directory,
  then running executable's directory, then PATH. A real release CLI route
  succeeded with only `/usr/bin:/bin` on PATH; missing-command tests verify
  plugin id, command and searched locations in the error.
- SDK request handling is serial and cancellation is not interruptible.
- Direct llama-server receives parent-death SIGKILL on Linux; normal cleanup
  kills its process group. Arbitrary daemonizing wrapper descendants are not
  guaranteed cleanup after plugin SIGKILL; use the actual server executable.
- Ephemeral port selection has a release-before-spawn bind race.
- Audio inference, REPL, conversation memory, HTTP serving and tool execution
  remain later milestones. The audio manifest participates in routing, but
  `lh run` refuses non-chat tasks explicitly.
- Discovery is additive: isolate the LFM3 pack from mock candidates for real
  evaluation. Examples document running outside the repository root.
