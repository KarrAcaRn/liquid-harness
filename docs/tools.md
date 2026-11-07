# Developer tools and agent host (M4a/M4b)

Three standalone MCP stdio executables ship in `plugins/tools-{fs,shell,web}`.
Build with `cargo build --workspace`; install the binaries beside `lh` or on PATH.
Their manifests advertise `provides = ["tool"]`. M4b adds the MCP client,
bounded agent execution and approval prompts in `lh-core`/`lh-cli`. Do not send
these servers the backend's `{protocol:1,...}` initialize handshake.

## Wire protocol and configuration

The SDK's `lh_plugin_sdk::mcp` implements the MCP **2025-03-26** tools subset:
newline-delimited JSON-RPC 2.0 over stdio, `initialize`,
`notifications/initialized`, `ping`, `tools/list`, and `tools/call`. Requests may
use string or numeric IDs. Initialize advertises only tools; no resources,
prompts, sampling, logging subscription, list changes or cancellation support.
If a client proposes another version the server offers 2025-03-26; the client
must disconnect if it cannot speak that version. Close stdin to shut down.
Calls are serial; notifications/cancellation cannot interrupt a call.

Standard MCP clients can launch with `LH_TOOL_CONFIG` containing a JSON object.
For the harness, `initialize.params.config` is an optional plugin configuration
extension: it shallowly overrides that environment object. Omit both for defaults.
Configuration must come from the trusted host, not from model tool arguments.
`workspace_root` defaults to the server's inherited cwd: the MCP host inherits
the cwd of `lh`, not the plugin dir; trusted config may override the root.
Manifests' `[config]` values must be supplied by the host; standalone binaries do
not read TOML themselves.

