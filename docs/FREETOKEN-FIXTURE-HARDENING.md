# FreeToken offline fixture hardening

Task `t_5f4813f6`, following the [diagnosis](FREETOKEN-FIXTURE-DIAGNOSIS.md).
No production code, defaults, dependencies, or suite parallelism changed.

## Cause and fix

The old shared one-second spawn budget included interpreter startup, Python
imports, server setup and readiness polling. Imports alone were measured at
0.925 s in the investigation. Serial runs also failed: serialization is not a
remedy. Ordinary spawn unit fixtures now have a named five-second setup/load
budget. This is bounded headroom, not a guarantee against arbitrary host stalls.

Coverage is stronger rather than simply slower:

- A deterministic 1.2-second fixture delay exceeds the old entire budget and must
  still load successfully and clean up the direct child and descendant.
- The fake publishes its complete report with atomic rename only after binding
  its listening socket. An optional zero port allows the short-timeout test to
  retain an OS-assigned socket without a release/rebind race.
- The short readiness test first receives HTTP `state=loading`, then calls the
  production `ready` function with a separate 500 ms deadline. It asserts the
  exact timeout message/code, elapsed time >=500 ms and <2 s (scheduling slack),
  at least two recorded loading responses (setup plus readiness), and cleanup.
  The server stays loading throughout; interpreter setup is outside this timer.
- The end-to-end failed `backend/load` still includes startup and uses five
  seconds. It now requires the exact readiness-timeout error, recorded loading
  responses, and empty status BEFORE unload as well as after unload. This cannot
  pass on a pre-listening timeout or early process-exit error alone.
- Existing argument/environment, serving-vs-loading, shutdown, unload, direct
  child reaping and descendant cleanup checks remain. Both successful spawn
  assertions include response frames for diagnosis.

The response journal is appended after writing/flushing each HTTP response. It
is inspected after the timeout; no live multi-field JSON report is polled.
The short-test Resident owns the process before setup assertions, so panic also
triggers production RAII cleanup.

## Validation

Linux host only, no models/inference and no serial workaround. Environment:

```sh
export PATH=/opt/data/home/.cargo/bin:$PATH
export CARGO_HOME=/opt/data/home/.cargo RUSTUP_HOME=/opt/data/home/.rustup
unset RUST_TEST_THREADS
cargo test -p lh-backend-freetoken --offline
cargo test --workspace --offline
```

An initial focused run passed 17 tests. Subsequent consecutive repetitions:

| Group | Runs | Results | Tests per run | Wall seconds including Cargo |
| --- | --- | --- | --- | --- |
| FreeToken package | 8 | 8 passed, 0 failed | 17 | 6.682, 6.412, 6.428, 7.903, 8.936, 6.715, 7.006, 6.828 |
| Default-parallel workspace | 8 | 8 passed, 0 failed | 114 | 90.040, 10.179, 8.658, 7.865, 7.817, 7.293, 7.467, 7.689 |

The first workspace repetition includes compilation. All invocations preserve
libtest default parallelism; repetitions run sequentially. Focused load average
(1/5/15 min) began 7.29/6.42/7.09 and ended 6.79/6.48/7.07. Workspace load began
6.79/6.48/7.07, reached 14.96/9.37/8.05 after compilation, ended 7.68/8.32/7.76.
These are functional checks under host load, not latency benchmarks. Raw command
logs and per-run load/exit/test-count metadata are in the task's validation archive.

## Follow-up: integration test `tests/process.rs` (task `t_c4ba53c4`)

Review `t_601eb1ba` found the sibling integration test
`binary_spawn_stream_and_shutdown` still used the same `fake_ft.py` with a
three-second `load_timeout_secs`: 2 of 10 default-parallel workspace runs at
`3e4543a` failed with the readiness timeout during `backend/load` at load
average 10.4-16.2, and each failure leaked its `lh-ft-process-<pid>` temp dir.

- The budget now lives in test-only `src/fixture_budget.rs`
  (`FIXTURE_STARTUP_SECS` = 5), included via `#[path]` by both `src/tests.rs`
  and `tests/process.rs`, so the two fixtures cannot drift apart. It is setup
  headroom only; the test asserts serving, stream delta, `$/progress` and that
  the server pid is dead after shutdown, all unchanged.
- A `TempDir` guard removes the fixture directory on every exit path. On panic
  it first SIGKILLs the recorded fake `ft` process group (spawned with
  `setpgid`), which a killed plugin can no longer clean up.
- A `Plugin` guard kills and reaps the plugin child if it is still running on
  unwind. Stdout/stderr are drained by reader threads so the child stays owned
  by the guard instead of being consumed by `wait_with_output`.
- Class sweep: the only other spawned-fixture load budget in the workspace,
  `backend-llamacpp` `sdk_speak_status_and_lazy_http_restart`
  (`load_timeout_secs` 2 for a re-exec fake server), was raised to 5 s with the
  same rationale. Other short timeouts in tests deliberately test timeout
  behavior (`tools-web` `timeout_secs`, the 500 ms readiness test) and stay.
- Failure path checked by temporarily forcing `FT_TEST_STATE=loading`: the test
  failed after 5.01 s, and no `lh-ft-process-*` dir and no fake `ft` from it
  remained. The two dirs leaked by the review runs were deleted.

Validation (no serial workaround, `RUST_TEST_THREADS` unset, sequential runs):

| Group | Runs | Results | Tests per run | Load 1/5/15 begin -> end |
| --- | --- | --- | --- | --- |
| `cargo test -p lh-backend-freetoken --offline` | 10 | 10 passed, 0 failed | 17 | 6.23/6.07/4.75 -> 3.64/5.31/4.58 |
| `cargo test --workspace --offline` | 10 | 10 passed, 0 failed | 138 | 3.64/5.31/4.58 -> 5.42/7.82/5.86 |

Workspace run 1 includes compilation (load 18.56 afterwards; runs 2-4 started
at 18.56, 16.18 and 14.54, within or above the review's failing range). A concurrent
`coder` profile test run was active on the host during validation. No fixture
directories remained afterwards.

## Remaining limits

Eight repetitions do not prove absence of every timing failure. Arbitrary host
stalls can exceed either bounded setup or timeout slack. Production spawn still
has the documented port release/rebind race; no evidence implicated it and it
was not changed. The unrelated CLI ConnectionReset investigation remains a
separate task (`t_3f54311c`); it did not reproduce in these eight workspace runs.
Independent downstream review/repetition is task `t_601eb1ba`.
