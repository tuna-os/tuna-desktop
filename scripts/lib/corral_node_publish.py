"""Two-run original tool custody; candidate tooling, never a desktop release."""
import base64
import gzip
import hashlib
import json
from pathlib import Path
import re
import tarfile
import stat
import time
import zipfile

from corral_artifact import original_run, REPO

WORKFLOW = '.github/workflows/corral-visual-tools.yml'
TARGET = 'ghcr.io/tuna-os/benchmark-node-tools'
SOURCES = {'scripts/lib/corral_node_stage.py', 'scripts/lib/corral_node_worker.py',
           'scripts/lib/corral-node-tool-smoke.py', 'scripts/lib/corral-node-tool-closure.py',
           'scripts/tuna-corral-benchmark-import', 'packaging/benchmark/Containerfile.node-tools',
           'packaging/benchmark/python-private', 'scripts/lib/corral_node_tools.py'}


def sha(path):
    with Path(path).open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()


def publisher_sources():
    root=Path(__file__).resolve().parents[2]
    return {name:sha(root/name) for name in
            ('scripts/tuna-corral-node-tools-publish','scripts/lib/corral_node_publish.py',WORKFLOW)}


def current_admission(current, jobs, environment, source, mode, checkout, remote_blobs, remote_commit):
    """Validate actual live workflow context and committed executing Git blobs."""
    expected_job='node-tools-publish' if mode=='publish' else 'node-tools-anonymous-verify'
    run_id=environment.get('GITHUB_RUN_ID','')
    attempt=environment.get('GITHUB_RUN_ATTEMPT','')
    ref=environment.get('GITHUB_REF','')
    if (not run_id.isdecimal() or not attempt.isdecimal() or int(attempt)<=0
            or environment.get('GITHUB_REPOSITORY')!=REPO
            or environment.get('GITHUB_EVENT_NAME')!='workflow_dispatch'
            or environment.get('GITHUB_SHA')!=source
            or environment.get('GITHUB_JOB')!=expected_job
            or not ref.startswith('refs/heads/')
            or environment.get('GITHUB_WORKFLOW_REF')!=REPO+'/'+WORKFLOW+'@'+ref
            or current.get('id')!=int(run_id) or current.get('run_attempt')!=int(attempt)
            or current.get('status')!='in_progress' or current.get('conclusion') is not None
            or current.get('head_sha')!=source or current.get('head_branch')!=ref[len('refs/heads/'):]
            or current.get('path')!=WORKFLOW or current.get('event')!='workflow_dispatch'
            or current.get('repository',{}).get('full_name')!=REPO
            or current.get('head_repository',{}).get('full_name')!=REPO
            or checkout.get('head')!=source or checkout.get('clean') is not True
            or remote_commit.get('sha')!=source or remote_commit.get('tree',{}).get('sha')!=checkout.get('tree')
            or not re.fullmatch('[0-9a-f]{40}',checkout.get('tree',''))):
        raise ValueError('current live publisher source/context differs')
    if type(jobs.get('total_count')) is not int or not 0<jobs['total_count']<=100 or len(jobs.get('jobs',[]))!=jobs['total_count']:
        raise ValueError('complete bounded current-attempt jobs required')
    owned=[j for j in jobs['jobs'] if j.get('name')==expected_job]
    focused=[j for j in jobs['jobs'] if j.get('name')=='focused']
    if (len(owned)!=1 or len(focused)!=1
            or any(j.get('run_id')!=int(run_id) or j.get('run_attempt')!=int(attempt) or j.get('head_sha')!=source for j in [*owned,*focused])
            or owned[0].get('status')!='in_progress' or owned[0].get('conclusion') is not None
            or focused[0].get('status')!='completed' or focused[0].get('conclusion')!='success'):
        raise ValueError('current publisher and successful focused prerequisite required')
    if set(remote_blobs)!=set(publisher_sources()):raise ValueError('complete executing remote Git blobs required')
    root=Path(__file__).resolve().parents[2]
    blobs={}
    for name,remote in remote_blobs.items():
        data=(root/name).read_bytes()
        blob=hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()
        if (remote.get('path')!=name or remote.get('type')!='file' or remote.get('size')!=len(data)
                or remote.get('encoding')!='base64' or remote.get('sha')!=blob
                or base64.b64decode(remote.get('content',''),validate=False)!=data):
            raise ValueError('executing source differs from advertised remote Git blob')
        blobs[name]=blob
    return {'run_id':int(run_id),'run_attempt':int(attempt),'job_id':owned[0]['id'],
            'job':expected_job,'focused_job_id':focused[0]['id'],'checkout_tree':checkout['tree'],
            'executing_git_blobs':blobs}


