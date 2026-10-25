#!/usr/bin/env python3
"""Run offline APK startup, database reads and an existing-package world update.

This does not download or install a new package. The extracted Alpine root is
copied before any writes. Network package installation remains a separate gate.
"""
import argparse
import hashlib
import json
import shutil
import subprocess
import tempfile
from pathlib import Path
repo=Path(__file__).resolve().parent.parent
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--root',type=Path,required=True)
a=p.parse_args()
subprocess.run(['cargo','build','--release','--offline','-p','blink-cli'],cwd=repo,check=True)
results=[]
with tempfile.TemporaryDirectory(prefix='blink-apk-') as temp:
    root=Path(temp)/'root'
    shutil.copytree(a.root,root,symlinks=True)
    installed={line[2:] for line in (root/'lib/apk/db/installed').read_text().splitlines() if line.startswith('P:')}
    before=(root/'etc/apk/world').read_text().splitlines()
    candidates=sorted(installed-set(before))
    assert candidates, 'fixture needs an installed dependency absent from world'
    dependency='zlib' if 'zlib' in candidates else candidates[0]
    prefix=[str(repo/'target/release/blink-cli'),'--root',str(root),'--rootless-metadata','--timeout-ms','30000','/sbin/apk','--repositories-file','/dev/null']
    for name,args in [('version',['--version']),('info',['info']),('simulate',['add','--no-network','--simulate',dependency]),('world-update',['add','--no-network',dependency])]:
        run=subprocess.run(prefix+args,capture_output=True,timeout=35)
        passed=run.returncode==0 and not run.stderr
        if name=='version':passed &= run.stdout.startswith(b'apk-tools ')
        if name=='info':passed &= set(run.stdout.decode().splitlines())==installed
        if name=='simulate':passed &= (root/'etc/apk/world').read_text().splitlines()==before
        if name=='world-update':passed &= dependency in (root/'etc/apk/world').read_text().splitlines()
        results.append(dict(name=name,passed=passed,exit=run.returncode,stdout=run.stdout.decode(errors='replace'),stderr=run.stderr.decode(errors='replace')))
        print(f'{name}: {"PASS" if passed else "FAIL"}',flush=True)
        if not passed:break
output=repo/'target/apk-smoke'
output.mkdir(parents=True,exist_ok=True)
report=dict(apk_sha256=hashlib.sha256((a.root/'sbin/apk').read_bytes()).hexdigest(),results=results)
(output/'results.json').write_text(json.dumps(report,indent=2)+'\n')
raise SystemExit(0 if len(results)==4 and all(r['passed'] for r in results) else 1)
