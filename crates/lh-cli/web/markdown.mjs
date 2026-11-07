// Small, deliberately limited Markdown renderer. Raw HTML is always text.
export function escapeHTML(text) {
  return String(text).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
}
function inline(text) {
  const pattern = /(`[^`\n]+`|!?\[[^\]\n]*\]\([^\s)]+\)|\*\*[^*\n]+\*\*|\*[^*\n]+\*)/g;
  let out = '', offset = 0;
  for (const match of text.matchAll(pattern)) {
    out += escapeHTML(text.slice(offset, match.index));
    const token = match[0];
    if (token.startsWith('`')) out += `<code>${escapeHTML(token.slice(1,-1))}</code>`;
    else if (token.startsWith('![')) out += escapeHTML(token); // Never fetch Markdown images.
    else if (token.startsWith('[')) {
      const [, label, url] = token.match(/^\[([^\]]*)\]\((.*)\)$/);
      // Only explicit web URLs; no javascript, data, credentials or relative URLs.
      let safe = false;
      try { const u = new URL(url); safe = ['http:', 'https:'].includes(u.protocol) && !u.username && !u.password; } catch { /* Render invalid URLs as text. */ }
      out += safe ? `<a href="${escapeHTML(url)}" target="_blank" rel="noopener noreferrer">${escapeHTML(label)}</a>` : escapeHTML(token);
    } else if (token.startsWith('**')) out += `<strong>${escapeHTML(token.slice(2,-2))}</strong>`;
    else out += `<em>${escapeHTML(token.slice(1,-1))}</em>`;
    offset = match.index + token.length;
  }
  return out + escapeHTML(text.slice(offset));
}
export function renderMarkdown(source) {
  const lines = String(source).replace(/\r\n?/g, '\n').split('\n');
  let output = '', code = null, list = false;
  const closeList = () => { if (list) { output += '</ul>'; list = false; } };
  for (const line of lines) {
    if (/^\s*```/.test(line)) {
      closeList();
      if (code !== null) { output += `<pre><code>${escapeHTML(code.join('\n'))}</code></pre>`; code = null; }
      else code = [];
      continue;
    }
    if (code !== null) { code.push(line); continue; }
    const heading = line.match(/^(#{1,6})\s+(.*)$/);
    const item = line.match(/^\s*[-*+]\s+(.*)$/);
    if (item) { if (!list) { output += '<ul>'; list = true; } output += `<li>${inline(item[1])}</li>`; continue; }
    closeList();
    if (heading) output += `<h${heading[1].length}>${inline(heading[2])}</h${heading[1].length}>`;
    else if (line.startsWith('> ')) output += `<blockquote>${inline(line.slice(2))}</blockquote>`;
    else if (line.trim()) output += `<p>${inline(line)}</p>`;
  }
  closeList();
  if (code !== null) output += `<pre><code>${escapeHTML(code.join('\n'))}</code></pre>`;
  return output;
}