def artifact_contract(artifact, run, source, name, limit):
    original_run(run, source, WORKFLOW)
    if (artifact.get('name') != name or artifact.get('expired') is not False
            or artifact.get('workflow_run',{}).get('id') != run['id']
            or artifact.get('workflow_run',{}).get('head_sha') != source
            or type(artifact.get('size_in_bytes')) is not int
            or not 0 < artifact['size_in_bytes'] <= limit
            or not re.fullmatch('sha256:[0-9a-f]{64}',artifact.get('digest',''))):
        raise ValueError('original tool artifact contract differs')


def check_zip(path, artifact):
    if Path(path).stat().st_size != artifact['size_in_bytes'] or 'sha256:'+sha(path) != artifact['digest']:
        raise ValueError('original artifact ZIP bytes differ')


def read_build_closure(path):
    with zipfile.ZipFile(path) as z:
        entries=z.infolist()
        if len(entries)!=1 or entries[0].filename!='tool-closure.json' or entries[0].file_size>128*1024:
            raise ValueError('bounded original single build closure required')
        return json.loads(z.read(entries[0]))


def immutable_build(closure, source):
    if (closure.get('source_commit')!=source or closure.get('registry_published') is not False
            or closure.get('smoke_passed') is not True
            or set(closure.get('source_files_sha256',{})) != SOURCES
            or any(not re.fullmatch('[0-9a-f]{64}',v) for v in closure.get('source_files_sha256',{}).values())
            or not re.fullmatch(re.escape(TARGET)+r'@sha256:[0-9a-f]{64}',closure.get('image',''))
            or not re.fullmatch('[0-9a-f]{64}',closure.get('oci_sha256',''))):
        raise ValueError('original unpublished actual build required')


