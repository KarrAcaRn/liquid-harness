# CLI HTTP ConnectionReset reproduction

Task `t_62bfaa3e`, 2026-11-07. Diagnostic report only; no fix or production
change. Socket-lifecycle analysis belongs to `t_7704cc37`; hypothesis validation
and any correction belong to `t_c58537ec`. Architecture and protocol unchanged.

## Result

Reproduced twice on unchanged `c8bd9129d9111fa63f2ee6fd535d5a164e9ec382`.
Both new backtraces identify the **first GET /v1/models**, with an empty body:
`tests/server.rs:175:36` calls `Server::request`, whose
`socket.read_to_end(&mut response).unwrap()` panics at `:122:43` with OS 104,
`ConnectionReset`. Connect, header write and empty body write returned success;
this is not a request write failure, JSON assertion failure, or read timeout.
It occurs before multipart, streaming, media validation and oversized-body
requests. The helper did not log partial response bytes or child stderr on this
panic, so neither the HTTP status nor server-side reason is established.

The failures occurred with the five server tests running at default libtest
parallelism, not in the isolated endpoint sample. This is a correlation, not
proof of a parallel-only defect, CPU-load trigger, port collision or shutdown
cause. No FreeToken backend is required to reproduce: the standalone CLI server
suite failed. No shared cause with FreeToken startup budgeting is inferred.

## Evidence availability

Reviewed `docs/FREETOKEN-FIXTURE-DIAGNOSIS.md`. The attachment API for
`t_01305e20` returned an empty list, but its original local artifact was readable:
`/opt/data/home/projects/liquid-harness/.worktrees/t_01305e20/target/freetoken-diagnosis-logs.tar.gz`.
Copied/extracted it into this task's evidence bundle. Its
`ft-diagnosis/parallel-1.log:224-226` confirms the original error and line, but
has no backtrace; therefore the original request cannot retrospectively be
identified with certainty. No original packet trace or server stderr was available.

## Environment and method

Isolated shared-object local clone under this task's scratch workspace, detached
at the exact baseline; clean `git status --short` before and after reproduction.
No source edits/instrumentation for any reproduction run. Separate local target
build directory; no shared-main build or source mutation. Later documentation
was based on main with FreeToken hardening, not used for reproduction counts.

Linux GramLaptop 7.2.11-1-default, x86_64 GNU/Linux; 22 available logical CPUs.
Rust 1.99.1 (58cf8f9c3, 2026-10-13), Cargo 1.99.1 (5f94df478, 2026-08-27).
Initial UTC environment capture: 2026-11-07 22:27:32. Commands ran sequentially,
not parallel repeated invocations. No models/inference or artificial load used;
other host activity was not controlled. All invocations had a 180-second outer
subprocess bound. No bound was reached. Functional tests, not latency benchmarks.

Environment for every run:

```sh
export PATH=/opt/data/home/.cargo/bin:$PATH
export CARGO_HOME=/opt/data/home/.cargo RUSTUP_HOME=/opt/data/home/.rustup
export RUST_BACKTRACE=1
unset RUST_TEST_THREADS
cargo build --workspace --offline
# 20 sequential invocations:
cargo test -p lh-cli --offline --test server endpoints_stream_multipart_and_media_validation -- --exact --nocapture
# 5 sequential invocations:
cargo test --workspace --offline
# 20 sequential invocations:
cargo test -p lh-cli --offline --test server -- --nocapture
# 3 sequential invocations; ensure earlier failing targets cannot hide CLI results:
cargo test --workspace --offline --no-fail-fast
```

## Counts and logs

| Group | Invocations | Outcomes |
| --- | ---: | --- |
| workspace build | 1 | passed |
| endpoint only | 20 | 20 passed |
| ordinary workspace | 5 | all stopped at FreeToken failures before CLI server tests |
| server suite, five tests each | 20 | 19 passed; iteration 8 failed endpoint test, other four passed |
| workspace, no fail-fast | 3 | iteration 1: FreeToken failure, CLI passed; iteration 2: CLI reset, FreeToken passed; iteration 3: all 112 tests passed |

The five early-stop workspace failures are **not** successful CLI reset trials.
The unchanged endpoint test was exercised 43 times, with two resets. Do not
interpret this mixed, sequential, non-randomized sample as a population rate.
Passing isolated runs and subsequent workspace success do not establish resolution.

Actionable failure logs:

- `evidence/server-suite-8.log:6-27`: exact error and first-GET caller backtrace.
- `evidence/workspace-continue-2.log:104-127`: same failure under workspace run.
- `evidence/ft-diagnosis/parallel-1.log`: preserved original observation.

`evidence/baseline.json` and `evidence/extended.json` record every command,
return code, elapsed time (including builds), and load averages before/after.
Initial build load rose from 9.07/7.94/7.63 to 19.33/12.45/9.33. Standalone
reset run 8 measured 11.10/11.76/9.43 before and after; workspace reset run 2
started at 11.25/11.78/9.45 and ended at 10.43/11.60/9.41. Passing neighboring
runs had similar loads. There is no controlled evidence that load causes reset.

## Next diagnostic boundary

Preserve the exact first-GET failure rather than assume the 6 MiB request is
responsible. Inspect readiness connect/drop, listener ownership, concurrent
fixtures and shutdown interaction. On the next instrumented reproduction,
record fixture address/PID, request/partial response, child state/stderr and
accept/close events. A controlled hypothesis/regression scenario is still needed
before changing lifecycle behavior. Do not swallow reset or add blind retries.

The task evidence archive includes the runner, raw logs and structured metadata;
it is handed to downstream tasks with this report. No fixture fix is claimed.
