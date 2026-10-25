#!/usr/bin/env python3
"""Run pinned Debian/glibc shell, coreutils and GNU tar through the Rust runtime."""
import argparse
import json
import subprocess
import tempfile
from pathlib import Path
from smoke_support import copy_root

repo = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--root', type=Path, required=True)
args = parser.parse_args()
output = repo/'target/debian-smoke'
output.mkdir(parents=True, exist_ok=True)
subprocess.run(['cargo', 'build', '--release', '--offline', '-p', 'blink-cli'], cwd=repo, check=True)
cases = [
    ('glibc', ['/usr/bin/getconf', 'GNU_LIBC_VERSION'], b'glibc 2.36\n'),
    ('shell-exec-wait', ['/bin/sh', '-c', '/bin/echo glibc-ok; /bin/true; /bin/false; test "$?" = 1'], b'glibc-ok\n'),
    ('pipeline', ['/bin/sh', '-c', "printf 'pear\\napple\\npear\\n' | sort | uniq"], b'apple\npear\n'),
    ('float-format', ['/usr/bin/printf', '%.8f\n', '0.1'], b'0.10000000\n'),
    ('metadata', ['/bin/sh', '-ec', 'mkdir -p /work; printf "archive-test\\n" > /work/message; chown 123:456 /work/message; chmod 400 /work/message; stat -c "%u:%g:%a" /work/message'], b'123:456:400\n'),
    ('gnu-tar', ['/bin/sh', '-ec', 'tar --format=posix --numeric-owner --sort=name -cf /work/archive.tar -C /work message; mkdir /work/restored; tar -xpf /work/archive.tar -C /work/restored; stat -c "%u:%g:%a" /work/restored/message; cat /work/restored/message'], b'123:456:400\narchive-test\n'),
]
rows = []
with tempfile.TemporaryDirectory(prefix='blink-debian-') as temporary:
    root = Path(temporary)/'root'
    copy_root(args.root, root)
    for name, argv, expected in cases:
        run = subprocess.run([str(repo/'target/release/blink-cli'), '--root', str(root), '--rootless-metadata', '--env', 'PATH=/usr/bin:/bin', '--env', 'LC_ALL=C', '--timeout-ms', '30000', '--max-instructions', '300000000', *argv], capture_output=True, timeout=40)
        passed = run.returncode == 0 and run.stdout == expected and not run.stderr
        row = dict(name=name, argv=argv, exit=run.returncode, stdout=run.stdout.decode(errors='replace'), stderr=run.stderr.decode(errors='replace'), passed=passed)
        rows.append(row)
        print(json.dumps(row), flush=True)
        if not passed:
            break
(output/'results.json').write_text(json.dumps(rows, indent=2)+'\n')
raise SystemExit(0 if len(rows)==len(cases) and all(row['passed'] for row in rows) else 1)
