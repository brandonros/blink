#!/usr/bin/env python3
"""Extract the pinned Debian AMD64 test image from a verified OCI layout.

Use linux-oci-builder to download the layout. This intentionally supports only
this fixture, rejects whiteouts, and leaves extraction to the host archive tool.
It is not an OCI image assembler or a rootless metadata fidelity gate.
"""
import argparse,hashlib,json,subprocess,tarfile
from pathlib import Path,PurePosixPath
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--layout',required=True,type=Path)
parser.add_argument('--root',required=True,type=Path)
args=parser.parse_args();layout=args.layout.resolve();root=args.root.resolve()
MANIFEST='sha256:ec31d744f83f4a73447ba045aaab9409997efe5cc706eca4890c1be36cc1606a' 
assert not root.exists()
def blob(descriptor):
    digest=descriptor['digest'].removeprefix('sha256:');path=layout/'blobs/sha256'/digest
    assert hashlib.sha256(path.read_bytes()).hexdigest()==digest
    assert path.stat().st_size==descriptor['size']
    return path
index=json.loads((layout/'index.json').read_text());descriptor=index['manifests'][0]
assert descriptor['digest']==MANIFEST,'unexpected Debian fixture manifest'
manifest=json.loads(blob(descriptor).read_text());config=json.loads(blob(manifest['config']).read_text())
assert (config['os'],config['architecture'])==('linux','amd64')
root.mkdir()
for layer in manifest['layers']:
    archive=blob(layer)
    with tarfile.open(archive) as tar:
        entries=tar.getmembers()
        for item in entries:
            name=PurePosixPath(item.name)
            assert not name.is_absolute() and '..' not in name.parts
            assert not any(part.startswith('.wh.') for part in name.parts),'probe loader does not implement whiteouts'
            if item.islnk():
                target=PurePosixPath(item.linkname)
                assert not target.is_absolute() and '..' not in target.parts
    # The host archive tool rejects unsafe extraction through symlink parents.
    subprocess.run(['tar','--extract','--file',str(archive),'--directory',str(root),'--no-same-owner'],check=True)
receipt={'manifest':descriptor['digest'],'config':manifest['config']['digest'],'layers':[x['digest'] for x in manifest['layers']], 'root':str(root),'image_config':config['config']}
(root.parent/'debian-fixture.json').write_text(json.dumps(receipt,indent=2)+'\n')
print(json.dumps(receipt,indent=2))
