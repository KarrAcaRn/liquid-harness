# Web UI layout and structure – results (t_de1ed4d1)

Implements the layout/structure half of `docs/WEB-UI-PLAN.md` (sections 4–7).
Owned files only: `web/index.html`, `web/app.mjs`, new `web/layout.css`,
`src/server/web.rs`, `tests/server.rs`, `scripts/test-web.mjs`,
`docs/ARCHITECTURE.md` (asset list). `style.css` and `markdown.mjs` are untouched.
No route, API call, permission, guard, storage key or message text was removed.

## Changes

App shell
- `header.app-header`: brand (h1 + subtitle), `nav.app-nav`, Toggle theme
  (`.button-quiet`) on one row; collapsed `details.connection` below it;
  `#notice`/`#storage-warning` grouped in `div.notices` with `.alert` and
  `data-kind`. Desktop shell height 97 px (plan baseline 167).
- `body[data-page]` is set by `management()`; `body[data-page=chat]` makes the
  chat view fill the viewport (flex column, `100dvh`). Management pages scroll
  normally. If minimum sizes do not fit (320×640), the document scrolls.

Chat
- Sidebar `aside.conversations`: header (h2 + New conversation), list of
  `button.conversation-item` (keeps `.selected` and `aria-current`), footer
  (Export as `.button-quiet` + storage hint). Mobile: compact strip, list in
  one horizontally scrolling row, Export at the end of that row.
- `.conversation-header`: title + `.conversation-actions` (Rename, Delete with
  `.button-danger`), never split from each other.
- `#messages` is `flex: 1` and scrolls internally (min 10rem desktop, 120 px
  mobile); shows a static "No messages yet." hint when empty.
- Order below the messages: `#approvals`, `#run-status` (`.status-line`), composer.
- Composer: textarea → `#attachments` (bounded, scrolls: 8rem / 6rem mobile) →
  `.composer-options` (Model + Refresh models `.button-quiet` | Agent mode |
  Speak replies) → `.composer-actions` (Attach + Record … Cancel, Send
  `.button-primary` as the trailing control) → hint with both size limits.
- The approval "Always applies only to identical calls in this run" sentence
  moved into every approval card (`approval()`), above Allow/Deny/Always,
  which stay equal secondary buttons in `.toolbar.approval-actions`.
- Messages: `article.message.user/.assistant`; error, route line (`.hint`) and
  all disclosures grouped in `footer.message-meta`. Diagnostic disclosures get
  `details.raw-json`; "Tool arguments" in the approval card deliberately not.
- `notice(text, kind = 'error')` sets `data-kind`; confirmations use `info`
  (Code copied, Token saved/cleared, Cancellation requested, Speech ready,
  Recording stopped at the limit). Failures stay `error`.

Management
- `.page-header`: h2 + `#page-status` (snapshot note next to Refresh) + Refresh.
- Sections `section.panel` with `h3 "<Name> (<count>)"` for arrays; cards in
  `.card-grid` (auto-fill, min 22rem, one column on mobile).
- Card: `.card-header` (h4 name left, `.badges` right: Enabled/Disabled,
  `Process <state>`; tools: Destructive / requires policy review or Read-only
  annotation), description, `dl.meta` with one dt/dd per former sentence field
  (version, provides, PID, restarts, capabilities source, capabilities,
  manifest; task, inputs, outputs, agent, tools; plugin, original name; skill
  tools). Empty lists read "none". Long JSON/paths use full-width rows (`.wide`).
- Skills: Read button in `.card-actions`; the viewer appears right below it.
- Status: processes as small cards with state badge + PID/Restarts; resident
  models as `dl.meta`; cascade statistics and router stage counts as `<pre>`
  JSON blocks under h4 headings. Router: selected router, model calls, recent
  routes as `dl.meta`.

## Decisions and deviations from the plan

- `dl.meta` grid placement (display/grid columns/dt-dd margin) is in
  `layout.css` because the metadata otherwise rendered as stacked default
  `<dl>` (mobile Plugins page grew to 6835 px). Only placement; dt/dd
  typography/colour stay with `style.css`. Reviewer: check for duplicate rules.
- Composer hint: plan put the 4 MiB hint inside the action row; it is one hint
  line directly below the action row (with the 6 MiB limit), so Send can be the
  trailing control on every width without the hint wrapping between controls.
- Mobile hides the visible "Message" label with a clip technique (it remains
  the textarea's accessible name; the placeholder shows the purpose) to keep
  Send above the fold at 390×844 with 44 px targets.
- Mobile header follows plan 4.1 (row 1 brand + theme toggle, row 2 nav), so
  the theme toggle is visually above the nav but follows it in tab order.
  Accepted, single known A1 exception; desktop order matches visual order.
- Mobile nav wraps instead of scrolling (fits one row down to ~350 px) so
  focus rings are never clipped by a scroll box.
- Message role labels stay `h3` (plan typography says "h4 size"); `h4` would
  skip a level under the h2 conversation title (A4).
- Layout containers' padding (header, connection, notices, main gutters,
  composer top) is set in `layout.css` (legacy layout block owned it before).

## Measurements

