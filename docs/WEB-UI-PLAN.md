# Web UI cleanup – analysis and target structure

Status: plan (task t_a699e8d9). Implemented by two parallel tasks with disjoint
file ownership (section 6), then reviewed together (t_bf3172bc). Scope is the
embedded M4c browser UI in `crates/lh-cli/web/` at commit 3073b1b (includes
Ctrl+Enter send). No product feature is added or removed; every existing
control, route (hash page), API call, guard and message keeps working.

## 1. Inventory

| File | Size | Content |
|---|---:|---|
| `web/index.html` | 17 lines | App shell: header, nav, connection `<details>`, notices, Chat page (sidebar + chat main + composer), one generic management page container |
| `web/style.css` | 5 (minified) lines | Theme tokens (light/dark), base elements, all layout, all components, one 760 px breakpoint |
| `web/app.mjs` | ~350 lines | All behaviour and all dynamically rendered markup: messages, attachments, approvals, conversation list, management cards |
| `web/markdown.mjs` | 48 lines | Escaping Markdown renderer (security boundary – do not change) |
| `src/server/web.rs` | – | Compile-time asset table (`/`, `/index.html`, `/app.mjs`, `/markdown.mjs`, `/style.css`), CSP `style-src 'self'` (no inline `style=` attributes) |
| `tests/server.rs`, `scripts/test-web.mjs` | – | Asset/CSP tests; Node checks for Markdown, SSE, body limits, Ctrl+Enter |

Views: **Chat** (`#chat`, default), **Plugins** (sections Plugins, Specialists,
Router), **Tools**, **Skills** (per-skill Read viewer), **Status** (Status, Router).
Management views share one container filled by `management()` in `app.mjs`.
Reusable UI pieces today are implicit: `button()`, `el()`, `jsonDetails()`,
`.card`, `.panel`, `.message`, `.approval`, `.toolbar`, `.muted`.

## 2. Evidence (executable preview was available)

A preview **was** runnable: `cargo build --offline --workspace`, then
`python3 scripts/web-smoke-server.py --port 8495` (mock backends, no models,
no token required) and Chromium via CDP at 1366×900 and 390×844 (mobile
emulation). Screenshots were taken but are scratch-only, not committed.
Measured (x, y, w, h in CSS px):

| Measurement | 1366×900 | 390×844 |
|---|---|---|
| Header + nav + connection strip height | 167 | 161 |
| Chat document height (empty conversation) | 912 (page scrolls) | 1294 |
| `#prompt` top | 566 | 794 (50 px above the fold) |
| `#send` | 731,767 71×40 | 151,1127 – **283 px below the fold** |
| `#messages` | min 200, max 60vh | same |
| Plugins page height | 3355 | 4342 |
| Horizontal overflow | none | none |

Contrast (WCAG ratio, computed from the tokens): text/muted/accent/error all
≥ 5.8:1 in both themes (pass). **Control borders `--line` vs panel: 1.51:1
light, 1.62:1 dark – fails the 3:1 non-text contrast** for inputs and buttons.

## 3. Problems found

Orientation
- O1 Header, nav and connection are three stacked full-width bands (167 px)
  before any content; the theme toggle looks like a primary action.
- O2 Selected nav item is boxed, others are bare links: the nav does not read
  as one tab group. Accent blue marks links, selection, disclosures and Send
  alike, so it no longer signals priority.
- O3 Mobile chat: conversation list, title row and empty 200 px message box
  come first; the textarea starts at the fold and Send needs scrolling.
- O4 Desktop chat with a long conversation: `#messages` is 60vh and the page
  scrolls, so the composer leaves the viewport.

Information hierarchy
- H1 Same visual weight for nearly all buttons; Send differs only by border
  and bold. Delete (destructive) looks like Rename.
- H2 Sidebar: the "New conversation" action and a selected conversation titled
  "New conversation" look the same; Export sits inside the list.
