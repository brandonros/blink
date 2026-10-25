#!/usr/bin/env python3
"""Exercise offline APK reads, world updates, local installation and removal.

The new test package is constructed locally, unsigned, with APKv2 payload hashes.
--allow-untrusted applies only to that explicit fixture; --force-non-repository
permits its installation in the disposable root without a persistent APK cache. No repository or network
package is fetched. Each run works in a disposable copy of the supplied root.
"""
import argparse
import gzip
import hashlib
import io
import json
import shutil
import subprocess
import tarfile
import tempfile
from pathlib import Path

repo = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--root', type=Path, required=True)
args = parser.parse_args()
subprocess.run(['cargo', 'build', '--release', '--offline', '-p', 'blink-cli'], cwd=repo, check=True)

def archive(entries, control=False):
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode='w', format=tarfile.PAX_FORMAT) as tar:
        for name, data, mode, uid, gid in entries:
            item = tarfile.TarInfo(name)
            item.mode, item.uid, item.gid, item.mtime = mode, uid, gid, 1
            if data is None:
                item.type = tarfile.DIRTYPE
            else:
                item.size = len(data)
                if not control:
                    item.pax_headers = {'APK-TOOLS.checksum.SHA1': hashlib.sha1(data).hexdigest()}
            tar.addfile(item, None if data is None else io.BytesIO(data))
        length = tar.offset
    # APKv2 has separate gzip members but one continuous tar stream: the control
    # member must not contain the end-of-archive blocks preceding the data member.
    return gzip.compress(buffer.getvalue()[:length] if control else buffer.getvalue(), mtime=0)

def package():
    data = archive([
        ('usr', None, 0o755, 0, 0),
        ('usr/bin', None, 0o755, 0, 0),
        ('usr/bin/blink-local-smoke', b'#!/bin/sh\necho local-package-ran\n', 0o755, 0, 0),
        ('usr/share', None, 0o755, 0, 0),
        ('usr/share/blink-smoke', None, 0o755, 0, 0),
        ('usr/share/blink-smoke/message', b'installed by Rust guest\n', 0o640, 123, 456),
    ])
    info = f'''pkgname = blink-local-smoke
pkgver = 1.0-r0
pkgdesc = Local Rust runtime installation fixture
url = https://github.com/brandonros/blink
builddate = 1
packager = blink runtime test
size = 8192
arch = x86_64
origin = blink-local-smoke
license = ISC
datahash = {hashlib.sha256(data).hexdigest()}
'''.encode()
    control = archive([
        ('.PKGINFO', info, 0o644, 0, 0),
        ('.post-install', b'#!/bin/sh\nprintf "install script ran\\n" > /etc/blink-local-smoke\n', 0o755, 0, 0),
    ], control=True)
    return control + data

results = []
with tempfile.TemporaryDirectory(prefix='blink-apk-') as temporary:
    root = Path(temporary) / 'root'
    shutil.copytree(args.root, root, symlinks=True)
    installed = {line[2:] for line in (root / 'lib/apk/db/installed').read_text().splitlines() if line.startswith('P:')}
    before = (root / 'etc/apk/world').read_text().splitlines()
    candidates = sorted(installed - set(before))
    assert candidates, 'fixture needs an installed dependency absent from world'
    dependency = 'zlib' if 'zlib' in candidates else candidates[0]
    guest_package = '/tmp/blink-local-smoke-1.0-r0.apk'
    (root / guest_package.lstrip('/')).write_bytes(package())
    prefix = [str(repo / 'target/release/blink-cli'), '--root', str(root), '--rootless-metadata', '--timeout-ms', '30000']
    apk = ['/sbin/apk', '--repositories-file', '/dev/null']
    steps = [
        ('version', apk + ['--version']),
        ('info', apk + ['info']),
        ('simulate', apk + ['add', '--no-network', '--simulate', dependency]),
        ('world-update', apk + ['add', '--no-network', dependency]),
        ('local-install', apk + ['add', '--no-network', '--allow-untrusted', '--force-non-repository', guest_package]),
        ('installed-execution', ['/bin/sh', '-c', "/usr/bin/blink-local-smoke && cat /etc/blink-local-smoke /usr/share/blink-smoke/message && stat -c '%u:%g:%a:%Y' /usr/share/blink-smoke/message"]),
        ('local-remove', apk + ['del', '--no-network', 'blink-local-smoke']),
    ]
    for name, command in steps:
        run = subprocess.run(prefix + command, capture_output=True, timeout=35)
        passed = run.returncode == 0 and not run.stderr
        if name == 'version':
            passed &= run.stdout.startswith(b'apk-tools ')
        if name == 'info':
            passed &= set(run.stdout.decode().splitlines()) == installed
        if name == 'simulate':
            passed &= (root / 'etc/apk/world').read_text().splitlines() == before
        if name == 'world-update':
            passed &= dependency in (root / 'etc/apk/world').read_text().splitlines()
        if name == 'local-install':
            database = (root / 'lib/apk/db/installed').read_text()
            entry = next((e for e in database.split('\n\n') if '\nP:blink-local-smoke\n' in '\n'+e+'\n'), '')
            passed &= bool(entry) and '\nf:' not in entry
            file = root / 'usr/share/blink-smoke/message'
            passed &= file.is_file() and file.stat().st_uid == root.stat().st_uid
        if name == 'installed-execution':
            passed &= run.stdout == b'local-package-ran\ninstall script ran\ninstalled by Rust guest\n123:456:640:1\n'
        if name == 'local-remove':
            passed &= not (root / 'usr/share/blink-smoke/message').exists()
            passed &= not (root / 'usr/bin/blink-local-smoke').exists()
            passed &= 'P:blink-local-smoke\n' not in (root / 'lib/apk/db/installed').read_text()
        results.append(dict(name=name, passed=passed, exit=run.returncode, stdout=run.stdout.decode(errors='replace'), stderr=run.stderr.decode(errors='replace')))
        print(f'{name}: {"PASS" if passed else "FAIL"}', flush=True)
        if not passed:
            break
output = repo / 'target/apk-smoke'
output.mkdir(parents=True, exist_ok=True)
report = dict(apk_sha256=hashlib.sha256((args.root / 'sbin/apk').read_bytes()).hexdigest(), local_package_sha256=hashlib.sha256(package()).hexdigest(), results=results)
(output / 'results.json').write_text(json.dumps(report, indent=2)+'\n')
raise SystemExit(0 if len(results) == len(steps) and all(r['passed'] for r in results) else 1)
