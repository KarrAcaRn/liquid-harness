# CLI HTTP fixture: socket lifecycle and reset analysis

Task `t_7704cc37`. Analysis only; hypothesis validation and any correction belong
to `t_c58537ec`. No production or fixture source changes. Architecture unchanged.

## Scope and evidence boundary

Reviewed baseline `c8bd9129d9111fa63f2ee6fd535d5a164e9ec382` against current
`cd88de4525c3c346cb74b1888f436ec8e4082adf`. `git diff c8bd912 HEAD -- crates/lh-cli
Cargo.lock` is empty. All project source line references below therefore apply
to both revisions. `test:` means `crates/lh-cli/tests/server.rs`; `server:` means
`crates/lh-cli/src/server.rs`.

The upstream [reproduction report](CLI-RESET-REPRODUCTION.md) (lines 9-17) identifies two
new failures as `test:175 -> test:122`: the first empty-body GET /v1/models.
The original failure lacks a caller backtrace and cannot be assigned a request
retrospectively. No captured partial response, child status at failure, listener
identity, server connection result, or packet trace establishes the cause.
This report does not reclassify a plausible mechanism as a demonstrated cause.
The standalone server suite reproduced the error without FreeToken; its startup
budget is not part of this investigation.

## Ownership and lifecycle

1. Each `Server` owns one `Child`, directory and address (`test:13-17`). Directory
   names combine test-process PID with an atomic counter (`test:12,20-24`); relaxed
   atomic ordering still guarantees distinct increments within this process.
   Environment overrides are applied to each Command, not the parent process
   (`test:44-73`). There is no shared Server, child handle, directory or mutable
   process environment between these tests.
2. Startup binds a temporary TCP listener to loopback port zero, reads its address,
   then DROPS it before spawning the real server (`test:41-44`). This is port
   discovery, not reservation through child startup. Another bind can win the gap.
3. The child validates configured token presence before its bind (`server:516-531`),
   binds its own listener and prints its actual address. The fixture pipes stderr
   but only reads it if startup observes an exited child (`test:75,88-99`). Normal
   running and panic paths do not drain or preserve it.
4. Readiness repeatedly connects to the address (`test:83-106`). A successful
   connect is sufficient and occurs BEFORE checking child status. The temporary
   stream is dropped without sending HTTP. This proves that some socket is
   listening, not that this child owns it, accepted it, or can answer HTTP.
   A temporary reservation listener from another fixture could also satisfy it.
5. The real server owns a listener and a JoinSet. Each accepted TCP stream moves
   into one spawned Hyper HTTP/1 connection task with cloned shared state
   (`server:530-552`). Completion of that task drops its socket, not the listener.
   The Hyper result is discarded at `server:548`; join results, including task
   panics, are discarded at `server:552`. A closed readiness probe is confined to
   its own connection. It does NOT directly close the next GET connection.
6. Every request opens a NEW blocking TcpStream (`test:115`), writes headers/body,
   reads to transport EOF, then parses response headers/chunks (`test:116-153`).
   There is no pooled connection or previous-request data on this stream. The
   endpoint test performs requests sequentially on a live local Server binding.
7. `Server::drop` kills and waits for its owned child and removes its directory
   (`test:164-169`). At the failing read, `request(&self)` is still executing;
   ordinary same-fixture Drop happens after return or during panic unwinding.
   A kill caused by the read panic is an EFFECT, not the cause of that read.
   The concurrent-stream test uses scoped threads (`test:433-453`), which join
   before Server drops. There is no unjoined client task in that test.
8. The Ctrl-C test targets its own positive child PID (`test:411-429`), not a
   process group. Server shutdown drops the listener, waits up to two seconds
   for connections, aborts remaining connection tasks, aborts stream workers,
   then shuts down the engine (`server:538-571`). It does not explicitly call
   Hyper connection graceful_shutdown. This can truncate active connections
   during shutdown, but no signal is sent by the failing endpoint test.
   Another fixture's teardown can affect it only with an additional ownership
   failure (wrong listener/port) or an external actor, not merely parallelism.

## Request/response framing and unread data

The helper emits HTTP/1.1, Host, Connection: close, Content-Type and the exact
byte length of body followed by the header terminator (`test:119-120`). For the
failing GET the length is zero, no Authorization field is emitted, and there
are no body bytes. `write!` need not be a single network write; valid HTTP must
support segmentation. A successful write only establishes that the local stack
accepted the bytes, not that the peer application consumed them. No flush is
needed for this unbuffered TcpStream. The client does not half-close its write
side; changing that is not an evidence-backed fix.