- H3 Management: "Plugins" appears as nav, page h2 and section h3; Tools,
  Skills and Status repeat the same pattern. Card metadata is one
  dot-joined sentence (`Enabled · version … · PID …`), raw arrays
  (`JSON.stringify(capabilities)`, stage counts, cascade) are inline text.
- H4 Chat messages: user/assistant differ only by a 3 px border; route line,
  "Full route details", agent steps and response metadata are not grouped
  as message metadata; disclosures are as loud as content.
- H5 `#notice` uses error styling (red, `role=alert`) also for success notes
  ("Token saved", "Code copied.").

Grouping
- G1 Composer mixes four concerns in two `.toolbar` rows: model routing
  (+Refresh models), agent mode, speech output, media input, submission.
  Send is in the middle of the row, Cancel next to Record audio.
- G2 One help paragraph mixes attachment limits, request limit and the
  approval "Always" rule, far from the related controls.
- G3 Snapshot note ("not automatically refreshed") is separated from Refresh.

Visual consistency
- V1 File input renders a native "Choose Files" control with a label above
  it; the row is vertically staggered against the buttons.
- V2 No spacing/typography scale: ~25 ad-hoc rem values, h3 = h4 = 1rem,
  `.muted` 0.88rem vs `label` 0.92rem vs connection note 0.85rem.
- V3 Section panels and record cards are identical boxes nested in each other.
- V4 Control border contrast below 3:1 (section 2).

## 4. Target structure

All IDs used by `app.mjs`/tests stay (`composer`, `prompt`, `send`, `cancel`,
`model`, `agent`, `speak`, `files`, `record`, `attachments`, `approvals`,
`messages`, `run-status`, `conversation-list`, `conversation-title`,
`new-chat`, `rename-chat`, `delete-chat`, `export`, `auth-form`, `token`,
`clear-token`, `theme`, `notice`, `storage-warning`, `chat-page`,
`management-page`, `page-title`, `page-status`, `refresh-page`,
`management-content`, `speech-player`; `refresh-models` was later removed –
the model list refreshes when `#model` is opened). Wrappers and classes
may be added; DOM order must equal visual order (no CSS `order` that breaks
tab order).

### 4.1 App shell (all views)
```
header.app-header   [h1 brand + subtitle] [nav.app-nav: Chat Plugins Tools Skills Status] [Toggle theme (quiet)]
details.connection  (collapsed strip directly under the header, content width)
div.notices         #notice, #storage-warning (alert banners, content width)
main                (view)
```
- Desktop (≥ 761 px): brand, nav and theme toggle on one row; shell height
  (header + collapsed connection) ≤ 120 px.
- Mobile (≤ 760 px): row 1 brand + theme toggle, row 2 nav (single line,
  horizontally scrollable if needed); subtitle hidden as today.
- Nav: all items share one tab style; current item via `aria-current=page`.

### 4.2 Chat view (priority 1)
```
#chat-page (grid: sidebar | chat main; mobile: stacked)
  aside.conversations
    header: h2 "Conversations" + #new-chat
    #conversation-list (list items, visually distinct from action buttons)
    footer: #export + storage hint ("Saved only in this browser …")
  .chat-main (column filling the viewport height on desktop)
    .conversation-header: h2#conversation-title + [Rename] [Delete (danger)]
    #messages (flex: 1, scrolls internally)
    #approvals
    #composer
      .composer-input: [mic button #record] textarea#prompt (fills the row) (+ label "Message" above)
      #attachments
      .composer-options: Model select (refreshes on open) | Speak replies (agent mode is always on, no toggle)
      .composer-actions: [Attach images or audio] + hint "max 4 MiB per turn"   …   [Cancel][Send (primary)]
      hint: request limit 6 MiB
    #run-status (status line directly above .composer-actions or composer)
    #speech-player
```
- Send is the trailing (rightmost) control of the composer action row on all
  widths; Cancel sits immediately before it.
