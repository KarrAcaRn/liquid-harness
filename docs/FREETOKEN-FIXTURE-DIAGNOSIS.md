# FreeToken offline fixture readiness diagnosis

Investigated 2026-11-07 on Linux at baseline
`c8bd9129d9111fa63f2ee6fd535d5a164e9ec382`, task `t_01305e20`.
This is an investigation, not a production or fixture fix. Temporary diagnostic
changes described below were reverted. No inference/models were run.

## Reproduction

Use the installed Rust toolchain (this worker's HOME differs from the dev home):

```sh
export PATH=/opt/data/home/.cargo/bin:$PATH
export CARGO_HOME=/opt/data/home/.cargo RUSTUP_HOME=/opt/data/home/.rustup
# Repeat sequentially; each invocation retains libtest's default parallelism.
env -u RUST_TEST_THREADS cargo test --workspace --offline
# Diagnostic control, not a proposed fix:
RUST_TEST_THREADS=1 cargo test --workspace --offline
# Focused reproduction:
env -u RUST_TEST_THREADS cargo test -p lh-backend-freetoken --offline \
  --bin lh-backend-freetoken fake_ft -- --nocapture
```

Eight consecutive unmodified default-parallel workspace invocations:

| Run | Result | Wall seconds (includes Cargo) |
| --- | --- | --- |
| 1 | FreeToken passed; unrelated CLI HTTP test failed | 67.786 (initial build) |
| 2 | FreeToken unload test failed at tests.rs:470 | 1.328 |
| 3 | Both FreeToken spawn tests failed, explicit readiness timeout at :442 | 1.202 |
| 4 | All 112 tests passed | 4.426 |
| 5 | All 112 tests passed | 4.058 |
| 6 | All 112 tests passed | 5.021 |
| 7 | FreeToken unload test failed at :470 | 1.234 |
| 8 | All 112 tests passed | 5.030 |

Three subsequent serial controls: 112 tests passed twice (8.610 and 7.771 s),
then the shutdown-cleanup spawn test failed with readiness timeout (3.470 s).
Thus this is NOT exclusively a concurrent-test isolation bug; serialization
neither explains nor reliably fixes it. Failure runs stop early and do not
exercise all 112 tests.

Load averages (1/5/15 minute): initial 3.17/8.06/7.68, after initial build
11.83/9.86/8.35, final parallel 10.16/9.59/8.29, final serial 8.89/9.34/8.24.
These are functional reproductions under residual host load, not benchmarks.
Initial attempts before setting CARGO_HOME/RUSTUP_HOME failed before compiling
and are excluded from these test counts.

The unrelated CLI failure was `endpoints_stream_multipart_and_media_validation`
at `crates/lh-cli/tests/server.rs:122:43`: `ConnectionReset`, OS 104. It has its
own follow-up `t_3f54311c`; it is not evidence about FreeToken.

## Startup evidence

Temporarily instrumented `fake_ft.py` with monotonic stderr timestamps before
imports, after imports, around HTTPServer construction/getfqdn, and (in the
five-second experiment) upon GET. Timer starts in Python before `http.server`
import, so interpreter startup before that point is NOT included.

With the original one-second budget, eight focused parallel invocations had
three failures (runs 1, 3, 7). Representative trace from run 3:

- PID 2062411: imports complete 0.924913 s, before HTTPServer 0.926395 s,
  before getfqdn 0.926595 s, after getfqdn 0.934389 s, listening 0.934411 s.
- Shutdown-cleanup test then reports the real production readiness timeout.
- A loading-state fixture in that run reached `before_getfqdn` at 0.894578 s
  but never logged `listening` before being killed. Nevertheless its test passed!
- On focused runs 1 and 7, one failed child did not even reach the post-import
  diagnostic before the deadline.

This directly demonstrates insufficient startup headroom. Imports alone can
consume nearly the entire deadline, before interpreter startup, fork/report,
server construction, and the backend's 100 ms polling interval. The precise
contribution of CPU scheduling vs filesystem/import cost is not isolated.
Observed getfqdn times do not establish DNS as the primary cause. No bind
error or unexpected process-exit error appeared in these logs.

For diagnosis ONLY, temporarily increased the shared fixture budget from 1 to
5 seconds, leaving production code untouched. Five focused parallel runs passed
both tests each time; all loading-state processes served GET requests and then
timed out/cleaned up. Load decreased from 5.26/8.13/7.90 to 3.92/7.54/7.71.
This small non-randomized sample supports more startup headroom, but is not a
validated final fix or proof that five seconds bounds every host. Raising the
shared budget indiscriminately is not the recommendation below.

Raw logs and JSON run metadata are attached to task t_01305e20 as
`freetoken-diagnosis-logs.tar.gz`. Entries `parallel-*`/`serial-*` are baseline;
`trace-*` are one-second instrumented runs; `budget5-*` are the temporary
five-second instrumented experiment. Successful full logs sum to 112 tests.

## Isolation, synchronization, cleanup and assertions

- `Fixture::new` uses process ID plus AtomicU64 counter for private directories;
  reports/scripts are not shared. Atomic Relaxed is sufficient for uniqueness.
- Spawn env is applied to Command, not mutated process-global environment.
  Each Backend owns a separate mutex/state/TTL worker. The mutex excludes TTL
  eviction during load. No shared readiness flag or cross-test worker was found.
- URL fake listeners retain their ephemeral socket. Spawn mode releases the
  reserved ephemeral port before the Python process binds: a real documented
  bind race remains, but no observed error implicates it in these failures.
- Python writes its report BEFORE creating HTTPServer. Report existence proves
  fork/argument capture, not readiness or receipt of a cache-status request.
- `ready` starts its deadline after spawn and requires JSON `state=serving`;
  HTTP success alone does not count. It checks process exits, bounds HTTP calls
  by remaining time, polls every 100 ms, and returns -32001 on readiness timeout.
- Resident RAII kills the process group and waits for the direct child, including
  failed load; unload and backend shutdown drop residents. Passing cleanup tests
  verify direct child absence/ECHILD and descendant absent-or-zombie. This is not
  a promise to reap grandchildren. No live Python ft fixture remained after
  the experiments (process inspection; no real llama-server was started).
- The loading test currently checks only -32001: connection refusal, timeout
  before the server listens, or early process exit can satisfy it. This is a
  genuine coverage/synchronization weakness even though not an isolation defect.
- The unload serving assertion lacks the frame dump that the shutdown serving
  assertion has, obscuring its error reason.

## Minimal hardening recommendation for t_5f4813f6

1. Use a named ordinary startup budget of 5 seconds for successful spawn
   fixtures (or a similarly bounded, justified margin). Keep production
   defaults/request/readiness behavior unchanged; do not serialize the suite.
2. Separate the intentional loading-state deadline from ordinary startup.
   Ensure the loading server has actually answered cache-status before starting
   a deliberately short readiness-timeout check. A small fixture-controlled
   state/report handshake or a directly constructed Resident with an already
   listening fake is preferable to sleeping and hoping imports completed.
   Preserve an end-to-end failed backend/load cleanup check as well: it must
   assert the readiness-timeout message, evidence of a loading response, empty
   backend/status, and direct-child/descendant cleanup. If that path needs a
   larger startup-inclusive budget, retain a separate short already-listening
   readiness test rather than weakening timeout coverage.
3. Retain all argument/env, serving-vs-loading, unload/shutdown, and failed-load
   cleanup assertions; include frames in both successful-load assertions.
   Publish any handshake/report atomically or read it only after a guaranteed
   completed write. Do not treat the existing pre-bind report as ready.
4. Add deterministic slow-start coverage: a fixture-only delay exceeding the
   former 1 s budget (e.g. 1.2 s) must succeed with the ordinary startup budget,
   still checking cleanup. A permanently loading already-listening fixture must
   fail by the short deadline with bounded slack and observed requests.
5. Repeat focused and default-parallel workspace runs; retain serial only as
   control and report unrelated CLI failures separately.

No source changes or speculative fixes are included in this diagnostic commit.
Architecture/protocol remain unchanged. The implementation and independent
verification are pre-created downstream tasks.

Pre-commit checks: fmt and workspace/all-targets clippy with warnings denied
passed. Two additional default-parallel workspace checks reproduced FreeToken
failures; the subsequent serial workspace check passed all 112 tests (load at
end 8.94/7.66/7.67). These extra checks are separate from the fixed reproduction
sample above; a green serial check does not mean the flakiness is repaired.
