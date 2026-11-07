# Developer tool plugins (M4a)

Three standalone MCP stdio executables ship in `plugins/tools-{fs,shell,web}`.
Build with `cargo build --workspace`; install the binaries beside `lh` or on PATH.
Their manifests advertise `provides = ["tool"]`. M4a does not change `lh-core`:
agent execution, the MCP client, and approval prompts belong to M4b. Do not send
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
`workspace_root` defaults to the server's inherited cwd: the future MCP host must
inherit the cwd of `lh` or explicitly send its workspace root, not the plugin dir.
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

Known gaps are the explicit trust/race/process boundaries above, serial
non-cancellable MCP dispatch, limited HTML extraction, and no host/agent approval
UI until M4b. No HTTP/inference dependency or agent-loop change was added to core.
