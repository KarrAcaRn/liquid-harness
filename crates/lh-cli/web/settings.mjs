// Browser-local options (docs/ARCHITECTURE.md "Options"). Pure and storage-injected:
// tests pass a fake Storage, app.mjs passes localStorage (or null if inaccessible).

// localStorage key of the default context for new chats. The value is the raw text
// as saved in Options; '' (saved deliberately empty) disables the extra context.
export const DEFAULT_CONTEXT_KEY = 'lh.defaultContext';
// Written once when no value is stored yet (fresh install or older version).
export const SUGGESTED_DEFAULT_CONTEXT = 'Erfinde keine Zahlen und Fakten. Wenn du dir unsicher bist, führe eine Websuche durch. Wenn du nicht weißt, was in einer Datei steht, fordere das Lesen der Datei an.';
export const MAX_DEFAULT_CONTEXT = 8000;

// Settings over `storage` (a Web Storage object or null). Storage failures never
// throw: an unsaved value then lives in memory for this tab (`persisted: false`).
export function createSettings(storage) {
  let unsaved = null; // Value whose write failed; wins until a write succeeds.
  let lastRead = null; // Last stored value seen; used when reading fails.
  const write = value => {
    try {
      if (!storage) throw new Error('storage unavailable');
      storage.setItem(DEFAULT_CONTEXT_KEY, value);
      unsaved = null; lastRead = value; return true;
    } catch { unsaved = value; return false; }
  };
  const read = () => {
    if (unsaved !== null) return { value: unsaved, stored: true, persisted: false };
    try {
      if (!storage) throw new Error('storage unavailable');
      const value = storage.getItem(DEFAULT_CONTEXT_KEY);
      if (value !== null) lastRead = value;
      return { value: value ?? SUGGESTED_DEFAULT_CONTEXT, stored: value !== null, persisted: true };
    } catch { return { value: lastRead ?? SUGGESTED_DEFAULT_CONTEXT, stored: lastRead !== null, persisted: false }; }
  };
  return {
    // Boot: stores the suggested text only if no value exists. An existing value,
    // including '', is never overwritten. created tells whether it was written now.
    init() {
      const current = read();
      if (current.stored) return { value: current.value, created: false, persisted: current.persisted };
      return { value: SUGGESTED_DEFAULT_CONTEXT, created: true, persisted: write(SUGGESTED_DEFAULT_CONTEXT) };
    },
    // Raw saved text for the Options textarea (re-read each call, so saves from
    // other tabs are seen).
    defaultContext() { return read().value; },
    // Saves exactly `text`; '' disables the default context. Throws only on invalid
    // input (not a string, too long), leaving the stored value unchanged.
    saveDefaultContext(text) {
      if (typeof text !== 'string') throw new TypeError('The default context must be text.');
      if (text.length > MAX_DEFAULT_CONTEXT) throw new Error(`The default context is limited to ${MAX_DEFAULT_CONTEXT} characters.`);
      return { value: text, persisted: write(text) };
    },
    // Chat-creation access: the text to snapshot into a conversation created now,
    // trimmed; '' (empty or whitespace-only setting) means add nothing.
    defaultContextForNewChat() { return read().value.trim(); },
  };
}