`read_to_end` requires transport EOF even if the HTTP response is already fully
framed. It can append response bytes and then return ConnectionReset. The unwrap
loses that distinction; a reset here does NOT prove that zero response bytes or
no complete HTTP response arrived. A ten-second timeout would be a different
IO error, not the observed OS 104. Chunk decoding and status parsing happen
only after a successful read, so their limitations cannot cause this panic.
The decoder does not rigorously validate trailers or all truncation cases;
that is separate from the observed transport failure.

Ordinary replies are `Full<Bytes>` (`server:45-63`), allowing Hyper to frame a
known-length body. SSE is a channel-backed StreamBody (`server:199-255`), framed
by Hyper as streaming HTTP/1 (the helper handles chunked encoding). Connection:
close prevents reuse; this is not an instruction to reset TCP. The lockfile pins
Hyper 1.12.0 (`Cargo.lock:290-293`). Its source calls connection `poll_shutdown`
on normal dispatch completion (`hyper-1.12.0/src/proto/h1/dispatch.rs:146-160`).
An HTTP FIN/EOF and an RST/error must not be treated as interchangeable.

GET /v1/models locks the engine and returns registry JSON immediately
(`server:338-347`), without inference, SSE, multipart parsing or a backend load.
It does not collect the request body, but Content-Length: 0 means there is no
legitimate unread body for this particular request. A theory that this GET
leaves a 6 MiB body unread contradicts its call arguments.

Other routes have distinct unread-data behavior:

- Authentication rejection, unknown path and wrong method return before body
  collection (`server:325-363`). A nonempty rejected request could leave input
  unread. The fixture's auth/path/method examples use empty GET bodies.
- Normal POSTs collect the entire Incoming through `Limited(MAX_BODY)` with a
  30-second timeout BEFORE MIME, JSON, media or multipart validation
  (`server:364-398`). Invalid JSON, URL/base64 restrictions and unsupported WAV
  response formats therefore do not themselves leave network request data unread.
- Multipart operates on already-collected Bytes (`server:285-323,383-384`), not
  directly on the socket. Its duplicate/unsupported fields, missing boundary,
  empty file and 5 MiB audio limit can reject parsing without a socket drain
  problem for an otherwise fully collected body. MIME/file contents are not a
  TCP closure mechanism. The successful fixture supplies explicit CRLF boundary
  delimiters and matching Content-Length (`test:119-120,233-240`).
- The final oversized POST (`test:363-375`) can fail collection at the 6 MiB cap.
  A limit failure or timeout can drop Incoming with additional network input
  still pending (`server:370-381`). Depending on Hyper buffering, request chunk
  sizes and close timing, an early response/close with unread TCP input can
  produce a Linux reset; the client writes the entire body before reading.
  This is a credible SEPARATE edge case, not an explanation of the two first-GET
  failures. Even MAX_BODY+1 does not prove unread bytes remained in the kernel:
  the over-limit frame may already have consumed the last bytes.

SSE holds the engine lock across complete inference, drains deltas, then sends
[DONE]; the channel closes when its sender drops (`server:181-255`). Disconnects
ignore send errors rather than immediately release the engine. Shutdown aborts
stream workers. These are real lifetime concerns, but streaming has not begun
at the first GET and therefore cannot be its direct cause.

## Ranked hypotheses (not measured probabilities)

1. **Wrong listener due to release/rebind plus identity-blind readiness.** This
   is the strongest concrete fixture defect candidate visible in source.
   Two fixtures can select the same freed address before either child binds;
   one child then loses its bind, but its parent can connect to the winner and
   skip checking the loser. Or readiness/request connects to a sibling's short-
   lived reservation listener, which is closed with queued request data. A real
   wrong server may return another auth policy (401), or later be killed by its
   own test while this client is active. Closing an unaccepted queued connection
   or an accepted connection with unread input can produce RST. A simple bind
   failure alone would normally produce startup error/refused connect, not this
   read reset: false readiness and closure timing are necessary extra steps.
   Parallel-suite correlation is consistent, but NO duplicate port or wrong PID
   has been captured. Do not call this the root cause without identity evidence.
2. **Unexpected close/error of the intended connection or child.** A Hyper error,
   connection-task panic, accept-loop error, child crash or external signal could
   drop a socket while request bytes remain unread (or truncate output). The
   server currently hides the first two result types. Linux may deliver RST rather
   than EOF depending on pending input and shutdown state. The mechanism fits
   the IO error but there is no observed panic, exit, accept error or signal, and
   no obvious path in the empty GET handler that requires any of them. Verify
   identity first, then capture connection result and process lifetime.
