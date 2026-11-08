# Web UI cleanup – review results (t_bf3172bc)

Review of the merged layout (t_de1ed4d1, a09ee18) and design-system (t_8d48f3dc,
3e9f896) changes against `docs/WEB-UI-PLAN.md` section 7. Scope: find and fix
regressions caused by the rework, remove the legacy layout block, add
regression tests. No product feature was added; all IDs, routes, API calls,
guards and texts are unchanged.

## Method

- `cargo build --offline --workspace`, `python3 scripts/web-smoke-server.py --port 8711`
  (mock backends, no models), headless Chromium 145 over CDP. Mobile = device
  emulation (`mobile: true`, touch) at 390×844 and 320×640; desktop 1920×1080,
  1366×900, 1024×768. "Long conversation" = 20 alternating ~400-character
  messages plus a second conversation via `localStorage['lh.conversations']`;
  "long title" = a 60–90 character title with Markdown syntax.
- DOM probes per view and viewport: page scroll, element boxes outside the
  viewport or their parent, control heights, heading levels, Send position,
  card header geometry; keyboard walk with real `Tab` key events (order,
  `:focus-visible`, outline, outline clipped by a scroll box).
- Legacy block: every view × viewport × theme (plus connection open, both
  notices shown) was snapshotted (box + 36 computed properties of every
  element), the legacy rules were deleted in-page via CSSOM and the snapshot
  repeated.
- Screenshots (scratch only, not committed; listed in the task handoff).
- After rebasing onto `ce660ff` (generation statistics line under the chat,
  speak-ID selection) the whole probe matrix was repeated on the merged build,
  including a chat with a reply (statistics line visible) and a pending
  approval at all five sizes: same results as below.
- Host load average 1–11 during probes (another worker's builds and an idle
  llama-server); no timing numbers are reported, only geometry.

## Regressions found and fixed

| # | Finding (before) | Fix |
|---|---|---|
| R1 | **Blocker:** a pending agent approval card was squeezed by `#approvals { flex: 0 1 auto; min-height: 0; overflow: auto }`: 0 px tall (invisible, Allow/Deny/Always unreachable by sight) at 1024×768, 390×844 and 320×640; at 1366×900 the box was 200 px for a 237 px card, cutting the buttons. | `#approvals` no longer shrinks; while it has content `#messages` may shrink to 6rem; beyond that the document scrolls (documented fallback). `approval()` calls `card.scrollIntoView({block:'nearest'})`. Now: card fully visible at 1920/1366 without page scroll (1366: card 295–533, Send 806–846), at 1024 and 390 visible on load (page scrolls to reach Cancel/composer), Always/Deny/Cancel flows complete. |
| R2 | Card header badges were `white-space: nowrap` inside a 60 % group: "Destructive / requires policy review" overlapped the `write_file` title at 1366, and at 320 px it started at x = −25 (cut off, not scrollable); "Read-only annotation" covered names on mobile. | `.badge` may wrap (`max-width: 100%`, `overflow-wrap: anywhere`), the badge group can shrink (`min-width: 0`). 0 overlaps / 0 out-of-card badges on Plugins, Tools, Status at all five sizes; badges stay on the first row (11/11, 5/5, 5/5). |
| R3 | Long conversation titles wrapped: 390×844 Send bottom 859 (> 844, page scroll), 1024×768 page scrolled 11 px (D4). | Title clamped to 1 line on desktop and 2 lines on mobile (layout.css, `-webkit-line-clamp` + `line-clamp`); the full title stays in the DOM, in the new `title` attribute and in the sidebar list. Long title now: 390 Send bottom 791, 1024 no page scroll. |
| R4 | Mobile conversation items (max 200 px) were wider than the 147 px list box: the item text and its focus ring were clipped (Tab walk: ring clipped on both items, second item at x = −4). | Item `max-width: min(12.5rem, 100%)`, list `scroll-padding-inline: .5rem`. Tab walk 390/320, light/dark: 0 clipped rings. |
| R5 | 320×640: the native file input made `.file-label` 303 px wide in a 296 px column (7 px into the gutter); `#files` was the rightmost control instead of Send. | `.file-label` and its input `max-width: 100%; min-width: 0`. Send is the rightmost control at all sizes. |
| R6 | Removing the legacy block changed exactly one thing across all 55 snapshots: the 1 px `--line` divider under the mobile conversation strip (`aside { border-bottom }` in the legacy media query; on mobile chat everything below it moved up 1 px). Desktop snapshots were identical. | Divider kept as a design rule in style.css (`@media (max-width: 760px) { .conversations { border-bottom: 1px solid var(--line) } }`), then the block was deleted. |

The legacy layout block at the end of `style.css` is removed (plan section 6).

## Acceptance criteria (after fixes)

