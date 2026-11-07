# CLI HTTP fixture: controlled reset investigation and ownership correction

Task `t_c58537ec`, Linux host, 2026-11-07. Starting revision:
`111684d02248a910ac0cb2950513d99b03242cad`.

## Outcome and boundary

A controlled experiment establishes an actual fixture defect: release/rebind
port selection plus connect-only readiness can accept an unrelated listener,
including when the intended child has already failed to bind. Closing that
listener with an empty-body GET queued produced `ConnectionResetError`, errno
104. The fixture now lets its own child allocate port zero and waits for that
child's existing bound-address announcement. This corrects the demonstrated
ownership/readiness defect, not a proven attribution of every historical reset.

The spontaneous failures in [reproduction evidence](CLI-RESET-REPRODUCTION.md)
lack listener identity and child stderr. Their specific root cause remains
unresolved. Green reruns are validation, not proof that this mechanism caused
those failures. The [socket analysis](CLI-RESET-SOCKET-ANALYSIS.md) describes
alternative intended-connection failures and separates later oversized-body
closure from the recorded first GET. No FreeToken change is made here.

## Controlled experiment

The evidence archive contains `decoy_probe.py`, run from the repository root
with `python3 ../decoy_probe.py` after
`cargo test -p lh-cli --test server --offline --no-run`. The production CLI
binary and HTTP implementation were unchanged. The script:

1. Binds a temporary loopback port-zero listener, obtains A, then releases it,
   exactly the fixture's old reservation sequence.
2. Binds a decoy to A before spawning `lh serve --bind A`.
3. Applies the old successful-TCP-connect readiness condition; it succeeds.
4. Waits for the intended child to exit, capturing its bind error.
5. Queues the same empty-body first GET on the decoy, then closes the listener
   without accepting/consuming the request and records the client read result.

One controlled run, exit 0 (the script asserts both the bind error and reset):

```json
{"address":["127.0.0.1",36679],"child_pid":2116223,"legacy_readiness":true,"child_exit":1,"stderr":"lh: Address already in use (os error 98)\n","first_get":{"error":"ConnectionResetError","errno":104}}
```

There is no timing sleep or random port collision required: ordering establishes
the competing bind and queued request before closure. Waiting for child exit
makes the false positive particularly clear, but does not assert that historical
children were already exited. This probe reproduces a possible failure chain,
not the natural scheduling frequency or historical listener owner. Its decoy
is deliberately not an HTTP server, modeling a sibling's temporary reservation.

## Correction

Only `crates/lh-cli/tests/server.rs` changes executable Rust:

- Default startup passes `127.0.0.1:0` directly to the child. No parent-selected
  port is freed and rebound. The child keeps the bound listener alive.
- Startup reads the existing `lh server listening on ADDRESS` line from that
  child's private stderr file, accepts only newline-complete parseable addresses,
  and checks child exit. It no longer probes an arbitrary address for readiness.
- File-backed stderr avoids an undrained pipe and supplies startup/read-failure
  diagnostics. Read errors still fail the test, including complete-response RST;
  no resets are swallowed, requests replayed, or HTTP framing relaxed.
- Failure diagnostics include method/path, child PID, destination address,
  received byte count and stderr, without dumping request bodies or credentials.
- The ten-second startup bound and twenty-millisecond polling interval remain
  unchanged. Existing RAII teardown kills/reaps children and removes fixture files,
  including when the new startup regression catches a panic.

No production server, CLI flag, dependency, authentication behavior, HTTP body
limit, FreeToken startup budget or test-thread setting changes.

## Regression evidence

`occupied_listener_cannot_satisfy_child_readiness` holds a real unrelated
listener on the explicitly requested address. Startup must fail with the child's
bind error rather than return success or time out; the decoy must have no queued
probe connections. This tests listener identity, not just eventual HTTP success.

Negative control: temporarily restoring the old `TcpStream::connect(bind)`
success-before-child-status branch makes the regression fail with
`an unrelated listener must not make startup succeed`, cargo exit 101. Command:

```sh
cargo test -p lh-cli --test server --offline occupied_listener_cannot_satisfy_child_readiness -- --exact
```

The temporary branch was removed before all green checks and is not committed.
`negative-control.log` records the failure.

`child_owned_listeners_survive_sibling_teardown_and_split_get` starts two real
children (one authenticated), asserts distinct bound addresses, drops the
sibling, opens/drops an empty probe, then writes an empty-body GET in separate
header writes. It requires 200, matching Content-Length, valid model JSON and
clean EOF. Separate writes do not guarantee separate TCP packets; this is not
a packet-level segmentation proof. Existing tests retain multipart, SSE, auth,
concurrent requests, oversized-body rejection and Ctrl-C coverage.

## Validation record

Environment for Cargo on this worker:

```sh
export CARGO_HOME=/opt/data/home/.cargo
export RUSTUP_HOME=/opt/data/home/.rustup
export PATH=/opt/data/home/.cargo/bin:$PATH
```

The initial invocation with only `$HOME/.cargo/bin` failed before executing Cargo
(exit 127); correcting the worker-specific toolchain environment fixed setup.
No test failure was discarded as an environment failure.

Commands and observed results, default test parallelism, no model/inference runs:

| Command | Result |
| --- | --- |
| `cargo build --workspace --offline` | Passed; supplies real mock-backend binary |
| `cargo test -p lh-cli --test server --offline` (initial corrected fixture) | 7 passed |
| `cargo test -p lh-cli --test server --offline endpoints_stream_multipart_and_media_validation -- --exact` | 20/20 runs passed |
| `cargo test -p lh-cli --test server --offline` (repeated full server suite) | 30/30 runs passed, 7 tests/run |
| `cargo fmt --all --check` | Passed |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | Passed |
| `cargo test --workspace --offline` | 3/3 runs passed, 116 tests/run |

Initial load average: 3.16 / 5.07 / 6.65; after repeated checks:
4.57 / 4.37 / 5.92. These runs were not a controlled load-matched comparison
against upstream baseline failures (which had higher load). No latency, race
frequency reduction or soak claim is made. Logs and the probe script are in
`cli-reset-validation.tar.gz` attached to this task. Integrated-main checks and
exact pushed commit are recorded in the task handoff after integration.

## Remaining uncertainty and next discriminating step

The historical first-GET failures are not conclusively attributed. If another
reset occurs, preserve the new method/PID/address/byte-count/stderr diagnostic
before teardown, then instrument the intended server's accept peer tuple,
Hyper connection result, JoinError, shutdown reason and child exit status.
Correlate client local port with accepted peer and PID. A reset on a confirmed
own listener would shift attention to connection/process failure, not port reuse.
The current helper still lacks live child-status and accepted-peer correlation
on read error; it is not a comprehensive transport recorder. Early rejection
with a pending nonempty/oversized upload is a separate investigation, not an
excuse to ignore resets on the empty first GET.