- The approval "Always applies only to identical calls in this run" text moves
  into each approval card (`approval()` in `app.mjs`). No text is dropped, only
  relocated. (The former Agent mode checkbox was removed later: chat always
  runs as an agent run.)
- Mobile: sidebar becomes a compact strip in unchanged DOM order – row 1
  heading + New conversation; row 2 the list as a single horizontally
  scrolling row; row 3 (or the end of row 2) Export as a quiet button; the
  storage hint stays hidden on mobile as today. Strip height ≤ 150 px. `#messages` min-height 120 px on mobile. Rename and Delete never wrap onto
  separate rows from each other.
- Messages: `article.message.user` / `.message.assistant`; role label;
  content; then `footer.message-meta` containing route line, error and all
  `<details>` (route, agent steps, response metadata). Disclosures are quiet.

### 4.3 Management views (priority 2)
```
#management-page
  .page-header: h2#page-title  [#page-status (snapshot note)] [Refresh]
  #management-content
    section.panel  h3 "<Section> (<count>)"   (count only for arrays)
      .card-grid   (auto-fill, min 22rem; 1 column on mobile)
        article.card
          .card-header: h4 name + badges (Enabled/Disabled, process state; tool: Destructive / Read-only)
          p description (if any)
          dl.meta  (one dt/dd pair per existing field: version, provides, PID, restarts,
                    capabilities source, capabilities, manifest path; task, inputs, outputs,
                    agent, tools; plugin, original name; skill tools)
          card actions (Skills: Read <name>), skill viewer
          details.raw-json "Full entry JSON"
      details.raw-json "Full management JSON" / "Built-in tool schemas"
```
- Section heading text: keep the API key name, capitalised; when the page has
  a single section of the same name, its `h3` stays (accessibility) but is
  styled as a section label, not a second title.
- Status/Router: processes and resident models as `dl.meta` rows; cascade and
  stage counts as `<pre>` JSON blocks instead of inline JSON text.
- Empty values: show "none" (existing wording), never a dangling label
  (today `tools ` with empty list).

### 4.4 Priority list
- P1 App shell compaction and consistent nav (O1, O2).
- P1 Composer grouping and Send placement; chat viewport fit (G1, G2, O3, O4).
- P1 Button hierarchy, control border contrast, focus ring (H1, V4).
- P2 Sidebar grouping (H2); message metadata grouping (H4); notice kinds (H5).
- P2 Management page header, card header + badges + `dl.meta`, card grid (H3, G3, V3).
- P3 File input styling (V1); empty-state hint in `#messages` (static text,
  e.g. "No messages yet."); typography clean-up inside rendered Markdown.

## 5. Design rules (shared contract)

Tokens are defined in `style.css` `:root` (both themes). `layout.css` may only
consume them, always with the literal fallback given here, e.g.
`var(--space-4, 1rem)`.

Spacing scale: `--space-1 .25rem`, `--space-2 .5rem`, `--space-3 .75rem`,
`--space-4 1rem`, `--space-5 1.5rem`, `--space-6 2rem`. No other spacing values
except 0 and 1–3 px borders.
- related controls within a group: `--space-2`; between groups: `--space-4`;
  between page sections: `--space-5`; component padding: `--space-3`
  (compact) or `--space-4`; page gutter `--space-5` desktop, `--space-3` mobile.

Typography: body `1rem/1.55 system-ui`; `--font-size-s .875rem` (hints,
meta, muted – never smaller), `--font-size-m 1rem`, `--font-size-l 1.125rem`,
`--font-size-xl 1.375rem`; headings line-height 1.25, weight 600.
- h1 brand: l; h2 view title (conversation title, page title): xl;
  sidebar "Conversations" h2: styled as section label (m, 600);
  h3 section/panel: l; h4 card title and message role label: m.
- Monospace for code, paths, IDs, JSON: `ui-monospace, monospace`, s.
- Headings never skip levels in UI chrome (h1 → h2 → h3 → h4). Rendered
  Markdown/skill content is user content and exempt.

