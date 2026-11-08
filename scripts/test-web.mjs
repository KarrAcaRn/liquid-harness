import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {renderMarkdown} from '../crates/lh-cli/web/markdown.mjs';
import {readSSE,requestBody,MAX_BODY,composerShortcut} from '../crates/lh-cli/web/app.mjs';
import {normalizeStats,startStats,completeStats,endStats,restoreStats,latestStats} from '../crates/lh-cli/web/stats.mjs';
for (const name of ['app.mjs','markdown.mjs','stats.mjs']) {
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
console.log('Web: syntax, escaped Markdown, chunked UTF-8 SSE, body limits, Ctrl+Enter shortcut and generation stats passed.');
