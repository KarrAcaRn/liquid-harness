"""Sequential CPU acceptance runs; never downloads weights or tunes held-out data."""
import hashlib
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
from smoke import ROOT, prepare, idle_host


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--only', choices=['tuning-baseline', 'tuning-cascade', 'heldout-baseline', 'heldout-cascade'])
    args = parser.parse_args()
    # Preserve the original M2 set. M2c requires actual media references, and
    # separates TTS from ASR. Only schema/fixture migration, no prompt edits.
    cases = [json.loads(line) for line in (ROOT / 'evals/routing.jsonl').read_text().splitlines()]
    for index, case in enumerate(cases, 1):
        if index in (27, 28, 29, 30):
            case['expected'] = 'lfm3-speak'
        if case['expected'] == 'lfm3-vl':
            case.setdefault('images', ['fixture.png'])
        if case['expected'] == 'lfm3-audio':
            case.setdefault('audio', ['fixture.wav'])
    tuning = ROOT / 'evals/routing-tuning.jsonl'
    tuning.write_text(''.join(json.dumps(c, ensure_ascii=False) + '\n' for c in cases))
    heldout = ROOT / 'evals/routing-heldout.jsonl'
    assert hashlib.sha256(heldout.read_bytes()).hexdigest() == 'd12f70a5fbaf7841d06f335733bbc861da3c493872871b9cc7cf1cec4565dfd2'
    os.environ['LH_BIN'] = str(ROOT / 'target/release/lh')
    results = ROOT / 'docs/measurements'
    with tempfile.TemporaryDirectory(prefix='lh-m2c-') as tmp:
        directory = Path(tmp)
        lh, env, _ = prepare(directory)
        for name, path in [('tuning', tuning), ('heldout', heldout)]:
            for mode in ['baseline', 'cascade']:
                if args.only and args.only != f'{name}-{mode}':
                    continue
                idle_host()
                command = [str(lh), 'eval', 'routing', '--file', str(path)]
                if mode == 'baseline':
                    command.append('--baseline')
                run = subprocess.run(command, cwd=directory, env=env, capture_output=True, text=True, timeout=300)
                (ROOT / 'target' / f'm2c-{name}-{mode}.log').write_text(run.stderr)
                if run.returncode:
                    raise RuntimeError(run.stderr + run.stdout)
                report = json.loads(run.stdout)
                (results / f'm2c-{name}-{mode}.json').write_text(json.dumps(report, indent=2) + '\n')
                print(name, mode, json.dumps({k: report[k] for k in ['correct', 'total', 'd1_call_rate', 'stage_counts', 'latency', 'loadavg']}), flush=True)
                idle_host()


if __name__ == '__main__':
    main()
