"""Authenticated finite benchmark artifact acquisition and conservative import plans."""
import hashlib,json,os
from pathlib import Path
import re,stat,subprocess,time,zipfile
from corral_perf import GIB

REPO='tuna-os/tuna-desktop'


def gh_json(path):
    result=subprocess.run(['gh','api','repos/'+REPO+'/'+path],capture_output=True,check=True,timeout=45)
    if len(result.stdout)>4*1024*1024:raise ValueError('GitHub response exceeds bound')
    return json.loads(result.stdout)


def original_run(run,source,path):
    if run.get('status')!='completed' or run.get('conclusion')!='success' or run.get('event')!='workflow_dispatch' or run.get('head_sha')!=source:
        raise ValueError('successful original dispatched source run required')
    if run.get('head_repository',{}).get('full_name')!=REPO or run.get('repository',{}).get('full_name')!=REPO or run.get('path')!=path:
        raise ValueError('original repository/workflow identity differs')


def disk_plan(manifest, *, method='qcow2-upload'):
    storage=manifest['storage']
    if storage.get('virtual_bytes')!=18*GIB or not 0<storage.get('transfer_bytes',0)<=2*GIB or manifest.get('requested_pvc')!='20Gi' or manifest.get('firmware')!='uefi':
        raise ValueError('bounded18Gi/20Gi UEFI export required')
    if method!='qcow2-upload':raise ValueError('raw optimization requires separately qualified actual method')
    # Reserve full target AND scratch capacity until a measured/source-proven
    # method supports a smaller physical bound. Mapped estimates are not enough.
    return {'method':method,'target_pvc_bytes':20*GIB,'scratch_pvc_bytes':20*GIB,
            'transfer_temporary_allowance_bytes':storage['transfer_bytes'],
            'guest_write_allowance_bytes':GIB//2,'runtime_layer_allowance_bytes':GIB,
            'estimated_peak_bytes':40*GIB+storage['transfer_bytes']+GIB+GIB//2,
            'performance_qualified':False}


def safe_members(archive):
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
        if entry.file_size>2*GIB or total>3*GIB:raise ValueError('artifact expansion exceeds bound')
    if 'manifest.json' not in names or 'disk.qcow2' not in names:raise ValueError('disk artifact members missing')
    return entries,total


def download(run_id,artifact_id,source,profile,out):
    if type(run_id) is not int or run_id<=0 or type(artifact_id) is not int or artifact_id<=0 or not re.fullmatch(r'[0-9a-f]{40}',source) or profile not in ('stock','diagnostic'):
        raise ValueError('explicit original run/artifact/source/profile required')
    out=Path(out);out.mkdir(parents=True,exist_ok=False);out.chmod(0o700)
    run=gh_json(f'actions/runs/{run_id}');original_run(run,source,'.github/workflows/performance-baseline.yml')
    artifact=gh_json(f'actions/artifacts/{artifact_id}')
    if artifact.get('name')!='marlin-benchmark-disk-'+profile or artifact.get('expired') is not False or artifact.get('workflow_run',{}).get('id')!=run_id or artifact.get('workflow_run',{}).get('head_sha')!=source:
        raise ValueError('artifact identity/retention differs')
    expected=artifact.get('digest','');size=artifact.get('size_in_bytes')
    if not re.fullmatch(r'sha256:[0-9a-f]{64}',expected) or type(size) is not int or not 0<size<=3*GIB:raise ValueError('bounded original archive digest required')
    free=os.statvfs(out).f_bavail*os.statvfs(out).f_frsize
    if free<size+3*GIB+GIB//2:raise ValueError('local download/extraction disk headroom insufficient')
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
        entries,_=safe_members(archive)
        for entry in entries:archive.extract(entry,export)
    manifest=json.loads((export/'manifest.json').read_text())
    if manifest.get('export_succeeded') is not True or manifest.get('observer_source_commit')!=source or manifest.get('gnome_profile')!=profile:
        raise ValueError('exported manifest source/profile differs')
    disk=export/'disk.qcow2'
    with disk.open('rb') as stream:actual=hashlib.file_digest(stream,'sha256').hexdigest()
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
