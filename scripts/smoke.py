"""Offline smoke fixture preparation, shared by local acceptance measurements."""
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tempfile
import zlib

ROOT = Path(__file__).resolve().parents[1]


def idle_host():
    processes = subprocess.run(['pgrep', '-ax', 'llama-server'], capture_output=True, text=True)
    if processes.returncode == 0:
        raise RuntimeError('another llama-server is running: ' + processes.stdout.strip())
    print('loadavg:', Path('/proc/loadavg').read_text().strip(), flush=True)


def cached(cache, name):
    matches = sorted(cache.rglob('*' + name))
    return next((p.resolve() for p in matches if p.is_file()), None)


def prepare(directory):
    """Create isolated, file-source manifests; never download during smoke."""
    directory = Path(directory)
    cache = Path(os.environ.get('LLAMA_CACHE', '/opt/data/models/llama-cache'))
    server = os.environ.get('LLAMA_SERVER', shutil.which('llama-server') or '/opt/data/tools/llama.cpp/llama-b12189/llama-server')
    lh = Path(os.environ.get('LH_BIN', ROOT / 'target/debug/lh')).resolve()
    backend = lh.parent / 'lh-backend-llamacpp'
    models = {name: cached(cache, name) for name in [
        'd1-2B-Q4_0.gguf', 'LFM3-1.7B-Q4_K_M.gguf',
        'LFM3-VL-2B-Q4_K_M.gguf', 'mmproj-LFM3-VL-2B-F16.gguf']}
    if not Path(server).is_file() or not lh.is_file() or not backend.is_file() or not all(models.values()):
        raise FileNotFoundError('llama-server, lh/backend binaries or cached Q4 models/projector missing')
    pack = (ROOT / 'plugins/lfm3/plugin.toml').read_text()
    for repo, name in [('d1-2B', 'd1-2B-Q4_0.gguf'), ('LFM3-1.7B', 'LFM3-1.7B-Q4_K_M.gguf'), ('LFM3-VL-2B', 'LFM3-VL-2B-Q4_K_M.gguf')]:
        pack = re.sub('hf:LiquidAI/' + re.escape(repo) + '-GGUF:[A-Z0-9_]+', 'file:' + str(models[name]), pack)
    pack = pack.replace('mmproj = "auto"', 'mmproj = ' + json.dumps(str(models['mmproj-LFM3-VL-2B-F16.gguf'])))
    for name, text in [('lfm3', pack), ('backend', (ROOT / 'plugins/backend-llamacpp/plugin.toml').read_text().replace('command = "lh-backend-llamacpp"', 'command = ' + json.dumps(str(backend))).replace('llama_server = "llama-server"', 'llama_server = ' + json.dumps(server)))]:
        dest = directory / 'plugins' / name
        dest.mkdir(parents=True)
        (dest / 'plugin.toml').write_text(text)
    config = directory / 'config.toml'
    config.write_text('router = "d1"\ndefault_specialist = "lfm3-text"\n')
    env = dict(os.environ, LH_PLUGIN_PATH='', LH_CONFIG=str(config), XDG_CONFIG_HOME=str(directory))
    env['LD_LIBRARY_PATH'] = str(Path(server).parent) + ':' + env.get('LD_LIBRARY_PATH', '')
    return lh, env, models


def image(path):
    # A small, deterministic real PNG: red square on a white background.
    def chunk(kind, data):
        return struct.pack('!I', len(data)) + kind + data + struct.pack('!I', zlib.crc32(kind + data))
    pixels = b''.join(b'\0' + b''.join(b'\xff\0\0' if 16 <= x < 48 and 16 <= y < 48 else b'\xff\xff\xff' for x in range(64)) for y in range(64))
    path.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('!2I5B', 64, 64, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(pixels)) + chunk(b'IEND', b''))


def main():
    with tempfile.TemporaryDirectory(prefix='lh-smoke-') as tmp:
        directory = Path(tmp)
        try:
            lh, env, _ = prepare(directory)
        except FileNotFoundError as error:
            print('SKIP:', error)
            return
        idle_host()
        def run(*args):
            # Each lh process shuts down its backend before the next one starts.
            result = subprocess.run([str(lh), *args], cwd=directory, env=env, text=True, capture_output=True, timeout=120)
            if result.returncode:
                raise RuntimeError(result.stderr)
            print(result.stdout.strip(), flush=True)
            return result.stdout
        decision = json.loads(run('route', 'Say hello in one short sentence.'))
        assert decision['specialist'] == 'lfm3-text', decision
        assert run('run', '--model', decision['specialist'], 'Say hello in one short sentence.').strip()
        picture = directory / 'square.png'
        image(picture)
        assert run('run', '--image', str(picture), 'Describe the colored shape in one sentence.').strip()
        idle_host()
        print('PASS: d1 routing, streamed text and image answers; servers shut down.')


if __name__ == '__main__':
    main()
