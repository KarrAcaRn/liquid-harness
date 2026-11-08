# Web UI generation statistics line – results (task t_b4aab721)

Builds on the stats interface of t_cf39fbdc (`lh.stats`, `web/stats.mjs`).

## Change

- `index.html`: `#run-status` and the new `#chat-stats` share one
  `.chat-status` row between `#approvals` and the composer.
- `stats.mjs`: `formatStats(stats, locale)` (pure, unit-tested) returns
  `{text, title, phase}` or null (hide). Main value: tokens/s with one decimal in
  the browser locale; `· N tokens` only when `completion_tokens` was reported.
  Details (rate source and window, prompt tokens, model-call time) are in the
  `title`. Missing values are `–`; non-complete phases never show numbers.
- `app.mjs`: `renderStats()` called at the end of `renderMessages()`.
- `layout.css`: row placement (flex, wrap, stats right-aligned);
  `style.css`: `tabular-nums` (colour/size come from `.status-line`).

## Manually checked (mock fixture, Chromium via CDP, 2026-11-08)

`python3 scripts/web-smoke-server.py --port 8613`; the mock backend reports no
timings, so rates were checked with a seeded localStorage conversation and with
a `fetch` stub replaying a final chunk carrying `lh.stats`.

| Case | Result |
|---|---|
| New/empty conversation | line hidden |
| Answer saved before stats existed | line hidden |
| Real mock answer (no timings/usage) | `– tokens/s`, title "backend reported no generation rate" |
| Streaming (stub, 300 ms per chunk) | `– tokens/s` during the stream, then `31.5 tokens/s · 42 tokens`; no other intermediate text |
| Cancel during an open stream | `– tokens/s (cancelled)`, run status `Cancelled` |
| HTTP 500 | `– tokens/s (error)`, run status `Request failed` |
| Conversation switch (stats / error / legacy) | `24.3 tokens/s · 1,234 tokens` / `– tokens/s (error)` / hidden |
| Reload | completed values restored unchanged |
| Locale de-DE | `24,3 tokens/s · 1.234 tokens`, title `50,9 s` |
| Long chat (40 messages), 1366×900 | row 553–574 between `#messages` (bottom 545) and composer (top 582); no page scroll |
| 390×844 | same row as `Ready`, 459–481, `#messages` bottom 455, composer top 485; no horizontal scroll |
| 320×640 | same row, no horizontal scroll; page scrolls vertically as before (existing small-screen fallback) |

Screenshot at 390×844 inspected: line readable, muted, right of `Ready`, no
overlap with messages or the message field. Existing flows (send, stream,
cancel, error, switch) behaved as before.

## Automated

`scripts/test-web.mjs` (run by `cargo test`): `formatStats` texts for complete,
no-rate, pending, cancelled/error/interrupted, junk, en-US and de-DE number
formats, singular token, ms/s durations, chat switch via `latestStats`, unique
`#chat-stats` id, and the `renderMessages()` → `renderStats()` wiring.

## Known gaps

- No live tokens/s while streaming: the server only sends final stats (deltas are
  not exact token counts), so the pending phase shows the placeholder.
- No real-model check: another llama-server (port 36859) was resident on the
  host, so per AGENTS.md no second real-model run was started.
