#!/usr/bin/env python3
"""Compare guest TCP client behavior with native Linux against a loopback peer."""
import hashlib
import json
import platform
import socket
import subprocess
import threading
import time
from pathlib import Path
repo=Path(__file__).resolve().parent.parent
if (platform.system(),platform.machine())!=('Linux','x86_64'):
    raise SystemExit('Native TCP comparison requires Linux AMD64')
subprocess.run(['cargo','build','--release','--offline','-p','blink-cli'],cwd=repo,check=True)
output=repo/'target/tcp-smoke';output.mkdir(parents=True,exist_ok=True)
results=[];errors=[]
with socket.socket() as listener:
    listener.bind(('127.0.0.1',0));listener.listen();listener.settimeout(15)
    port=listener.getsockname()[1]
    program=output/'tcp-x86_64'
    subprocess.run(['cc','-nostdlib','-static','-ffreestanding','-fno-stack-protector','-mno-sse','-mno-mmx','-O2',f'-DPORT={port}','-Wl,--build-id=none','-o',str(program),str(repo/'rust/fixtures/tcp-x86_64.c')],check=True)
    def serve():
        try:
            for index in range(5):
                connection,_=listener.accept()
                with connection:
                    connection.settimeout(10)
                    request=b''
                    while not request.endswith(b'\n'):
                        part=connection.recv(64)
                        assert part,'unexpected client EOF'
                        request+=part
                    assert request==b'ping:request\n',request
                    if index==4:
                        time.sleep(.25)
                        continue
                    time.sleep(.01)
                    connection.sendall(b'he')
                    time.sleep(.01)
                    connection.sendall(b'llo\n')
        except Exception as error:
            errors.append(str(error))
    server=threading.Thread(target=serve,daemon=True);server.start()
    for name,prefix in [('native',[]),('rust',[str(repo/'target/release/blink-cli'),'--network'])]:
        run=subprocess.run(prefix+[str(program)],capture_output=True,timeout=15)
        passed=run.returncode==0 and run.stdout==b'hello\nhello\n' and not run.stderr
        results.append(dict(engine=name,exit=run.returncode,stdout=run.stdout.decode(errors='replace'),stderr=run.stderr.decode(errors='replace'),passed=passed))
        print(name,'PASS' if passed else 'FAIL',flush=True)
    run=subprocess.run([str(repo/'target/release/blink-cli'),'--network','--timeout-ms','50',str(program)],capture_output=True,timeout=3)
    passed=run.returncode==124 and not run.stdout and not run.stderr
    results.append(dict(engine='rust-timeout',exit=run.returncode,stdout=run.stdout.decode(errors='replace'),stderr=run.stderr.decode(errors='replace'),passed=passed))
    print('rust-timeout','PASS' if passed else 'FAIL',flush=True)
    server.join(timeout=16)
report=dict(fixture_sha256=hashlib.sha256(program.read_bytes()).hexdigest(),results=results,server_errors=errors)
(output/'results.json').write_text(json.dumps(report,indent=2)+'\n')
raise SystemExit(0 if not errors and all(r['passed'] for r in results) else 1)