def verify_oci(path, closure, *, deadline=None):
    """Read/hash original OCI blobs and decompressed diff IDs without extraction."""
    def budget():
        if deadline is not None and time.monotonic()>=deadline:
            raise TimeoutError('absolute OCI verification deadline')
    budget()
    if sha(path)!=closure['oci_sha256']:raise ValueError('original OCI archive bytes differ')
    with tarfile.open(path) as archive:
        entries=[]
        for entry in archive:
            budget();entries.append(entry)
            if len(entries)>512:raise ValueError('OCI member count exceeds bound')
        members={e.name:e for e in entries}
        if len(entries)!=len(members) or len(entries)>512 or any(
                e.name.startswith('/') or '..' in Path(e.name).parts or not(e.isfile() or e.isdir()) for e in entries):
            raise ValueError('bounded regular OCI members required')
        def metadata(name):
            if members[name].size>1024*1024:raise ValueError('OCI metadata exceeds bound')
            return archive.extractfile(name).read()
        index=json.loads(metadata('index.json'))
        if len(index['manifests'])!=1:raise ValueError('single original platform manifest required')
        descriptor=index['manifests'][0];digest=descriptor['digest']
        if closure['image']!=TARGET+'@'+digest:raise ValueError('original OCI manifest identity differs')
        def blob(value):return 'blobs/sha256/'+value.removeprefix('sha256:')
        raw=metadata(blob(digest))
        if type(descriptor.get('size')) is not int or descriptor['size']!=len(raw) or members[blob(digest)].size!=len(raw):
            raise ValueError('OCI manifest descriptor size differs')
        if 'sha256:'+hashlib.sha256(raw).hexdigest()!=digest:raise ValueError('OCI manifest bytes differ')
        manifest=json.loads(raw);config_raw=metadata(blob(manifest['config']['digest']))
        if 'sha256:'+hashlib.sha256(config_raw).hexdigest()!=manifest['config']['digest']:raise ValueError('OCI config bytes differ')
        if type(manifest['config'].get('size')) is not int or manifest['config']['size']!=len(config_raw) or members[blob(manifest['config']['digest'])].size!=len(config_raw):
            raise ValueError('OCI config descriptor size differs')
        config=json.loads(config_raw)
        if manifest!=closure['original_manifest'] or config!=closure['config']:raise ValueError('original closure metadata differs')
        diff_ids=config.get('rootfs',{}).get('diff_ids',[])
        if len(diff_ids)!=len(manifest['layers']) or len(diff_ids)!=len(closure['layers']):raise ValueError('original layer count differs')
        for i, layer in enumerate(manifest['layers']):
            if members[blob(layer['digest'])].size!=layer['size']:raise ValueError('original layer length differs')
            f=archive.extractfile(blob(layer['digest']))
            if 'sha256:'+hashlib.file_digest(f,'sha256').hexdigest()!=layer['digest']:raise ValueError('original layer SHA differs')
            f=archive.extractfile(blob(layer['digest']))
            if layer['mediaType'].endswith('+gzip'):f=gzip.GzipFile(fileobj=f)
            total=0;h=hashlib.sha256()
            while data:=f.read(1024*1024):
                budget();total+=len(data);h.update(data)
                if total>4*1024**3:raise ValueError('layer expansion exceeds bound')
            measured=closure['layers'][i]
            if ('sha256:'+h.hexdigest()!=diff_ids[i] or measured['diff_id']!=diff_ids[i]
                    or total!=measured['uncompressed_bytes'] or measured['compressed_bytes']!=layer['size']
                    or measured['digest']!=layer['digest']):raise ValueError('actual original diff-ID chain differs')
    return raw,config_raw


def anonymous_copy(directory, manifest_raw, config_raw, closure):
    """Require actual anonymous manifest AND all config/layer bytes."""
    directory=Path(directory)
    manifest_path=directory/'manifest.json'
    if manifest_path.is_symlink() or not manifest_path.is_file() or manifest_path.stat().st_size != len(manifest_raw):
        raise ValueError('anonymous manifest is not an exact regular file')
    if manifest_path.read_bytes()!=manifest_raw:raise ValueError('anonymous registry manifest differs')
    manifest=closure['original_manifest'];config=directory/manifest['config']['digest'][7:]
    if config.is_symlink() or not config.is_file() or config.stat().st_size != len(config_raw):
        raise ValueError('anonymous config is not an exact regular blob')
    if config.read_bytes()!=config_raw:raise ValueError('anonymous registry config differs')
    blobs={manifest['config']['digest']:sha(config)}
    for layer in manifest['layers']:
        p=directory/layer['digest'][7:]
        if p.is_symlink() or p.stat().st_size!=layer['size'] or 'sha256:'+sha(p)!=layer['digest']:
            raise ValueError('actual anonymous layer bytes differ')
        blobs[layer['digest']]=sha(p)
    return {'anonymous':True,'manifest_sha256':hashlib.sha256(manifest_raw).hexdigest(),
            'config_sha256':hashlib.sha256(config_raw).hexdigest(),'blob_sha256':blobs,
            'all_blobs_verified':True}