Radius / elevation: `--radius-s .4rem` (controls, code), `--radius-m .7rem`
(cards, panels, messages). Shadow only on top-level cards/messages; nested
boxes use border only (panel inside panel: no second shadow).

Colour: existing tokens; add `--line-strong` for control borders with ≥ 3:1
against `--panel` and `--bg` in both themes. Accent is reserved for primary
action, current nav item, focus ring, links. Text contrast ≥ 4.5:1.

Component classes (styled in `style.css`, applied by markup in
`index.html`/`app.mjs`):

| Class / selector | Meaning |
|---|---|
| `button` (default) | secondary action |
| `.button-primary` (+ fallback `#send`) | one per region: Send; Save token |
| `.button-danger` (+ fallback `#delete-chat`) | destructive: Delete conversation |
| `.button-quiet` | low emphasis: Toggle theme, Copy code, Clear token |
| `.alert`, `[data-kind=error]` (default) / `[data-kind=info]` | `#notice`, `#storage-warning` |
| `.badge`, `.badge-ok`, `.badge-warn`, `.badge-muted` | state labels on cards |
| `dl.meta` | key/value metadata grid |
| `.hint` | help text next to a control (s, muted) |
| `.status-line` | `#run-status`, `#page-status` |
| `.panel`, `.card`, `.message`, `.message.user`, `.message.assistant`, `.message-meta`, `.approval` | containers |
| `details.raw-json` | quiet diagnostic disclosure |
| `.conversation-item` | entry in `#conversation-list` (with `aria-current`) |
| `.visually-hidden` | accessible-only text |

Approval buttons Allow / Deny / Always stay equal secondary buttons (no
primary nudge towards allowing tools).

Interaction states: hover, active, disabled (opacity + `not-allowed`),
`:focus-visible` 3 px accent outline with 2 px offset on every interactive
element including `summary`, file input, checkbox, nav links. Minimum target
height 40 px; 44 px at ≤ 760 px. One breakpoint: `@media (max-width: 760px)`
in both CSS files (the value is shared, do not introduce others without
updating this plan).

## 6. Ownership (parallel implementation)

| File | t_8d48f3dc design/components | t_de1ed4d1 layout/structure |
|---|---|---|
| `crates/lh-cli/web/style.css` | **owner** | do not edit |
| `crates/lh-cli/web/layout.css` (new) | do not edit | **owner** (creates it) |
| `crates/lh-cli/web/index.html` | do not edit | **owner** (adds `<link rel=stylesheet href=/layout.css>` after style.css) |
| `crates/lh-cli/web/app.mjs` | do not edit | **owner** (markup classes, notice kind, metadata grouping) |
| `crates/lh-cli/web/markdown.mjs` | do not edit | do not edit (security boundary) |
| `crates/lh-cli/src/server/web.rs` | do not edit | **owner** (adds `/layout.css`, `text/css`) |
| `crates/lh-cli/tests/server.rs` | do not edit | **owner** (adds `/layout.css` to asset test) |
| `scripts/test-web.mjs` | do not edit | **owner** |
| `docs/ARCHITECTURE.md` (asset list) | do not edit | **owner** |
| `docs/WEB-UI-STYLE-RESULTS.md` (new) | **owner** | – |
| `docs/WEB-UI-LAYOUT-RESULTS.md` (new) | – | **owner** |
| `docs/WEB-UI-PLAN.md` (this file) | read only | read only |

Selector ownership:
- `style.css`: `:root` tokens and theme blocks, element selectors (`*`, `body`,
  `a`, headings, `button`, `input`, `select`, `textarea`, `label`, `code`,
  `pre`, `details`, `summary`, `audio`, `img`), `:focus-visible`, and all
  component classes in section 5 (visual properties: colour, border, radius,
  padding, font, shadow, states).
