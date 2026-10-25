#!/usr/bin/env python3
"""Run real musl/BusyBox workloads in a disposable copy of an extracted AMD64 root.

No network downloads or host chroot; the supplied image must contain /bin/sh.
This is an early workload gate, not the linux-oci-builder checkpoint suite.
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

root = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--root', required=True, type=Path)
parser.add_argument('--c-blink', type=Path)
parser.add_argument('--output', type=Path, default=root/'target/alpine-smoke')
args = parser.parse_args()
args.output.mkdir(parents=True, exist_ok=True)
subprocess.run(['cargo', 'build', '--offline', '-p', 'blink-cli'], cwd=root, check=True)
cases = [
    ('echo', 'echo shell-hello', b'shell-hello\n'),
    ('pipeline', 'printf pipeline | /bin/cat; /bin/true; echo status:$?', b'pipelinestatus:0\n'),
    ('redirect', 'printf file-data > /tmp/rust-smoke; printf append >> /tmp/rust-smoke; read value < /tmp/rust-smoke; echo "$value"', b'file-dataappend\n'),
    ('subshell', 'value=$(printf substitution); (echo "$value"); /bin/false; echo status:$?', b'substitution\nstatus:1\n'),
]
results = []
for engine in ['rust'] + (['c-interpreter'] if args.c_blink else []):
    with tempfile.TemporaryDirectory(prefix='blink-alpine-') as temp:
        guest = Path(temp)/'root'
        shutil.copytree(args.root, guest, symlinks=True)
        prefix = [str(root/'target/debug/blink-cli'), '--root', str(guest)] if engine == 'rust' else [str(args.c_blink.resolve()), '-m', '-j']
        for name, script, expected in cases:
            command = prefix + ['/bin/sh', '-c', script]
            try:
                run = subprocess.run(command, capture_output=True, timeout=30, env={**os.environ, "BLINK_PREFIX": str(guest)} if engine == "c-interpreter" else None)
            except subprocess.TimeoutExpired as error:
                run = subprocess.CompletedProcess(command, 124, error.stdout or b'', (error.stderr or b'')+b'timeout')
            passed = run.returncode == 0 and run.stdout == expected and not run.stderr
            if name == 'redirect':
                passed &= (guest/'tmp/rust-smoke').read_bytes() == b'file-dataappend' if (guest/'tmp/rust-smoke').exists() else False
            row = dict(engine=engine, case=name, passed=passed, exit=run.returncode, stdout=run.stdout.decode(errors='replace'), stderr=run.stderr.decode(errors='replace'))
            results.append(row)
            print(f'{engine:14} {name:12} {"PASS" if passed else "FAIL"} {run.stdout!r} {run.stderr!r}')
report = dict(busybox_sha256=hashlib.sha256((args.root/'bin/busybox').read_bytes()).hexdigest(), results=results)
(args.output/'results.json').write_text(json.dumps(report, indent=2)+'\n')
raise SystemExit(0 if all(r['passed'] for r in results) else 1)
