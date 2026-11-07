#!/usr/bin/env python3
"""Offline MCP stdio contract smoke; --internet adds optional real web probes."""
import argparse
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading

parser = argparse.ArgumentParser()
parser.add_argument('--internet', action='store_true')
parser.add_argument('--bin-dir', default='target/debug')
args = parser.parse_args()
bins = Path(args.bin_dir).resolve()
print('Load average:', Path('/proc/loadavg').read_text().strip())


def session(binary, config, calls, strict=True):
    messages = [
        {'jsonrpc': '2.0', 'id': 'init', 'method': 'initialize', 'params': {
            'protocolVersion': '2025-03-26', 'capabilities': {},
            'clientInfo': {'name': 'smoke', 'version': '1'}, 'config': config}},
        {'jsonrpc': '2.0', 'method': 'notifications/initialized'},
        {'jsonrpc': '2.0', 'id': 'list', 'method': 'tools/list'},
    ]
    messages += [{'jsonrpc': '2.0', 'id': i, 'method': 'tools/call',
                  'params': {'name': name, 'arguments': arguments}}
                 for i, (name, arguments) in enumerate(calls)]
    env = dict(os.environ)
    env.pop('LH_TOOL_CONFIG', None)
    result = subprocess.run([str(bins / binary)], input=''.join(json.dumps(m)+'\n' for m in messages),
                            text=True, capture_output=True, timeout=60, env=env)
    assert result.returncode == 0, result.stderr
    responses = [json.loads(line) for line in result.stdout.splitlines()]
    assert len(responses) == len(messages)-1
    print('\nSERVER', binary)
    for message in messages:
        print('> ' + json.dumps(message, ensure_ascii=False))
    for response in responses:
        print('< ' + json.dumps(response, ensure_ascii=False))
    assert responses[0]['result']['protocolVersion'] == '2025-03-26'
    names = {t['name'] for t in responses[1]['result']['tools']}
    assert all(name in names for name, _ in calls)
    if strict:
        assert all(not r.get('error') and not r['result'].get('isError') for r in responses)
    return responses[2:]


class Fake(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path.startswith('/search?'):
            body = json.dumps({'results': [{'title': 'Offline result', 'url': 'https://example.com/', 'content': 'fixture'}]}).encode()
            kind = 'application/json'
        else:
            body = b'<html><head><title>hidden</title></head><body><h1>Offline page</h1><p>Readable &amp; bounded.</p></body></html>'
            kind = 'text/html'
        self.send_response(200)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format, *args):
        pass


with tempfile.TemporaryDirectory(prefix='lh-mcp-', dir=os.environ.get('TMPDIR')) as root:
    responses = session('lh-tools-fs', {'workspace_root': root}, [
        ('write_file', {'path': 'src/demo.txt', 'content': 'alpha\nbeta\n'}),
        ('read_file', {'path': 'src/demo.txt', 'offset': 2, 'limit': 1}),
        ('edit_file', {'path': 'src/demo.txt', 'old_string': 'beta', 'new_string': 'gamma'}),
        ('list_dir', {'path': 'src'}),
        ('search_files', {'pattern': 'gamma', 'target': 'content'}),
    ])
    assert responses[1]['result']['content'][0]['text'] == '2|beta\n'
    assert Path(root, 'src/demo.txt').read_text() == 'alpha\ngamma\n'
    assert 'src/demo.txt:2:gamma' in responses[-1]['result']['content'][0]['text']
    responses = session('lh-tools-shell', {'workspace_root': root}, [
        ('run_command', {'command': 'printf hello; printf error >&2', 'cwd': 'src'})])
    assert json.loads(responses[0]['result']['content'][0]['text'])['output'] == 'helloerror'
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Fake)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        url = 'http://127.0.0.1:' + str(server.server_port)
        responses = session('lh-tools-web', {'provider': 'searxng', 'base_url': url, 'allow_private_network': True}, [
            ('web_search', {'query': 'fixture', 'limit': 1}), ('web_fetch', {'url': url + '/page'})])
        assert len(json.loads(responses[0]['result']['content'][0]['text'])['results']) == 1
        assert responses[1]['result']['content'][0]['text'] == 'Offline page Readable & bounded.'
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
if args.internet:
    session('lh-tools-web', {}, [('web_search', {'query': 'Rust programming language', 'limit': 2}),
                                ('web_fetch', {'url': 'https://example.com/'})], strict=False)
print('\nAll offline stdio assertions passed. Load average:', Path('/proc/loadavg').read_text().strip())
