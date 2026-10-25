#!/usr/bin/env python3
"""Build static fixtures; compare Rust, C interpreter/JIT, and native Linux AMD64."""
import argparse, hashlib, json, platform, subprocess, time
from pathlib import Path
root=Path(__file__).resolve().parent.parent
p=argparse.ArgumentParser();p.add_argument('--c-blink',type=Path,default=root/'o/blink/blink');p.add_argument('--output',type=Path,default=root/'target/differential');a=p.parse_args()
a.output.mkdir(parents=True,exist_ok=True)
subprocess.run(['cargo','build','--offline','-p','blink-cli'],cwd=root,check=True)
fixtures={}
for name in ['hello','process','exec','kill','thread','signal']:
 path=a.output/f'{name}-x86_64'
 subprocess.run(['cc','-nostdlib','-static','-Wl,--build-id=none','-o',str(path),str(root/f'rust/fixtures/{name}-x86_64.S')],check=True)
 fixtures[name]=path.resolve()
path=a.output/'integer-x86_64'
subprocess.run(['cc','-nostdlib','-static','-ffreestanding','-fno-stack-protector','-mno-sse','-mno-mmx','-O2','-Wl,--build-id=none','-o',str(path),str(root/'rust/fixtures/integer-x86_64.c')],check=True)
fixtures['integer']=path.resolve()
engines={'rust':[str(root/'target/debug/blink-cli')],'c-interpreter':[str(a.c_blink),'-j'],'c-jit':[str(a.c_blink)]}
if platform.system()=='Linux' and platform.machine()=='x86_64':engines['native']=[]
results=[]
failed=False
for name,path in fixtures.items():
 for engine,prefix in engines.items():
  command=prefix+[str(path)]+([str(fixtures['hello'])] if name=='exec' else [])
  start=time.monotonic()
  try:
   r=subprocess.run(command,capture_output=True,timeout=10)
  except subprocess.TimeoutExpired as e:
   r=subprocess.CompletedProcess(command,124,e.stdout or b'',(e.stderr or b'')+b'timeout')
  expected=b'9c39dd0ec11a7dd03f8c47090a6a46f5\n' if name=='integer' else b'child\n' if name=='process' else b'' if name in ('kill','thread','signal') else b'hello\n'
  row=dict(fixture=name,engine=engine,exit=r.returncode,stdout=r.stdout.decode(errors='replace'),stderr=r.stderr.decode(errors='replace'),seconds=time.monotonic()-start,sha256=hashlib.sha256(path.read_bytes()).hexdigest())
  results.append(row);print(f'{name:10} {engine:14} exit={r.returncode} output={r.stdout!r}')
  if r.returncode!=0 or r.stdout!=expected or r.stderr:failed=True
(a.output/'results.json').write_text(json.dumps(results,indent=2)+'\n')

raise SystemExit(1 if failed else 0)
