#!/usr/bin/env python3
"""Exercise persistent stage contexts through the trusted worker protocol."""
import argparse
import json
import shutil
import subprocess
import tempfile
from pathlib import Path

repo = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--root', type=Path, required=True)
args = parser.parse_args()
subprocess.run(['cargo', 'build', '--release', '--offline', '-p', 'blink-cli'], cwd=repo, check=True)
with tempfile.TemporaryDirectory(prefix='blink-worker-') as directory:
    base = Path(directory)
    for context in ['one', 'two']:
        shutil.copytree(args.root, base/context, symlinks=True)
    worker = subprocess.Popen([str(repo/'target/release/blink-worker')], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    sequence = 0
    records = []
    def request(message):
        worker.stdin.write(json.dumps(message)+'\n')
        worker.stdin.flush()
        reply = worker.stdout.readline()
        if not reply:
            raise RuntimeError(f'worker exited: {worker.stderr.read()}')
        result = json.loads(reply)
        records.append(dict(request=message, reply=result))
        return result
    def run(context, script, expected=b'', termination='exited', code=0, timeout=10000, uid=0, gid=0):
        global sequence
        sequence += 1
        stdout, stderr = base/f'{sequence}.out', base/f'{sequence}.err'
        result = request(dict(op='run', context=context, argv=['/bin/sh', '-c', script],
                              cwd='/', env={'PATH':'/bin:/usr/bin'}, uid=uid, gid=gid,
                              timeout_ms=timeout, stdout=str(stdout), stderr=str(stderr)))
        assert result.get('ok') and result['termination'] == termination and result['code'] == code, result
        assert stdout.read_bytes() == expected, stdout.read_bytes()
        assert stderr.read_bytes() == b'', stderr.read_bytes()
    try:
        for context in ['one', 'two']:
            assert request(dict(op='create', context=context, rootfs=str(base/context), rootless_metadata=True))['ok']
        assert not request(dict(op='create', context='one', rootfs=str(base/'one')))['ok']
        run('one', 'printf stage-one > /tmp/stage.txt; chmod 000 /tmp/stage.txt; chown 123:456 /tmp/stage.txt')
        run('one', "cat /tmp/stage.txt; stat -c '%u:%g:%a' /tmp/stage.txt", b'stage-one123:456:0\n')
        run('two', 'test ! -e /tmp/stage.txt; id -u; id -g', b'17\n19\n', uid=17, gid=19)
        run('one', 'id -u; id -g; exit 7', b'0\n0\n', code=7)
        run('one', 'sleep 20', termination='timed_out', code=None, timeout=20)
        # Guest stdin is EOF, never the worker's JSON protocol stream.
        run('one', 'cat; printf recovered', b'recovered')
        assert request(dict(op='close', context='one'))['ok']
        assert not request(dict(op='close', context='one'))['ok']
        run('two', 'printf independent', b'independent')
        assert request(dict(op='close', context='two'))['ok']
        worker.stdin.close()
        assert worker.wait(timeout=10) == 0
        assert worker.stderr.read() == ''
    finally:
        if worker.poll() is None:
            worker.kill()
            worker.wait()
    output = repo/'target/worker-smoke'
    output.mkdir(parents=True, exist_ok=True)
    (output/'results.json').write_text(json.dumps(records, indent=2)+'\n')
    print(f'worker contexts: PASS ({len(records)} protocol requests)')
