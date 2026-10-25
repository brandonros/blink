#!/usr/bin/env python3
"""Check rootless guest metadata across separate CLI runs and inode aliases."""
import argparse
import json
from smoke_support import copy_root
import subprocess
import tempfile
from pathlib import Path
repo = Path(__file__).resolve().parent.parent
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--root', type=Path, required=True)
a = p.parse_args()
subprocess.run(['cargo','build','--release','--offline','-p','blink-cli'],cwd=repo,check=True)
with tempfile.TemporaryDirectory(prefix='blink-rootless-') as temp:
    root = Path(temp)/'root'
    copy_root(a.root,root)
    prefix = [str(repo/'target/release/blink-cli'),'--root',str(root),'--rootless-metadata','/bin/sh','-c']
    scripts = [
        "mkdir /metadata && printf data > /metadata/file && chmod 6750 /metadata/file && chown 123:456 /metadata/file && ln /metadata/file /metadata/hard && mv /metadata/file /metadata/renamed && chmod 000 /metadata/renamed && stat -c '%u:%g:%a' /metadata/hard",
        "stat -c '%u:%g:%a' /metadata/renamed; cat /metadata/hard",
    ]
    expected = [b'123:456:0\n',b'123:456:0\ndata']
    owner = root.stat().st_uid
    results = []
    for script, output in zip(scripts,expected):
        run = subprocess.run(prefix+[script],capture_output=True,timeout=30)
        results.append(dict(exit=run.returncode,stdout=run.stdout.decode(errors='replace'),stderr=run.stderr.decode(errors='replace'),passed=run.returncode==0 and run.stdout==output and not run.stderr))
    assert (root/'metadata/hard').stat().st_uid == owner, 'host ownership changed'
    assert (root/'metadata/hard').stat().st_ino == (root/'metadata/renamed').stat().st_ino
    assert (root/'metadata/hard').stat().st_mode & 0o600 == 0o600, 'host access lost'
    print(json.dumps(results,indent=2))
    destination = repo/'target/rootless-smoke'
    destination.mkdir(parents=True,exist_ok=True)
    (destination/'results.json').write_text(json.dumps(results,indent=2)+'\n')
    raise SystemExit(0 if all(r['passed'] for r in results) else 1)
