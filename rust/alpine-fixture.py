#!/usr/bin/env python3
"""Prepare a pinned single-layer AMD64 Alpine test root, without a container engine.

This fixture loader is deliberately restricted: digest-checked OCI blobs, no
whiteouts/devices, regular files before symlinks, and rootless owner/mode xattrs.
It is not a general OCI unpacker/exporter. --layout reuses a local OCI layout.
"""
import argparse
import hashlib
import io
import json
import os
import platform
import shutil
import struct
import subprocess
import tarfile
import urllib.request
from pathlib import Path, PurePosixPath

MANIFEST='dc92f017beea8e7f43263073d717f7bc011228c95d5312e440deb4d035bb4eec'
p=argparse.ArgumentParser(description=__doc__)
p.add_argument('--root',required=True,type=Path)
p.add_argument('--layout',type=Path)
p.add_argument('--cache',type=Path,default=Path('target/alpine-fixture-cache'))
a=p.parse_args()
assert not a.root.exists(),'fixture destination must not exist'
a.cache.mkdir(parents=True,exist_ok=True)
token=None

def fetch(digest,kind,size=None):
    global token
    digest=digest.removeprefix('sha256:')
    assert len(digest)==64 and all(c in '0123456789abcdef' for c in digest)
    cache=a.layout/'blobs/sha256'/digest if a.layout else a.cache/digest
    if cache.exists():
        data=cache.read_bytes()
    else:
        assert not a.layout,f'missing OCI blob {digest}'
        if token is None:
            url='https://auth.docker.io/token?service=registry.docker.io&scope=repository:library/alpine:pull'
            with urllib.request.urlopen(url,timeout=30) as response:
                token=json.load(response)['token']
        request=urllib.request.Request(f'https://registry-1.docker.io/v2/library/alpine/{kind}/sha256:{digest}',headers={'Authorization':f'Bearer {token}','Accept':'application/vnd.oci.image.manifest.v1+json'})
        with urllib.request.urlopen(request,timeout=60) as response:
            data=response.read(32*1024*1024+1)
        assert len(data)<=32*1024*1024,'fixture blob too large'
    assert hashlib.sha256(data).hexdigest()==digest,f'digest mismatch {digest}'
    assert size is None or len(data)==size,f'size mismatch {digest}'
    if not cache.exists():cache.write_bytes(data)
    return data

manifest=json.loads(fetch(MANIFEST,'manifests'))
config=json.loads(fetch(manifest['config']['digest'],'blobs',manifest['config']['size']))
assert (config['os'],config['architecture'])==('linux','amd64')
assert len(manifest['layers'])==1,'only the pinned single-layer fixture is supported'
layer=manifest['layers'][0]
assert layer['mediaType']=='application/vnd.oci.image.layer.v1.tar+gzip'
blob=fetch(layer['digest'],'blobs',layer['size'])
a.root.mkdir(parents=True)

attribute_prefix='user.' if platform.system()=='Linux' else ''
def set_attribute(path, key, value):
    if platform.system()=='Darwin':
        subprocess.run(['xattr','-wx',key,value.hex(),str(path)],check=True)
    else:
        os.setxattr(path,key,value)

def attributes(path,item):
    os.chmod(path,item.mode | (0o700 if item.isdir() else 0o600))
    set_attribute(path,attribute_prefix+'org.image-forge.owner.v1',struct.pack('<II',item.uid,item.gid))
    set_attribute(path,attribute_prefix+'org.image-forge.mode.v1',struct.pack('<I',item.mode&0o7777))
    os.utime(path,(item.mtime,item.mtime))
def name(raw):
    path=PurePosixPath(raw)
    assert not path.is_absolute() and '..' not in path.parts and '\x00' not in raw,'unsafe archive name'
    assert not any(part.startswith('.wh.') for part in path.parts),'whiteouts unsupported'
    return path

with tarfile.open(fileobj=io.BytesIO(blob),mode='r:gz') as tar:
    entries=tar.getmembers()
    symlinks=[];hardlinks=[];directories=[]
    for item in entries:
        relative=name(item.name)
        destination=a.root.joinpath(*relative.parts)
        if item.issym():
            symlinks.append((destination,item))
        elif item.islnk():
            hardlinks.append((destination,item))
        elif item.isdir():
            destination.mkdir(parents=True,exist_ok=True)
            directories.append((destination,item))
        elif item.isfile():
            destination.parent.mkdir(parents=True,exist_ok=True)
            assert not destination.exists(),f'duplicate file {item.name}'
            with tar.extractfile(item) as source, destination.open('xb') as output:
                shutil.copyfileobj(source,output)
            attributes(destination,item)
        elif not (item.ischr() or item.isblk() or item.isfifo()):
            raise ValueError(f'unsupported tar type {item.name}')
    for destination,item in hardlinks:
        source=a.root.joinpath(*name(item.linkname).parts)
        assert source.is_file() and not source.is_symlink(),'hardlink target must be an extracted regular file'
        destination.parent.mkdir(parents=True,exist_ok=True)
        os.link(source,destination)
        attributes(destination,item)
    # Absolute symlink contents are intentional Linux guest paths. Creating them
    # last ensures extraction never follows them into the host filesystem.
    for destination,item in symlinks:
        assert item.uid==0 and item.gid==0,'non-root symlink ownership needs a portable metadata representation'
        destination.parent.mkdir(parents=True,exist_ok=True)
        destination.symlink_to(item.linkname)
    for destination,item in reversed(directories):attributes(destination,item)
print(json.dumps(dict(manifest='sha256:'+MANIFEST,layer=layer['digest'],root=str(a.root),architecture='amd64',version='3.22.7'),indent=2))