Initialize first, then send the initialized notification before listing/calling:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"example","version":"1"},"config":{"workspace_root":"/path/to/project"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"Cargo.toml"}}}
```

Successful calls return `content: [{type:"text", text:...}]`, `isError:false`;
execution/argument failures return the same shape with `isError:true`. Invalid
RPC methods/envelopes, lifecycle violations and unknown tools use JSON-RPC errors.
Frames, including newline, are limited to 8 MiB. Tool schemas are returned by
`tools/list`; required fields/types/ranges are checked in handlers (there is no
full JSON Schema validation engine). `write_file`, `edit_file`, `run_command`
carry `readOnlyHint:false`, `destructiveHint:true`; other tools are read-only.
Annotations are hints, NOT approval enforcement. The host must gate calls.

## Filesystem: `lh-tools-fs`

Configuration: `workspace_root` (default `.`), `allow_secrets` (default false).

| Tool | Arguments | Result |
|---|---|---|
| `read_file` | `path`, `offset=1`, `limit=200` (max 2000) | UTF-8 lines prefixed `N|`; continuation marker when capped |
| `write_file` | `path`, `content` | Create/overwrite, creating parents; byte count |
| `edit_file` | `path`, `old_string`, `new_string` | Exact unique replacement; small before/after excerpt and line number |
| `list_dir` | `path="."`, `limit=200` (max 1000) | Immediate relative children, directory suffix `/` |
| `search_files` | `pattern`, `target="content"`, `path="."`, `limit=200` (max 1000) | Regex content (`path:line:text`) or glob names (`target="name"`/`"files"`) |

Files are capped at 8 MiB; text output at 64 KiB. Empty/no-op/zero-match/multiple
match edits fail without mutation; overlapping matches also count. Diff excerpts
contain at most ten lines per side, 200 characters per line, not a patch file.
Search honors local `.gitignore`/`.ignore`, skips `.git`, `target`, `node_modules`,
symlinks and protected names, and stops after 10,000 visited entries. Content
search skips binary/NUL, unreadable, oversized and non-UTF-8 files. No matches
is empty text, not an error. Directory enumeration has the same visit budget;
entries are sorted within the visited set. Traversal library directory sorting
and ignore-file loading happen before some visit checks; this is not a hard
wall-clock or total-memory guarantee on adversarial directory layouts.

All arguments are workspace-relative. Absolute paths and `..` components are
rejected, as are all symlinks (including safe in-workspace ones and dangling
links). Any component named `.env`, `.env.*`, `*.pem`, or `id_*` is denied unless
`allow_secrets=true`; listing/search omit it. This is a name-based safety default,
not comprehensive secret detection. Never assume other files contain no secrets.

Security boundary: intended for a trusted local workspace and trusted host.
Component checks are not atomic with file operations: hostile concurrent rename/
symlink substitution, hard links, and mounted filesystems are not sandboxed.
Do not grant these servers access to an attacker-controlled mutable filesystem;
use OS/container isolation for that threat model. Writes are not transactional
and there is no optimistic concurrency check against other editors.

## Shell: `lh-tools-shell` (Linux/Unix)

Configuration:

```toml
[config]
workspace_root = "/path/to/project"
max_timeout_secs = 120   # permitted 1..86400
max_output_bytes = 65536 # permitted 128..1048576, captured raw bytes
allow = []              # optional regex strings; any match permits
deny = []               # any match denies, takes precedence
```

`run_command(command, cwd=".", timeout_secs=120)` executes `sh -c` with null
stdin and combined stdout/stderr. Default timeout is clamped to the configured
maximum; explicit values must be positive and not exceed it. Returns JSON text:
`{output, exit_code, timed_out, truncated}`. Signal exit codes are null; nonzero
exit codes/timeouts are normal structured results, not transport errors.

`cwd` is a relative directory below the configured root; absolute paths, parent
components and symlinks are rejected. The command itself has the user's full
permissions, inherited environment, network and filesystem access: it can `cd`
outside the root or read secrets. This tool is NOT a sandbox. Regex matching is
against the entire raw command string with normal regex search semantics, not a
shell parser; anchor patterns deliberately. Allow/deny patterns cannot safely
constrain an adversarial shell language. Require host approval.

Output retains a bounded head and tail plus a visible omitted-byte marker. Bytes
are decoded lossily; splitting a multibyte character at a truncation edge can
produce replacement characters. Metadata and UTF-8 replacement expansion are
outside the raw-byte budget. The child has a new process group; timeout and RAII
cleanup send SIGKILL to that group and reap the shell. A background child holding
output open is still subject to the deadline, even after the shell exits.
Deliberate `setsid`/process-group escapes and plugin SIGKILL are not a process
sandbox/cgroup; arbitrary escaped descendants cannot be guaranteed cleanup.

## Web: `lh-tools-web`

Configuration defaults:

```toml
[config]
provider = "duckduckgo"  # also searxng, brave
# base_url = "https://search.example.org" # required for searxng only
api_key_env = "BRAVE_SEARCH_API_KEY"
timeout_secs = 15        # 1..120
max_bytes = 1048576      # 1..4194304 per response, before text extraction
allow_private_network = false
```

`web_search(query, limit=5)` accepts a nonblank query up to 4096 bytes and limit
1..20. Returns JSON text with `provider`, `query`, `results` (title, URL, snippet).
DuckDuckGo uses the keyless HTML endpoint; SearXNG uses `/search?format=json`;
Brave uses its web-search API and reads the key from the named environment variable
at call time. Do not store API keys in TOML. Search markup/availability can change;
CAPTCHA pages can yield an empty result set. Brave and SearXNG may require service
configuration/account access. Only result parsing and a local SearXNG-compatible
fixture are tested offline; no real Brave account was used.

`web_fetch(url)` returns readable UTF-8 text from HTML (scripts/styles/head/hidden
nodes removed), text MIME types or JSON. It is not full article extraction or a
browser: no JS, PDF, images, non-UTF-8 decoding or CSS layout. Whitespace is
normalized for HTML and markup is removed; original URLs should be retained by
the caller for citations. Oversized responses fail rather than silently truncate.

Only HTTP(S), without URL credentials. All resolved destination addresses must
pass a conservative global-unicast filter, using the exact resolver addresses
ureq connects to (no validation/use DNS rebinding gap). Loopback, RFC1918, link
local, CGNAT, multicast, reserved/documentation ranges, IPv4-mapped IPv6 and common
translation/tunnel ranges are denied. Environment proxies are ignored. Redirects
are disabled rather than followed without validation. This can reject otherwise
legitimate sites; use their final public URL. Trusted config can opt into private
networks for local SearXNG/tests, relaxing this protection for every web tool.
System DNS workers may outlive their caller's bounded wait. Application filtering
is not a replacement for egress firewall rules against unusual host routing/NAT.

## Verification and gaps

Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --offline --workspace`, then `cargo build --workspace` and
`python3 scripts/tools-smoke.py`. The smoke calls all eight tools through real
stdio binaries, checks outputs and filesystem side effects, using an ephemeral
local fake HTTP server. `--internet` adds optional DuckDuckGo/example.com probes;
those results are printed without forcing success if the service is unavailable.
No model is needed. [Recorded transcript](M4A-STDIO-TRANSCRIPT.txt) includes a
successful two-result DuckDuckGo query and readable example.com fetch; load
average before/after was 4.22/4.79/4.63. No inference, latency or throughput was
measured. Tests include confinement/secrets/symlinks, exact edits, output/search
caps, process-group timeout, open-pipe descendants, proxy avoidance and HTTP errors.

Known gaps include the trust/race/process boundaries above, serial MCP dispatch
and limited HTML extraction. M4b can terminate a tool plugin on cancel/timeout,
but cannot roll back effects or guarantee cleanup of escaped shell descendants.
Core still has no HTTP client or inference dependency.

## M4b host policy

