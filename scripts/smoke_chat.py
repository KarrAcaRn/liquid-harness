"""Bounded sequential M3b acceptance using cached weights and real curl requests."""
import base64
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
import wave

from smoke import ROOT, cached, idle_host, image, prepare


def main():
    out = ROOT / 'target/m3b-smoke'
    out.mkdir(parents=True, exist_ok=True)
    results = []
    idle_host()
    if subprocess.run(['pgrep', '-x', 'llama-tts'], capture_output=True).returncode == 0:
        raise RuntimeError('another llama-tts is running')
    with tempfile.TemporaryDirectory(prefix='lh-chat-smoke-') as tmp:
        directory = Path(tmp)
        lh, env, _ = prepare(directory)
        cache = Path(os.environ.get('LLAMA_CACHE', '/opt/data/models/llama-cache'))
        model = cached(cache, 'LFM3-Audio-2B-Q4_K_M.gguf')
        projector = cached(cache, 'mmproj-LFM3-Audio-2B-Q8_0.gguf')
        if not model or not projector:
            raise FileNotFoundError('audio cached weights/projector missing')
        pack_path = directory / 'plugins/lfm3/plugin.toml'
        pack = pack_path.read_text().replace('hf:LiquidAI/LFM3-Audio-2B-GGUF:Q4_K_M', 'file:' + str(model))
        pack = pack.replace('mmproj = "auto"', 'mmproj = ' + json.dumps(str(projector)))
        pack_path.write_text(pack)
        with (directory / 'config.toml').open('a') as config:
            config.write('\n[plugins.llamacpp.config]\nrequest_timeout_secs=90\nload_timeout_secs=30\nhost_timeout_secs=130\n')
        picture = out / 'square.png'
        image(picture)
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            port = sock.getsockname()[1]
        base = f'http://127.0.0.1:{port}/v1'
        log = (out / 'serve.log').open('w')
        proc = subprocess.Popen([str(lh), 'serve', '--bind', f'127.0.0.1:{port}'], cwd=directory, env=env, stdout=log, stderr=log)
        try:
            for _ in range(100):
                try:
                    with socket.create_connection(('127.0.0.1', port), timeout=.1):
                        break
                except OSError:
                    if proc.poll() is not None:
                        raise RuntimeError('serve exited')
                    time.sleep(.05)
            def request(label, endpoint, payload=None, form=None):
                body = out / (label + '.body')
                header = out / (label + '.headers')
                args = ['curl', '--silent', '--show-error', '--fail-with-body', '--max-time', '150', '-D', str(header), '-o', str(body)]
                if payload is not None:
                    path = out / (label + '.request.json')
                    path.write_text(json.dumps(payload))
                    args += ['-H', 'Content-Type: application/json', '--data-binary', '@' + str(path)]
                if form:
                    args += ['-F', 'file=@' + str(form), '-F', 'model=auto', '-F', 'language=en']
                args += [base + endpoint]
                before = Path('/proc/loadavg').read_text().strip()
                start = time.monotonic()
                done = subprocess.run(args, capture_output=True, text=True, timeout=155)
                entry = dict(operation=label, command=args, seconds=round(time.monotonic()-start,3), load_before=before, load_after=Path('/proc/loadavg').read_text().strip(), returncode=done.returncode, headers=header.read_text())
                if label != 'speech':
                    entry['response'] = body.read_text()
                else:
                    with wave.open(str(body)) as wav:
                        entry.update(sample_rate=wav.getframerate(), duration_ms=round(wav.getnframes()*1000/wav.getframerate()))
                results.append(entry)
                (out / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
                print(json.dumps({k:v for k,v in entry.items() if k not in ('response','command')}), flush=True)
                if done.returncode:
                    raise RuntimeError(done.stderr or entry.get('response'))
                return body
            text_body = request('text','/chat/completions',dict(model='auto',stream=True,max_tokens=40,messages=[dict(role='user',content='Explain a Rust mutex in one short sentence.')]))
            chunks = [json.loads(line[6:]) for line in text_body.read_text().splitlines() if line.startswith('data: {')]
            assert ''.join(c.get('choices',[{}])[0].get('delta',{}).get('content') or '' for c in chunks)
            request('image','/chat/completions',dict(model='auto',max_tokens=50,messages=[dict(role='user',content=[dict(type='text',text='Describe the colored shape in one sentence.'),dict(type='image_url',image_url=dict(url='data:image/png;base64,'+base64.b64encode(picture.read_bytes()).decode()))])]))
            recording = request('speech','/audio/speech',dict(model='auto',input='Hello world. This is a short test.',voice='female',language='en',response_format='wav'))
            request('transcription','/audio/transcriptions',form=recording)
            request('audio-chat','/chat/completions',dict(model='auto',max_tokens=50,messages=[dict(role='user',content=[dict(type='text',text='Repeat what the speaker said, in one sentence.'),dict(type='input_audio',input_audio=dict(format='wav',data=base64.b64encode(recording.read_bytes()).decode()))])]))
        finally:
            if proc.poll() is None:
                proc.send_signal(signal.SIGINT)
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
            log.close()
        idle_host()
        script = f'Explain a Rust mutex in one short sentence.\n/image {picture}\nDescribe the colored shape in one sentence.\nWhat color was it? Answer briefly.\n/explain\n/quit\n'
        start = time.monotonic()
        before = Path('/proc/loadavg').read_text().strip()
        done = subprocess.run([str(lh),'chat'],input=script,cwd=directory,env=env,capture_output=True,text=True,timeout=180)
        (out / 'chat.log').write_text(done.stderr)
        routes = [json.loads(line[len('[route] '):]) for line in done.stderr.splitlines() if line.startswith('[route] {')]
        entry = dict(operation='chat-three-turns',script=script,seconds=round(time.monotonic()-start,3),load_before=before,load_after=Path('/proc/loadavg').read_text().strip(),returncode=done.returncode,stdout=done.stdout,routes=routes)
        results.append(entry)
        (out / 'results.json').write_text(json.dumps(results,indent=2)+'\n')
        print(json.dumps(entry),flush=True)
        assert done.returncode == 0 and len(routes)==3, done.stderr
        assert [r['specialist'] for r in routes] == ['lfm3-text','lfm3-vl','lfm3-vl']
    idle_host()
    assert subprocess.run(['pgrep','-x','llama-tts'],capture_output=True).returncode != 0
    print('PASS: serial HTTP text/image/audio/TTS and three-turn REPL; processes cleaned up.')


if __name__ == '__main__':
    main()