3. **Early rejection / unread input on later requests.** Credible for oversize,
   timeout or nonempty unauthorized requests, but excluded as the direct cause
   of the two recorded first-GET failures. Keep a separate regression case for
   it rather than assume fixing it resolves the observed symptom.

Lower-priority/unsupported explanations: the probe's empty connection resets a
subsequent independent connection; the endpoint fixture drops while borrowed;
Ctrl-C targets every child; invalid multipart framing causes a prior GET to fail;
a read timeout manifests as OS 104; FreeToken initialization causes this mock-
only server suite failure. No source evidence supports these claims. Stderr
pipe saturation is possible if enough logs accumulate, but tends to stall and
cause a timeout, not explain an immediate first-GET reset; capture it anyway.
Load may change interleavings but is not itself proof of a reset mechanism.

## Discriminating diagnostics and regression proposals

The next task should retain failing evidence BEFORE panic triggers Server Drop.
No swallowing ConnectionReset, blind retries, blanket serialization or production
body-drain change is justified by this analysis alone.

1. Log each fixture's test label, directory, selected address, intended child PID,
   readiness timestamps and child status. Capture child stderr continuously into
   a bounded buffer or per-fixture file so bind failures and panics survive and
   logging cannot fill the pipe. Do not log bearer values or request media.
   On read error retain operation number/method/path, local/peer socket address,
   error kind/raw code, bytes received, sanitized header prefix, declared response
   length and whether a complete response had arrived. Record live child status
   before cleanup; preserve event order using monotonic timestamps.
2. Temporarily log server PID, bound address, accepted peer tuple, connection
   completion Result, JoinError, and shutdown reason. Correlate a client's local
   ephemeral port with the server's accepted peer, not just destination port.
   `try_wait == None` is not proof of listener ownership: the child may still be
   pre-bind. A confirmed child-emitted listening address or OS socket owner plus
   accept correlation is needed. Packet capture/syscall tracing, if permitted,
   can establish which connection emits RST and whether unread bytes/response
   data precede it; avoid inferring application identity from a packet alone.
3. For port hypothesis, use a deterministic barrier-controlled scenario: reserve
   then release address A, bind a decoy listener to A before the intended child,
   and let the current readiness path run. Demonstrate false readiness and the
   real child's bind failure. Hold the decoy until the GET is queued, then close
   without consuming request input; capture the actual read outcome, not merely
   assume RST. A decoy returning a distinguishable HTTP response also proves
   misrouting independent of platform-specific TCP reset behavior. This proves
   the mechanism is possible, not that it caused the historical failures.
4. Compare instrumented parallel server-suite repetitions against a variant
   where the child itself binds port zero and publishes its address (or receives
   an inherited reserved listener). Confirm no source changes outside the
   diagnostic variant and preserve all run counts. Unique owned listener identity
   should remove collision-induced failures; a residual reset on a proven-own
   socket with no shutdown points toward hypothesis 2. Mere absence in a small
   rerun sample is not proof. Do not use arbitrary sleeps as synchronization.
5. Independently test probe-only connect/drop followed by a correctly framed
   empty GET on a known-owned listener; deliberately split headers across writes,
   and require complete framed response plus clean EOF. This discriminates a
   genuine close/segmentation issue from wrong-server readiness. Do not half-close
   the client unless explicitly testing that distinct behavior (Hyper's default
   half-close handling can change the experiment).
6. Isolate oversized, slow-upload and early-auth-rejection cases. Control sender
   progress with barriers/chunks, record whether the server consumed the body,
   and check both response framing and connection termination. Keep these tests
   separate from first-GET ownership regression so one cannot mask the other.
   Test same-fixture cleanup only after scoped request workers join; if validating
   Ctrl-C interruption, expect/document interruption separately from clean drain.

A useful decision table: wrong accept PID/address or child bind error establishes
an ownership failure; intended PID with connection error/panic and child alive
narrows to the connection task; intended PID exiting before read failure narrows
to process lifecycle; complete response then RST differs from no/truncated response;
normal GETs plus only oversized failures indicate a different early-close problem.

## Verification record

Source and dependency comparison completed; no CLI source or lockfile difference
from baseline to the reviewed current revision. This task performs static analysis
and documentation checks, not new repeated reset trials or a fix validation.
Upstream reproduction counts remain in the linked report, not duplicated as new
measurements. Initial host load average: 2.31 / 7.13 / 8.06. No inference or soak
runs. Repository check results and integrated commit are recorded in the task
handoff; all follow-up implementation remains with `t_c58537ec`.
