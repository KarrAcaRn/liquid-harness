#!/usr/bin/env node
// Static design-system checks for crates/lh-cli/web/style.css (docs/WEB-UI-PLAN.md section 5,
// results in docs/WEB-UI-STYLE-RESULTS.md). Offline, no dependencies: node scripts/test-style.mjs
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const css = readFileSync(new URL('../crates/lh-cli/web/style.css', import.meta.url), 'utf8');
const design = css;

const block = selector => {
  const start = design.indexOf(selector + ' {');
  assert(start >= 0, `missing block ${selector}`);
  return design.slice(start, design.indexOf('}', start));
};
const tokens = text => Object.fromEntries([...text.matchAll(/(--[\w-]+):\s*([^;]+);/g)].map(m => [m[1], m[2].trim()]));
const light = tokens(block(':root'));
const dark = tokens(block(':root[data-theme=dark]'));
const darkMedia = tokens(block(':root:not([data-theme=light])'));

// Fixed scale from the plan (names and values are a shared contract with layout.css fallbacks).
const scale = {
  '--space-1': '.25rem', '--space-2': '.5rem', '--space-3': '.75rem', '--space-4': '1rem', '--space-5': '1.5rem', '--space-6': '2rem',
  '--font-size-s': '.875rem', '--font-size-m': '1rem', '--font-size-l': '1.125rem', '--font-size-xl': '1.375rem',
  '--radius-s': '.4rem', '--radius-m': '.7rem',
};
for (const [name, value] of Object.entries(scale)) assert.equal(light[name], value, name);

// Both dark blocks (explicit toggle and system preference) must stay identical.
assert.deepEqual(darkMedia, dark, 'dark theme blocks differ');

const colours = ['--bg', '--panel', '--text', '--muted', '--line', '--line-strong', '--accent', '--accent-hover', '--on-accent', '--hover',
  '--active', '--user-bg', '--code', '--error', '--error-bg', '--ok', '--ok-bg', '--warn', '--warn-bg'];
