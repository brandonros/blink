#!/usr/bin/env python3
"""Validate approximate x87 transcendentals with native state and an independent 200-bit oracle."""
import argparse
import hashlib
import json
import platform
import struct
import subprocess
from pathlib import Path
import mpmath as mp

repo = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--replay', type=Path)
args = parser.parse_args()
output = args.replay or repo/'target/transcend-regressions'
output.mkdir(parents=True, exist_ok=True)
subprocess.run(['cargo', 'build', '--release', '--offline', '-p', 'blink-cli'], cwd=repo, check=True)
fixture = output/'transcend-x86_64'
if not args.replay:
    assert (platform.system(), platform.machine()) == ('Linux', 'x86_64')
    subprocess.run(['cc', '-nostdlib', '-static', '-ffreestanding', '-fno-stack-protector', '-mno-sse', '-mno-mmx', '-mno-red-zone', '-O2', '-Wl,--build-id=none', '-o', str(fixture), str(repo/'rust/fixtures/transcend-x86_64.c')], check=True)
    native = subprocess.run([str(fixture.resolve())], capture_output=True, timeout=60)
    assert native.returncode == 0 and not native.stderr
    (output/'native.bin').write_bytes(native.stdout)
native = (output/'native.bin').read_bytes()
rust = subprocess.run([str(repo/'target/release/blink-cli'), '--max-instructions', '500000000', str(fixture.resolve())], capture_output=True, timeout=120)
assert rust.returncode == 0 and not rust.stderr, (rust.returncode, rust.stderr)
(output/'rust.bin').write_bytes(rust.stdout)
assert len(native) == len(rust.stdout) and len(native) % 40 == 0
mp.mp.prec = 200
largest_extended = (mp.mpf(2)-mp.power(2, -63))*mp.power(2, 16383)

def decode(data):
    fraction, raw_exp = struct.unpack('<QH', data)
    exponent = raw_exp & 0x7fff
    if exponent and not fraction & (1 << 63):
        return None
    sign = -1 if raw_exp & 0x8000 else 1
    if exponent == 0x7fff:
        return sign*mp.inf if fraction == 1 << 63 else None
    return sign*mp.mpf(fraction)*mp.power(2, max(exponent, 1)-16383-63)

errors = []
max_error = {'native': mp.mpf(0), 'rust': mp.mpf(0)}
last_bit_differences = 0
for index in range(len(native)//40):
    a, b = native[index*40:(index+1)*40], rust.stdout[index*40:(index+1)*40]
    assert a[:20] == b[:20] and a[32:] == b[32:], f'input/record mismatch {index}'
    # C1 describes rounding within each implementation's approximation. Other
    # status bits, stack effects and exceptions must agree with the native run.
    native_sw, rust_sw = struct.unpack('<H', a[30:32])[0], struct.unpack('<H', b[30:32])[0]
    if (native_sw ^ rust_sw) & ~0x200:
        errors.append(dict(record=index, reason='status', native=hex(native_sw), rust=hex(rust_sw)))
    x, y = decode(a[:10]), decode(a[10:20])
    operation = struct.unpack('<H', a[34:36])[0]
    if x is None or (operation != 1 and y is None):
        if a[20:30] != b[20:30]:
            errors.append(dict(record=index, reason='special result'))
        continue
    # mpmath does not preserve signed zero. Native comparison covers its sign
    # and the quadrant cases whose input contains zero or infinity.
    if not x or mp.isinf(x) or (operation != 1 and (not y or mp.isinf(y))):
        if a[20:30] != b[20:30]:
            errors.append(dict(record=index, reason='zero/infinity quadrant'))
        continue
    if (operation == 2 and x < 0) or (operation == 3 and x <= -1):
        if a[20:30] != b[20:30]:
            errors.append(dict(record=index, reason='invalid logarithm'))
        continue
    reference = (mp.atan2(y, x) if operation == 0 else mp.expm1(x*mp.ln(2)) if operation == 1
                 else y*mp.log(x, 2) if operation == 2 else y*mp.log1p(x)/mp.ln(2))
    if not reference and a[20:30] != b[20:30]:
        errors.append(dict(record=index, reason='exact zero'))
    if abs(reference) > largest_extended:
        if a[20:30] != b[20:30]:
            errors.append(dict(record=index, reason='overflow result'))
        continue
    if a[20:30] != b[20:30]:
        last_bit_differences += 1
    for engine, data in [('native', a), ('rust', b)]:
        actual = decode(data[20:30])
        if actual is None:
            errors.append(dict(record=index, reason=engine+' nonfinite result'))
            continue
        # One extended-precision ulp at the exact result, with subnormal spacing.
        exponent = int(mp.floor(mp.log(abs(reference), 2))) if reference else -16382
        ulp = mp.power(2, max(exponent, -16382)-63)
        error = abs(actual-reference)/ulp
        max_error[engine] = max(max_error[engine], error)
        # Directed rounding adds up to one ulp to the approximation error;
        # nearest rounding adds at most half an ulp. Test both implementations
        # against the same explicit regression bound, not against each other.
        rounding = (struct.unpack('<H', data[32:34])[0] >> 10) & 3
        limit = mp.mpf(1) if rounding == 0 else mp.mpf('1.5')
        if error > limit:
            errors.append(dict(record=index, reason=engine+' accuracy', ulps=float(error)))
report = dict(records=len(native)//40, fixture_sha256=hashlib.sha256(fixture.read_bytes()).hexdigest(),
              last_bit_differences=last_bit_differences, max_error_ulps={k:float(v) for k,v in max_error.items()}, differences=errors)
(output/'results.json').write_text(json.dumps(report, indent=2)+'\n')
print(json.dumps({k:v for k,v in report.items() if k!='differences'}))
if errors:
    print(json.dumps(errors[:5], indent=2))
raise SystemExit(bool(errors))
