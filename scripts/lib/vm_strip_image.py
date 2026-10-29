"""Bind same-run package bytes to the exact final CI image; no release admission."""
import hashlib
import json
from pathlib import Path
import re


def canonical_image_id(observed):
    """Canonicalize Podman's full image ID without admitting tags or short IDs."""
    if not isinstance(observed, str) or not re.fullmatch(r'(?:sha256:)?[0-9a-f]{64}', observed):
        raise ValueError('full immutable Podman image ID required')
    return observed if observed.startswith('sha256:') else 'sha256:' + observed


def admission(package, observed, root, commit, image_id, base_image):
    if not re.fullmatch(r'[0-9a-f]{40}', commit):
        raise ValueError('exact source commit missing')
    if not re.fullmatch(r'sha256:[0-9a-f]{64}', image_id):
        raise ValueError('immutable final image ID missing')
    if not re.fullmatch(r'ghcr.io/tuna-os/marlin:gnome@sha256:[0-9a-f]{64}', base_image):
        raise ValueError('immutable base image missing')
    if observed.get('schema') != 1 or observed.get('image_id') != image_id:
        raise ValueError('image inspection identity differs')
    expected = {'/' + name: data['sha256'] for name, data in package['required_members'].items()
                if name.startswith('usr/bin/')}
    for name in ('tuna-vm-input', 'tuna-vm-lifecycle'):
        path = Path(root) / 'packaging/marlin/vm-lane' / name
        expected['/usr/libexec/' + name] = hashlib.sha256(path.read_bytes()).hexdigest()
    if observed.get('member_sha256') != expected:
        raise ValueError('final image installed executables differ from package/source bytes')
    digest = package['package']['sha256']
    if not re.fullmatch(r'[0-9a-f]{64}', digest):
        raise ValueError('original package byte hash missing')
    return {'schema': 1, 'commit': commit, 'image_id': image_id, 'base_image': base_image,
            'package_sha256': digest, 'member_sha256': expected,
            'package_inspection': package,
            'installed_signed_release_qualified': False,
            'scope': 'same-run CI package and final image, not signed release or kernel presentation'}


# Executed inside the final immutable image by the existing CI job. No host
# filesystem mounts, guest configuration changes, or credentials are required.
IMAGE_INSPECTION = '''import hashlib,json,os,stat,sys
names=["/usr/bin/tuna-"+n for n in ("compositor","session","shell-host","shell-gtk","ibus-bridge")]+["/usr/libexec/tuna-vm-input","/usr/libexec/tuna-vm-lifecycle"]
result={}
for name in names:
 fd=os.open(name,os.O_RDONLY|os.O_NOFOLLOW)
 try:
  before=os.fstat(fd)
  if not stat.S_ISREG(before.st_mode) or not 0<before.st_size<=67108864: raise ValueError("installed member size/type")
  h=hashlib.sha256()
  while True:
   chunk=os.read(fd,65536)
   if not chunk: break
   h.update(chunk)
  after=os.fstat(fd)
  if (before.st_dev,before.st_ino,before.st_size,before.st_mtime_ns,before.st_ctime_ns)!=(after.st_dev,after.st_ino,after.st_size,after.st_mtime_ns,after.st_ctime_ns): raise ValueError("installed bytes changed")
  result[name]=h.hexdigest()
 finally: os.close(fd)
print(json.dumps({"schema":1,"image_id":sys.argv[1],"member_sha256":result}))
'''