- `layout.css`: placement only – `header`/`.app-header`, `nav`/`.app-nav`
  arrangement, `main`, `.connection` placement, `.notices`, `#chat-page`,
  `aside`/`.conversations`, `.chat-main`, `.conversation-header`,
  `#messages` sizing/scroll, `#composer` and `.composer-*` arrangement,
  `.toolbar`, `.page-header`, `.card-grid`, `.card-header`, `#attachments`
  arrangement, and their 760 px media query. Properties: display, grid/flex,
  gap, order-free alignment, width/height/max-*, overflow, position, margin.
- Legacy layout rules currently in `style.css` (`header…`, `nav{display…}`,
  `main{padding}`, `#chat-page`, `aside`, `#conversation-list` arrangement,
  `.chat-main`, `.toolbar`, `#messages` sizing, `#composer` border/padding,
  `#attachments`, the layout part of the 760 px media query): the design task
  keeps them **verbatim** in one block at the end of `style.css` marked
  `/* legacy layout – superseded by layout.css, removed in t_bf3172bc */`.
  `layout.css` loads later and overrides them (it must reset legacy properties
  it does not want, e.g. `aside{border-right}`). The review task deletes the
  block once both are merged. This keeps `main` working whichever task lands first.
- The design task may target stable IDs as fallbacks (`#send`, `#delete-chat`,
  `#notice`, `#storage-warning`, `#run-status`, `#page-status`,
  `#conversation-list button`) so its result is visible before the layout
  markup lands.
- Label text / `aria-*` changes are markup and belong to the layout task; the
  design task lists any label problems it finds in its results doc.

## 7. Acceptance criteria (checkable)

Run with `scripts/web-smoke-server.py` (mock) and a Chromium DOM probe;
"long conversation" = 20 alternating messages of ~400 characters preloaded via
`localStorage['lh.conversations']` before reload.

Desktop (1366×900, and 1920×1080, 1024×768)
- D1 No horizontal page scroll (`scrollWidth ≤ clientWidth`) on all five views.
- D2 Collapsed shell (header + nav + connection) ≤ 120 px tall at 1366.
- D3 Empty chat: no page scroll at 1366×900; textarea and Send fully visible.
- D4 Long conversation: page does not scroll; `#messages` scrolls; textarea
  and Send stay fully in the viewport at 1366×900 and 1024×768.
- D5 Send is the rightmost control of the composer action row; Cancel directly before it.
- D6 Management cards: name and state badges in the first row of every card;
  no dot-joined metadata sentences remain; Plugins page height at 1366 lower
  than the measured 3355 px with identical data.

Mobile (390×844 and 320×640, mobile emulation)
- M1 No horizontal page scroll on all five views (nav may scroll internally).
- M2 Empty chat at 390×844: `#prompt` top and `#send` bottom within 844 px without scrolling.
- M3 Rename and Delete share one row; conversation list occupies at most one row.
- M4 All buttons, inputs, nav links ≥ 44 px tall at ≤ 760 px.

Both
- A1 Tab order follows visual order; every interactive element shows a
  visible focus ring in light and dark theme.
- A2 Every form control has an accessible name; unchanged IDs from section 4.
- A3 Text contrast ≥ 4.5:1, control borders/focus ring ≥ 3:1, both themes.
- A4 Exactly one `h1`; no skipped heading levels in UI chrome.
- A5 Success notices are not styled as errors (`data-kind=info`).
- F1 Unchanged behaviour: `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace` (includes
  `node scripts/test-web.mjs`) green; manual mock flows from
  `docs/M4C-RESULTS.md` (stream, Ctrl+Enter, approvals Allow/Deny/Always,
  cancel, attach, export, rename, delete, theme, token save/clear, hash
  navigation, Refresh, skill Read) still work.

## 8. Known limits

- Only Chromium (CDP) on this Linux host; no Firefox/Safari, no real touch
  device, no screen reader run. Mobile = emulated viewport.
- Microphone/MediaRecorder hardware not testable (see M4C-RESULTS).
- Real model output is not needed for layout checks; mock backends suffice.
