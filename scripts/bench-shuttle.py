#!/usr/bin/env python3
"""Download pinned public sensor data and run the real decision benchmark.

Source: UCI Statlog Shuttle, https://doi.org/10.24432/C5WS31, CC BY 4.0.
The official split has 43,500 training and 14,500 test examples. Its original
time ordering was randomized by Statlog; imposed reward delays are synthetic.
No input label is supplied to the decision context. Test feedback is disabled.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import platform
import subprocess
import urllib.request
import zipfile

ROOT = Path(__file__).resolve().parents[1]
URL = 'https://archive.ics.uci.edu/static/public/148/statlog+shuttle.zip'
HASHES = {
    'source.zip': 'a03e1f23755093eff5b8d9656f944ae3240a376065cf4360d090f5fe0aff9bef',
    'shuttle.trn': '87b24ee9fb5137e1d417659cf905d84d0e15342bbaa60770f1ae83da1a38200a',
    'shuttle.tst': 'f776934a628d9b94c482cb058a76ddaa63823e0f74aec065813c9dff3b89d661',
}


def verify(name, data):
    if hashlib.sha256(data).hexdigest() != HASHES[name]:
        raise ValueError(f'{name}: unexpected dataset content; refusing to run')


def quality_only(result):
    return {'baselines': result['baselines'], 'runs': [{k: v for k, v in r.items()
            if 'latency' not in k} for r in result['runs']]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, default=ROOT/'target/shuttle-benchmark')
    parser.add_argument('--no-build', action='store_true')
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    archive = args.out/'source.zip'
    if not archive.exists():
        with urllib.request.urlopen(URL, timeout=60) as response:
            data = response.read(2_000_001)
        if len(data) > 2_000_000:
            raise ValueError('download exceeds size bound')
        verify('source.zip', data)
        archive.write_bytes(data)
    data = archive.read_bytes()
    verify('source.zip', data)
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        for name in ('shuttle.trn', 'shuttle.tst'):
            member = name+'.Z' if name.endswith('trn') else name
            if z.getinfo(member).file_size > 2_000_000:
                raise ValueError('archive member exceeds size bound')
            content = z.read(member)
            if member.endswith('.Z'):
                content = subprocess.run(['gzip', '-dc'], input=content, capture_output=True, check=True).stdout
            verify(name, content)
            (args.out/name).write_bytes(content)
    if not args.no_build:
        subprocess.run(['cargo', 'build', '--locked', '--release', '--example', 'shuttle_decisions'], cwd=ROOT, check=True)
    command = [str(ROOT/'target/release/examples/shuttle_decisions'),
               str(args.out/'shuttle.trn'), str(args.out/'shuttle.tst')]
    first = json.loads(subprocess.check_output(command))
    second = json.loads(subprocess.check_output(command))
    if quality_only(first) != quality_only(second):
        raise AssertionError('same seed and dataset produced different predictions or scores')
    first['reproducibility'] = {'quality_identical_across_two_processes': True}
    first['provenance'] = {'url': URL, 'doi': '10.24432/C5WS31', 'license': 'CC BY 4.0', 'sha256': HASHES}
    first['environment'] = {'os': platform.system(), 'arch': platform.machine(),
                            'rustc': subprocess.check_output(['rustc', '--version'], text=True).strip()}
    (args.out/'results.json').write_text(json.dumps(first, indent=2)+'\n')
    majority = first['baselines']['majority']['accuracy']
    for row in first['runs']:
        held = row['held_out']
        print(f"features={row['features']} delay={row['delay_decisions']} seed={row['seed']}: "
              f"accuracy={held['accuracy']:.4%} balanced={held['balanced_accuracy']:.4%} "
              f"p99={row['held_out_decision_latency']['p99_us']:.3f}us "
              f"beats-majority={held['accuracy'] > majority}")
    print('Identical predictions and quality scores in two independent runs.')


if __name__ == '__main__':
    main()
