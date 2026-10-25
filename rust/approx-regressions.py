#!/usr/bin/env python3
"""Validate permitted RCP/RSQRT approximations, not processor-specific result bits."""
import argparse
import hashlib
import json
import math
import platform
import struct
import subprocess
from pathlib import Path

root = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser()
parser.add_argument('--replay', type=Path, help='Linux fixture and native.bin artifact directory')
args = parser.parse_args()
output = root / 'target/approx-regressions'
output.mkdir(parents=True, exist_ok=True)
subprocess.run(['cargo', 'build', '--release', '--offline', '-p', 'blink-cli'], cwd=root, check=True)
if args.replay:
    fixture = args.replay / 'approx-x86_64'
    native = (args.replay / 'native.bin').read_bytes()
else:
    assert (platform.system(), platform.machine()) == ('Linux', 'x86_64')
    fixture = output / 'approx-x86_64'
    subprocess.run(['cc', '-nostdlib', '-static', '-ffreestanding', '-fno-stack-protector', '-O2', '-msse2', '-mno-red-zone', '-Wl,--build-id=none', '-o', str(fixture), str(root / 'rust/fixtures/approx-x86_64.c')], check=True)
    run = subprocess.run([str(fixture)], capture_output=True, timeout=30)
    assert run.returncode == 0 and not run.stderr
    native = run.stdout
    (output / 'native.bin').write_bytes(native)
run = subprocess.run([str(root / 'target/release/blink-cli'), str(fixture)], capture_output=True, timeout=30)
assert run.returncode == 0 and not run.stderr, (run.returncode, run.stderr)
rust = run.stdout
(output / 'rust.bin').write_bytes(rust)
assert len(native) == len(rust) == 131072 * 64
errors = []
maximum = {}
def f32(bits):
    return struct.unpack('<f', struct.pack('<I', bits))[0]
for engine, data in [('native', native), ('rust', rust)]:
    worst = 0.0
    for offset in range(0, len(data), 64):
        record = struct.unpack_from('<16I', data, offset)
        inputs, results, previous = record[:4], record[4:8], record[8:12]
        control, status, op, pair = record[12:]
        if status != control:
            errors.append(dict(engine=engine, record=offset//64, reason='MXCSR changed'))
        for lane, (source, result) in enumerate(zip(inputs, results)):
            expected = None
            magnitude, sign = source & 0x7fffffff, source & 0x80000000
            if op in (0, 2) and lane != 0:
                expected = previous[lane]
            elif magnitude > 0x7f800000:
                expected = source | 0x400000
            elif magnitude < 0x800000:
                expected = sign | 0x7f800000
            elif op >= 2 and sign:
                expected = 0xffc00000
            elif magnitude == 0x7f800000:
                expected = sign
            if expected is not None:
                valid = result == expected
            else:
                reference = 1.0 / (math.sqrt(f32(source)) if op >= 2 else f32(source))
                actual = f32(result)
                # An approximation just below the minimum normal result may flush.
                if abs(reference) <= 2**-126 * (1 + 1.5 * 2**-12) and result == sign:
                    valid = True
                else:
                    error = abs((actual-reference)/reference)
                    valid = math.isfinite(actual) and error <= 1.5 * 2**-12
                    worst = max(worst, error)
            if not valid:
                errors.append(dict(engine=engine, record=offset//64, op=op, lane=lane, input=hex(source), result=hex(result), expected=hex(expected) if expected is not None else 'relative error <= 1.5*2^-12'))
    maximum[engine] = worst
# Input streams, controls and operation identities must match the native oracle.
for offset in range(0, len(native), 64):
    assert native[offset:offset+16] == rust[offset:offset+16]
    assert native[offset+32:offset+52] == rust[offset+32:offset+52]
    assert native[offset+56:offset+64] == rust[offset+56:offset+64]
report = dict(records=len(native)//64, errors=errors, maximum_relative_error=maximum,
              fixture_sha256=hashlib.sha256(fixture.read_bytes()).hexdigest(),
              native_sha256=hashlib.sha256(native).hexdigest(), rust_sha256=hashlib.sha256(rust).hexdigest())
(output / 'results.json').write_text(json.dumps(report, indent=2)+'\n')
print(f'{report["records"]} reciprocal records: {len(errors)} errors; maximum relative error {maximum}')
if errors:
    print(json.dumps(errors[:5], indent=2))
raise SystemExit(bool(errors))
