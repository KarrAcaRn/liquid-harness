import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {renderMarkdown} from '../crates/lh-cli/web/markdown.mjs';
import {readSSE,requestBody,MAX_BODY,composerShortcut,speakIdsFor,resolveSpeakSelection,speakOptions,speakChoiceFromValue,SPEAK_NONE,newConversation,conversationContext,requestMessages,historyWithoutContext,loadingLabel,formatBytes,modelStateLabel,newBinaryAvailable,aboutModels,restartedSince,modelOptions,opensSelect,singleFlight,micView,insertTranscript,MIC_LABEL,agentFallback,NOT_AGENT} from '../crates/lh-cli/web/app.mjs';
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
// Model dropdown: rebuilt on open, ids once, selection kept while still offered.
assert.deepEqual(modelOptions([{id:'auto'},{id:'a'},{id:'a'},{id:''},{},null,{id:7},{id:'b'}],'b'),{options:[{value:'auto',label:'Auto route'},{value:'a',label:'a'},{value:'b',label:'b'}],selected:'b'});
assert.equal(modelOptions([{id:'auto'},{id:'a'}],'gone').selected,'auto');
assert.deepEqual(modelOptions(undefined,'a'),{options:[],selected:''});
assert.equal(opensSelect({type:'pointerdown',button:0}),true);
assert.equal(opensSelect({type:'pointerdown',button:2}),false);
for (const init of [{key:'F4'},{key:'ArrowDown',altKey:true},{key:'ArrowUp',altKey:true},{key:' '},{key:'Enter'}]) assert.equal(opensSelect({type:'keydown',...init}),true,JSON.stringify(init));
for (const init of [{key:'ArrowDown'},{key:'a'},{key:'Tab'},{key:' ',ctrlKey:true}]) assert.equal(opensSelect({type:'keydown',...init}),false,JSON.stringify(init));
assert.equal(opensSelect({type:'focus'}),false);
{
  let calls = 0, release; const flight = singleFlight(() => { calls++; return new Promise(r => { release = r; }); });
  const first = flight(), second = flight(), third = flight();
  assert.equal(first,second); assert.equal(second,third);
  await Promise.resolve(); release('ok'); assert.equal(await first,'ok'); assert.equal(calls,1);
  const failing = singleFlight(() => { calls++; throw new Error('down'); });
  await assert.rejects(failing(),/down/); await assert.rejects(failing(),/down/); assert.equal(calls,3);
  const again = flight(); await Promise.resolve(); release('again'); assert.equal(await again,'again'); assert.equal(calls,4,'a new run starts after the previous one settled');
}
// Model lifecycle: loading events and status entries.
assert.equal(loadingLabel({specialist:'lfm3-text'}),'Loading lfm3-text…');
assert.equal(loadingLabel({specialist:'lfm3-text',progress:0.424,eta_ms:2100}),'Loading lfm3-text… 42% ~3 s left');
assert.equal(loadingLabel({id:'x',progress:7}),'Loading x… 100%');
assert.equal(modelStateLabel({state:'loading',progress:0.5}),'Loading… 50%');
assert.equal(modelStateLabel({state:'ready'}),'Ready');
// About this harness (GET /lh/v1/self).
assert.equal(newBinaryAvailable({build:{binary_changed:true}}),true);
assert.equal(newBinaryAvailable({build:{binary_changed:null}}),false);
assert.equal(newBinaryAvailable(undefined),false);
assert.deepEqual(aboutModels({models:[
  {id:'off',kind:'router',active:false,backends:[]},
  {id:'text',kind:'specialist',active:true,state:'ready',backends:[{backend:'freetoken',enabled:false,source:'hf:a',local:null},{backend:'llamacpp',enabled:true,source:'hf:b',local:{downloaded:true,model_bytes:1024,projector_bytes:1024}}]},
  {id:'vl',kind:'specialist',active:true,backends:[{backend:'llamacpp',enabled:true,source:'hf:c',local:{downloaded:false}}]},
  {id:'remote',kind:'specialist',active:true,backends:[{backend:'freetoken',enabled:true,source:'hf:d',local:null}]},
]}),[
  {id:'text',kind:'specialist',source:'hf:b',backend:'llamacpp',disk:'on disk 2.0 KiB',downloaded:true,state:'ready'},
  {id:'vl',kind:'specialist',source:'hf:c',backend:'llamacpp',disk:'not downloaded',downloaded:false,state:'stopped'},
  {id:'remote',kind:'specialist',source:'hf:d',backend:'freetoken',disk:'managed by backend',downloaded:null,state:'stopped'},
]);
assert.equal(restartedSince({build:{started_unix:1}},{build:{started_unix:2}}),true);
assert.equal(restartedSince({build:{started_unix:1}},{build:{started_unix:1}}),false);
assert.equal(restartedSince({build:{started_unix:1}},null),false);
assert.equal(modelStateLabel({state:'failed'}),'Failed');
assert.equal(modelStateLabel({}),'Stopped');
assert.equal(formatBytes(512),'512 B');
assert.equal(formatBytes(1536*1024*1024),'1.5 GiB');
assert.equal(formatBytes(null),'unknown');
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
assert.match(source,/speakCatalog = catalog; syncSpeak\(\);/);
assert.match(source,/\$\('model'\)\.addEventListener\('change',syncSpeak\)/);
// "Speak replies" dropdown: "<none>" first, then each offered ID once; the shown value mirrors the choice.
const labels = view => view.options.map(o => o.label);
assert.equal(SPEAK_NONE,'<none>');
assert.deepEqual(speakOptions([],null),{options:[{value:'',label:'<none>'}],selected:''},'model without speak IDs: only <none>');
for (const junk of [undefined,null,'x',{}]) assert.deepEqual(labels(speakOptions(junk,'x')),['<none>']);
assert.deepEqual(speakOptions(['mock-speak'],'mock-speak'),{options:[{value:'',label:'<none>'},{value:'mock-speak',label:'mock-speak'}],selected:'mock-speak'});
assert.deepEqual(labels(speakOptions(['a','b','a','',7],'b')),['<none>','a','b'],'no duplicates or junk');
assert.equal(speakOptions(['a','b'],'b').selected,'b');
for (const choice of [null,undefined,true,'stale','']) assert.equal(speakOptions(['a'],choice).selected,'',String(choice));
// Option value → setting: <none> stops automatic speech, an ID is used verbatim.
assert.equal(speakChoiceFromValue(''),null);
for (const junk of [undefined,null,0,{}]) assert.equal(speakChoiceFromValue(junk),null);
assert.equal(speakChoiceFromValue('mock-speak'),'mock-speak');
// Model switch round trip as the UI runs it (syncSpeak → renderSpeakControl): options follow the model,
// the stale choice is dropped and stays dropped, and the control never shows a value it does not hold.
const ui = {choice:null,model:'auto'};
const sync = () => { const available = speakIdsFor(catalog,ui.model); ui.choice = resolveSpeakSelection(ui.choice,available); return speakOptions(available,ui.choice); };
let shown = sync(); assert.deepEqual([labels(shown),shown.selected],[['<none>','mock-speak'],'']);
ui.choice = speakChoiceFromValue('mock-speak'); shown = sync(); assert.equal(shown.selected,'mock-speak');
ui.model = 'mock-transcribe'; shown = sync(); assert.deepEqual([labels(shown),shown.selected,ui.choice],[['<none>','mock-speak'],'mock-speak','mock-speak']);
ui.model = 'mock-text'; shown = sync(); assert.deepEqual([labels(shown),shown.selected,ui.choice],[['<none>'],'',null]);
ui.model = 'two'; shown = sync(); assert.deepEqual([labels(shown),shown.selected],[['<none>','a','b'],'']);
ui.choice = speakChoiceFromValue('b'); shown = sync(); assert.equal(shown.selected,'b');
ui.choice = speakChoiceFromValue(''); shown = sync(); assert.deepEqual([shown.selected,ui.choice],['',null]);
// app.mjs/index.html wiring: the old checkbox is gone, the select is rebuilt (not appended to) and
// starts at <none> on every page load (no browser form restore of a not-yet-built option).
assert.match(source,/let speakCatalog = null, speakAvailable = \[\], speakChoice = null;/);
assert.match(source,/\$\('speak'\)\.replaceChildren\(\.\.\.options\.map/);
assert.match(source,/\$\('speak'\)\.value = selected;/);
assert.match(source,/\$\('speak'\)\.addEventListener\('change',event => chooseSpeak\(speakChoiceFromValue\(event\.target\.value\)\)\);/);
assert.doesNotMatch(source,/\$\('speak'\)\.checked/);
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
for (const id of ['composer','prompt','send','cancel','model','speak','files','record','attachments','approvals','messages','run-status','chat-stats','conversation-list','conversation-title','new-chat','rename-chat','delete-chat','export','auth-form','token','clear-token','theme','notice','storage-warning','chat-page','management-page','page-title','page-status','refresh-page','management-content','speech-player'])
  assert.equal(html.split(`id="${id}"`).length,2,`exactly one #${id}`);
assert.equal(html.match(/<h1[\s>]/g).length,1);
const tag = id => html.match(new RegExp(`<[a-z]+ id="${id}"[^>]*>`))[0];
assert.match(tag('send'),/class="button-primary"/); assert.match(tag('delete-chat'),/class="button-danger"/);
for (const id of ['theme','clear-token','export']) assert.match(tag(id),/class="button-quiet"/,id);
// No separate model refresh control: the Model dropdown refreshes itself when opened.
assert.doesNotMatch(html,/refresh-models|Refresh models/i); assert.doesNotMatch(source,/refresh-models/);
assert.match(html,/<div class="composer-model"><label for="model">Model<\/label><select id="model"><option value="auto">Auto route<\/option><\/select><\/div>/);
assert.match(source,/\$\('model'\)\.addEventListener\('pointerdown',refreshModelsOnOpen\); \$\('model'\)\.addEventListener\('keydown',refreshModelsOnOpen\);/);
assert.match(source,/const models = singleFlight\(/);
assert.match(html,/<button class="button-primary" type="submit">Save token<\/button>/);
// Agent mode is always on: no toggle, every turn starts as an agent run (tools execute).
assert.doesNotMatch(html,/id="agent"|Agent mode|execute tools|class="check"/i); assert.doesNotMatch(source,/\$\('agent'\)|'agent'\]/);
assert.match(html,/<\/select><\/div><div class="composer-speak">/,'Speak replies directly follows Model');
assert.match(source,/try \{ response = await api\('\/lh\/v1\/agent\/runs',\{value:payload,signal:run\.controller\.signal\}\); \}\n\s*catch \(error\) \{ if \(!agentFallback\(error\)\) throw error; run\.agent = false; response = await api\('\/v1\/chat\/completions',\{value:chatPayload,signal:run\.controller\.signal\}\); \}/);
assert.match(source,/const run = \{controller:new AbortController\(\),id:null,agent:true,cancelled:false\}/);
assert.equal(source.match(/\/lh\/v1\/agent\/runs',/g).length,1,'one agent run request site'); assert.equal(source.match(/'\/v1\/chat\/completions'/g).length,1,'chat only as the fallback');
// Only the server's "not an agent" rejection falls back; every other failure is reported.
const failure = (status,detail) => Object.assign(new Error(`HTTP ${status}: ${detail}`),{status,detail});
assert.equal(agentFallback(failure(400,NOT_AGENT)),true);
for (const other of [failure(409,'engine busy'),failure(400,'agent request accepts only model and messages'),failure(500,NOT_AGENT),new Error(NOT_AGENT),{status:400},null,undefined]) assert.equal(agentFallback(other),false);
assert(readFileSync(fileURLToPath(new URL('../crates/lh-cli/src/server/agent_api.rs',import.meta.url)),'utf8').includes(`bad("${NOT_AGENT}")`),'fallback string matches the server error');
assert.doesNotMatch(layout,/\.check\b/);
// Composer input row: microphone (voice input) with the paperclip (attach) below it, then the message field.
assert.match(html,/<label for="prompt">Message<\/label><div class="composer-input"><div class="composer-tools"><button id="record" class="mic-button icon-button" type="button" title="Sprachaufnahme" aria-label="Sprachaufnahme" aria-pressed="false" data-state="idle"><svg class="mic-icon button-icon"[^>]*aria-hidden="true"[^>]*>[\s\S]*?<\/svg><\/button><button id="attach" class="icon-button" type="button" title="Datei anhängen" aria-label="Datei anhängen" aria-controls="files"><svg class="button-icon"[^>]*aria-hidden="true"[^>]*>[\s\S]*?<\/svg><\/button><input id="files" type="file" accept="image\/png,image\/jpeg,image\/gif,image\/webp,audio\/\*,\.wav,\.mp3,\.flac,\.ogg,\.m4a,\.webm,\.mp4" multiple hidden tabindex="-1"><\/div><textarea id="prompt"[^>]*><\/textarea><\/div>/);
assert.doesNotMatch(html,/Record audio|Stop recording/); assert.doesNotMatch(source,/Record audio|Stop recording/);
// Sidebar header: the New conversation action is an icon-only "+" button; its name lives in title/aria-label.
assert.match(html,/<div class="sidebar-header"><h2 id="conversations-heading">Conversations<\/h2><button id="new-chat" class="icon-button" type="button" title="New conversation" aria-label="New conversation"><svg class="button-icon"[^>]*aria-hidden="true"[^>]*><path d="M12 5v14M5 12h14"[^>]*\/><\/svg><\/button><\/div>/);
assert.equal(html.match(/<button id="new-chat"[^>]*>([\s\S]*?)<\/button>/)[1].replace(/<svg[\s\S]*<\/svg>/,'').trim(),'','new-chat button shows only the plus icon');
assert.equal(html.match(/<button id="record"[^>]*>([\s\S]*?)<\/button>/)[1].replace(/<svg[\s\S]*<\/svg>/,'').trim(),'','microphone button shows only the icon');
// The paperclip replaces the old "Attach images or audio" file label; it only opens the one hidden picker.
assert.equal(html.match(/<button id="attach"[^>]*>([\s\S]*?)<\/button>/)[1].replace(/<svg[\s\S]*<\/svg>/,'').trim(),'','paperclip button shows only the icon');
assert.doesNotMatch(html,/Attach images or audio|file-label|composer-media/); assert.doesNotMatch(layout + web('style.css'),/file-label|composer-media|file-selector-button/);
assert.equal(html.match(/type="file"/g).length,1,'exactly one file input');
assert.match(source,/\$\('attach'\)\.addEventListener\('click',\(\) => \$\('files'\)\.click\(\)\);/);
assert.equal(source.match(/attach\(\[\.\.\./g).length,1,'one attach path (the picker change handler)');
assert.match(source,/for \(const id of \[[^\]]*'files','attach','record'[^\]]*\]\) \$\(id\)\.disabled = value;/);
assert.equal(MIC_LABEL,'Sprachaufnahme');
assert.deepEqual(micView('idle'),{state:'idle',pressed:'false',busy:'false',disabled:false,title:'Sprachaufnahme'});
assert.deepEqual(micView('recording'),{state:'recording',pressed:'true',busy:'false',disabled:false,title:'Sprachaufnahme beenden'});
assert.deepEqual(micView('transcribing'),{state:'transcribing',pressed:'false',busy:'true',disabled:true,title:'Sprachaufnahme wird erkannt…'});
assert.equal(micView('bogus').state,'idle');
assert.deepEqual(insertTranscript('','0','0','  Hallo Welt '),{value:'Hallo Welt',caret:10},'non-integer caret appends');
assert.deepEqual(insertTranscript('',0,0,'Hallo'),{value:'Hallo',caret:5});
assert.deepEqual(insertTranscript('Frage:',6,6,'wie spät'),{value:'Frage: wie spät',caret:15});
assert.deepEqual(insertTranscript('ab',1,1,'X'),{value:'a X b',caret:4});
assert.deepEqual(insertTranscript('a b',1,2,'X'),{value:'a X b',caret:4},'a selected space is replaced');
assert.deepEqual(insertTranscript('alt neu',4,7,'frisch'),{value:'alt frisch',caret:10},'selection is replaced');
assert.deepEqual(insertTranscript('text',99,99,'mehr'),{value:'text mehr',caret:9});
for (const empty of ['',' \n',null,undefined,7]) assert.deepEqual(insertTranscript('abc',1,1,empty),{value:'abc',caret:1});
// Wiring: multipart to the transcription API with auto routing, inserted into the field, never sent.
assert.match(source,/form\.append\('model','auto'\); form\.append\('file',new File\(\[blob\]/);
assert.match(source,/api\('\/v1\/audio\/transcriptions',\{form\}\)/);
assert.match(source,/notice\(''\); insertIntoPrompt\(text\); \}/);
const recordHandler = source.slice(source.indexOf("$('record').addEventListener"), source.indexOf("let theme;"));
assert.doesNotMatch(recordHandler,/requestSubmit|send\(|attach\(/,'voice input never submits or attaches');
assert.match(recordHandler,/showMic\('recording'\)/); assert.match(recordHandler,/showMic\('transcribing'\)/); assert.match(recordHandler,/finally \{ transcribing = false; showMic\('idle'\); \}/);
// Send is the last control of the composer action row, Cancel directly before it.
assert.match(html,/<div class="composer-submit"><button id="cancel"[^>]*>Cancel<\/button><button id="send"[^>]*>Send<\/button><\/div><\/div>/);
// Cancel and Send share one row with Model and Speak replies (inside .composer-options, after both).
assert.match(html,/<div class="composer-options"><div class="composer-model">.*?<\/select><\/div><div class="composer-speak">.*?<\/select><\/div><div class="composer-submit">/);
assert.doesNotMatch(html,/composer-actions/);
assert.match(layout,/\n\.composer-options \{ display: flex; flex-wrap: nowrap;/);
// The approval rule moved into each approval card; the composer hint keeps the limits.
assert.doesNotMatch(html,/applies only to identical calls/); assert.match(source,/Agent approval “Always” applies only to identical calls in this run\./);
assert.match(html,/Attachments: at most 4 MiB total per turn\. Full conversation requests must fit 6 MiB\./);
// Speak replies is a labelled dropdown whose static markup offers only <none>.
assert.match(html,/<div class="composer-speak"><label for="speak">Speak replies<\/label><select id="speak" autocomplete="off"><option value="">&lt;none&gt;<\/option><\/select><\/div>/);
assert.doesNotMatch(html,/id="speak" type="checkbox"|speack/i);
// At 390x844 the dropdown takes its own row; the composer limits hint is hidden on mobile instead.
assert.match(layout.slice(layout.indexOf('@media (max-width: 760px)')),/#composer > \.hint \{ display: none; \}/);
// The mobile nav is one internally scrolling line (plan section 4, M1) with ring-safe padding,
// and a focused link is scrolled fully into view (found when the Options link was added).
assert.match(source,/querySelector\('\.app-nav'\)\.addEventListener\('focusin',event => event\.target\.scrollIntoView\(\{block:'nearest',inline:'nearest'\}\)\)/);
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
assert.match(rule(mobile,'.app-nav'),/flex-wrap: nowrap; overflow-x: auto;[^}]*padding: 5px; margin: 0 -5px; scroll-padding-inline: 5px/);
// (The native file input that spilled past the 320 px gutter is gone: the paperclip opens a hidden picker.)
// The message field fills the row next to the fixed-size square microphone and paperclip buttons at every width.
assert.match(rule(layout,'.composer-input'),/display: flex/); assert.match(rule(layout,'.composer-input > #prompt'),/flex: 1 1 0; min-width: 0/);
assert.match(rule(layout,'.icon-button'),/flex: 0 0 auto;[^}]*width: var\(--control-height, 40px\); height: var\(--control-height, 40px\)/);
// The paperclip sits under the microphone; on mobile the field is as tall as both buttons (2 x 44 px + .5rem gap).
assert.match(rule(layout,'.composer-tools'),/flex: 0 0 auto; display: flex; flex-direction: column; gap: var\(--space-2, \.5rem\)/);
assert.match(rule(mobile,'#prompt'),/height: 6rem; min-height: 6rem/);
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
{ // End-to-end flow as boot()/fresh()/send() run it, over one storage across restarts (t_d69f6cdc).
  const storage = new MemoryStorage();
  const boot = () => { const settings = createSettings(storage); settings.init(); const raw = storage.getItem('lh.conversations'); return {settings, conversations: raw ? JSON.parse(raw) : []}; };
  const persist = conversations => storage.setItem('lh.conversations',JSON.stringify(conversations));
  const create = (app, id) => { const c = newConversation(id, app.settings.defaultContextForNewChat()); app.conversations.unshift(c); persist(app.conversations); return c; };
  const send = (app, c, text) => { const history = [...c.history,{role:'user',content:text}]; const sent = requestMessages(conversationContext(c),history); c.history = [...historyWithoutContext(conversationContext(c),sent),{role:'assistant',content:'ok'}]; persist(app.conversations); return sent; };
  const systems = sent => sent.filter(m => m.role === 'system').map(m => m.content);
  // 1. No stored option: the German suggestion is stored and the first chat (boot) takes it.
  let app = boot(); const a = create(app,'a');
  assert.equal(storage.getItem(DEFAULT_CONTEXT_KEY),SUGGESTED_DEFAULT_CONTEXT);
  assert.deepEqual(systems(send(app,a,'eins')),[SUGGESTED_DEFAULT_CONTEXT]); assert.deepEqual(systems(send(app,a,'zwei')),[SUGGESTED_DEFAULT_CONTEXT]);
  // 2. A saved change applies to the next chat only; 3. an empty value adds nothing.
  app.settings.saveDefaultContext('Antworte auf Deutsch.'); const b = create(app,'b');
  app.settings.saveDefaultContext(''); const c = create(app,'c');
  assert.deepEqual(systems(send(app,a,'drei')),[SUGGESTED_DEFAULT_CONTEXT]);
  assert.deepEqual(systems(send(app,b,'eins')),['Antworte auf Deutsch.']);
  assert.deepEqual(systems(send(app,c,'eins')),[]);
  // 4. Restart: the empty option survives (not re-initialized), each reopened chat keeps its own snapshot.
  const before = storage.getItem('lh.conversations'); app = boot();
  assert.equal(storage.getItem(DEFAULT_CONTEXT_KEY),''); assert.equal(storage.getItem('lh.conversations'),before,'boot rewrites no conversation');
  const [rc,rb,ra] = app.conversations;
  assert.deepEqual([ra,rb,rc].map(x => systems(send(app,x,'nach Neustart'))),[[SUGGESTED_DEFAULT_CONTEXT],['Antworte auf Deutsch.'],[]]);
  // Exactly once per chat: the snapshot is never stored as a turn, so every request carries it once.
  for (const x of app.conversations) assert(!x.history.some(m => m.role === 'system'),x.id);
  assert.equal(ra.history.length,8);
}
console.log('Web: syntax, escaped Markdown, chunked UTF-8 SSE, body limits, Ctrl+Enter shortcut, speak ID selection, generation stats, layout structure, default-context options and conversation snapshots passed.');
