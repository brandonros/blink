#!/usr/bin/env python3
"""Compare fpexcept extended result bits and status flags against native Linux AMD64."""
import hashlib
import json
import platform
import subprocess
from pathlib import Path

root = Path(__file__).resolve().parent.parent
if (platform.system(), platform.machine()) != ('Linux', 'x86_64'):
    raise SystemExit('This oracle requires native Linux AMD64')
output = root / 'target/fpexcept-regressions'
output.mkdir(parents=True, exist_ok=True)
subprocess.run(['cargo', 'build', '--release', '--offline', '-p', 'blink-cli'], cwd=root, check=True)
fixture = output / 'fpexcept-x86_64'
subprocess.run(['cc', '-nostdlib', '-static', '-ffreestanding', '-fno-stack-protector', '-O2', '-mno-sse', '-mno-mmx', '-mno-red-zone', '-Wl,--build-id=none', '-o', str(fixture), str(root / 'rust/fixtures/fpexcept-x86_64.c')], check=True)
results = {}
for name, prefix in [('native', []), ('rust', [str(root / 'target/release/blink-cli')])]:
    run = subprocess.run(prefix + [str(fixture)], capture_output=True, timeout=30)
    (output / f'{name}.bin').write_bytes(run.stdout)
    results[name] = run
native, rust = results['native'], results['rust']
assert native.returncode == 0 and not native.stderr and len(native.stdout) > 0 and len(native.stdout) % 64 == 0, 'native oracle failed'
assert rust.returncode == 0 and not rust.stderr, f'Rust failed: {rust.returncode}, {rust.stderr!r}'
differences = []
for offset in range(0, max(len(native.stdout), len(rust.stdout)), 64):
    a, b = native.stdout[offset:offset+64], rust.stdout[offset:offset+64]
    if a != b:
        def record(data):
            return data.hex()
        differences.append(dict(record=offset//64, native=record(a), rust=record(b)))
report = dict(records=len(native.stdout)//64, fixture_sha256=hashlib.sha256(fixture.read_bytes()).hexdigest(), native_sha256=hashlib.sha256(native.stdout).hexdigest(), rust_sha256=hashlib.sha256(rust.stdout).hexdigest(), differences=differences)
(output / 'results.json').write_text(json.dumps(report, indent=2)+'\n')
print(f'{report["records"]} fpexcept records: {len(differences)} differences')
if differences:
    print(json.dumps(differences[:3], indent=2))
raise SystemExit(bool(differences))