| Criterion | Result |
|---|---|
| D1 / M1 no horizontal page scroll, 5 views | yes at 1920, 1366, 1024, 390, 320; no element outside the viewport |
| D2 shell ≤ 120 px at 1366 | 113 (baseline 167); 390: 153, 320: 201 |
| D3 empty chat 1366×900 | no page scroll; `#prompt` 625–750, Send 806–846 |
| D4 long conversation, page fixed, `#messages` scrolls | yes at 1920, 1366, 1024 (also with long title); 390 too; 320 uses the documented page-scroll fallback (doc 1020 px) |
| D5 Send rightmost, Cancel before it | yes at all five sizes (320 was `#files` before R5) |
| D6 badges first row, no dot-joined metadata, Plugins < 3355 px at 1366 | 11/11, 5/5, 4/4, 5/5; 0 dot-joined; 2476 px (2390 before rebasing onto ce660ff) |
| M2 empty chat 390×844 | `#prompt` top 498, Send bottom 791, no scroll |
| M3 Rename+Delete one row, list one row | yes (390 and 320) |
| M4 controls ≥ 44 px at ≤ 760 px | 0 visible buttons/inputs/selects/nav links/summaries below 44 px (checkbox squares 20 px, their labels 44 px) |
| A1 tab order / focus ring | desktop: nav → theme → connection → sidebar → title actions → messages → composer → Send; mobile: theme after nav (documented exception). Every stop `:focus-visible` with 3 px solid accent outline in light and dark; no ring clipped (after R4) |
| A2 accessible names, IDs | unchanged IDs (test-web contract); not re-audited beyond that |
| A3 contrast | tokens unchanged; `test-style.mjs` pairs pass (see WEB-UI-STYLE-RESULTS) |
| A4 one h1, no skipped levels | yes, all views and sizes |
| A5 success notices `data-kind=info` | Token saved / Token cleared → info; unsupported file → error |
| F1 behaviour | see flows below; fmt/clippy/test green |

## Flows exercised on the mock (after fixes)

Ctrl+Enter send (trusted key events) → streamed echo, Markdown rendered,
`footer.message-meta`; Send button turn; image attach preview (page still fits
at 1366, Send 846) and Remove; unsupported file → error notice; Rename (stubbed
`prompt`, long title clamped, `title` attribute set); New conversation
("No messages yet."); Delete (stubbed `confirm`); Export (one download);
Toggle theme light ↔ dark (persisted); token Save / Clear (info notices,
sessionStorage updated); hash navigation to all five views with
`aria-current` and `body[data-page]`; Refresh; Skill Read viewer; agent run
with `web-agent`: Always at 390×844 → `Complete · 2 steps`, Cancel while
pending at 1366×900 → `Cancelled`, composer re-enabled, Deny at 1024×768 →
`Complete · 2 steps` (fresh fixture per approval flow).

## Automated checks

- `cargo test` now runs both Node checks (`tests/server.rs`
  `javascript_checks_when_node_is_installed`): `scripts/test-web.mjs` and
  `scripts/test-style.mjs` (the latter was not wired before).
- New regression assertions: style.css has no legacy block and no bare
  `header`/`nav`/`main`/`aside`/`#messages`/`#attachments`/`.toolbar` layout
  rules, keeps the mobile strip divider, badges may wrap; layout.css keeps the
  title clamp (1/2 lines), badge group `min-width: 0`, mobile item width and
  scroll padding, file-label bounds, non-shrinking `#approvals` with the
  `:has()` rule; app.mjs sets the title attribute and scrolls the approval card
  into view. Mutation-checked: reverting each fix makes a check fail (9/9).
- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --offline --workspace`: green, 23 suites, 166 tests.

These are static source checks; the geometry itself was verified only with the
browser probes above (no browser in `cargo test`, tests stay offline).

## Remaining limits (known, not fixed here)

- Chromium only (CDP), emulated mobile, no real touch device, no screen
  reader, no Firefox/Safari, no `forced-colors` check. `:has()` needs
  Firefox ≥ 121 / Safari ≥ 15.4; without it `#messages` keeps 10rem/120 px and
  the page scrolls earlier while an approval is pending (card still visible).
- While an approval is pending at 1024×768 and on mobile the page scrolls:
  the card is in view, Cancel and the (disabled) composer are below it.
- 320×640 chat always scrolls the page (fallback, as before).
- Conversation titles are the raw first prompt, so Markdown syntax appears in
  the title and list (pre-existing; a plain-text title would be a behaviour
  change). Truncated titles are complete only in the sidebar on desktop and the
  Rename dialog on mobile (tooltips do not exist on touch).
- On 320 px the badge group (max 60 %) wraps "Destructive / requires policy
  review" to three lines and long plugin names to 2–3 lines.
- Mobile conversation strip shows a horizontal scrollbar for the list (by
  design); the mobile theme toggle precedes the nav visually but follows it in
  tab order (documented A1 exception).
- `aria-label` on generic `div`s (`#conversation-list`, `#messages`,
  `#approvals`, `#attachments`) is not announced by most screen readers –
  reported by the design task, unchanged (markup/ARIA change, not a regression).
- The "Cancelled …" notice after a cancel is `data-kind=error` (the run ends
  with an error message); unchanged behaviour.
