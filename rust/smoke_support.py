"""Copy disposable guest roots while retaining rootless metadata and symlinks."""
import platform
import shutil
import subprocess

def copy_root(source, destination):
    if platform.system() == 'Darwin':
        # Python's macOS copy2 does not preserve arbitrary extended attributes.
        subprocess.run(['ditto', '--rsrc', '--extattr', '--noacl', str(source), str(destination)], check=True)
    else:
        shutil.copytree(source, destination, symlinks=True)
