import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {renderMarkdown} from '../crates/lh-cli/web/markdown.mjs';
import {readSSE,requestBody,MAX_BODY,composerShortcut} from '../crates/lh-cli/web/app.mjs';
for (const name of ['app.mjs','markdown.mjs']) {
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
console.log('Web: syntax, escaped Markdown, chunked UTF-8 SSE, body limits and Ctrl+Enter shortcut passed.');
