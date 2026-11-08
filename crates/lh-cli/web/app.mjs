import { renderMarkdown } from './markdown.mjs';
import { startStats, completeStats, endStats, restoreStats, latestStats, formatStats } from './stats.mjs';
import { createSettings, DEFAULT_CONTEXT_KEY, SUGGESTED_DEFAULT_CONTEXT } from './settings.mjs';

export const MAX_BODY = 6 * 1024 * 1024;
export const MAX_ATTACHMENTS = 4 * 1024 * 1024;
export function requestBody(value) {
  const body = JSON.stringify(value);
  if (new TextEncoder().encode(body).length > MAX_BODY) throw new Error('Conversation exceeds the 6 MiB request limit. Export it and start a new conversation or remove attachments.');
  return body;
}
// Ctrl+Enter in the focused message field submits the composer through the normal
// form path (constraint validation, submit handler, busy guard). Plain Enter keeps
// inserting a newline. Auto-repeat and IME composition never submit, so one press
// sends at most one message.
export function composerShortcut(event, form) {
  if (event.key !== 'Enter' || !event.ctrlKey || event.altKey || event.metaKey || event.shiftKey || event.isComposing) return false;
  event.preventDefault();
  if (!event.repeat) form.requestSubmit();
  return true;
}
// Spoken replies (docs/ARCHITECTURE.md "Spoken replies"). The setting is one optional
// speak ID (a `task = "speak"` specialist), never a hard-coded list: `/v1/models`
// reports `speak_ids` per model. null means no automatic speech.
export function speakIdsFor(catalog, model) {
  const entry = Array.isArray(catalog) ? catalog.find(m => m?.id === model) : undefined;
  const ids = Array.isArray(entry?.speak_ids) ? entry.speak_ids : [];
  return [...new Set(ids.filter(id => typeof id === 'string' && id))];
}
// Keeps `selection` only when it is exactly one of `available`. Legacy boolean `true`
// (the former "Speak replies" checkbox) maps to the single offered ID if unambiguous.
// Everything else (false, unknown/stale ID, several candidates, junk) means none.
export function resolveSpeakSelection(selection, available) {
  if (typeof selection === 'string') return available.includes(selection) ? selection : null;
  if (selection === true && available.length === 1) return available[0];
  return null;
}
// Default context (docs/ARCHITECTURE.md "Default context in conversations"). Every new
// conversation is created here: `contextText` is read once, at creation, from
// settings.defaultContextForNewChat() and snapshotted as `defaultContext` ('' = none).
// Later option changes never touch an existing conversation.
export function newConversation(id, contextText) {
  return { id, title: 'New conversation', messages: [], history: [], defaultContext: typeof contextText === 'string' ? contextText.trim() : '' };
}
// The snapshot of a conversation; conversations from before this feature (no field)
// or with an invalid value have none.
export function conversationContext(conversation) {
  const text = conversation?.defaultContext;
  return typeof text === 'string' ? text.trim() : '';
}
const isContextMessage = (message, context) => message?.role === 'system' && message.content === context;
// Messages for a model request: the snapshot as one leading system message, then the
// conversation history unchanged (other system/context messages are kept). The context
// is never stored in history, and a history that already starts with it is not
// prefixed again, so it is sent exactly once per request.
export function requestMessages(context, history) {
  if (!context || isContextMessage(history[0], context)) return [...history];
  return [{ role: 'system', content: context }, ...history];
}
// Inverse for histories returned by the server (agent `lh_messages` echoes the request):
// drops the leading context message so it is not stored and later sent twice.
export function historyWithoutContext(context, messages) {
  return context && isContextMessage(messages[0], context) ? messages.slice(1) : [...messages];
}
// The "Speak replies" dropdown: "<none>" (empty value, never a valid ID) first, then the
// offered IDs once each. `selected` is the option value that mirrors `choice`.
export const SPEAK_NONE = '<none>';
export function speakOptions(available, choice) {
  const ids = [...new Set((Array.isArray(available) ? available : []).filter(id => typeof id === 'string' && id))];
  const value = typeof choice === 'string' && ids.includes(choice) ? choice : '';
  return { options: [{ value: '', label: SPEAK_NONE }, ...ids.map(id => ({ value: id, label: id }))], selected: value };
}
// Option value → setting: '' ("<none>") and anything that is not a nonempty string → null.
export function speakChoiceFromValue(value) { return typeof value === 'string' && value ? value : null; }
// Handles UTF-8 split across chunks, CR/LF/CRLF, comments and multiline data.
export async function readSSE(body, onEvent) {
  if (!body) throw new Error('The server returned no stream.');
  const reader = body.getReader(), decoder = new TextDecoder();
  let buffer = '', data = [], event = 'message';
  const line = async text => {
    if (!text) {
      if (data.length) await onEvent({ event, data: data.join('\n') });
      data = []; event = 'message'; return;
    }
    if (text.startsWith(':')) return;
    const colon = text.indexOf(':');
    const field = colon < 0 ? text : text.slice(0, colon);
    let value = colon < 0 ? '' : text.slice(colon + 1);
    if (value.startsWith(' ')) value = value.slice(1);
    if (field === 'data') data.push(value);
    if (field === 'event') event = value;
  };
  const drain = async final => {
    while (true) {
      const index = buffer.search(/[\r\n]/);
      if (index < 0 || (!final && index === buffer.length - 1 && buffer[index] === '\r')) break;
      const size = buffer[index] === '\r' && buffer[index + 1] === '\n' ? 2 : 1;
      const text = buffer.slice(0,index); buffer = buffer.slice(index + size); await line(text);
    }
    if (buffer.length > 8 * 1024 * 1024 || data.reduce((n,s) => n+s.length,0) > 8 * 1024 * 1024) throw new Error('Stream event exceeds the safety limit.');
  };
  try {
    while (true) {
      const { value, done } = await reader.read();
      buffer += done ? decoder.decode() : decoder.decode(value, { stream: true });
      await drain(done);
      if (done) { if (buffer) await line(buffer); await line(''); break; }
    }
  } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
}

