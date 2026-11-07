"""Short sequential audio acceptance. No downloads; missing prerequisites skip."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import wave

from smoke import ROOT, cached, idle_host, prepare


def wer(reference, actual):
    """Word Levenshtein distance / reference length, case/punctuation ignored."""
    a, b = (re.findall(r"\w+", s.lower()) for s in (reference, actual))
    row = list(range(len(b) + 1))
    for i, word in enumerate(a, 1):
        next_row = [i]
        for j, other in enumerate(b, 1):
            next_row.append(min(next_row[-1] + 1, row[j] + 1, row[j-1] + (word != other)))
        row = next_row
    return {"word_errors": row[-1], "reference_words": len(a), "wer": row[-1] / max(1, len(a))}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--out', type=Path, default=ROOT / 'target/m3a-smoke')
    parser.add_argument('--sample', type=Path, default=os.environ.get('LH_ASR_SAMPLE'))
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    cache = Path(os.environ.get('LLAMA_CACHE', '/opt/data/models/llama-cache'))
    model = cached(cache, 'LFM3-Audio-2B-Q4_K_M.gguf')
    projector = cached(cache, 'mmproj-LFM3-Audio-2B-Q8_0.gguf')
    if not model or not projector:
        print('SKIP: cached audio Q4_K_M model / Q8_0 projector missing')
        return
    results = []
    with tempfile.TemporaryDirectory(prefix='lh-audio-smoke-') as tmp:
        directory = Path(tmp)
        try:
            lh, env, _ = prepare(directory)
        except FileNotFoundError as error:
            print('SKIP:', error)
            return
        pack_path = directory / 'plugins/lfm3/plugin.toml'
        pack = pack_path.read_text().replace('hf:LiquidAI/LFM3-Audio-2B-GGUF:Q4_K_M', 'file:' + str(model))
        pack = pack.replace('mmproj = "auto"', 'mmproj = ' + json.dumps(str(projector)))
        pack_path.write_text(pack)
        # Backend deadlines leave time for orderly cleanup before the outer guard.
        with (directory / 'config.toml').open('a') as config:
            config.write('\n[plugins.llamacpp.config]\nrequest_timeout_secs=120\nload_timeout_secs=30\nhost_timeout_secs=160\n')

        def run(label, *arguments):
            idle_host()
            if subprocess.run(['pgrep', '-x', 'llama-tts'], capture_output=True).returncode == 0:
                raise RuntimeError('another llama-tts is running')
            before = Path('/proc/loadavg').read_text().strip()
            start = time.monotonic()
            proc = subprocess.run([str(lh), *arguments], cwd=directory, env=env, text=True,
                                  capture_output=True, timeout=180)
            entry = {"operation": label, "seconds": round(time.monotonic() - start, 3),
                     "load_before": before, "load_after": Path('/proc/loadavg').read_text().strip(),
                     "stdout": proc.stdout.strip(), "returncode": proc.returncode}
            (out / (label + '.log')).write_text(proc.stderr)
            results.append(entry)
            (out / 'results.json').write_text(json.dumps(results, ensure_ascii=False, indent=2) + '\n')
            idle_host()
            if proc.returncode:
                raise RuntimeError(f'{label} failed; see {out / (label + ".log")}')
            return entry

        for lang, text in [('en', 'Hello world. This is a short test.'),
                           ('de', 'Guten Morgen. Das ist ein kurzer Test.')]:
            wav_path = out / (lang + '.wav')
            if wav_path.exists():
                raise FileExistsError(f'refusing to overwrite {wav_path}; choose a new --out directory')
            tts = run('tts-' + lang, 'run', '--speak-out', str(wav_path), '--language', lang, text)
            with wave.open(str(wav_path)) as audio:
                tts.update(wav=str(wav_path), sample_rate=audio.getframerate(),
                           duration_ms=round(1000 * audio.getnframes() / audio.getframerate()))
            asr = run('asr-' + lang, 'run', '--audio', str(wav_path), '--language', lang)
            asr.update(reference=text, **wer(text, asr['stdout']))
            print(json.dumps({'tts': tts, 'asr': asr}, ensure_ascii=False), flush=True)
            if not asr['stdout']:
                raise RuntimeError('empty transcript')
        if args.sample:
            sample = run('asr-sample', 'run', '--audio', str(args.sample.resolve()), '--language', 'en')
            sample.update(reference='And so my fellow Americans ask not what your country can do for you ask what you can do for your country')
            sample.update(wer(sample['reference'], sample['stdout']))
        else:
            print('SKIP: independent JFK sample (set LH_ASR_SAMPLE to local jfk.wav)')
        (out / 'results.json').write_text(json.dumps(results, ensure_ascii=False, indent=2) + '\n')
    print('PASS: audio TTS/ASR commands, nonempty WAV and transcripts; inspect WER for quality:', out)


if __name__ == '__main__':
    main()
