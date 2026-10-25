#!/usr/bin/env python3
"""Run the pinned musl Node version gate; does not claim JavaScript execution."""
import argparse,hashlib,json,subprocess,time
from pathlib import Path
repo=Path(__file__).resolve().parent.parent
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--root',required=True,type=Path)
args=parser.parse_args();root=args.root.resolve()
subprocess.run(['cargo','build','--release','--offline','-p','blink-cli'],cwd=repo,check=True)
command=[str(repo/'target/release/blink-cli'),'--root',str(root),'--max-memory','536870912','--max-image-size','268435456','--timeout-ms','60000','/usr/local/bin/node','--version']
start=time.monotonic();run=subprocess.run(command,capture_output=True,timeout=70)
report=dict(case='version',exit=run.returncode,stdout=run.stdout.decode(errors='replace'),stderr=run.stderr.decode(errors='replace'),seconds=time.monotonic()-start,node_sha256=hashlib.sha256((root/'usr/local/bin/node').read_bytes()).hexdigest(),passed=run.returncode==0 and run.stdout==b'v22.23.4\n' and not run.stderr)
output=repo/'target/node-smoke';output.mkdir(parents=True,exist_ok=True)
(output/'results.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report,indent=2))
raise SystemExit(not report['passed'])
