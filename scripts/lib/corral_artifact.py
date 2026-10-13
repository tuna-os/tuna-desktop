"""Authenticated finite benchmark artifact acquisition and conservative import plans."""
import hashlib,json,os
from pathlib import Path
import re,stat,subprocess,time,zipfile
from corral_perf import GIB

REPO='tuna-os/tuna-desktop'
# Reviewed dense consumer bounds (explicit, no silent widening).
# Compact 20Gi path is FROZEN: transfer <=2Gi. Dense 28Gi path is NEW:
# transfer <=5Gi to admit the measured full-paired export (transfer
# 4105446912 = 76.47% of 5Gi, allocated 4074299392 = 75.89% of 5Gi).
# Archive: member <=5Gi covers the 5Gi disk.qcow2 member; total/ZIP <=6Gi
# covers 5Gi + ~1Gi manifest/evidence/ZIP overhead. Download headroom:
# free >= size + 6Gi + 0.5Gi (ZIP + expansion + allowance, max 12.5Gi).
COMPACT_PVC='20Gi'
COMPACT_PVC_BYTES=20*GIB
COMPACT_MAX_TRANSFER_BYTES=2*GIB
DENSE_PVC='28Gi'
DENSE_PVC_BYTES=28*GIB
DENSE_MAX_TRANSFER_BYTES=5*GIB
ARCHIVE_MAX_MEMBER_BYTES=5*GIB
ARCHIVE_MAX_TOTAL_BYTES=6*GIB
ARCHIVE_MAX_ZIP_BYTES=6*GIB


def gh_json(path):
    result=subprocess.run(['gh','api','repos/'+REPO+'/'+path],capture_output=True,check=True,timeout=45)
    if len(result.stdout)>4*1024*1024:raise ValueError('GitHub response exceeds bound')
    return json.loads(result.stdout)


def original_run(run,source,path):
    if run.get('status')!='completed' or run.get('conclusion')!='success' or run.get('event')!='workflow_dispatch' or run.get('head_sha')!=source:
        raise ValueError('successful original dispatched source run required')
    if run.get('head_repository',{}).get('full_name')!=REPO or run.get('repository',{}).get('full_name')!=REPO or run.get('path')!=path:
        raise ValueError('original repository/workflow identity differs')


