# Web UI design system – results (t_8d48f3dc)

Scope: `crates/lh-cli/web/style.css` (rewritten, readable, no longer minified)
and the new offline check `scripts/test-style.mjs`. No markup, script,
`layout.css` or Rust file was changed (ownership per `docs/WEB-UI-PLAN.md`
section 6). No new dependency, no UI library, no inline styles (CSP).
The layout task (t_de1ed4d1, a09ee18) was already on `main`, so the results
below are measured with both stylesheets active.

## Design rules (the contract `style.css` implements)

Tokens (`:root`, both themes; layout.css consumes them with literal fallbacks):

| Group | Tokens |
|---|---|
| Spacing | `--space-1..6` = .25 / .5 / .75 / 1 / 1.5 / 2 rem – the only margin/padding/gap values in the design part (plus 0, auto, -1px for `.visually-hidden`) |
| Type | `--font-size-s .875rem` (hints, meta, labels, code – the minimum), `-m 1rem`, `-l 1.125rem`, `-xl 1.375rem`; body `1rem/1.55 system-ui`; headings 600 / 1.25; mono `--font-mono` |
| Radius | `--radius-s .4rem` controls, code, badges, alerts; `--radius-m .7rem` panels, cards, messages |
| Target | `--control-height` 40px, 44px at `max-width: 760px` (the only breakpoint) |
| Colour (new) | `--line-strong` control borders; `--accent-hover`, `--on-accent` primary button; `--active` pressed; `--user-bg` user message; `--error-bg`; `--ok`/`--ok-bg`, `--warn`/`--warn-bg` badges and approval |

Headings in UI chrome: h1 brand l; h2 view title xl (l on mobile so the
conversation title shares its row with Rename/Delete); sidebar "Conversations"
h2 as section label m; h3 section l; h4 card title m. Message role label (h3)
and approval title (h3) are m. Rendered Markdown is scaled below the chrome
(h1 xl, h2 l, h3–h6 m).

Components:

| Component | Rule |
|---|---|
| `button` | secondary: panel fill, `--line-strong` border, 500 weight; hover `--hover` + accent border; active `--active`; disabled opacity .5 + `not-allowed` |
| `.button-primary` / `#send` | accent fill, `--on-accent` text, 600; hover `--accent-hover` |
| `.button-danger` / `#delete-chat` | error text and border, `--error-bg` on hover |
| `.button-quiet` | transparent, borderless, muted text; border + text colour appear on hover |
| `.conversation-item` / `#conversation-list button` | borderless list entry; selected = `--hover` fill, 600, 3px inset accent bar (box-shadow, so the focus outline is untouched) |
| nav links | one tab style; `aria-current=page` = `--hover` fill, 600, 3px inset accent underline |
| `select`, text inputs, `textarea` | `--line-strong` border, control height; placeholder `--muted` |
| checkbox | 1.25rem, accent; its wrapping label gets the control height |
| file input | native file name text (s, muted); `::file-selector-button` styled as secondary button |
| `summary` | control height, text colour (not accent); `details.raw-json` and message-meta disclosures muted and s |
| `.alert` / `#notice`, `#storage-warning` | 4px left bar; `data-kind=error` (default) error border + `--error-bg`, `data-kind=info` accent border + `--hover`; text stays `--text` |
| `.badge`, `-ok`, `-warn`, `-muted` | s, 600, tinted fill + matching border |
| `dl.meta` | s; `dt` muted 600; `dd` text; code inside without tint |
| `.hint`, `.muted`, `.status-line` (`#run-status`, `#page-status`) | s, muted |
| `.panel`, `.card`, `.message`, `.approval` | border, `--radius-m`, shadow; nested card in panel: border only, `--bg` surface |
| `.message.user` / `.assistant` | user: `--user-bg` + 4px accent bar; assistant: panel + 4px `--line-strong` bar; `.message-meta` footer: top rule, s, muted |
| `.approval` | 4px `--warn` bar; Allow / Deny / Always stay plain secondary buttons |
| `.visually-hidden` | clip pattern |

Focus: `:focus-visible { outline: 3px solid var(--accent); outline-offset: 2px }`
is the only outline rule; no rule removes it. The legacy
`nav a[aria-current=page], button.selected { outline: 1px … }` that overrode
the ring (reported by the layout task) is gone.

Legacy layout rules are kept verbatim at the end of `style.css` under
`/* legacy layout – superseded by layout.css, removed in t_bf3172bc */`.
Only placement rules are in that block; the former visual rules mixed into it
(`header h1` font size, `nav a` padding/radius, `#notice` colours,
`.file-label input` 0.8rem font, `.approval button` margin) moved into the
design part as token-based rules.

## Contrast (WCAG ratios)

Token pairs (checked by `scripts/test-style.mjs`, minimum per theme):

| Pair | Light | Dark | Required |
|---|---:|---:|---:|
| `--line-strong` vs `--bg` / `--panel` / `--user-bg` / `--code` / `--hover` | 3.41 / 3.69 / 3.31 / 3.25 / 3.17 | 4.62 / 4.04 / 3.60 / 4.50 / 3.02 | 3 |
| old `--line` vs `--panel` (now only for decorative dividers) | 1.51 | 1.62 | – |
| `--text` on any surface (min: `--active`) | 12.03 | 8.00 | 4.5 |
| `--muted` on any surface (min: `--hover`) | 5.47 | 5.84 | 4.5 |
| `--on-accent` on `--accent` / `--accent-hover` | 6.97 / 9.20 | 8.70 / 10.94 | 4.5 |
| badge ok / warn text on fill | 5.70 / 6.20 | 8.64 / 9.48 | 4.5 |
| accent (links, focus ring) vs `--bg` / `--panel` | 6.44 / 6.97 | 8.93 / 7.81 | 4.5 |