def publication_chain(publication, build_run, publisher_run, build_source, publisher_source, closure, *, publisher_commit, publisher_jobs):
    original_run(build_run,build_source,WORKFLOW);original_run(publisher_run,publisher_source,WORKFLOW)
    if (build_run['id']==publisher_run['id'] or publication.get('schema')!=1 or publication.get('completed') is not True
            or publication.get('built_source_commit')!=build_source
            or publication.get('publisher_source_commit')!=publisher_source
            or publication.get('build_run_id')!=build_run['id']
            or publication.get('publisher_run_id')!=publisher_run['id']
            or publication.get('anonymous_pull',{}).get('anonymous') is not True
            or publication.get('anonymous_pull',{}).get('all_blobs_verified') is not True
            or publication.get('private_scratch_removed') is not True
            or publication.get('registry_published') is not True
            or publication.get('image') != closure['image']
            or publication.get('original_oci_sha256') != closure['oci_sha256']
            or publication.get('registry_manifest_digest') != closure['image'].split('@')[1]
            or publication.get('mode') not in ('publish', 'verify-only')
            or publication.get('authenticated_copy_succeeded') is not (publication.get('mode') == 'publish')
            or publication.get('built_source_sha256') != closure['source_files_sha256']
            or publication.get('publisher_source_sha256') != publisher_sources()):
        raise ValueError('actual registered two-run publication chain required')
    admission=publication.get('current_publisher_admission',{})
    expected_job='node-tools-publish' if publication['mode']=='publish' else 'node-tools-anonymous-verify'
    root=Path(__file__).resolve().parents[2]
    expected_blobs={}
    for name in publisher_sources():
        data=(root/name).read_bytes()
        expected_blobs[name]=hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()
    if (admission.get('run_id')!=publisher_run['id']
            or admission.get('run_attempt')!=publisher_run.get('run_attempt')
            or type(admission.get('run_attempt')) is not int or admission['run_attempt']<=0
            or admission.get('job')!=expected_job or admission.get('executing_git_blobs')!=expected_blobs
            or publisher_commit.get('sha')!=publisher_source
            or not re.fullmatch('[0-9a-f]{40}',admission.get('checkout_tree',''))
            or admission['checkout_tree']!=publisher_commit.get('tree',{}).get('sha')):
        raise ValueError('original actual publisher admission receipt differs')
    if (type(publisher_jobs.get('total_count')) is not int or not 0<publisher_jobs['total_count']<=100
            or len(publisher_jobs.get('jobs',[]))!=publisher_jobs['total_count']):
        raise ValueError('complete original publisher attempt jobs required')
    for name,key in ((expected_job,'job_id'),('focused','focused_job_id')):
        matches=[j for j in publisher_jobs['jobs'] if j.get('name')==name]
        if (len(matches)!=1 or matches[0].get('id')!=admission.get(key)
                or matches[0].get('run_id')!=publisher_run['id']
                or matches[0].get('run_attempt')!=admission['run_attempt']
                or matches[0].get('head_sha')!=publisher_source
                or matches[0].get('status')!='completed' or matches[0].get('conclusion')!='success'):
            raise ValueError('original publisher and focused jobs did not succeed')
    manifest=closure['original_manifest']
    expected={d['digest']:d['digest'][7:] for d in [manifest['config'], *manifest['layers']]}
    anonymous=publication['anonymous_pull']
    if (anonymous.get('blob_sha256') != expected
            or anonymous.get('manifest_sha256') != closure['image'].split('@sha256:')[1]
            or anonymous.get('config_sha256') != manifest['config']['digest'][7:]):
        raise ValueError('anonymous receipt does not bind every original blob')


def publication_members(path):
    """Read bounded small custody files; preserve original nested build ZIP bytes."""
    names={'publication.json','build-artifact-api.json','originals-artifact-api.json','build-closure.zip'}
    with zipfile.ZipFile(path) as archive:
        entries=archive.infolist()
        if (len(entries)!=len(names) or {e.filename for e in entries}!=names
                or any(e.file_size>1024*1024 or stat.S_ISLNK(e.external_attr>>16) for e in entries)):
            raise ValueError('bounded exact publication artifact members required')
        return {e.filename:archive.read(e) for e in entries}
