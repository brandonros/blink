#!/usr/bin/env python3
"""Build static fixtures; compare Rust, C interpreter/JIT, and native Linux AMD64."""
import argparse, hashlib, json, platform, subprocess, time, tempfile
from pathlib import Path
root=Path(__file__).resolve().parent.parent
p=argparse.ArgumentParser();p.add_argument('--rust-native-only',action='store_true');p.add_argument('--c-blink',type=Path,default=root/'o/blink/blink');p.add_argument('--output',type=Path,default=root/'target/differential');a=p.parse_args()
a.output.mkdir(parents=True,exist_ok=True)
subprocess.run(['cargo','build','--offline','-p','blink-cli'],cwd=root,check=True)
fixtures={}
for name in ['hello','process','exec','kill','thread','signal','vfork']:
 path=a.output/f'{name}-x86_64'
 subprocess.run(['cc','-nostdlib','-static','-Wl,--build-id=none','-o',str(path),str(root/f'rust/fixtures/{name}-x86_64.S')],check=True)
 fixtures[name]=path.resolve()
for name in ['integer', 'descriptors', 'filesystem', 'restart', 'socketpair', 'timers', 'altstack', 'sigchld', 'fxstate', 'capget', 'epoll', 'prctl', 'madvise', 'mremap', 'futex', 'atomic']:
 path=a.output/f'{name}-x86_64'
 subprocess.run(['cc','-nostdlib','-static','-ffreestanding','-fno-stack-protector','-msse2' if name=='fxstate' else '-mno-sse','-mmmx' if name=='fxstate' else '-mno-mmx','-O2','-Wl,--build-id=none','-o',str(path),str(root/f'rust/fixtures/{name}-x86_64.c')],check=True)
 fixtures[name]=path.resolve()
path=a.output/'libc-x86_64'
subprocess.run(['cc','-static','-O2','-Wl,--build-id=none','-o',str(path),str(root/'rust/fixtures/libc-x86_64.c')],check=True)
fixtures['libc']=path.resolve()
engines={'rust':[str(root/'target/debug/blink-cli')]}
if not a.rust_native_only:engines.update({'c-interpreter':[str(a.c_blink.resolve()),'-j'],'c-jit':[str(a.c_blink.resolve())]})
if platform.system()=='Linux' and platform.machine()=='x86_64':engines['native']=[]
results=[]
failed=False
for name,path in fixtures.items():
 for engine,prefix in engines.items():
  command=prefix+[str(path)]+([str(fixtures['hello'])] if name=='exec' else [])
  start=time.monotonic()
  try:
   with tempfile.TemporaryDirectory(prefix="blink-diff-") as cwd:
    r=subprocess.run(command,capture_output=True,timeout=10,cwd=cwd)
  except subprocess.TimeoutExpired as e:
   r=subprocess.CompletedProcess(command,124,e.stdout or b'',(e.stderr or b'')+b'timeout')
  expected=b'libc hello\n' if name=='libc' else b'9c39dd0ec11a7dd03f8c47090a6a46f5\n' if name=='integer' else b'child\n' if name=='process' else b'' if name in ('kill','thread','signal','descriptors','filesystem','restart','vfork','socketpair','timers','altstack','sigchld','fxstate','capget','epoll','prctl','madvise','mremap','futex','atomic') else b'hello\n'
  row=dict(fixture=name,engine=engine,exit=r.returncode,stdout=r.stdout.decode(errors='replace'),stderr=r.stderr.decode(errors='replace'),seconds=time.monotonic()-start,sha256=hashlib.sha256(path.read_bytes()).hexdigest())
  results.append(row);print(f'{name:10} {engine:14} exit={r.returncode} output={r.stdout!r}')
  if r.returncode!=0 or r.stdout!=expected or r.stderr:failed=True
(a.output/'results.json').write_text(json.dumps(results,indent=2)+'\n')

raise SystemExit(1 if failed else 0)