function boot() {
  const $ = id => document.getElementById(id);
  const el = (tag, text, cls) => { const node = document.createElement(tag); if (text !== undefined) node.textContent = text; if (cls) node.className = cls; return node; };
  const button = (text, action) => { const b = el('button', text); b.type = 'button'; b.addEventListener('click', () => Promise.resolve().then(action).catch(report)); return b; };
  // kind 'error' (default) for failures, 'info' for confirmations; style.css colours by data-kind.
  const notice = (text, kind = 'error') => { $('notice').textContent = text; $('notice').dataset.kind = kind; $('notice').hidden = !text; };
  const report = error => notice(error.message || String(error));
  let storageBlocked = false, token = '', active = null, attachments = [], attachmentBusy = false, recorder = null, recordingTimer = null, recordingPending = false, speechURL = null;
  let conversations = [], selected, pageVersion = 0, savedSnapshot = null;
  // Spoken replies: speakChoice is the optional speak ID (null = no automatic speech),
  // in memory per tab. Every page load starts with "<none>" (the #speak select has
  // autocomplete=off, so the browser does not restore an option that is not built yet).
  let speakCatalog = null, speakAvailable = [], speakChoice = null;
  const storageWarning = text => { $('storage-warning').hidden = false; $('storage-warning').textContent = text; };
  try { token = sessionStorage.getItem('lh.token') || ''; } catch { storageWarning('Session storage unavailable: the token will only remain in memory.'); }
  $('token').value = token;
  // Options (docs/ARCHITECTURE.md "Options"). Chat creation reads the snapshot text
  // via settings.defaultContextForNewChat(); '' means no default context.
  let browserStorage = null; try { browserStorage = localStorage; } catch { /* createSettings keeps options in memory. */ }
  const settings = createSettings(browserStorage);
  if (!settings.init().persisted) storageWarning('Options cannot be saved in this browser: changes apply in this tab only.');
  let optionsLoaded = null; // Text last loaded into or saved from #default-context.
  const optionsDirty = () => optionsLoaded !== null && $('default-context').value !== optionsLoaded;
  // Refills the form from storage unless it holds unsaved edits.
  function loadOptions() {
    if (optionsDirty()) return;
    optionsLoaded = settings.defaultContext(); $('default-context').value = optionsLoaded; $('options-status').textContent = '';
  }
  try {
    const raw = localStorage.getItem('lh.conversations');
    savedSnapshot = raw;
    if (raw) {
      const saved = JSON.parse(raw);
      if (!Array.isArray(saved) || !saved.every(c => typeof c.id === 'string' && typeof c.title === 'string' && Array.isArray(c.messages))) throw new Error('Invalid saved conversations');
      conversations = saved;
      for (const conversation of conversations) for (const message of conversation.messages) {
        if (message.role === 'assistant' && 'stats' in message) message.stats = restoreStats(message.stats);
        if (message.pending) { message.error = 'Interrupted by a page reload. Partial response is not included in follow-up context.'; delete message.pending; }
      }
    }
  } catch (error) { storageBlocked = true; storageWarning(`Cannot read saved conversations (${error.message}). Existing storage will not be overwritten. New changes stay in memory: export before closing this tab.`); }
  function persist() {
    if (storageBlocked) return;
    try {
      if (localStorage.getItem('lh.conversations') !== savedSnapshot) {
        storageBlocked = true;
        storageWarning('Conversations changed in another tab. Saving is paused to avoid overwriting them. Export this tab before reloading to load the other version.');
        return;
      }
      const snapshot = JSON.stringify(conversations);
      localStorage.setItem('lh.conversations', snapshot); savedSnapshot = snapshot;
    }
    catch (error) { storageWarning(`Conversations could not be saved (${error.message}). Your conversation remains in this tab. Export before closing; no messages were discarded.`); }
  }
  const current = () => conversations.find(c => c.id === selected);
  function canChangeConversation() {
    if (active || attachmentBusy || recorder || recordingPending) { report(new Error('Finish or cancel the response, attachment loading or recording before changing conversations.')); return false; }
    attachments = []; renderAttachments(); return true;
  }
  function fresh() {
    if (!canChangeConversation()) return;
    const id = [...crypto.getRandomValues(new Uint8Array(16))].map(b => b.toString(16).padStart(2,'0')).join('');
    // The only creation path (boot, New chat, deleting the last chat): snapshot the option now.
    const c = newConversation(id, settings.defaultContextForNewChat()); conversations.unshift(c); selected = c.id; persist(); render();
  }
  function jsonDetails(title, value, cls = 'raw-json') { const d = el('details', undefined, cls || undefined); d.append(el('summary', title), el('pre', JSON.stringify(value,null,2))); return d; }
  // Key/value metadata: one dt/dd pair per field; empty values read "none", never a dangling label.
  // A third element `true` marks a long value (JSON, paths) that gets the full row width.
  const list = values => (values || []).join(', ') || 'none';
  const code = text => el('code', String(text));
  function meta(pairs) {
    const dl = el('dl', undefined, 'meta');
    for (const [key, value, wide] of pairs) { const dt = el('dt', key, wide ? 'wide' : undefined), dd = el('dd', undefined, wide ? 'wide' : undefined); if (value instanceof Node) dd.append(value); else dd.textContent = value === undefined || value === null || value === '' ? 'none' : String(value); dl.append(dt, dd); }
    return dl;
  }
  const badge = (text, kind) => el('span', text, `badge badge-${kind}`);
  function cardHeader(title, badges = []) { const header = el('div', undefined, 'card-header'); header.append(el('h4', title)); if (badges.length) { const group = el('span', undefined, 'badges'); group.append(...badges); header.append(group); } return header; }
  function markdown(node, text) {
    node.innerHTML = renderMarkdown(text); // Only escaped, locally generated renderer output.
    for (const pre of node.querySelectorAll('pre')) { const copy = button('Copy code', async () => { await navigator.clipboard.writeText(pre.querySelector('code').textContent); notice('Code copied.', 'info'); }); copy.className = 'button-quiet'; pre.append(copy); }
  }
  function media(parent, part) {
    if (part.type === 'text') { const p = el('div'); markdown(p, part.text || ''); parent.append(p); }
    else if (part.type === 'image_url' && /^data:image\/(png|jpeg|gif|webp);base64,/i.test(part.image_url?.url || '')) { const img = el('img'); img.src = part.image_url.url; img.alt = 'Attached image'; parent.append(img); }
    else if (part.type === 'input_audio' && /^(wav|mp3|flac|ogg|m4a|webm|mp4)$/.test(part.input_audio?.format)) { const audio = el('audio'); audio.controls = true; audio.src = `data:audio/${part.input_audio.format};base64,${part.input_audio.data}`; audio.setAttribute('aria-label','Attached audio'); parent.append(audio); }
  }
  function renderMessage(message) {
    const article = el('article', undefined, `message ${message.role}`); article.append(el('h3', message.role === 'user' ? 'You' : 'Assistant'));
    const body = el('div', undefined, 'markdown');
    if (Array.isArray(message.content)) message.content.forEach(p => media(body,p)); else markdown(body, message.content || '');
    article.append(body);
    if (/^data:audio\/wav;base64,/i.test(message.audio?.audio || '')) { const player = el('audio'); player.controls = true; player.src = message.audio.audio; player.setAttribute('aria-label','Generated speech'); article.append(player); }
    // Error, route and diagnostics are grouped in one footer after the content.
    const footer = el('footer', undefined, 'message-meta');
    if (message.error) footer.append(el('p', message.error, 'error'));
    if (message.route) { const r = message.route; footer.append(el('p', `Route: ${r.specialist || 'unknown'} · backend ${r.backend || 'not reported'} · confidence ${r.confidence ?? 'unknown'} · ${r.reason || ''}`, 'hint muted'), jsonDetails('Full route details',r)); }
    if (message.events?.length) footer.append(jsonDetails(`Agent steps and tool calls/results (${message.steps ?? 'in progress'})`,message.events));
    if (message.response) footer.append(jsonDetails('Response metadata',message.response));
    if (footer.childElementCount) article.append(footer);
    return article;
  }
  function renderMessages() {
    const box = $('messages');
    const follow = box.scrollHeight - box.scrollTop - box.clientHeight < 100;
    const opened = [...box.querySelectorAll('details')].map(d => d.open);
    const shown = current()?.messages || [], context = conversationContext(current());
    const items = shown.length ? shown.map(renderMessage) : [el('p', 'No messages yet.', 'hint muted empty-state')];
    // The snapshot is sent as a system message, not shown as a chat turn: a quiet disclosure only.
    if (context) { const d = el('details', undefined, 'raw-json conversation-context'); d.append(el('summary', 'Default context of this conversation'), el('pre', context)); items.unshift(d); }
    box.replaceChildren(...items);
    [...box.querySelectorAll('details')].forEach((d,i) => { d.open = Boolean(opened[i]); });
    if (follow) box.scrollTop = box.scrollHeight;
    renderStats(shown);
  }
  // Statistics line under the chat: the shown conversation's latest answer only
  // (docs/ARCHITECTURE.md "Generation statistics"); hidden until one has stats.
  function renderStats(messages) {
    const line = $('chat-stats'), view = formatStats(latestStats(messages), navigator.languages?.[0] || navigator.language);
    line.hidden = !view; line.textContent = view?.text || ''; line.title = view?.title || ''; line.dataset.phase = view?.phase || '';
  }
  function render() {
    $('conversation-title').textContent = $('conversation-title').title = current()?.title || 'New conversation';
    $('conversation-list').replaceChildren(...conversations.map(c => { const b = button(c.title, () => { if (!canChangeConversation()) return; selected = c.id; render(); }); b.className = c.id === selected ? 'conversation-item selected' : 'conversation-item'; b.setAttribute('aria-current',String(c.id === selected)); return b; }));
    renderMessages();
  }
  async function api(path, { value, signal } = {}) {
    const headers = {};
    if (token) headers.Authorization = `Bearer ${token}`;
    if (value !== undefined) headers['Content-Type'] = 'application/json';
    const response = await fetch(path, { method: value === undefined ? 'GET' : 'POST', headers, body: value === undefined ? undefined : requestBody(value), signal, credentials: 'same-origin' });
    if (!response.ok) {
      const raw = await response.text(); let message = raw;
      try { const error = JSON.parse(raw); message = error.error?.message || error.message || raw; } catch { /* Keep text errors. */ }
      throw new Error(`HTTP ${response.status}${response.status === 409 ? ' — server busy or approval expired; retry after the active run finishes' : ''}: ${message}`);
    }
    return response;
  }
  async function models() {
    const data = await (await api('/v1/models')).json(), previous = $('model').value;
    $('model').replaceChildren(...(data.data || []).map(m => { const o = el('option', m.id === 'auto' ? 'Auto route' : m.id); o.value = m.id; return o; }));
    if ([...$('model').options].some(o => o.value === previous)) $('model').value = previous;
    speakCatalog = data.data || []; syncSpeak();
  }
  // Recomputes the speak IDs of the selected model and drops a choice it does not offer.
  function syncSpeak() {
    if (speakCatalog === null) return;
    speakAvailable = speakIdsFor(speakCatalog, $('model').value || 'auto');
    speakChoice = resolveSpeakSelection(speakChoice, speakAvailable);
    renderSpeakControl();
  }
  // value: a speak ID or null for none (resolveSpeakSelection also accepts a legacy boolean).
  function chooseSpeak(value) { speakChoice = value; syncSpeak(); }
  // The dropdown only mirrors speakAvailable/speakChoice; it is not the source of truth.
  // Options are rebuilt from scratch, so a model switch leaves no stale or duplicate entries.
  function renderSpeakControl() {
    const { options, selected } = speakOptions(speakAvailable, speakChoice);
    $('speak').replaceChildren(...options.map(o => { const option = el('option', o.label); option.value = o.value; return option; }));
    $('speak').value = selected;
  }
  function renderAttachments() {
    $('attachments').replaceChildren(...attachments.map((a,index) => { const box = el('div', undefined,'attachment'); box.append(el('span',a.name)); media(box,a.part); box.append(button(`Remove ${a.name}`, () => { attachments.splice(index,1); renderAttachments(); })); return box; }));
    $('prompt').required = !attachments.length;
  }
  async function attach(files) {
    if (attachmentBusy) throw new Error('Please wait for the previous attachments to finish loading.');
    attachmentBusy = true;
    try {
      if (attachments.length + files.length > 8) throw new Error('At most eight attachments are allowed per turn.');
      if (attachments.reduce((n,a) => n+a.size,0) + files.reduce((n,f) => n+f.size,0) > MAX_ATTACHMENTS) throw new Error('Attachments exceed the 4 MiB total limit.');
      const additions = [];
      for (const file of files) {
        if (!file.size) throw new Error(`Empty attachment: ${file.name}`);
        const image = /^image\/(png|jpeg|gif|webp)$/.test(file.type);
        let format = file.name.split('.').pop().toLowerCase();
        if (!image && !/^(wav|mp3|flac|ogg|m4a|webm|mp4)$/.test(format)) throw new Error(`Unsupported attachment: ${file.name}. Use PNG, JPEG, GIF, WebP or supported audio.`);
        const encoded = await new Promise((resolve,reject) => { const reader = new FileReader(); reader.onload = () => resolve(String(reader.result)); reader.onerror = () => reject(new Error('Could not read attachment.')); reader.readAsDataURL(file); });
        additions.push({ name:file.name, size:file.size, part:image ? {type:'image_url',image_url:{url:encoded}} : {type:'input_audio',input_audio:{format,data:encoded.split(',')[1]}} });
      }
      attachments.push(...additions); renderAttachments();
    } finally { attachmentBusy = false; }
  }
  function busy(value) {
    for (const id of ['send','new-chat','rename-chat','delete-chat','model','agent','files','record','prompt']) $(id).disabled = value;
    $('cancel').disabled = !value;
  }
  async function speak(text, id) {
    const response = await api('/v1/audio/speech',{value:{model:id,input:text,response_format:'wav'}});
    if (speechURL) URL.revokeObjectURL(speechURL);
    speechURL = URL.createObjectURL(await response.blob());
    $('speech-player').src = speechURL; $('speech-player').hidden = false;
    try { await $('speech-player').play(); } catch { notice('Speech is ready. Press play in the audio player.', 'info'); }
  }
  function approval(run, data) {
    const card = el('section',undefined,'approval'), actions = el('div',undefined,'toolbar approval-actions');
    // The tool arguments are content to review, not a quiet diagnostic disclosure.
    card.append(el('h3',`Approval required: ${data.name}`),el('p','Review the complete arguments before allowing this tool. Shell commands are not sandboxed.'),jsonDetails('Tool arguments',data,''),el('p','Agent approval “Always” applies only to identical calls in this run.','hint muted'),actions);
    // Allow, Deny and Always stay equal secondary buttons: no visual nudge towards allowing tools.
    for (const [label,allow,always] of [['Allow',true,false],['Deny',false,false],['Always',true,true]]) actions.append(button(label, async () => {
      if (active !== run || !run.id) throw new Error('This run is no longer active.');
      const controls = [...card.querySelectorAll('button')]; controls.forEach(b => b.disabled = true);
      try { await api(`/lh/v1/agent/runs/${encodeURIComponent(run.id)}/approve`,{value:{call_id:data.call_id,allow,always}}); card.replaceChildren(el('p',`${data.name}: ${label} submitted. Waiting for the run result.`)); }
      catch (error) { controls.forEach(b => b.disabled = false); throw error; }
    }));
    $('approvals').append(card);
    // The card may sit below the fold on small screens: bring it into view (no-op when visible).
    card.scrollIntoView({block:'nearest'});
  }
  async function send(event) {
    event.preventDefault(); if (active) return;
    notice('');
    if (attachmentBusy || recorder || recordingPending) { report(new Error('Finish loading or recording attachments before sending.')); return; }
    const text = $('prompt').value.trim(); if (!text && !attachments.length) return;
    const c = current(), agent = $('agent').checked;
    const user = {role:'user', content: attachments.length ? [...(text ? [{type:'text',text}] : []),...attachments.map(a => a.part)] : text};
    const history = [...(c.history || c.messages.filter(m => !m.error).map(({role,content}) => ({role,content}))), user];
    const context = conversationContext(c);
    const payload = {model:$('model').value || 'auto',messages:requestMessages(context,history)}; if (!agent) payload.stream = true;
    try { requestBody(payload); } catch (error) { report(error); return; }
    const answer = {role:'assistant',content:'',events:[],pending:true,stats:startStats()}; c.messages.push(user,answer); c.history = history;
    if (c.title === 'New conversation') c.title = text.slice(0,60) || 'Media conversation';
    const run = {controller:new AbortController(),id:null,agent,cancelled:false}; active = run;
    $('prompt').value = ''; attachments = []; renderAttachments(); busy(true); render(); persist(); $('run-status').textContent = 'Connecting…';
    let done = false, responseMessage = null, savedHistory = null, streamError = null, finalStats = null;
    try {
      const response = await api(agent ? '/lh/v1/agent/runs' : '/v1/chat/completions',{value:payload,signal:run.controller.signal});
      run.id = response.headers.get('x-lh-run-id');
      if (!response.headers.get('content-type')?.includes('text/event-stream')) throw new Error('Expected a server-sent event stream.');
      $('run-status').textContent = agent ? 'Agent running' : 'Receiving response…';
      await readSSE(response.body, frame => {
        if (frame.data === '[DONE]') { done = true; return; }
        let value; try { value = JSON.parse(frame.data); } catch { throw new Error('Invalid JSON in response stream.'); }
        if (agent) {
          run.id = value.run_id || run.id;
          const kind = value.event || frame.event, data = value.data || {};
          if (kind === 'route') answer.route = data;
          else if (kind === 'delta') { answer.content += data.content || ''; answer.steps = data.step; }
          else if (kind === 'approval_required') { answer.events.push(value); approval(run,data); }
          else if (kind === 'error') { streamError = data.message || JSON.stringify(data); answer.events.push(value); }
          else if (kind === 'done') {
            done = true; answer.steps = data.steps ?? data.response?.steps; answer.response = data; finalStats = data.response?.lh_stats ?? null;
            if (data.error) streamError = typeof data.error === 'string' ? data.error : JSON.stringify(data.error);
            if (data.response?.message) { responseMessage = data.response.message; if (typeof responseMessage.content === 'string') answer.content = responseMessage.content; savedHistory = data.response.lh_messages; }
          } else answer.events.push(value);
          $('run-status').textContent = `Agent ${kind} · step ${answer.steps ?? '—'}`;
        } else {
          if (value.error) streamError = value.error.message || JSON.stringify(value.error);
          if (value.lh) { const { stats, ...route } = value.lh; answer.route = route; if (stats !== undefined) finalStats = stats; }
          for (const choice of value.choices || []) {
            const delta = choice.delta || {};
            if (typeof delta.content === 'string') answer.content += delta.content;
            if (delta.audio) answer.audio = delta.audio;
            if (delta.tool_calls) answer.events.push({tool_calls:delta.tool_calls});
          }
        }
        renderMessages();
      });
      if (streamError) throw new Error(streamError);
      if (!done) throw new Error('Connection ended before completion. Partial response retained; it is not included in follow-up context.');
      c.history = [...(Array.isArray(savedHistory) ? historyWithoutContext(context,savedHistory) : history), responseMessage || {role:'assistant',content:answer.content}];
      answer.stats = completeStats(answer.stats, finalStats);
      $('run-status').textContent = `Complete${answer.steps !== undefined ? ` · ${answer.steps} steps` : ''}`;
    } catch (error) {
      answer.error = run.cancelled ? 'Cancelled. Partial response retained; not included in follow-up context.' : error.message;
      answer.stats = endStats(answer.stats, run.cancelled ? 'cancelled' : 'error');
      // Keep the submitted user message, but never treat a partial answer as complete.
      c.history = history; report(new Error(answer.error)); $('run-status').textContent = run.cancelled ? 'Cancelled' : 'Request failed';
    } finally {
      delete answer.pending; active = null; busy(false); $('approvals').replaceChildren(); renderMessages(); persist();
    }
    const speakId = speakChoice; // The setting at completion time.
    if (!answer.error && typeof speakId === 'string' && answer.content.trim()) await speak(answer.content, speakId);
  }
  async function management() {
    const page = ['plugins','tools','skills','status','options'].includes(location.hash.slice(1)) ? location.hash.slice(1) : 'chat';
    document.querySelectorAll('nav a').forEach(a => { if (a.hash === `#${page}`) a.setAttribute('aria-current','page'); else a.removeAttribute('aria-current'); });
    $('chat-page').hidden = page !== 'chat'; $('options-page').hidden = page !== 'options'; $('management-page').hidden = page === 'chat' || page === 'options';
    document.body.dataset.page = page; // layout.css: the chat view fits the viewport, other pages scroll.
    const version = ++pageVersion; if (page === 'chat') return;
    if (page === 'options') { loadOptions(); return; } // Browser-local: no server reads.
    $('page-title').textContent = page[0].toUpperCase()+page.slice(1); $('management-content').replaceChildren(); $('page-status').textContent = 'Loading…';
    const paths = page === 'plugins' ? ['plugins','specialists','router'] : page === 'status' ? ['status','router'] : [page];
    // Parallel management reads contend for the nonblocking engine lock.
    const results = [];
    for (const path of paths) {
      try { results.push({path,data:await (await api(`/lh/v1/${path}`)).json()}); }
      catch (error) { results.push({path,error}); }
      if (version !== pageVersion) return;
    }
    if (version !== pageVersion) return;
    $('page-status').textContent = results.some(r => r.error) ? 'Some data could not be loaded. Refresh to retry.' : 'Current server snapshot (not automatically refreshed).';
    const processBadge = state => badge(`Process ${state || 'unknown'}`, state === 'running' ? 'ok' : state === 'failed' ? 'warn' : 'muted');
    const block = (title, value) => { const box = el('div', undefined, 'meta-block'); box.append(el('h4', title), el('pre', JSON.stringify(value, null, 2))); return box; };
    for (const {path,data,error} of results) {
      const name = path[0].toUpperCase()+path.slice(1), group = el('section',undefined,'panel'), heading = el('h3',name); group.append(heading);
      if (error) { group.append(el('p',error.message,'error')); $('management-content').append(group); continue; }
      const entries = data[path];
      if (Array.isArray(entries)) {
        heading.textContent = `${name} (${entries.length})`;
        if (!entries.length) group.append(el('p','No entries available.'));
        const grid = el('div',undefined,'card-grid');
        for (const item of entries) {
          const card = el('article',undefined,'card'), badges = [];
          if (path === 'plugins') badges.push(badge(item.enabled ? 'Enabled' : 'Disabled', item.enabled ? 'ok' : 'muted'), processBadge(item.process?.state));
          if (path === 'tools') badges.push(item.destructive ? badge('Destructive / requires policy review','warn') : badge('Read-only annotation','muted'));
          card.append(cardHeader(item.id || item.name, badges));
          if (item.description || item.route) card.append(el('p',item.description || item.route));
          if (path === 'plugins') card.append(meta([['Version',item.version],['Provides',list(item.provides)],['PID',item.process?.pid ?? 'none'],['Restarts',item.process?.restarts ?? 0],['Capabilities source',item.capabilities_source],['Capabilities',code(JSON.stringify(item.capabilities)),true],['Manifest',code(item.manifest_path),true]]));
          if (path === 'specialists') card.append(meta([['Task',item.task],['Inputs',list(item.inputs)],['Outputs',list(item.outputs)],['Agent',String(Boolean(item.agent))],['Tools',list(item.tools)]]));
          if (path === 'tools') card.append(meta([['Plugin',item.plugin],['Original name',code(item.original_name)]]));
          if (path === 'skills') {
            card.append(meta([['Tools',list(item.tools)]]));
            const actions = el('div',undefined,'card-actions');
            actions.append(button(`Read ${item.name}`,async () => { const detail = await (await api(`/lh/v1/skills/${encodeURIComponent(item.name)}`)).json(); const body = el('div',undefined,'markdown'); markdown(body,detail.content || ''); const viewer = el('section'); viewer.append(body,jsonDetails('Full skill JSON',detail)); const old = card.querySelector('.skill-content'); if (old) old.remove(); viewer.className = 'skill-content'; actions.after(viewer); }));
            card.append(actions);
          }
          card.append(jsonDetails('Full entry JSON',item)); grid.append(card);
        }
        if (entries.length) group.append(grid);
      } else if (path === 'status') {
        const processes = Object.entries(data.processes || {}), grid = el('div',undefined,'card-grid');
        for (const [id,p] of processes) { const card = el('article',undefined,'card'); card.append(cardHeader(id,[processBadge(p.state)]),meta([['PID',p.pid ?? 'none'],['Restarts',p.restarts]])); grid.append(card); }
        if (processes.length) group.append(grid);
        if (data.resident_models?.length) { const box = el('div',undefined,'meta-block'); box.append(el('h4','Resident models'),meta(data.resident_models.map(r => [r.backend, r.error || code(JSON.stringify(r.loaded)), true]))); group.append(box); }
        group.append(block('Cascade statistics',data.cascade));
      } else if (path === 'router') group.append(meta([['Selected router',data.selected],['Model calls',data.stats?.model_calls ?? 0],['Recent routes',data.stats?.recent_routes?.length ?? 0]]),block('Stage counts',data.stats?.stage_counts || {}));
      if (data.builtins) group.append(jsonDetails('Built-in tool schemas',data.builtins));
      group.append(jsonDetails('Full management JSON',data)); $('management-content').append(group);
    }
  }
  $('composer').addEventListener('submit', event => send(event).catch(report));
  $('prompt').addEventListener('keydown', event => composerShortcut(event, $('composer')));
  $('cancel').addEventListener('click', async () => {
    const run = active; if (!run) return;
    try {
      if (run.agent && run.id) await api(`/lh/v1/agent/runs/${encodeURIComponent(run.id)}/cancel`,{value:{}});
      run.cancelled = true; run.controller.abort();
      notice('Cancellation requested. Backend inference may still drain; tool effects cannot be rolled back.', 'info');
    } catch (error) { report(error); }
  });
  $('new-chat').addEventListener('click',fresh);
  $('rename-chat').addEventListener('click',() => { const title = prompt('Conversation name',current().title); if (title?.trim()) { current().title = title.trim().slice(0,200); persist(); render(); } });
  $('delete-chat').addEventListener('click',() => { if (!confirm(`Delete “${current().title}”? This cannot be undone.`) || !canChangeConversation()) return; conversations = conversations.filter(c => c.id !== selected); selected = conversations[0]?.id; persist(); if (!selected) fresh(); else render(); });
  $('export').addEventListener('click',() => { const url = URL.createObjectURL(new Blob([JSON.stringify(conversations,null,2)],{type:'application/json'})); const a = el('a'); a.href = url; a.download = 'liquid-harness-conversations.json'; a.click(); setTimeout(() => URL.revokeObjectURL(url),1000); });
  $('auth-form').addEventListener('submit',event => { event.preventDefault(); token = $('token').value.trim(); try { sessionStorage.setItem('lh.token',token); notice('Token saved for this tab.', 'info'); } catch { storageWarning('Token remains in memory only: sessionStorage is unavailable.'); } models().catch(report); management().catch(report); });
  $('clear-token').addEventListener('click',() => { token = ''; $('token').value = ''; try { sessionStorage.removeItem('lh.token'); notice('Token cleared.', 'info'); } catch { storageWarning('Could not clear sessionStorage; close this tab to clear its stored token. In-memory token cleared.'); } });
  $('files').addEventListener('change',async event => { try { await attach([...event.target.files]); } catch (error) { report(error); } finally { event.target.value = ''; } });
  $('record').addEventListener('click',async () => {
    if (recorder) { recorder.stop(); return; }
    if (recordingPending) return;
    let stream; recordingPending = true;
    try {
      if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === 'undefined') throw new Error('Audio recording is unavailable. Use localhost/HTTPS and a supported browser, or upload audio.');
      stream = await navigator.mediaDevices.getUserMedia({audio:true});
      const mime = ['audio/webm','audio/ogg','audio/mp4'].find(m => MediaRecorder.isTypeSupported(m));
      if (!mime) throw new Error('No supported recording format. Upload an audio file instead.');
      recorder = new MediaRecorder(stream,{mimeType:mime}); const chunks = []; let size = 0, oversized = false;
      recorder.ondataavailable = event => { size += event.data.size; if (size > MAX_ATTACHMENTS) { oversized = true; if (recorder?.state === 'recording') recorder.stop(); } else chunks.push(event.data); };
      recorder.onerror = () => { report(new Error('Audio recording failed.')); if (recorder?.state === 'recording') recorder.stop(); };
      recorder.onstop = async () => {
        clearTimeout(recordingTimer); stream.getTracks().forEach(t => t.stop()); recorder = null; $('record').textContent = 'Record audio';
        try { if (oversized) throw new Error('Recording exceeded 4 MiB and was not attached. Record a shorter clip.'); await attach([new File(chunks,`recording.${mime.split('/')[1]}`,{type:mime})]); } catch (error) { report(error); }
      };
      recorder.start(500); $('record').textContent = 'Stop recording'; recordingTimer = setTimeout(() => { if (recorder?.state === 'recording') { notice('Recording stopped at the two-minute limit.', 'info'); recorder.stop(); } },120000);
    } catch (error) { stream?.getTracks().forEach(t => t.stop()); report(error); } finally { recordingPending = false; }
  });
  let theme; try { theme = localStorage.getItem('lh.theme'); } catch { /* System theme is the fallback. */ }
  if (theme === 'light' || theme === 'dark') document.documentElement.dataset.theme = theme;
  $('theme').addEventListener('click',() => { const dark = document.documentElement.dataset.theme ? document.documentElement.dataset.theme === 'dark' : matchMedia('(prefers-color-scheme: dark)').matches; const next = dark ? 'light' : 'dark'; document.documentElement.dataset.theme = next; try { localStorage.setItem('lh.theme',next); } catch { notice('Theme changed for this tab; browser storage unavailable.'); } });
  $('model').addEventListener('change',syncSpeak);
  $('speak').addEventListener('change',event => chooseSpeak(speakChoiceFromValue(event.target.value)));
  $('refresh-models').addEventListener('click',() => models().catch(report)); $('refresh-page').addEventListener('click',() => management().catch(report));
  window.addEventListener('hashchange',() => management().catch(report));
  // On mobile the nav scrolls internally; browsers leave a partly hidden focused link where it
  // is, so bring it (and its focus ring, via scroll-padding) fully into view.
  document.querySelector('.app-nav').addEventListener('focusin',event => event.target.scrollIntoView({block:'nearest',inline:'nearest'}));
  window.addEventListener('storage',event => {
    if (event.key === 'lh.conversations' || event.key === null) {
      storageBlocked = true;
      storageWarning('Browser conversation storage changed in another tab. Saving paused. Export this tab before reloading to avoid losing either version.');
    }
  });
  $('options-form').addEventListener('submit',event => {
    event.preventDefault();
    try {
      const saved = settings.saveDefaultContext($('default-context').value); optionsLoaded = saved.value;
      if (!saved.persisted) storageWarning('Options could not be saved in browser storage; they apply in this tab only.');
      $('options-status').textContent = `${saved.persisted ? 'Saved' : 'Saved for this tab only'}. ${saved.value.trim() ? 'New conversations start with this context.' : 'New conversations start without a default context.'}`;
    } catch (error) { report(error); }
  });
  $('default-context').addEventListener('input',() => { $('options-status').textContent = optionsDirty() ? 'Unsaved changes.' : ''; });
  $('restore-default-context').addEventListener('click',() => { $('default-context').value = SUGGESTED_DEFAULT_CONTEXT; $('options-status').textContent = optionsDirty() ? 'Unsaved changes.' : ''; $('default-context').focus(); });
  window.addEventListener('storage',event => {
    if (event.key !== DEFAULT_CONTEXT_KEY && event.key !== null) return;
    if (optionsDirty()) $('options-status').textContent = 'Changed in another tab. Saving here replaces that version.'; else loadOptions();
  });
  window.addEventListener('beforeunload',event => { if (active || recorder || optionsDirty() || !$('storage-warning').hidden) { event.preventDefault(); event.returnValue = ''; } });
  if (!conversations.length) fresh(); else { selected = conversations[0].id; render(); }
  models().catch(report); management().catch(report);
}
if (typeof document !== 'undefined') boot();
