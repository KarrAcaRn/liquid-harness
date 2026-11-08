// Per-answer generation statistics for the chat UI.
//
// The server computes the numbers (`lh.stats` on the final chat SSE chunk or
// the non-streaming response, `response.lh_stats` on the agent `done` event);
// see docs/ARCHITECTURE.md "Generation statistics". This module never derives
// token counts from text. It only validates server values and tracks the
// lifecycle of one answer:
//
//   startStats()            -> phase 'pending'   (new answer; every value null)
//   completeStats(s, raw)   -> phase 'complete'  (validated server values)
//   endStats(s, 'cancelled' | 'error' | 'interrupted')
//                           -> that phase, every value null
//
// Each value is a finite non-negative number or null (= not available). A
// stats object lives on its assistant message, so switching conversations
// shows that conversation's own latest answer (`latestStats`) and never values
// left over from another chat.

export const STAT_FIELDS = Object.freeze({
  completionTokens: 'completion_tokens', promptTokens: 'prompt_tokens',
  tokensPerSecond: 'tokens_per_second', rateWindowMs: 'rate_window_ms',
  elapsedMs: 'elapsed_ms', firstDeltaMs: 'first_delta_ms',
});
export const RATE_SOURCES = Object.freeze(['backend_timings', 'harness_stream']);
const TERMINAL = new Set(['cancelled', 'error', 'interrupted']);

const number = value => typeof value === 'number' && Number.isFinite(value) && value >= 0 ? value : null;
const integer = value => Number.isSafeInteger(value) && value >= 0 ? value : null;

function empty(phase, startedAt) {
  return { phase, startedAt: number(startedAt), completionTokens: null, promptTokens: null, tokensPerSecond: null, rateSource: null, rateWindowMs: null, elapsedMs: null, firstDeltaMs: null };
}

// Validate a server stats object; anything missing or malformed becomes null.
export function normalizeStats(raw) {
  const source = raw && typeof raw === 'object' ? raw : {};
  const stats = {};
  for (const [key, wire] of Object.entries(STAT_FIELDS)) stats[key] = key.endsWith('Tokens') ? integer(source[wire]) : number(source[wire]);
  stats.rateSource = RATE_SOURCES.includes(source.rate_source) ? source.rate_source : null;
  // A rate is only meaningful together with its source and window.
  if (stats.tokensPerSecond === null || stats.rateSource === null || !stats.rateWindowMs) { stats.tokensPerSecond = null; stats.rateSource = null; stats.rateWindowMs = null; }
  return stats;
}

// A new answer starts with no values: nothing carries over from the previous one.
export function startStats(now = Date.now()) { return empty('pending', now); }

export function completeStats(stats, raw) {
  return { ...normalizeStats(raw), phase: 'complete', startedAt: number(stats?.startedAt) };
}

// Cancel, failure or page reload: partial streams report no server statistics,
// so every value is cleared rather than kept from an earlier state.
export function endStats(stats, phase) {
  if (!TERMINAL.has(phase)) throw new Error(`Unknown terminal stats phase: ${phase}`);
  return empty(phase, stats?.startedAt);
}

// Restore persisted stats; unknown shapes or unfinished answers are not trusted.
export function restoreStats(saved) {
  if (!saved || typeof saved !== 'object') return null;
  if (saved.phase === 'pending') return endStats(saved, 'interrupted');
  if (TERMINAL.has(saved.phase)) return empty(saved.phase, saved.startedAt);
  if (saved.phase !== 'complete') return null;
  const wire = Object.fromEntries(Object.entries(STAT_FIELDS).map(([key, name]) => [name, saved[key]]));
  return completeStats(saved, { ...wire, rate_source: saved.rateSource });
}

// Stats of the conversation's latest assistant answer, or null when there is
// none (new conversation, or answers saved before statistics existed).
export function latestStats(messages) {
  const answer = [...(messages || [])].reverse().find(m => m?.role === 'assistant');
  return answer?.stats ?? null;
}
