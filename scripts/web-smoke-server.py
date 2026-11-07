#!/usr/bin/env python3
"""Temporary, localhost-only browser fixture. Ctrl-C cleans up all child processes.
Build the workspace first. --real uses cached models only, at eight threads.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--real', action='store_true')
parser.add_argument('--port', type=int, default=8485)
args = parser.parse_args()
llama = Path('/opt/data/tools/llama.cpp/llama-b12189/llama-server')
cache = Path('/opt/data/models/llama-cache')
if args.real:
    running = subprocess.run(['pgrep', '-x', 'llama-server'], capture_output=True)
    if running.returncode == 0:
        raise SystemExit('Refusing concurrent real-model run')
    print('Load before:', Path('/proc/loadavg').read_text().strip(), flush=True)
with tempfile.TemporaryDirectory(prefix='lh-web-') as temporary:
    directory = Path(temporary)
    pack = directory / 'pack'
    pack.mkdir()
    plugins = [pack, ROOT / 'plugins/tools-fs']
    if args.real:
        plugins.append(ROOT / 'plugins/backend-llamacpp')
        text = cache / 'LFM3-1.7B-LFM3-1.7B-Q4_K_M.gguf'
        vision = next(cache.glob('*VL*Q4_K_M.gguf'))
        projector = next(cache.glob('*VL*mmproj*'))
        for path in [llama, text, vision, projector]:
            if not path.exists():
                raise SystemExit(f'Missing {path}')
        manifest = '[plugin]\nid="web-smoke"\nversion="1"\nprovides=["specialist"]\n'
        for name, model, image in [('web-text', text, False), ('web-vision', vision, True)]:
            manifest += f'''[[specialist]]
id={json.dumps(name)}
route="Browser smoke"
task="chat"
inputs={json.dumps(['text', 'image'] if image else ['text'])}
requires={json.dumps(['image'] if image else [])}
outputs=["text"]
agent={str(not image).lower()}
tools={json.dumps([] if image else ['tools-fs'])}
backend="llamacpp"
model={{source={json.dumps('file:' + str(model))},ctx=8192,threads=8{',mmproj=' + json.dumps(str(projector)) if image else ''}}}
params={{temperature=0.1,max_tokens=192,chat_template_kwargs={{enable_thinking=false}}}}
'''
        config = f'''default_specialist="web-text"
[plugins.llamacpp.config]
llama_server={json.dumps(str(llama))}
load_timeout_secs=120
request_timeout_secs=180
host_timeout_secs=200
'''
    else:
        plugins += [ROOT / 'plugins/mock-backend', ROOT / 'tests/fixtures/mock-pack', ROOT / 'tests/fixtures/audio-pack']
        manifest = '''[plugin]
id="web-agent-backend"
version="1"
provides=["backend","specialist"]
[run]
command="lh-mock-backend"
[config]
scripted_chat=[{role="assistant",tool_calls=[{id="browser-write",type="function",function={name="write_file",arguments='{"path":"approved.txt","content":"Browser approved"}'}}]},{role="assistant",content="Tool run completed."}]
[[specialist]]
id="web-agent"
route="Agent approval fixture"
task="chat"
inputs=["text"]
outputs=["text","tool_calls"]
agent=true
tools=["tools-fs"]
backend="web-agent-backend"
model={source="file:mock"}
'''
        config = 'default_specialist="mock-text"\n'
    (pack / 'plugin.toml').write_text(manifest)
    config += f'\n[agent]\nmax_steps=5\napproval_timeout_secs=120\n[plugins.tools-fs.config]\nworkspace_root={json.dumps(temporary)}\n'
    (directory / 'config.toml').write_text(config)
    env = dict(os.environ, LH_PLUGIN_PATH=os.pathsep.join(map(str, plugins)), LH_SKILL_PATH=str(ROOT / 'skills'), LD_LIBRARY_PATH=str(llama.parent), PATH=str(ROOT / 'target/debug') + os.pathsep + os.environ.get('PATH', ''))
    child = subprocess.Popen([str(ROOT / 'target/debug/lh'), '--config', str(directory / 'config.toml'), 'serve', '--bind', f'127.0.0.1:{args.port}'], cwd=directory, env=env)
    print(f'Fixture workspace: {temporary}', flush=True)
    try:
        child.wait()
    except KeyboardInterrupt:
        pass
    finally:
        if child.poll() is None:
            child.send_signal(signal.SIGINT)
            try:
                child.wait(timeout=15)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        print('Load after:', Path('/proc/loadavg').read_text().strip(), flush=True)
        for file in directory.glob('*.txt'):
            print(f'Fixture result {file.name}: {file.read_text()}', flush=True)
