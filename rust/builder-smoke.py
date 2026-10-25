#!/usr/bin/env python3
"""Execute the builder's small Dockerfile examples against an extracted root.

Only FROM/RUN/CMD are supported; this is not an OCI image builder or checkpoint
replacement. Each Dockerfile gets a disposable root, retained between its steps.
"""
import argparse
import hashlib
import json
import shutil
import subprocess
import tempfile
from pathlib import Path

repo = Path(__file__).resolve().parent.parent
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--root', type=Path, required=True)
p.add_argument('--builder', type=Path, required=True)
p.add_argument('--filter', default='')
p.add_argument('--output', type=Path, default=repo/'target/builder-smoke')
a = p.parse_args()
a.output.mkdir(parents=True, exist_ok=True)
subprocess.run(['cargo','build','--offline','-p','blink-cli'],cwd=repo,check=True)
expected = {'00-hello': b'Hello, world!\n', '01-files': b'hello\n', '02-pipelines': b'apple\npear\n', '03-script': b'Hello, image-forge!\n', '04-archives': (hashlib.sha256(b'archive-test\n').hexdigest()+'  /work/restored/message\n').encode(), '05-environment': b'cwd=/ arch=x86_64\n'}
results = []
for name, final_output in expected.items():
    if a.filter not in name:
        continue
    dockerfile = a.builder/'examples/dockerfiles'/f'{name}.Dockerfile'
    with tempfile.TemporaryDirectory(prefix='blink-builder-') as temp:
        root = Path(temp)/'root'
        shutil.copytree(a.root, root, symlinks=True)
        steps = []
        for line in dockerfile.read_text().splitlines():
            if not line.strip() or line.startswith('#'): continue
            instruction, value = line.split(None,1)
            if instruction == 'FROM': continue
            if instruction == 'RUN': command = ['/bin/sh','-c',value]
            elif instruction == 'CMD': command = json.loads(value)
            else: raise ValueError(f'Unsupported Dockerfile instruction: {instruction}')
            try:
                run = subprocess.run([str(repo/'target/debug/blink-cli'),'--root',str(root),'--rootless-metadata','--timeout-ms','25000','--max-instructions','300000000']+command,capture_output=True,timeout=30)
            except subprocess.TimeoutExpired as error:
                run = subprocess.CompletedProcess(command,124,error.stdout or b'',(error.stderr or b'')+b'host timeout')
            passed = run.returncode == 0 and not run.stderr and (instruction != 'CMD' or run.stdout == final_output)
            steps.append(dict(instruction=instruction,command=command,exit=run.returncode,stdout=run.stdout.decode(errors='replace'),stderr=run.stderr.decode(errors='replace'),passed=passed))
            if not passed: break
        passed = all(s['passed'] for s in steps)
        row = dict(name=name,dockerfile_sha256=hashlib.sha256(dockerfile.read_bytes()).hexdigest(),passed=passed,steps=steps)
        results.append(row)
        print(f'{name:16} {"PASS" if passed else "FAIL"}: {steps[-1]}',flush=True)
(a.output/'results.json').write_text(json.dumps(results,indent=2)+'\n')
raise SystemExit(0 if results and all(r['passed'] for r in results) else 1)
