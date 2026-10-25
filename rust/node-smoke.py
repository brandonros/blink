#!/usr/bin/env python3
"""Pinned musl Node gates; the filesystem body matches the builder Node checkpoint."""
import argparse
import hashlib
import json
import subprocess
import time
from pathlib import Path

repo = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--root', required=True, type=Path)
args = parser.parse_args()
root = args.root.resolve()
subprocess.run(['cargo', 'build', '--release', '--offline', '-p', 'blink-cli'], cwd=repo, check=True)
prefix = [str(repo/'target/release/blink-cli'), '--root', str(root),
          '--max-memory', '536870912', '--max-image-size', '268435456',
          '--max-instructions', '1000000000', '--timeout-ms', '180000', '/usr/local/bin/node']
warning = b'Warning: disabling flag --expose_wasm due to conflicting flags\n'
filesystem = "const fs = require('node:fs'); fs.writeFileSync('/tmp/node-check.txt', 'node-ok'); console.log(fs.readFileSync('/tmp/node-check.txt', 'utf8'))"
cases = [('version', ['--version'], b'v22.23.4\n', b''),
         ('javascript-jitless', ['--jitless', '-p', '1 + 2'], b'3\n', warning),
         ('filesystem-jitless', ['--jitless', '-e', filesystem], b'node-ok\n', warning)]
results = []
digest = hashlib.sha256((root/'usr/local/bin/node').read_bytes()).hexdigest()
for name, command, expected, stderr in cases:
    start = time.monotonic()
    run = subprocess.run(prefix+command, capture_output=True, timeout=190)
    passed = run.returncode == 0 and run.stdout == expected and run.stderr == stderr
    if name == 'filesystem-jitless':
        path = root/'tmp/node-check.txt'
        passed = passed and path.is_file() and path.read_bytes() == b'node-ok'
    results.append(dict(case=name, args=command, exit=run.returncode,
                        stdout=run.stdout.decode(errors='replace'), stderr=run.stderr.decode(errors='replace'),
                        seconds=time.monotonic()-start, node_sha256=digest, passed=passed))
    print(f'{name}: {"PASS" if passed else "FAIL"} ({results[-1]["seconds"]:.2f}s)', flush=True)
output = repo/'target/node-smoke'
output.mkdir(parents=True, exist_ok=True)
(output/'results.json').write_text(json.dumps(results, indent=2)+'\n')
raise SystemExit(not all(row['passed'] for row in results))