def disk_plan(manifest, *, method='qcow2-upload', raw_proof=None):
    if method == 'raw-gzip-upload':
        from corral_gzip import disk_plan as gzip_plan
        return gzip_plan(manifest, raw_proof)
    storage=manifest.get('storage')
    if not isinstance(storage,dict):raise ValueError('bounded storage manifest required')
    transfer=storage.get('transfer_bytes')
    if type(transfer) is not int:raise ValueError('bounded integer transfer bytes required')
    virtual=storage.get('virtual_bytes')
    if type(virtual) is not int:raise ValueError('bounded integer virtual bytes required')
    # Reviewed bound: 20Gi path keeps its exact 2Gi transfer cap (frozen);
    # 28Gi dense path admits transfer <=5Gi. No other PVC ask is admitted.
    if manifest.get('requested_pvc')==COMPACT_PVC:
        if virtual!=18*GIB or not 0<transfer<=COMPACT_MAX_TRANSFER_BYTES or manifest.get('firmware')!='uefi':
            raise ValueError('bounded18Gi/20Gi UEFI export required')
        pvc_bytes=COMPACT_PVC_BYTES
    elif manifest.get('requested_pvc')==DENSE_PVC:
        if virtual!=18*GIB or not 0<transfer<=DENSE_MAX_TRANSFER_BYTES or manifest.get('firmware')!='uefi':
            raise ValueError('bounded18Gi/28Gi dense UEFI export required (5Gi transfer)')
        pvc_bytes=DENSE_PVC_BYTES
    else:
        raise ValueError('bounded18Gi/20Gi-or-28Gi UEFI export required (20Gi:2Gi transfer; 28Gi:5Gi transfer)')
    if method=='raw-upload':
        if not raw_proof or raw_proof.get('full_image_import_qualified') is not False or not re.fullmatch('[0-9a-f]{64}',raw_proof.get('original_probe_receipt_sha256','')):raise ValueError('backend-bound original RAW probe required')
        return {'method':method,'target_pvc_bytes':pvc_bytes,'scratch_pvc_bytes':pvc_bytes,'scratch_physical_metadata_allowance_bytes':GIB,'runtime_layer_allowance_bytes':GIB,'guest_write_allowance_bytes':GIB//2,'estimated_peak_bytes':pvc_bytes+GIB+GIB+GIB//2,'raw_method_proof':raw_proof,'full_image_import_qualified':False,'performance_qualified':False,'node_peak_qualified':False,'limitation':'node admission pending measurement: full target+scratch reserve and CDI scratch/preallocation plus guest writes must be measured on a pressure-free node; no node is admitted by this plan'}
    if method!='qcow2-upload':raise ValueError('raw optimization requires separately qualified actual method')
    # Reserve full target AND scratch capacity until a measured/source-proven
    # method supports a smaller physical bound. Mapped estimates are not enough.
    # Dense 28Gi example: 56Gi + transfer 4105446912 + 1Gi + 0.5Gi = 65845601792
    # (~61.32Gi). Node admission stays pending measurement (see the plan
    # limitation + node_peak_qualified False); no node claim is made here.
    return {'method':method,'target_pvc_bytes':pvc_bytes,'scratch_pvc_bytes':pvc_bytes,
            'transfer_temporary_allowance_bytes':storage['transfer_bytes'],
            'guest_write_allowance_bytes':GIB//2,'runtime_layer_allowance_bytes':GIB,
            'estimated_peak_bytes':2*pvc_bytes+storage['transfer_bytes']+GIB+GIB//2,
            'performance_qualified':False,'node_peak_qualified':False,
            'limitation':'node admission pending measurement: full target+scratch reserve and CDI scratch/preallocation plus guest writes must be measured on a pressure-free node; no node is admitted by this plan'}


def safe_members(archive, disk_member="disk.qcow2"):
    entries=archive.infolist()
    if not 0<len(entries)<=256:raise ValueError('artifact member count exceeds bound')
    names=set();total=0
    for entry in entries:
        path=Path(entry.filename)
        if path.is_absolute() or '..' in path.parts or '\\' in entry.filename or entry.filename in names or stat.S_ISLNK(entry.external_attr>>16):
            raise ValueError('unsafe artifact member')
        normalized=path.as_posix()
        if normalized!=entry.filename.rstrip('/') or normalized in names:raise ValueError('ambiguous artifact path')
        names.add(normalized);total+=entry.file_size
        if entry.file_size>ARCHIVE_MAX_MEMBER_BYTES or total>ARCHIVE_MAX_TOTAL_BYTES:raise ValueError('artifact expansion exceeds bound')
    if 'manifest.json' not in names or disk_member not in names:raise ValueError('disk artifact members missing')
    return entries,total


def download(run_id,artifact_id,source,profile,out, *, method="qcow2-upload"):
    if type(run_id) is not int or run_id<=0 or type(artifact_id) is not int or artifact_id<=0 or not re.fullmatch(r'[0-9a-f]{40}',source) or profile not in ('stock','diagnostic'):
        raise ValueError('explicit original run/artifact/source/profile required')
    out=Path(out);out.mkdir(parents=True,exist_ok=False);out.chmod(0o700)
    run=gh_json(f'actions/runs/{run_id}');original_run(run,source,'.github/workflows/performance-baseline.yml')
    artifact=gh_json(f'actions/artifacts/{artifact_id}')
    if artifact.get('name')!='marlin-benchmark-disk-'+profile or artifact.get('expired') is not False or artifact.get('workflow_run',{}).get('id')!=run_id or artifact.get('workflow_run',{}).get('head_sha')!=source:
        raise ValueError('artifact identity/retention differs')
    expected=artifact.get('digest','');size=artifact.get('size_in_bytes')
    if not re.fullmatch(r'sha256:[0-9a-f]{64}',expected) or type(size) is not int or not 0<size<=ARCHIVE_MAX_ZIP_BYTES:raise ValueError('bounded original archive digest required')
    free=os.statvfs(out).f_bavail*os.statvfs(out).f_frsize
    if free<size+ARCHIVE_MAX_TOTAL_BYTES+GIB//2:raise ValueError('local download/extraction disk headroom insufficient')
    archive_path=out/'original-artifact.zip'
    with archive_path.open('xb') as stream,(out/'download-error.log').open('wb') as error:
        process=subprocess.Popen(['gh','api','repos/'+REPO+f'/actions/artifacts/{artifact_id}/zip'],stdout=stream,stderr=error,start_new_session=True)
        deadline=time.monotonic()+900
        try:
            while process.poll() is None:
                if stream.tell()>size or time.monotonic()>deadline:raise TimeoutError('artifact download bound')
                time.sleep(.1)
            if process.returncode or stream.tell()!=size:raise ValueError('artifact download failed/incomplete')
        finally:
            if process.poll() is None:
                import signal
                os.killpg(process.pid,signal.SIGKILL);process.wait(timeout=5)
    with archive_path.open('rb') as stream:actual=hashlib.file_digest(stream,'sha256').hexdigest()
    if 'sha256:'+actual!=expected:raise ValueError('original archive bytes differ')
    export=out/'export';export.mkdir()
    with zipfile.ZipFile(archive_path) as archive:
        entries,_=safe_members(archive, "disk.raw.gz" if method == "raw-gzip-upload" else "disk.qcow2")
        for entry in entries:archive.extract(entry,export)
    manifest=json.loads((export/'manifest.json').read_text())
    if manifest.get('export_succeeded') is not True or manifest.get('observer_source_commit')!=source or manifest.get('gnome_profile')!=profile:
        raise ValueError('exported manifest source/profile differs')
    disk=export/('disk.raw.gz' if method == 'raw-gzip-upload' else 'disk.qcow2')
    with disk.open('rb') as stream:actual=hashlib.file_digest(stream,'sha256').hexdigest()
    if method == 'raw-gzip-upload' and manifest.get('import_method') != method:raise ValueError('distinct gzip manifest required')
    if actual!=manifest.get('disk_sha256'):raise ValueError('exported disk bytes differ')
    if manifest['storage']['transfer_bytes']!=disk.stat().st_size:raise ValueError('actual disk transfer size differs')
    from gnome_reference import validate_image
    validate_image(manifest.get('reference'),profile)
    if manifest.get('paired_desktops')!=['gnome','tuna'] or manifest.get('candidate_source_commit')!=manifest.get('package_selection',{}).get('head_sha'):
        raise ValueError('matched paired candidate source required')
    evidence=manifest.get('evidence')
    if not isinstance(evidence,dict) or not evidence:raise ValueError('original exporter evidence missing')
    for name,digest in evidence.items():
        if Path(name).name!=name or not re.fullmatch(r'[0-9a-f]{64}',digest):raise ValueError('invalid evidence member identity')
        with (export/name).open('rb') as stream:
            if hashlib.file_digest(stream,'sha256').hexdigest()!=digest:raise ValueError('exporter evidence bytes differ')
    package=manifest['package_selection'];original_run(gh_json(f'actions/runs/{package["run_id"]}'),package['head_sha'],'.github/workflows/ci.yml')
    receipt={'schema':1,'run_id':run_id,'artifact_id':artifact_id,'source_sha':source,'archive_sha256':expected,
             'manifest_sha256':hashlib.sha256((export/'manifest.json').read_bytes()).hexdigest(),'disk_sha256':actual,
             'authenticated_original_export':True,'performance_qualified':False}
    (out/'download-receipt.json').write_text(json.dumps(receipt,indent=2))
    return export,manifest,receipt
