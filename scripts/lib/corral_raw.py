"""Backend-bound RAW upload admission; tiny proof does not qualify a full image.

CDI v1.66.1 upload-datasource.go selects TransferDataFile for raw, writing the
final target directly. upload-controller.go still creates a scratch PVC.
https://github.com/kubevirt/containerized-data-importer/tree/v1.66.1/pkg
"""
import hashlib,json,os,re,shutil,subprocess
from pathlib import Path
from corral_perf import kubectl,GIB
PROBE_SHA='874a71266b1e1eb928f482fe252d8cf9c362e083a73accfbdbe948169c1af94b'
UPLOAD_IMAGE='quay.io/kubevirt/cdi-uploadserver@sha256:d3a6b735efcc914a0c9679fa8f64ab3dda764f7997d53bcafa6e2944f123420a'

def evidence_sha(directory):
 directory=Path(directory);names=['receipt.json','upload-observations.json','source/tuna-corral-raw-upload-probe','source/corral_session.py','source/corral_owned.py','source/corral_perf.py']
 return hashlib.sha256(json.dumps({n:hashlib.sha256((directory/n).read_bytes()).hexdigest() for n in names},sort_keys=True).encode()).hexdigest()

def proof(directory,context,node,receipt_sha):
 directory=Path(directory);raw=(directory/'receipt.json').read_bytes()
 if evidence_sha(directory)!=receipt_sha:raise ValueError('explicit original RAW probe evidence bundle SHA differs')
 r=json.loads(raw)
 if r.get('source_sha256')!=PROBE_SHA or r.get('context')!=context or r.get('completed') is not True or r.get('cleanup',{}).get('all_owned_resources_absent') is not True:raise ValueError('qualified source-owned RAW probe required')
 readback=r.get('readback',{})
 if readback.get('matched') is not True or readback.get('prefix_bytes')!=64*1024*1024 or readback.get('sha256')!=r.get('raw_sha256') or readback.get('node')!=node or readback.get('pvc_uid')!=r.get('pvc_uid'):raise ValueError('actual RAW readback identity differs')
 frozen=directory/'source'
 for name,digest in {**r['helper_sha256'],'tuna-corral-raw-upload-probe':PROBE_SHA}.items():
  if hashlib.sha256((frozen/name).read_bytes()).hexdigest()!=digest:raise ValueError('original probe source bytes differ')
 actual=kubectl(context,'get','node',node,'-o','json')
 if actual['metadata']['uid']!=r.get('node_uid'):raise ValueError('RAW proof node replaced')
 config=kubectl(context,'get','configmap','local-path-config','-n','local-path-storage','-o','json')
 data=config.get('data',{})
 if hashlib.sha256(json.dumps(data,sort_keys=True).encode()).hexdigest()!=r.get('local_path_config_sha256') or data.get('setup')!='#!/bin/sh\nset -eu\nmkdir -m 0777 -p "$VOL_DIR"':raise ValueError('directory-only local-path backend differs')
 cdi=kubectl(context,'get','cdi','-o','json')['items']
 if len(cdi)!=1 or any(cdi[0].get('status',{}).get(k)!='v1.66.1' for k in ('targetVersion','observedVersion','operatorVersion')):raise ValueError('RAW direct-transfer CDI version differs')
 if hashlib.sha256(Path(shutil.which('virtctl')).read_bytes()).hexdigest()!=r.get('virtctl_sha256'):raise ValueError('qualified upload client bytes differ')
 observations=json.loads((directory/'upload-observations.json').read_text())
 running=[o for o in observations if o.get('phase')=='Running' and o.get('directory_usage_returncode')==0]
 if not running or any(o.get('node')!=node or not re.search(r'^0\s+/scratch$',o.get('directory_usage',''),re.M) or not any(c.get('imageID')==UPLOAD_IMAGE for c in o.get('container_statuses',[])) for o in running):raise ValueError('actual unused scratch / exact RAW server evidence missing')
 return {'original_probe_receipt_sha256':hashlib.sha256(raw).hexdigest(),'original_probe_evidence_sha256':receipt_sha,'node_uid':r['node_uid'],'upload_image_id':UPLOAD_IMAGE,'local_path_config_sha256':r['local_path_config_sha256'],'full_image_import_qualified':False}

def convert(export,manifest,out):
 """Sparse local conversion, independently matched to original exported raw SHA."""
 export=Path(export);out=Path(out);storage=manifest['storage'];virtual=18*GIB
 digest=manifest.get('raw_sha256','')
 if not re.fullmatch('[0-9a-f]{64}',digest):raise ValueError('original raw image SHA required')
 info=json.loads(subprocess.check_output(['qemu-img','info','--output=json',str(export/'disk.qcow2')],timeout=30))
 if info.get('format')!='qcow2' or info.get('virtual-size')!=virtual or info.get('backing-filename') or info.get('data-file'):raise ValueError('standalone exact18Gi qcow2 required')
 mapped=storage.get('mapped_nonzero_bytes')
 if type(mapped) is not int or mapped<=0 or mapped>5*GIB:raise ValueError('bounded raw conversion extent estimate required')
 free=os.statvfs(out).f_bavail*os.statvfs(out).f_frsize
 if free<mapped+GIB:raise ValueError('local sparse conversion headroom refused')
 target=out/'upload.raw'
 if target.exists():raise ValueError('refusing local raw overwrite')
 try:
  subprocess.run(['qemu-img','convert','-f','qcow2','-O','raw','-S','4k',str(export/'disk.qcow2'),str(target)],check=True,timeout=900)
  stat=target.stat()
  if stat.st_size!=virtual or stat.st_blocks*512>mapped+GIB//2:raise ValueError('actual raw allocation exceeds admitted bound')
  with target.open('rb') as f:
   if hashlib.file_digest(f,'sha256').hexdigest()!=digest:raise ValueError('converted raw differs from original exported raw bytes')
  return target,{'uploaded_raw_sha256':digest,'virtual_bytes':virtual,'local_allocated_bytes':stat.st_blocks*512}
 except BaseException:
  target.unlink(missing_ok=True);raise
