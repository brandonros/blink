#!/usr/bin/env python3
"""Run the currently supported subset of Blink's original assembly regressions."""
from pathlib import Path
import argparse, json, platform, subprocess
root=Path(__file__).resolve().parent.parent
parser=argparse.ArgumentParser();parser.add_argument('--cc',default='cc');parser.add_argument('--c-blink',type=Path);args=parser.parse_args()
out=root/'target/original-asm';out.mkdir(parents=True,exist_ok=True)
subprocess.run(['cargo','build','--offline','-p','blink-cli'],cwd=root,check=True)
names='add cmc cmov cmp cmpxchg entryrdx exit movi movsxd movz nop overflow pop_rsp_memory push setcc write xchg lock lahf repmovsb repmovsb_self ssemov'.split()
engines={'rust':[str(root/'target/debug/blink-cli')]}
if platform.system()=='Linux' and platform.machine()=='x86_64':engines['native']=[]
if args.c_blink:engines.update({'c-interpreter':[str(args.c_blink.resolve()),'-j'],'c-jit':[str(args.c_blink.resolve())]})
results=[];failed=False
for name in names:
    binary=out/name
    subprocess.run([args.cc,'-I.','-nostdlib','-static','-Wl,--omagic,--build-id=none','-o',str(binary),f'test/asm/{name}.S'],cwd=root,check=True,capture_output=True)
    row={'name':name}
    for engine,prefix in engines.items():
        result=subprocess.run(prefix+[str(binary)],capture_output=True,timeout=10)
        row[engine]={'status':result.returncode,'stdout':result.stdout.decode(errors='replace'),'stderr':result.stderr.decode(errors='replace')}
        expected=b'hello world\n' if name=='write' else b''
        if result.returncode or result.stdout!=expected or result.stderr:failed=True
    print(name, ' '.join(f'{engine}={row[engine]["status"]}' for engine in engines));results.append(row)
(out/'results.json').write_text(json.dumps(results,indent=2)+'\n')
raise SystemExit(1 if failed else 0)