Mock fixture `python3 scripts/web-smoke-server.py --port 8611`, Chromium via
CDP, device metrics override (mobile emulation for 390/320). Long conversation
= 20 alternating ~400-character messages via `localStorage['lh.conversations']`.
Values in CSS px. Legacy `style.css` (design task not yet merged) was active.

| Check | 1920×1080 | 1366×900 | 1024×768 | 390×844 | 320×640 |
|---|---|---|---|---|---|
| D1/M1 horizontal page scroll, all 5 views | none | none | none | none | none |
| D2 shell height (header + connection) | 97 | 97 | 97 | 128 | 204 |
| D3/M2 empty chat: page scrolls | no | no | no | no (Send bottom 785) | yes (fallback) |
| D4 long chat: page scrolls / `#messages` scrolls | no / yes | no / yes | no / yes | no / yes | yes / yes |
| `#prompt` top, `#send` bottom | 807, 1027 | 627, 847 | 417, 715 | 528, 785 | 609, 937 |
| D5 Send rightmost, Cancel directly before | yes | yes | yes | yes | yes |
| M3 Rename+Delete one row; list one row | – | – | – | yes; yes (56) | yes; yes |
| D6 Plugins page height (baseline 3355 at 1366) | 2459 | 2459 | 3223 | 5649 (baseline 4342) | 6307 |
| D6 badges on first card row; dot-joined sentences | yes; 0 | yes; 0 | yes; 0 | yes; 0 | yes; 0 |
| A4 one h1, no skipped heading levels | yes | yes | yes | yes | yes |
| Focus ring room ≥ 5 px inside scroll boxes | yes | yes | yes | yes | yes |

- With simulated 44 px targets on mobile (test-only style injected with CSP
  bypass, mimicking plan section 5 for buttons/inputs/selects/summary/check
  labels): 390×844 `#prompt` top 512, `#send` bottom 807 – M2 still holds.
- Agent approval card pending at 1366×900: `#messages` shrinks to its minimum,
  page does not scroll, Send stays at 807–847.
- Attachment preview pending: 390×844 Send bottom 836 (fits); 1366×900 846
  (fits); 1024×768 836 → page scrolls ~70 px (fallback, preview box bounded).
- Mobile Plugins page is taller than the baseline: card metadata is now one
  row per field instead of one wrapped sentence. Desktop target (D6) is met.
- One desktop screenshot (1366×900, long chat) was checked visually (scratch
  only, not committed): no overlap, clipping or broken rows. Screenshots under
  mobile emulation timed out in the CDP harness; mobile checks are DOM probes.

## Flows exercised on the mock (F1)

Ctrl+Enter send (trusted CDP key events) streamed the mock echo with route
footer; agent run via `web-agent` showed the approval card with arguments and
the "Always" rule; Allow → `Complete · 2 steps`; Deny → denied tool result,
`Complete · 2 steps`; Cancel during a pending approval → `Cancelled`, composer
re-enabled; image attach preview + Remove; unsupported file → error notice
(`data-kind=error`); Rename (stubbed prompt), New conversation (empty-state
hint), Delete (stubbed confirm); Export triggered one download; Toggle theme;
token save/clear → info notices and sessionStorage updated; hash navigation to
Skills with `aria-current` and `body[data-page]`; Skill Read viewer; Refresh;
back to Chat. Keyboard Tab order on desktop: nav → theme → connection →
sidebar → conversation actions → composer.

Note: the mock `web-agent` script is consumed once per fixture start, so each
approval flow (Allow, Deny, Cancel) needs a fresh fixture.

## Automated checks

- `scripts/test-web.mjs` (run by `cargo test`): new layout-structure section –
  layout.css after style.css, no inline `style=`, every plan section-4 ID
  exactly once, one h1, class contract (primary/danger/quiet), Send last with
  Cancel before it, approval rule moved into `approval()`, limits hint kept,
  notice kinds, no dot-joined metadata templates, message footer, conversation
  items, one 760 px breakpoint, all tokens with fallbacks, no CSS `order`, no
  visual properties in layout.css.
- `tests/server.rs`: `/layout.css` served as `text/css` with CSP/nosniff, HEAD
  and 405 like the other assets.
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo test --offline --workspace`: green (23 suites, 166 tests).

## Left for the design task (t_8d48f3dc) and review (t_bf3172bc)

- M4 (44 px at ≤ 760 px) for buttons, inputs, select (36 px today) and the
  connection `summary` (25 px) is a `style.css` rule (plan section 5); the
  layout budget above already accounts for it.
- Legacy `nav a[aria-current=page]` and `button.selected` set a 1 px outline
  that overrides `:focus-visible` (current nav item and selected conversation
  show a 1 px ring when focused). Selector specificity issue in `style.css`.
- Visual styles for `.badge*`, `.alert[data-kind]`, `.button-*`, `.hint`,
  `.status-line`, `details.raw-json`, `.message-meta`, `dl.meta dt/dd`.
- Review: delete the legacy layout block; `layout.css` already resets the
  legacy properties it overrides (aside border/padding, nav padding/border/
  margin, `#messages` max-height, `#storage-warning` margin, composer padding).

## Known limits

Chromium only, emulated mobile viewports, no real touch device or screen
reader. Measurements under light load (load average 0.9–1.5 during probes;
another worker's llama-server was idle on the host). No real-model run.