const hex = value => { const h = value.replace('#', ''); return h.length === 3 ? [...h].map(c => c + c).join('') : h; };
const luminance = value => {
  const [r, g, b] = [0, 2, 4].map(i => parseInt(hex(value).slice(i, i + 2), 16) / 255)
    .map(c => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
};
const ratio = (a, b) => { const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };

// [foreground, background, minimum]: text 4.5:1, control borders / focus ring / filled buttons 3:1 (WCAG 1.4.3, 1.4.11).
const pairs = [
  ...['--bg', '--panel', '--hover', '--active', '--user-bg', '--code', '--error-bg', '--ok-bg', '--warn-bg'].map(bg => ['--text', bg, 4.5]),
  ...['--bg', '--panel', '--hover', '--user-bg', '--code'].map(bg => ['--muted', bg, 4.5]),
  ...['--bg', '--panel', '--hover', '--user-bg'].map(bg => ['--accent', bg, 4.5]),
  ...['--bg', '--panel', '--hover', '--error-bg'].map(bg => ['--error', bg, 4.5]),
  ['--ok', '--ok-bg', 4.5], ['--ok', '--panel', 4.5], ['--warn', '--warn-bg', 4.5], ['--warn', '--panel', 4.5],
  ['--on-accent', '--accent', 4.5], ['--on-accent', '--accent-hover', 4.5],
  ...['--bg', '--panel', '--user-bg', '--code', '--hover'].map(bg => ['--line-strong', bg, 3]),
  ...['--bg', '--panel', '--user-bg', '--code'].map(bg => ['--accent', bg, 3]),
  ['--error', '--panel', 3], ['--warn', '--panel', 3],
];
for (const [theme, values] of [['light', light], ['dark', dark]]) {
  for (const name of colours) assert.match(values[name] || '', /^#[0-9a-f]{3}([0-9a-f]{3})?$/i, `${theme} ${name}`);
  for (const [fg, bg, min] of pairs) {
    const r = ratio(values[fg], values[bg]);
    assert(r >= min, `${theme}: ${fg} on ${bg} is ${r.toFixed(2)}:1, needs ${min}:1`);
  }
}

// Spacing, padding, margins and gaps only use the scale (or 0 / auto / px borders).
for (const m of design.matchAll(/(?:^|[\s;{])(margin|padding|gap)(?:-[a-z]+)?:\s*([^;}]+)/g)) {
  for (const part of m[2].replace(/!important/, '').trim().split(/\s+/))
    assert.match(part, /^(0|auto|-1px|var\(--space-[1-6]\))$/, `${m[1]} value ${part}`);
}
// Font sizes only from the scale; never smaller than --font-size-s.
for (const m of design.matchAll(/font-size:\s*([^;}]+)/g)) assert.match(m[1].trim(), /^var\(--font-size-(s|m|l|xl)\)$/, `font-size ${m[1]}`);

// Single breakpoint; colour-scheme queries are themes, not breakpoints.
const media = [...new Set([...css.matchAll(/@media\s*([^{]+)/g)].map(m => m[1].replace(/\s+/g, '')))];
assert.deepEqual(media.filter(q => !q.includes('prefers-color-scheme')).sort(), ['(max-width:760px)']);

// Focus ring, target sizes, disabled state.
assert.match(design, /:focus-visible \{ outline: 3px solid var\(--accent\); outline-offset: 2px; \}/);
assert.doesNotMatch(design, /outline:\s*(none|0)/, 'no rule may remove the focus outline');
assert.doesNotMatch(design.replace(/:focus-visible \{[^}]*\}/, ''), /(^|[\s;{])outline:/, 'only :focus-visible sets an outline');
assert.match(design, /--control-height: 40px/);
assert.match(design, /@media \(max-width: 760px\) \{\s*:root \{ --control-height: 44px; \}/);
assert.match(design, /button:disabled \{ opacity: \.5; cursor: not-allowed; \}/);
for (const selector of ['summary', 'input[type=file]::file-selector-button', 'label:has(> input[type=checkbox])'])
  assert.match(block(selector), /min-height: var\(--control-height\)/, `${selector} target height`);

// Component classes from plan section 5 (and their ID fallbacks) are styled.
for (const selector of ['.button-primary', '#send', '.button-danger', '#delete-chat', '.button-quiet', '.alert', '[data-kind=info]', '#notice', '#storage-warning',
  '.badge', '.badge-ok', '.badge-warn', '.badge-muted', 'dl.meta', '.hint', '.status-line', '#run-status', '#page-status', '.panel', '.card',
  '.message.user', '.message.assistant', '.message-meta', '.approval', 'details.raw-json', '.conversation-item', '#conversation-list button', '.visually-hidden'])
  assert(design.includes(selector), `component selector ${selector}`);
// Approval buttons stay equal secondary buttons: no approval-specific button styling.
assert.doesNotMatch(design, /\.approval[^{]*button/, 'approval buttons must not be styled differently');

// No placement in the design part beyond component internals; layout.css owns arrangement.
for (const selector of ['#chat-page', '.chat-main', '.composer-options', '.composer-actions', '.card-grid', '.page-header', '.app-header {'])
  assert(!design.includes(selector), `layout selector ${selector} belongs to layout.css`);

// The legacy layout block was removed after both stylesheets were merged (review t_bf3172bc):
// no bare element layout rules may come back and fight layout.css.
assert(!css.includes('legacy layout'), 'legacy layout block must stay removed');
for (const pattern of [/(^|\n)header\s*[,{]/, /(^|\n)nav\s*[,{]/, /(^|\n)main\s*[,{]/, /(^|\n)aside\s*[,{]/, /(^|\n)\s*#messages\s*\{/, /(^|\n)\s*#attachments\s*\{/, /\.toolbar\s*\{/])
  assert.doesNotMatch(css, pattern, `layout rule ${pattern} belongs to layout.css`);
// Kept from the legacy block on purpose: the mobile conversation strip divider.
assert.match(css, /@media \(max-width: 760px\) \{[\s\S]*\.conversations \{ border-bottom: 1px solid var\(--line\); \}/);
// Regression (t_bf3172bc): nowrap badges overflowed card titles and the 320px viewport.
assert.doesNotMatch(block('.badge'), /white-space:\s*nowrap/, 'badges must be able to wrap');
assert.match(block('.badge'), /max-width: 100%/);

console.log('style checks ok');
