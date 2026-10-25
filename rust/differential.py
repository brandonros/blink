#!/usr/bin/env python3
"""Build static fixtures; compare Rust, C interpreter/JIT, and native Linux AMD64."""
import argparse, hashlib, json, platform, subprocess, time
from pathlib import Path
root=Path(__file__).resolve().parent.parent
p=argparse.ArgumentParser();p.add_argument('--c-blink',type=Path,default=root/'o/blink/blink');p.add_argument('--output',type=Path,default=root/'target/differential');a=p.parse_args()
a.output.mkdir(parents=True,exist_ok=True)
subprocess.run(['cargo','build','--offline','-p','blink-cli'],cwd=root,check=True)
fixtures={}
for name in ['hello','process','exec','kill']:
 path=a.output/f'{name}-x86_64'
 subprocess.run(['cc','-nostdlib','-static','-Wl,--build-id=none','-o',str(path),str(root/f'rust/fixtures/{name}-x86_64.S')],check=True)
 fixtures[name]=path.resolve()
engines={'rust':[str(root/'target/debug/blink-cli')],'c-interpreter':[str(a.c_blink),'-j'],'c-jit':[str(a.c_blink)]}
if platform.system()=='Linux' and platform.machine()=='x86_64':engines['native']=[]
results=[]
failed=False
for name,path in fixtures.items():
 for engine,prefix in engines.items():
  command=prefix+[str(path)]+([str(fixtures['hello'])] if name=='exec' else [])
  start=time.monotonic();r=subprocess.run(command,capture_output=True,timeout=10)
  expected=b'child\n' if name=='process' else b'' if name=='kill' else b'hello\n'
  row=dict(fixture=name,engine=engine,exit=r.returncode,stdout=r.stdout.decode(errors='replace'),stderr=r.stderr.decode(errors='replace'),seconds=time.monotonic()-start,sha256=hashlib.sha256(path.read_bytes()).hexdigest())
  results.append(row);print(f'{name:10} {engine:14} exit={r.returncode} output={r.stdout!r}')
  if r.returncode!=0 or r.stdout!=expected or r.stderr:failed=True
(a.output/'results.json').write_text(json.dumps(results,indent=2)+'\n')

raise SystemExit(1 if failed else 0)
