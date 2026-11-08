import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {renderMarkdown} from '../crates/lh-cli/web/markdown.mjs';
import {readSSE,requestBody,MAX_BODY,composerShortcut,speakIdsFor,resolveSpeakSelection,newConversation,conversationContext,requestMessages,historyWithoutContext} from '../crates/lh-cli/web/app.mjs';
import {normalizeStats,startStats,completeStats,endStats,restoreStats,latestStats,formatStats} from '../crates/lh-cli/web/stats.mjs';
import {createSettings,DEFAULT_CONTEXT_KEY,SUGGESTED_DEFAULT_CONTEXT,MAX_DEFAULT_CONTEXT} from '../crates/lh-cli/web/settings.mjs';
for (const name of ['app.mjs','markdown.mjs','stats.mjs','settings.mjs']) {
  const result = spawnSync(process.execPath,['--check',fileURLToPath(new URL(`../crates/lh-cli/web/${name}`,import.meta.url))],{encoding:'utf8'});
  assert.equal(result.status,0,result.stderr);
}
for (const input of ['<script>alert(1)</script>', '<img src=x onerror=alert(1)>', '[x](javascript:alert)', '[x](data:text/html,evil)', '[x](https://user:pass@example.com)', '![x](https://example.com/a.png)']) {
  const out = renderMarkdown(input);
  assert(!/<(script|img)\b|href="(?:javascript|data):/.test(out),out);
}
assert.match(renderMarkdown('```js\n<script>\n```'),/<pre><code>&lt;script&gt;<\/code><\/pre>/);
assert.match(renderMarkdown('# Title\n- one\n**bold**'),/<h1>Title<\/h1><ul><li>one<\/li><\/ul><p><strong>bold<\/strong><\/p>/);
const text = ':comment\r\nevent: delta\r\ndata: {"content":"Grüße 😀"}\r\n\r\ndata: a\ndata: b\n\ndata: [DONE]\r\r';
const bytes = new TextEncoder().encode(text);
for (let size = 1; size <= bytes.length; size++) {
  const events = []; let offset = 0;
  const body = new ReadableStream({pull(controller) { if(offset===bytes.length) return controller.close(); controller.enqueue(bytes.slice(offset,offset+size)); offset = Math.min(offset+size,bytes.length); }});
  await readSSE(body,e=>events.push(e));
  assert.deepEqual(events,[{event:'delta',data:'{"content":"Grüße 😀"}'},{event:'message',data:'a\nb'},{event:'message',data:'[DONE]'}]);
}
assert.equal(requestBody({hello:'世界'}),'{"hello":"世界"}');
assert.throws(()=>requestBody({s:'a'.repeat(MAX_BODY)}),/6 MiB/);
// Ctrl+Enter submits through the form exactly once; other keys keep their default.
const key = init => { const e = {key:'Enter',ctrlKey:false,altKey:false,metaKey:false,shiftKey:false,isComposing:false,repeat:false,prevented:false,...init}; e.preventDefault = () => { e.prevented = true; }; return e; };
const shortcut = init => { const form = {submits:0,requestSubmit() { this.submits++; }}, event = key(init); return {handled:composerShortcut(event,form),prevented:event.prevented,submits:form.submits}; };
assert.deepEqual(shortcut({ctrlKey:true}),{handled:true,prevented:true,submits:1});
assert.deepEqual(shortcut({ctrlKey:true,repeat:true}),{handled:true,prevented:true,submits:0});
for (const init of [{},{shiftKey:true},{ctrlKey:true,shiftKey:true},{ctrlKey:true,altKey:true},{ctrlKey:true,metaKey:true},{metaKey:true},{ctrlKey:true,isComposing:true},{ctrlKey:true,key:'a'}])
  assert.deepEqual(shortcut(init),{handled:false,prevented:false,submits:0},JSON.stringify(init));
// The shortcut is bound only to the message field, never globally.
const source = readFileSync(fileURLToPath(new URL('../crates/lh-cli/web/app.mjs',import.meta.url)),'utf8');
assert.match(source,/\$\('prompt'\)\.addEventListener\('keydown', event => composerShortcut\(event, \$\('composer'\)\)\)/);
assert.doesNotMatch(source,/(document|window)\.addEventListener\('key/);
// Spoken replies: speak IDs come from /v1/models `speak_ids`; the setting is one optional ID.
const catalog = [{id:'auto',speak_ids:['mock-speak']},{id:'mock-text',speak_ids:[]},{id:'mock-transcribe',speak_ids:['mock-speak']},{id:'two',speak_ids:['a','b','a',7,'',null]},{id:'old-server'}];
assert.deepEqual(speakIdsFor(catalog,'mock-transcribe'),['mock-speak']);
assert.deepEqual(speakIdsFor(catalog,'two'),['a','b'],'deduplicated, junk dropped');
for (const model of ['mock-text','old-server','unknown']) assert.deepEqual(speakIdsFor(catalog,model),[],model);
for (const junk of [null,undefined,{},'x']) assert.deepEqual(speakIdsFor(junk,'auto'),[]);
// No selection: no speech.
assert.equal(resolveSpeakSelection(null,['mock-speak']),null);
assert.equal(resolveSpeakSelection(undefined,['mock-speak']),null);
// A valid ID is kept exactly; an invalid one falls back to none.
assert.equal(resolveSpeakSelection('mock-speak',['mock-speak']),'mock-speak');
assert.equal(resolveSpeakSelection('b',['a','b']),'b');
for (const invalid of ['lfm3-speak','','MOCK-SPEAK',' mock-speak']) assert.equal(resolveSpeakSelection(invalid,['mock-speak']),null,invalid);
// Model switch: a still-offered ID survives, a no-longer-offered one is dropped and stays dropped.
let choice = resolveSpeakSelection('mock-speak',speakIdsFor(catalog,'auto'));
choice = resolveSpeakSelection(choice,speakIdsFor(catalog,'mock-transcribe')); assert.equal(choice,'mock-speak');
choice = resolveSpeakSelection(choice,speakIdsFor(catalog,'mock-text')); assert.equal(choice,null);
choice = resolveSpeakSelection(choice,speakIdsFor(catalog,'mock-transcribe')); assert.equal(choice,null);
// Legacy boolean checkbox state: true maps only to an unambiguous single ID; otherwise none.
assert.equal(resolveSpeakSelection(true,['mock-speak']),'mock-speak');
assert.equal(resolveSpeakSelection(true,['a','b']),null);
assert.equal(resolveSpeakSelection(true,[]),null);
assert.equal(resolveSpeakSelection(false,['mock-speak']),null);
for (const junk of [0,1,{},['mock-speak']]) assert.equal(resolveSpeakSelection(junk,['mock-speak']),null);
// app.mjs wiring: speech uses exactly the chosen ID (never auto routing), only for a string choice,
// and model switches or reloaded catalogs re-validate it.
assert.match(source,/api\('\/v1\/audio\/speech',\{value:\{model:id,input:text,response_format:'wav'\}\}\)/);
assert.doesNotMatch(source,/audio\/speech',\{value:\{model:'auto'/);
assert.match(source,/typeof speakId === 'string' && answer\.content\.trim\(\)\) await speak\(answer\.content, speakId\)/);
assert.match(source,/speakCatalog = data\.data \|\| \[\]; syncSpeak\(\);/);
assert.match(source,/\$\('model'\)\.addEventListener\('change',syncSpeak\)/);
// Generation statistics: validation and per-answer lifecycle.
const wire = {completion_tokens:30,prompt_tokens:4,tokens_per_second:20,rate_source:'backend_timings',rate_window_ms:1500,elapsed_ms:1600,first_delta_ms:90};
const nulls = {completionTokens:null,promptTokens:null,tokensPerSecond:null,rateSource:null,rateWindowMs:null,elapsedMs:null,firstDeltaMs:null};
const pending = startStats(1000);
assert.deepEqual(pending,{phase:'pending',startedAt:1000,...nulls});
const complete = completeStats(pending,wire);
assert.deepEqual(complete,{phase:'complete',startedAt:1000,completionTokens:30,promptTokens:4,tokensPerSecond:20,rateSource:'backend_timings',rateWindowMs:1500,elapsedMs:1600,firstDeltaMs:90});
// Missing server stats (speech, old server) are explicitly unavailable, not zero.
for (const raw of [null,undefined,{},'x',42]) assert.deepEqual(completeStats(pending,raw),{phase:'complete',startedAt:1000,...nulls});
// Malformed values never produce a number; a rate needs a source and positive window.
assert.deepEqual(normalizeStats({completion_tokens:-1,prompt_tokens:1.5,tokens_per_second:Infinity,rate_source:'backend_timings',rate_window_ms:10,elapsed_ms:NaN,first_delta_ms:'5'}),nulls);
for (const patch of [{rate_source:'guess'},{rate_source:null},{rate_window_ms:0},{rate_window_ms:null},{tokens_per_second:-3},{tokens_per_second:null}]) {
  const stats = normalizeStats({...wire,...patch});
  assert.deepEqual([stats.tokensPerSecond,stats.rateSource,stats.rateWindowMs],[null,null,null],JSON.stringify(patch));
  assert.equal(stats.completionTokens,30);
}
assert.equal(normalizeStats({...wire,completion_tokens:0}).completionTokens,0);
assert.equal(normalizeStats({...wire,rate_source:'harness_stream'}).rateSource,'harness_stream');
// Cancel, error and reload clear every value, even after earlier completion.
for (const phase of ['cancelled','error','interrupted']) assert.deepEqual(endStats(complete,phase),{phase,startedAt:1000,...nulls});
assert.throws(()=>endStats(pending,'complete'),/Unknown terminal/);
// A new answer starts empty regardless of the previous answer.
assert.deepEqual(startStats(2000),{phase:'pending',startedAt:2000,...nulls});
// Persistence round trip; unfinished answers become interrupted, junk is dropped.
assert.deepEqual(restoreStats(JSON.parse(JSON.stringify(complete))),complete);
assert.deepEqual(restoreStats(JSON.parse(JSON.stringify(pending))),{phase:'interrupted',startedAt:1000,...nulls});
assert.deepEqual(restoreStats({...complete,phase:'cancelled'}),{phase:'cancelled',startedAt:1000,...nulls});
assert.deepEqual(restoreStats({...complete,tokensPerSecond:'fast',completionTokens:-2}),{...complete,tokensPerSecond:null,rateSource:null,rateWindowMs:null,completionTokens:null});
for (const saved of [null,'x',{phase:'weird'}]) assert.equal(restoreStats(saved),null);
// Conversation switch: each chat reports only its own latest answer.
const chatA = [{role:'user',content:'a'},{role:'assistant',content:'old',stats:endStats(pending,'error')},{role:'user',content:'b'},{role:'assistant',content:'new',stats:complete}];
assert.equal(latestStats(chatA),complete);
assert.equal(latestStats([]),null);
assert.equal(latestStats(undefined),null);
assert.equal(latestStats([{role:'user',content:'x'},{role:'assistant',content:'saved before stats'}]),null);
assert.equal(latestStats([...chatA,{role:'user',content:'c'},{role:'assistant',content:'',stats:startStats(3000)}]).phase,'pending');
// app.mjs wiring: every answer starts fresh, completes only after [DONE]/done, and errors/cancel clear.
assert.match(source,/const answer = \{role:'assistant',content:'',events:\[\],pending:true,stats:startStats\(\)\}/);
assert.match(source,/if \(!done\) throw[^\n]*\n[^\n]*\n\s*answer\.stats = completeStats\(answer\.stats, finalStats\);/);
assert.match(source,/answer\.stats = endStats\(answer\.stats, run\.cancelled \? 'cancelled' : 'error'\);/);
assert.match(source,/message\.stats = restoreStats\(message\.stats\)/);
// Statistics line: rate is the main value, tokens only when reported, placeholders never invent numbers.
assert.deepEqual(formatStats(complete,'en-US'),{text:'20.0 tokens/s · 30 tokens',title:'Generation rate from backend timings (generation phase) over 1.5 s. 30 generated tokens. 4 prompt tokens. Model call 1.6 s.',phase:'complete'});
assert.equal(formatStats({...complete,tokensPerSecond:24.25,completionTokens:1234},'de-DE').text,'24,3 tokens/s · 1.234 tokens');
assert.equal(formatStats({...complete,completionTokens:1},'en-US').text,'20.0 tokens/s · 1 token');
assert.equal(formatStats(completeStats(pending,{...wire,rate_source:'harness_stream'}),'en-US').title.startsWith('Generation rate from harness stream timing over 1.5 s.'),true);
assert.equal(formatStats(completeStats(pending,{...wire,rate_window_ms:250,elapsed_ms:0.4}),'en-US').title,'Generation rate from backend timings (generation phase) over 250 ms. 30 generated tokens. 4 prompt tokens. Model call < 1 ms.');
const noRate = completeStats(pending,{completion_tokens:12});
assert.deepEqual(formatStats(noRate,'en-US'),{text:'– tokens/s · 12 tokens',title:'The backend reported no generation rate for this answer. 12 generated tokens.',phase:'complete'});
assert.equal(formatStats(completeStats(pending,null),'en-US').text,'– tokens/s');
assert.equal(formatStats(pending,'en-US').text,'– tokens/s');
for (const phase of ['cancelled','error','interrupted']) assert.equal(formatStats(endStats(complete,phase),'en-US').text,`– tokens/s (${phase})`);
// Stray values on non-complete phases are never shown.
assert.equal(formatStats({...complete,phase:'pending'},'en-US').text,'– tokens/s');
for (const junk of [null,undefined,'x',{},{phase:'weird',tokensPerSecond:5}]) assert.equal(formatStats(junk,'en-US'),null);
// Chat switch: the line follows the shown conversation's latest answer; chats without stats hide it.
assert.equal(formatStats(latestStats(chatA),'en-US').text,'20.0 tokens/s · 30 tokens');
assert.equal(formatStats(latestStats([]),'en-US'),null);
assert.equal(formatStats(latestStats([{role:'user',content:'x'},{role:'assistant',content:'saved before stats'}]),'en-US'),null);
// app.mjs wiring: renderMessages (every delta, completion, error and conversation switch) refreshes the line.
assert.match(source,/if \(follow\) box\.scrollTop = box\.scrollHeight;\n\s*renderStats\(shown\);/);
assert.match(source,/formatStats\(latestStats\(messages\), navigator\.languages\?\.\[0\] \|\| navigator\.language\)/);
assert.match(source,/line\.hidden = !view; line\.textContent = view\?\.text \|\| ''/);
// Layout structure (docs/WEB-UI-PLAN.md sections 4-6): markup contract, CSP-safe CSS, kinds of notices.
const web = name => readFileSync(fileURLToPath(new URL(`../crates/lh-cli/web/${name}`,import.meta.url)),'utf8');
const html = web('index.html'), layout = web('layout.css').replace(/\/\*[\s\S]*?\*\//g,'');
assert(html.indexOf('href="/style.css"') >= 0 && html.indexOf('href="/style.css"') < html.indexOf('href="/layout.css"'),'layout.css loads after style.css');
assert.doesNotMatch(html,/\sstyle=/,'no inline style attributes (CSP style-src self)');
for (const id of ['composer','prompt','send','cancel','model','agent','speak','files','record','attachments','approvals','messages','run-status','chat-stats','conversation-list','conversation-title','new-chat','rename-chat','delete-chat','export','auth-form','token','clear-token','theme','notice','storage-warning','chat-page','management-page','page-title','page-status','refresh-page','management-content','refresh-models','speech-player'])
  assert.equal(html.split(`id="${id}"`).length,2,`exactly one #${id}`);
assert.equal(html.match(/<h1[\s>]/g).length,1);
const tag = id => html.match(new RegExp(`<[a-z]+ id="${id}"[^>]*>`))[0];
assert.match(tag('send'),/class="button-primary"/); assert.match(tag('delete-chat'),/class="button-danger"/);
for (const id of ['theme','refresh-models','clear-token','export']) assert.match(tag(id),/class="button-quiet"/,id);
assert.match(html,/<button class="button-primary" type="submit">Save token<\/button>/);
// Send is the last control of the composer action row, Cancel directly before it.
assert.match(html,/<div class="composer-submit"><button id="cancel"[^>]*>Cancel<\/button><button id="send"[^>]*>Send<\/button><\/div><\/div>/);
assert(html.indexOf('class="composer-options"') < html.indexOf('class="composer-actions"'));
// The approval rule moved into each approval card; the composer hint keeps the limits.
assert.doesNotMatch(html,/applies only to identical calls/); assert.match(source,/Agent approval “Always” applies only to identical calls in this run\./);
assert.match(html,/Attachments: at most 4 MiB total per turn\. Full conversation requests must fit 6 MiB\./);
// Success notes are info, failures default to error; metadata is dt/dd, not dot-joined sentences.
assert.match(source,/const notice = \(text, kind = 'error'\) => \{[^}]*dataset\.kind = kind/);
for (const text of ['Code copied.','Token saved for this tab.','Token cleared.']) assert.match(source,new RegExp(`notice\\('${text.replace('.','\\.')}', 'info'\\)`),text);
assert.doesNotMatch(source,/version \$\{item\.version\} ·|Task: \$\{item\.task\} ·|Plugin: \$\{item\.plugin\} ·|: \$\{p\.state\} ·/);
assert.match(source,/el\('footer', undefined, 'message-meta'\)/); assert.match(source,/'conversation-item selected' : 'conversation-item'/);
// layout.css: placement only, one breakpoint, tokens with literal fallbacks, no visual reordering.
assert.deepEqual([...new Set(layout.match(/@media[^{]+/g).map(s => s.trim()))],['@media (max-width: 760px)']);
assert.doesNotMatch(layout,/var\(--[a-z0-9-]+\)/,'every token has a fallback');
assert.doesNotMatch(layout,/(^|[\s;{])order\s*:/,'no CSS order: DOM order is visual order');
assert.doesNotMatch(layout,/(^|[\s;{])(color|background|border-color|font|box-shadow|border-radius)\s*:/m,'visual properties belong to style.css');
// Regressions found in review t_bf3172bc (Chromium probes at 1920-320 px):
// long conversation titles pushed Send below the fold (clamp 1 line desktop / 2 mobile, full text kept as title),
const rule = (text, selector) => { const start = text.indexOf(`${selector} {`); assert(start >= 0, `missing ${selector}`); return text.slice(start, text.indexOf('}', start)); };
const mobile = layout.slice(layout.indexOf('@media (max-width: 760px)'));
assert.match(rule(layout,'.conversation-header h2'),/-webkit-line-clamp: 1;[^}]*overflow: hidden/);
assert.match(rule(mobile,'.conversation-header h2'),/-webkit-line-clamp: 2/);
assert.match(source,/\$\('conversation-title'\)\.textContent = \$\('conversation-title'\)\.title = /);
// badges overlapped card titles, mobile conversation items were wider than their scroll box (clipped focus ring),
assert.match(rule(layout,'.card-header .badges'),/min-width: 0;[^}]*max-width: 60%/);
assert.match(rule(mobile,'.conversation-item'),/max-width: min\(12\.5rem, 100%\)/);
assert.match(rule(mobile,'#conversation-list'),/scroll-padding-inline: var\(--space-2, \.5rem\)/);
// and the native file input spilled past the 320 px gutter.
assert.match(rule(layout,'.file-label'),/max-width: 100%; min-width: 0/);
// A pending approval card collapsed to 0 px (1024x768, mobile) or clipped its buttons (1366x900):
// it must never shrink; #messages yields space instead, and the card is scrolled into view.
assert.match(rule(layout,'#approvals'),/flex: 0 0 auto/);
assert.doesNotMatch(rule(layout,'#approvals'),/overflow|max-height|min-height: 0/);
assert.match(layout,/\.chat-main:has\(> #approvals:not\(:empty\)\) > #messages \{ min-height: 6rem; \}/);
assert.match(source,/\$\('approvals'\)\.append\(card\);\s*\/\/[^\n]*\n\s*card\.scrollIntoView\(\{block:'nearest'\}\);/);
// Options: default context for new chats (docs/ARCHITECTURE.md "Options").
class MemoryStorage { constructor(entries = {}) { this.map = new Map(Object.entries(entries)); this.writes = 0; } getItem(k) { return this.map.has(k) ? this.map.get(k) : null; } setItem(k,v) { this.writes++; this.map.set(k,String(v)); } removeItem(k) { this.map.delete(k); } }
assert.equal(DEFAULT_CONTEXT_KEY,'lh.defaultContext');
assert.equal(SUGGESTED_DEFAULT_CONTEXT,'Erfinde keine Zahlen und Fakten. Wenn du dir unsicher bist, führe eine Websuche durch. Wenn du nicht weißt, was in einer Datei steht, fordere das Lesen der Datei an.');
{ // Initialization: fresh/older install without a stored value gets the suggested text once.
  const storage = new MemoryStorage({'lh.conversations':'[]','lh.theme':'dark'}), settings = createSettings(storage);
  assert.deepEqual(settings.init(),{value:SUGGESTED_DEFAULT_CONTEXT,created:true,persisted:true});
  assert.equal(storage.getItem(DEFAULT_CONTEXT_KEY),SUGGESTED_DEFAULT_CONTEXT);
  assert.equal(storage.getItem('lh.conversations'),'[]'); assert.equal(storage.getItem('lh.theme'),'dark');
  assert.deepEqual(settings.init(),{value:SUGGESTED_DEFAULT_CONTEXT,created:false,persisted:true});
  assert.deepEqual(createSettings(storage).init(),{value:SUGGESTED_DEFAULT_CONTEXT,created:false,persisted:true}); assert.equal(storage.writes,1);
  assert.equal(settings.defaultContext(),SUGGESTED_DEFAULT_CONTEXT); assert.equal(settings.defaultContextForNewChat(),SUGGESTED_DEFAULT_CONTEXT);
}
for (const existing of ['Mein eigener Kontext\nZeile 2','',' \n ']) { // Stored values, including empty ones, are never overwritten.
  const storage = new MemoryStorage({[DEFAULT_CONTEXT_KEY]:existing});
  assert.deepEqual(createSettings(storage).init(),{value:existing,created:false,persisted:true});
  assert.equal(storage.getItem(DEFAULT_CONTEXT_KEY),existing); assert.equal(storage.writes,0);
}
{ // Saving persists across a restart (new settings object over the same storage), text exact.
  const storage = new MemoryStorage(), settings = createSettings(storage); settings.init();
  const text = '  Antworte knapp.\nNenne Quellen.  ';
  assert.deepEqual(settings.saveDefaultContext(text),{value:text,persisted:true});
  const restarted = createSettings(storage);
  assert.equal(restarted.init().created,false);
  assert.equal(restarted.defaultContext(),text); assert.equal(restarted.defaultContextForNewChat(),'Antworte knapp.\nNenne Quellen.');
  // Another tab's save is seen on the next read.
  storage.setItem(DEFAULT_CONTEXT_KEY,'Aus Tab B'); assert.equal(settings.defaultContext(),'Aus Tab B');
}
{ // A deliberately empty (or whitespace-only) value disables the context and survives restarts.
  const storage = new MemoryStorage(), settings = createSettings(storage); settings.init();
  for (const empty of ['','  \n\t ']) {
    assert.deepEqual(settings.saveDefaultContext(empty),{value:empty,persisted:true});
    assert.equal(settings.defaultContextForNewChat(),'');
    const restarted = createSettings(storage);
    assert.deepEqual(restarted.init(),{value:empty,created:false,persisted:true});
    assert.equal(restarted.defaultContext(),empty); assert.equal(restarted.defaultContextForNewChat(),'');
  }
}
{ // Invalid input is rejected without touching the stored value.
  const storage = new MemoryStorage({[DEFAULT_CONTEXT_KEY]:'alt'}), settings = createSettings(storage);
  assert.throws(() => settings.saveDefaultContext('x'.repeat(MAX_DEFAULT_CONTEXT + 1)),/limited to 8000 characters/);
  assert.throws(() => settings.saveDefaultContext(null),TypeError);
  assert.equal(storage.getItem(DEFAULT_CONTEXT_KEY),'alt'); assert.equal(settings.saveDefaultContext('x'.repeat(MAX_DEFAULT_CONTEXT)).persisted,true);
}
{ // Unavailable or failing storage: no throw, value kept in memory for this tab, persisted false.
  const none = createSettings(null);
  assert.deepEqual(none.init(),{value:SUGGESTED_DEFAULT_CONTEXT,created:true,persisted:false});
  assert.deepEqual(none.saveDefaultContext(''),{value:'',persisted:false}); assert.equal(none.defaultContext(),''); assert.equal(none.defaultContextForNewChat(),'');
  const full = new MemoryStorage({[DEFAULT_CONTEXT_KEY]:'gespeichert'}); full.setItem = () => { throw new Error('QuotaExceededError'); };
  const settings = createSettings(full);
  assert.deepEqual(settings.saveDefaultContext('neu'),{value:'neu',persisted:false});
  assert.equal(settings.defaultContext(),'neu'); assert.equal(full.getItem(DEFAULT_CONTEXT_KEY),'gespeichert');
  const flaky = new MemoryStorage({[DEFAULT_CONTEXT_KEY]:'zuletzt'}), reader = createSettings(flaky); reader.init();
  flaky.getItem = () => { throw new Error('SecurityError'); };
  assert.equal(reader.defaultContext(),'zuletzt');
}
// Options markup: visible, editable textarea with label, hint and Save; app.mjs wiring.
for (const id of ['options-page','options-form','default-context','default-context-hint','save-options','restore-default-context','options-status'])
  assert.equal(html.split(`id="${id}"`).length,2,`exactly one #${id}`);
assert.match(html,/<a href="#options">Options<\/a><\/nav>/);
assert.match(html,/<label for="default-context">Default context for new chats<\/label><textarea id="default-context"/);
assert.match(tag('default-context'),new RegExp(`maxlength="${MAX_DEFAULT_CONTEXT}"`)); assert.doesNotMatch(tag('default-context'),/readonly|disabled/);
assert.match(html,/Changes apply only to conversations created after saving; existing conversations keep their context\./);
assert.match(tag('save-options'),/class="button-primary" type="submit"/); assert.match(tag('restore-default-context'),/class="button-quiet"/);
assert.match(source,/const settings = createSettings\(browserStorage\);\n\s*if \(!settings\.init\(\)\.persisted\)/);
assert(source.indexOf('settings.init()') < source.indexOf("if (!conversations.length) fresh();"),'options initialize before the first conversation is created');
assert.match(source,/\['plugins','tools','skills','status','options'\]/); assert.match(source,/if \(page === 'options'\) \{ loadOptions\(\); return; \}/);
assert.match(source,/settings\.saveDefaultContext\(\$\('default-context'\)\.value\)/);
// Default context in conversations: snapshot at creation, kept across reloads, sent once.
{ // Snapshot: a conversation keeps the text valid at its creation; later saves affect only new ones.
  const storage = new MemoryStorage(), settings = createSettings(storage); settings.init();
  const first = newConversation('a', settings.defaultContextForNewChat());
  assert.deepEqual(first,{id:'a',title:'New conversation',messages:[],history:[],defaultContext:SUGGESTED_DEFAULT_CONTEXT});
  settings.saveDefaultContext('  Antworte auf Deutsch.  ');
  const second = newConversation('b', settings.defaultContextForNewChat());
  assert.equal(conversationContext(first),SUGGESTED_DEFAULT_CONTEXT); assert.equal(conversationContext(second),'Antworte auf Deutsch.');
  settings.saveDefaultContext('');
  const third = newConversation('c', settings.defaultContextForNewChat());
  assert.equal(third.defaultContext,''); assert.equal(conversationContext(first),SUGGESTED_DEFAULT_CONTEXT); assert.equal(conversationContext(second),'Antworte auf Deutsch.');
  // Reload (lh.conversations JSON round trip, new settings over the same storage): snapshots survive unchanged.
  storage.setItem('lh.conversations',JSON.stringify([third,second,first]));
  const restarted = createSettings(storage); restarted.init(); restarted.saveDefaultContext('Neu nach dem Neustart.');
  const [r3,r2,r1] = JSON.parse(storage.getItem('lh.conversations'));
  assert.equal(conversationContext(r1),SUGGESTED_DEFAULT_CONTEXT); assert.equal(conversationContext(r2),'Antworte auf Deutsch.'); assert.equal(conversationContext(r3),'');
  assert.equal(conversationContext(newConversation('d', restarted.defaultContextForNewChat())),'Neu nach dem Neustart.');
}
{ // Model requests: one leading system message; empty context or older conversations add nothing.
  const user = {role:'user',content:'Wie hoch ist der Mount Everest?'};
  assert.deepEqual(requestMessages('Erfinde nichts.',[user]),[{role:'system',content:'Erfinde nichts.'},user]);
  assert.deepEqual(requestMessages('',[user]),[user]);
  for (const old of [{id:'x',title:'t',messages:[]},{id:'y',title:'t',messages:[],defaultContext:42},null]) {
    assert.equal(conversationContext(old),''); assert.deepEqual(requestMessages(conversationContext(old),[user]),[user]);
  }
  // Existing system instructions and chat context stay, after the default context.
  const existing = [{role:'system',content:'Du bist ein Spielleiter.'},user];
  assert.deepEqual(requestMessages('Erfinde nichts.',existing),[{role:'system',content:'Erfinde nichts.'},...existing]);
  // The input history is not mutated (the snapshot never becomes a stored chat turn).
  const history = [user]; requestMessages('Erfinde nichts.',history); assert.deepEqual(history,[user]);
}
{ // No duplicates over repeated requests, including agent runs whose lh_messages echo the request.
  const c = newConversation('a','Erfinde nichts.'), context = conversationContext(c);
  let history = [];
  for (let turn = 1; turn <= 3; turn++) {
    const sent = requestMessages(context,[...history,{role:'user',content:`Frage ${turn}`}]);
    assert.equal(sent.filter(m => m.role === 'system' && m.content === context).length,1,`turn ${turn}`);
    assert.deepEqual(sent[0],{role:'system',content:context});
    const lhMessages = turn % 2 ? sent : sent.slice(1); // Agent (echo incl. context) and chat (client history) turns.
    history = [...historyWithoutContext(context,lhMessages),{role:'assistant',content:`Antwort ${turn}`}];
    assert(!history.some(m => m.role === 'system'),'context is never stored in history');
  }
  assert.equal(history.length,6);
  // A history already starting with the context is not prefixed again; other system messages are not stripped.
  assert.equal(requestMessages(context,[{role:'system',content:context}]).length,1);
  assert.deepEqual(historyWithoutContext(context,[{role:'system',content:'anders'}]),[{role:'system',content:'anders'}]);
  assert.deepEqual(historyWithoutContext('',[{role:'system',content:''}]),[{role:'system',content:''}]);
}
// app.mjs wiring: fresh() is the only creation path and snapshots via settings; send() uses the snapshot.
assert.equal(source.match(/title: 'New conversation', messages: \[\]/g).length,1,'conversation literal only in newConversation');
assert.match(source,/const c = newConversation\(id, settings\.defaultContextForNewChat\(\)\);/);
assert.equal(source.match(/newConversation\(/g).length,2,'one definition, one call (fresh)');
for (const caller of ["if (!conversations.length) fresh();","$('new-chat').addEventListener('click',fresh);","if (!selected) fresh();"]) assert(source.includes(caller),caller);
assert.match(source,/const context = conversationContext\(c\);\n\s*const payload = \{model:\$\('model'\)\.value \|\| 'auto',messages:requestMessages\(context,history\)\}/);
assert.match(source,/historyWithoutContext\(context,savedHistory\)/);
assert.doesNotMatch(source,/defaultContext\s*=/,'existing conversations are never reassigned a context');
console.log('Web: syntax, escaped Markdown, chunked UTF-8 SSE, body limits, Ctrl+Enter shortcut, speak ID selection, generation stats, layout structure, default-context options and conversation snapshots passed.');