Rendered DOM (Chromium, computed colours against the effective background,
all five views, notice and storage warning shown, connection open):
lowest text ratio 5.61:1 light (muted badge) / 6.95:1 dark; lowest control
boundary 3.41:1 light / 4.62:1 dark. Before: control borders 1.51 / 1.62.

## Measurements (mock fixture, Chromium CDP, CSS px)

`python3 scripts/web-smoke-server.py --port 8633`; mobile = device emulation.

| Check | 1366×900 | 1024×768 | 390×844 | 320×640 |
|---|---|---|---|---|
| Horizontal page scroll, 5 views | none | none (chat) | none | none (chat) |
| Shell (header + collapsed connection) | 64 + 49 = 113 (D2 ≤ 120) | 113 | 108 + 45 | 156 + 45 |
| Empty chat `#prompt` top / `#send` bottom, page scroll | 625 / 846, no | 417 / 735, no | 498 / 791, no (M2) | 584 / 973, yes (fallback, as before) |
| Visible controls below target height (40 / 44) | 0 | 0 | 0 | 0 |
| Plugins page height (baseline 3355 / 4342) | 2455 | – | 5240 | – |

- Only inline links inside message text are below the target height (19 px):
  inline links are exempt from the target-size rule.
- Checkbox squares are 20 px; the whole label (44 px on mobile) is the target.
- Pending agent approval at 1366×900: page does not scroll, Send bottom 846.
- Tab order (desktop, both themes, chat and plugins): every focused element
  matched `:focus-visible` with a 3 px solid accent outline – nav (5), theme,
  connection summary, New conversation, conversation item, Export, Rename,
  Delete, prompt, model, Refresh models, Agent, Speak, file input, Record, Send;
  on Plugins also Refresh and every `Full … JSON` summary.

## Flows exercised (unchanged behaviour)

On the mock: send via Send (streamed reply, Markdown with code and link,
route footer); New conversation (empty-state hint); agent run with
`web-agent` + Agent mode → approval card (warn bar, three equal secondary
buttons) → Always → `Complete · 2 steps`; token Save → info notice
(`data-kind=info`, accent/hover style), Clear; Toggle theme light ↔ dark; hash
navigation to Plugins, Tools, Skills, Status and back. The remaining F1 flows
(Ctrl+Enter, Deny, Cancel, attach, export, rename, delete, skill Read) are
markup/script behaviour verified by the layout task on a09ee18; this change
touches only CSS.

## Automated checks

- `node scripts/test-style.mjs` (new, offline): scale tokens and values, both
  dark blocks identical, all contrast pairs above in both themes, spacing and
  font sizes only from the scale, single 760 px breakpoint, focus rule and no
  outline removal, 40/44 px target rule, disabled state, all plan section-5
  component selectors and ID fallbacks present, no approval-specific button
  styles, no layout selectors in the design part, legacy block marker present.
  Mutation-checked: lowering `--line-strong`, an off-scale padding, an
  `outline:none`, a second breakpoint and an `.approval button` rule each fail.
- `node scripts/test-web.mjs`, `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --offline --workspace`: green (23 suites, 166 tests; load
  average 0.92 before, 3.84 after the test run).

## Label / ARIA findings for the markup owner (not changed here)

- `#conversation-list` is a `div` with `aria-label` but no role; the label is
  not announced on a generic element. Suggest `role="list"`/`listitem` or a
  `nav` with `aria-label`.
- `#messages`, `#approvals`, `#attachments` likewise carry `aria-label` on
  generic `div`s (ignored by most screen readers); `role="log"` for
  `#messages` (with `aria-live` handled by `#run-status`) or `role="region"`
  would make the labels effective.
- The selected conversation sets `aria-current="true"` and `aria-current="false"`
  on the others – fine, but `false` could simply be omitted.
- The mobile prompt label is visually hidden in `layout.css` with its own clip
  rule; the shared `.visually-hidden` class now exists in `style.css`.
- Conversation titles are derived from the raw first prompt, so Markdown
  syntax (`**`, backticks, `[link](…)`) appears in the h2 and the list; long
  titles wrap to 3–5 lines on mobile (390 px: header 113 px, Send then ends
  below the fold, 859 > 844). Suggest a plain-text title and a 1–2 line clamp
  in `layout.css` (overflow/line-clamp is placement, not a `style.css` rule).
- Card header: the badge group (`max-width: 60%`) squeezes long names
  (`web-agent-backend` wraps). Process badge text shows the raw enum
  (`Process not_started`).
- The mobile conversation strip shows a horizontal scrollbar for the list by
  design (layout); acceptable, noted for review.

## Known limits

Chromium only (CDP), emulated mobile viewports, no real touch device, no
screen reader, no Windows High Contrast check (`forced-colors`). Background
tab: screenshots via CDP can time out, so most checks are DOM probes; three
screenshots (desktop dark chat, desktop light Plugins, 390 px light chat) were
reviewed. Button colour transitions were removed: they froze mid-way in a
background tab and made computed-colour probes unreliable, and they were not
needed. `scripts/test-style.mjs` is not yet run by `cargo test` because
`crates/lh-cli/tests/server.rs` belongs to the layout task; hooking it in is
left to the review task t_bf3172bc.
