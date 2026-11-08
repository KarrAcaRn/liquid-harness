# Web UI default context – integration check

Scope: the Options setting "Default context for new chats" (`lh.defaultContext`,
`web/settings.mjs`) together with the per-conversation snapshot (`defaultContext`
in `lh.conversations`, `web/app.mjs`). Design: docs/ARCHITECTURE.md "Options" and
"Default context in conversations".

The default context is an instruction to the model, nothing more. It does not
add tools, does not force the model to use web search or file reads, and does not
guarantee that answers are factually correct. Whether a search or file read can
happen at all depends on the selected specialist and its tools.

## Integration result

- Setting key and access match: Options writes `lh.defaultContext`; every new
  conversation is created by `fresh()` via
  `newConversation(id, settings.defaultContextForNewChat())`. No integration
  fixes were needed in the client.
- Entry points for new chats in the web UI: first boot without conversations,
  "New chat", deleting the last conversation. All three take the snapshot.
  `lh chat`, `lh run` and API clients are not affected (browser-local option).
- Agent runs: the server prepends its own tool/skill system message; the
  `lh_messages` echo starts with the client context, which the client strips.
  The LFM3 template (LFM3-1.7B Q4_K_M, llama-server `/apply-template`) renders
  both system messages, tools attached to the first.

## Automated tests

- `node scripts/test-web.mjs`: settings init/save/empty/restart, snapshot,
  request building, no duplicates over chat and agent turns, wiring of all
  creation paths, and an end-to-end scenario over one storage across restarts.
- `cargo test -p lh-mock-backend --test agent client_context_is_echoed_first_and_once`:
  real agent loop with a scripted mock backend and a tool call; the echoed
  history starts with the client context, contains it once, keeps a
  chat-specific system message and never contains the agent's tool message.

## Manual browser check (mock fixture)

Run `cargo build --workspace --bins`, then
`python3 scripts/web-smoke-server.py --port 8492` and open
`http://127.0.0.1:8492/` in a browser. To see request bodies, use the browser
devtools network tab (request payload of `/v1/chat/completions` and
`/lh/v1/agent/runs`).

1. Clear site data and reload. Options shows the German suggested text; the
   new chat shows the "Default context of this conversation" disclosure with
   the same text.
2. Send two messages with model `mock-text`. Each request has exactly one
   leading system message with the context, followed by the history.
3. Select `web-agent`, tick Agent, send two messages (allow the tool call if
   asked). Each `/lh/v1/agent/runs` request again has the context once; the
   stored history (`lh.conversations`) has no system message.
4. Options: save another text. The open chat keeps the old disclosure; "New
   chat" creates a chat with the new text.
5. Options: save an empty field. "New chat" creates a chat without
   disclosure; its requests have no system message.
6. Reload. Options stays empty (not refilled). Open each chat and send: each
   sends its own original snapshot (or none).
7. Delete all chats: the automatically created chat uses the current option.
   "Restore suggested text" + Save, delete again: the new chat has the German
   text.
8. Chats created before this feature (no `defaultContext` field) load
   unchanged, send no added context, and keep their own system messages.

Result on 2026-11-08 (Chromium, fixture on :8492, load average about 0.6–3):
all eight steps behaved as described. No latency was measured.