Specialist `tools` accepts plugin IDs or individual tool names. Collisions become
`plugin__tool`; colliding built-in skill names are also qualified. Missing
selectors fail rather than silently broaden permission. Annotation defaults
are conservative: tools not explicitly non-destructive/read-only need approval.
Only install trusted plugins: annotations are plugin claims, not code analysis.

Configuration defaults:

```toml
[agent]
approval = "ask" # ask | auto-edit | yolo
command_allow = []
max_steps = 20
tool_timeout_secs = 120
approval_timeout_secs = 60
output_budget_bytes = 65536
context_tool_bytes = 16384
```

`auto-edit` only permits `tools-fs` write_file/edit_file automatically; commands
still ask. `command_allow` matches the entire raw command, with `*` matching any
bytes (including shell operators). Prefer exact strings such as `git status`.
`cargo test*` also permits a trailing `; ...`: patterns are convenience, NOT a
sandbox. Modes and patterns are trusted configuration, never request overrides.
`always` only remembers identical plugin/tool/arguments during one run.
CLI asks y/n/always on its controlling terminal; noninteractive requests deny
unless configured for automatic execution. Pending prompts expire to denial.

### HTTP agent contract

All endpoints inherit bearer authentication, localhost default, JSON body limit
and no CORS policy from `lh serve`. `POST /lh/v1/agent/runs` accepts only
`{"model":"auto","messages":[{"role":"user","content":"..."}]}`. Selection must
resolve to an agent-enabled chat specialist. Routing/load errors precede SSE;
successful response has `Content-Type: text/event-stream` and `x-lh-run-id`.
SSE `event:` names match JSON `event`; JSON is `{event,data,run_id}`:

| Event | `data` |
|---|---|
| route | Complete loaded RouteDecision |
| delta | `{content,step}`; text deltas only |
| tool_call | `{call_id,name,arguments,step}`; validated complete call |
| approval_required | Same call fields; no effect performed yet |
| tool_result | `{call_id,result,step}`; result is serialized JSON text, possibly visibly truncated |
| error | `{code,message}` |
| done | `{response,steps}` on success, `{error}` on failure |

`response` contains backend message/usage/finish_reason, steps, and `lh_messages`
(retained history including tool exchanges, excluding the final answer and the
injected skill index). Append `response.message` before the next turn.
`POST /lh/v1/agent/runs/{id}/approve` requires `{call_id,allow}` and optional
boolean `always`. Unknown/completed run: 404; expired/duplicate approval: 409.
`POST /lh/v1/agent/runs/{id}/cancel {}` requests cancellation. A completed run
is removed; there is no durable run storage or event reconnection/replay.
Disconnected/slow (>5s blocked send) SSE clients cancel; inference still drains.
One run owns the engine at a time. New agent runs and management reads return
409 while busy, rather than waiting behind an unanswered approval. Approval and
cancel endpoints never acquire that lock. Routing/model load happens before the
run ID exists and cannot be cancelled through this endpoint.
`/v1/chat/completions` intentionally does not execute local tools.

### Read-only management JSON

| GET path | Shape |
|---|---|
| /lh/v1/plugins | `{plugins:[{id,version,description,provides,capabilities,capabilities_source:"manifest",enabled,manifest_path,process}]}` |
| /lh/v1/specialists | `{specialists:[Specialist]}` including tools/agent, hints, requires and backend choices |
| /lh/v1/router | `{selected,routers:[Router],cascade:CascadeConfig,stats}` |
| /lh/v1/tools | `{tools:[{name,plugin,original_name,description,input_schema,destructive}],builtins:[OpenAI tool schema]}` |
| /lh/v1/skills | `{skills:[{name,description,tools}]}` |
| /lh/v1/skills/{name} | `{name,description,tools,content}`; Markdown body, 404 if absent |
| /lh/v1/status | `{processes:{plugin_id:process},resident_models:[{backend,loaded}],cascade:stats}` |

`process` is `{state:"not_started|running|failed",pid:null|number,restarts:number}`;
restarts counts lifetime attempts, not just the rolling limit. Declarative and
disabled plugins appear as not_started. `stats` has `model_calls`, winning
`stage_counts`, and the last 32 `recent_routes` (no raw user prompts).
Resident entries use backend-owned `loaded` arrays (`key,pid,port,idle_secs` for
llama.cpp); failed backend status reads have `{backend,error}`. Status does not
load a model; `/tools` lazily starts MCP plugins to obtain authoritative schemas.
Configuration maps and environment values are never returned.

### M4b verification

After `cargo build --offline --workspace`, run
`python3 scripts/agent-smoke.py --out /path/to/results` for offline HTTP policy
checks using scripted mock replies and actual filesystem tools. `--real` runs
two sequential cached LFM3 cases; it refuses an already running llama-server.
See [M4B-RESULTS.md](M4B-RESULTS.md) for real results and explicit limitations.
