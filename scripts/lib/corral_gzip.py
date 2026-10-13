"""Separate RAW.gzip transport admission; old RAW/QCOW proofs cannot authorize it."""
import hashlib
import json
from pathlib import Path
import re
from corral_perf import GIB, kubectl
import tuna_raw_gzip as gzip
from corral_raw import UPLOAD_IMAGE


def disk_plan(manifest, proof):
    raw = manifest.get('raw_gzip', {})
    expected = gzip.storage(raw)
    if manifest.get('disk_format') != 'raw-gzip' or manifest.get('import_method') != gzip.METHOD or manifest.get('requested_pvc') != '20Gi' or manifest.get('storage') != expected or manifest.get('firmware') != 'uefi':
        raise ValueError('distinct complete RAW.gzip 20Gi UEFI manifest required')
    if raw.get('gzip_sha256') != manifest.get('disk_sha256') or raw.get('raw_sha256') != manifest.get('raw_sha256'):
        raise ValueError('gzip transfer/original RAW identity differs')
    if not proof or proof.get('method') != gzip.METHOD or proof.get('full_image_import_qualified') is not False or not re.fullmatch('[0-9a-f]{64}', proof.get('original_probe_evidence_sha256', '')):
        raise ValueError('separate actual gzip method proof required')
    return {'method': gzip.METHOD, 'target_pvc_bytes':20*GIB, 'scratch_pvc_bytes':20*GIB,
            'scratch_physical_metadata_allowance_bytes':GIB, 'runtime_layer_allowance_bytes':GIB,
            'guest_write_allowance_bytes':GIB//2, 'estimated_peak_bytes':20*GIB+2*GIB+GIB//2,
            'raw_method_proof':proof, 'full_image_import_qualified':False,
            'performance_qualified':False, 'node_peak_qualified':False}


def evidence_sha(directory):
    directory = Path(directory)
    names = ['receipt.json', 'upload-observations.json', 'source/tuna-corral-gzip-upload-probe',
             'source/corral_gzip.py', 'source/tuna_raw_gzip.py', 'source/corral_session.py',
             'source/corral_owned.py', 'source/corral_perf.py', 'source/corral_ssh.py',
             'source/corral_gzip_supervisor.py', 'process-owner.json']
    return hashlib.sha256(json.dumps({n:hashlib.sha256((directory/n).read_bytes()).hexdigest()
                                     for n in names},sort_keys=True).encode()).hexdigest()


def proof(directory, context, node, bundle_sha):
    directory = Path(directory)
    if evidence_sha(directory) != bundle_sha:
        raise ValueError('original gzip probe bundle SHA differs')
    r = json.loads((directory/'receipt.json').read_bytes())
    if r.get('method') != gzip.METHOD or r.get('context') != context or r.get('completed') is not True or r.get('cleanup',{}).get('all_owned_resources_absent') is not True or r.get('cleanup',{}).get('original_owned_pvs_absent') is not True:
        raise ValueError('completed separate gzip probe required')
    owner=json.loads((directory/'process-owner.json').read_bytes())
    if owner.get('whole_command_retirement_confirmed') is not True or owner.get('private_context_projection_removed_on_close') is not True or owner.get('context')!=context:raise ValueError('actual full probe command ownership missing')
    source = directory/'source'
    expected_sources={'tuna-corral-gzip-upload-probe','corral_gzip.py','tuna_raw_gzip.py','corral_session.py','corral_owned.py','corral_perf.py','corral_ssh.py','corral_gzip_supervisor.py'}
    if set(r.get('source_sha256',{}))!=expected_sources:raise ValueError('complete original gzip source custody required')
    gz=r.get('gzip',{})
    if gz.get('raw_bytes')!=64*1024**2 or gz.get('raw_sha256')!=r.get('raw_sha256') or gz.get('single_member_verified') is not True or not re.fullmatch('[0-9a-f]{64}',gz.get('gzip_sha256','')):raise ValueError('actual complete tiny gzip verification missing')
    for name, sha in r['source_sha256'].items():
        if Path(name).name != name or hashlib.sha256((source/name).read_bytes()).hexdigest() != sha:
            raise ValueError('probe original source differs')
        current = Path(__file__).parent/name if name.endswith('.py') else Path(__file__).parent.parent/name
        if hashlib.sha256(current.read_bytes()).hexdigest() != sha:
            raise ValueError('probe must qualify this exact observer source')
    rb = r.get('readback', {})
    if rb.get('matched') is not True or rb.get('prefix_bytes') != 64*1024**2 or rb.get('sha256') != r.get('raw_sha256') or rb.get('node') != node or rb.get('pvc_uid') != r.get('pvc_uid'):
        raise ValueError('gzip probe full readback identity differs')
    actual = kubectl(context,'get','node',node,'-o','json')
    if actual['metadata']['uid'] != r.get('node_uid'):
        raise ValueError('gzip node replaced')
    config = kubectl(context,'get','configmap','local-path-config','-n','local-path-storage','-o','json')
    if hashlib.sha256(json.dumps(config.get('data',{}),sort_keys=True).encode()).hexdigest() != r.get('local_path_config_sha256') or config.get('data',{}).get('setup') != '#!/bin/sh\nset -eu\nmkdir -m 0777 -p "$VOL_DIR"':
        raise ValueError('gzip directory-only backend changed')
    import shutil
    if hashlib.sha256(Path(shutil.which('virtctl')).read_bytes()).hexdigest() != r.get('virtctl_sha256'):
        raise ValueError('gzip client bytes changed')
    items=kubectl(context,'get','cdi','-o','json')['items']
    if len(items)!=1 or any(items[0].get('status',{}).get(k)!='v1.66.1' for k in ('targetVersion','observedVersion','operatorVersion')):
        raise ValueError('gzip CDI version differs')
    observations=json.loads((directory/'upload-observations.json').read_bytes())
    running=[o for o in observations if o.get('phase')=='Running' and o.get('directory_usage_returncode')==0]
    if not running or any(o.get('node')!=node or o.get('preallocation')!='false' or not re.search(r'^0\s+/scratch$',o.get('directory_usage',''),re.M) or not any(c.get('imageID')==UPLOAD_IMAGE for c in o.get('container_statuses',[])) for o in running):
        raise ValueError('gzip server/scratch observations missing')
    return {'method':gzip.METHOD,'original_probe_evidence_sha256':bundle_sha,
            'original_probe_receipt_sha256':hashlib.sha256((directory/'receipt.json').read_bytes()).hexdigest(),
            'node_uid':r['node_uid'],'upload_image_id':UPLOAD_IMAGE,
            'local_path_config_sha256':r['local_path_config_sha256'], 'full_image_import_qualified':False}


def target_capacity(context, namespace, pvc_name, pvc_uid, node, out):
    """Observe actual mounted free bytes BEFORE upload, with an owned read-only pod."""
    import subprocess
    import time
    import uuid
    import corral_owned
    deadline=time.monotonic()+120
    while time.monotonic()<deadline:
        claim=kubectl(context,'get','pvc',pvc_name,'-n',namespace,'-o','json',deadline=deadline)
        if claim['metadata']['uid']!=pvc_uid:
            raise ValueError('gzip target UID replaced before capacity observation')
        spec=claim['spec']
        if spec.get('volumeMode')!='Filesystem' or spec.get('storageClassName')!='local-path' or spec.get('resources',{}).get('requests',{}).get('storage')!='20Gi':
            raise ValueError('actual gzip target differs from admitted 20 GiB filesystem request')
        if claim['status'].get('phase')=='Bound':
            break
        time.sleep(min(.2,max(0,deadline-time.monotonic())))
    else:
        raise TimeoutError('gzip target binding deadline')
    pv=corral_owned.placement(context,namespace,claim,node)
    config=kubectl(context,'get','cdiconfig','config','-o','json')
    observed=config.get('status',{}).get('filesystemOverhead',{})
    overhead=observed.get('storageClass',{}).get(claim['spec']['storageClassName'],observed.get('global'))
    if not isinstance(overhead,str) or not re.fullmatch(r'0(?:\.[0-9]+)?',overhead):
        raise ValueError('effective actual CDI filesystem overhead unavailable')
    name='tuna-gzip-space-'+uuid.uuid4().hex[:10]
    image='alpine@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b'
    pod={'apiVersion':'v1','kind':'Pod','metadata':{'name':name,'namespace':namespace,'ownerReferences':[{'apiVersion':'v1','kind':'PersistentVolumeClaim','name':pvc_name,'uid':pvc_uid,'controller':False}]},'spec':{'restartPolicy':'Never','automountServiceAccountToken':False,'nodeSelector':{'kubernetes.io/hostname':node},'securityContext':{'runAsUser':107,'runAsGroup':107,'runAsNonRoot':True,'seccompProfile':{'type':'RuntimeDefault'}},'containers':[{'name':'reader','image':image,'command':['sh','-c','set -eu; test ! -e /data/disk.img; df -Pk /data'], 'securityContext':{'allowPrivilegeEscalation':False,'capabilities':{'drop':['ALL']}},'resources':{'requests':{'cpu':'20m','memory':'32Mi'},'limits':{'memory':'128Mi'}},'volumeMounts':[{'name':'disk','mountPath':'/data','readOnly':True}]}],'volumes':[{'name':'disk','persistentVolumeClaim':{'claimName':pvc_name,'readOnly':True}}]}}
    owned=[];out=Path(out)
    try:
        actual=corral_owned.create(context,namespace,'pod',pod,out/'target-space-pod.json',uuid.uuid4().hex,lambda *x:owned.append(x))
        uid=actual['metadata']['uid']
        while time.monotonic()<deadline:
            actual=kubectl(context,'get','pod',name,'-n',namespace,'-o','json',deadline=deadline)
            if actual['metadata']['uid']!=uid or actual['spec'].get('nodeName') not in (None,node):
                raise ValueError('gzip target capacity pod replaced or misplaced')
            if actual['status'].get('phase') in ('Failed','Succeeded'):
                break
            time.sleep(.2)
        else:
            raise TimeoutError('gzip target capacity observation deadline')
        images=[c.get('imageID','').split('://')[-1] for c in actual['status'].get('containerStatuses',[])]
        if actual['status'].get('phase')!='Succeeded' or images!=[image]:
            raise ValueError('target capacity observer execution/image differs')
        logs=subprocess.check_output(['kubectl','--context',context,'logs',name,'-n',namespace],timeout=min(20,max(.01,deadline-time.monotonic()))).decode()
        rows=logs.splitlines()
        if len(rows)!=2 or rows[1].split()[-1]!='/data':
            raise ValueError('target filesystem observation malformed')
        available=int(rows[1].split()[3])*1024
        usable=gzip.usable_target(min(available,20*GIB),float(overhead))
        current=kubectl(context,'get','pvc',pvc_name,'-n',namespace,'-o','json',deadline=deadline)
        current_config=kubectl(context,'get','cdiconfig','config','-o','json',deadline=deadline)
        if current['metadata']['uid']!=pvc_uid or current_config!=config or time.monotonic()>=deadline:
            raise ValueError('target/config identity changed during observation')
        if corral_owned.placement(context,namespace,current,node)!=pv:
            raise ValueError('target PV identity changed')
        receipt={'pvc_uid':pvc_uid,'pv_uid':pv['metadata']['uid'],'node':node,'pod_uid':uid,
                 'observer_image_id':image,'available_bytes':available,'filesystem_overhead':overhead,
                 'usable_bytes':usable,'minimum_required_bytes':gzip.RAW_BYTES+GIB//2,
                 'cdiconfig':config,'observed_before_upload':True}
        (out/'target-capacity.json').write_text(json.dumps(receipt,indent=2))
        return receipt
    finally:
        if owned:
            corral_owned.cleanup(context,namespace,owned,timeout=60)


def uploader_observation(pod, pvc_uid, node):
    """Admit only the actual owned uploader; never retain token-bearing environment."""
    if not any(o.get('uid')==pvc_uid for o in pod['metadata'].get('ownerReferences',[])):
        raise ValueError('gzip upload pod owner differs')
    if pod['spec'].get('nodeName')!=node:
        raise ValueError('gzip upload pod placement differs')
    containers=pod['spec'].get('containers',[])
    if len(containers)!=1 or pod['spec'].get('initContainers') or pod['spec'].get('ephemeralContainers'):
        raise ValueError('unexpected gzip uploader containers')
    values=[e.get('value') for e in containers[0].get('env',[]) if e.get('name')=='PREALLOCATION']
    if values!=['false']:
        raise ValueError('gzip upload preallocation is not false')
    statuses=pod['status'].get('containerStatuses',[])
    if len(statuses)!=1 or statuses[0].get('imageID')!=UPLOAD_IMAGE:
        raise ValueError('gzip actual uploader image differs')
    if statuses[0].get('restartCount')!=0:
        raise ValueError('gzip uploader restarted')
    return {'pod':pod['metadata']['name'],'pod_uid':pod['metadata']['uid'],
            'node':node,'phase':pod['status'].get('phase'),'preallocation':'false',
            'volumes':pod['spec'].get('volumes'), 'container_statuses':statuses}


def recover_probe_cleanup(context, namespace, directory):
    """Recover an ambiguous create only by its durably saved unpredictable nonce."""
    import corral_owned
    import os
    expected_namespace=os.environ.get('TUNA_GZIP_NAMESPACE_UID')
    if not expected_namespace or kubectl(context,'get','namespace',namespace,'-o','json')['metadata']['uid']!=expected_namespace:
        raise ValueError('gzip namespace replaced before owned recovery')
    directory=Path(directory)
    owned=json.loads((directory/'owned-objects.json').read_text()) if (directory/'owned-objects.json').exists() else []
    request_path=directory/'pvc-create.json'
    if request_path.exists():
        request=json.loads(request_path.read_text())
        md=request['metadata'];nonce=md.get('labels',{}).get('tuna-perf-nonce')
        if md.get('namespace')!=namespace or not re.fullmatch('[0-9a-f]{32}',nonce or ''):
            raise ValueError('ambiguous gzip create has no trusted owned nonce')
        current=corral_owned.get(context,namespace,'pvc',md['name'])
        if current is not None:
            if current['metadata'].get('labels',{}).get('tuna-perf-nonce')!=nonce:
                raise ValueError('ambiguous gzip create resolved to foreign resource')
            identity=['pvc',md['name'],current['metadata']['uid']]
            if identity not in owned:owned.append(identity)
    if not owned:
        return {'all_owned_resources_absent':True,'no_created_resource_observed':True}
    return corral_owned.cleanup(context,namespace,owned,timeout=120)


def fresh_capacity(context, node_name, estimate, *, deadline=None):
    """Add observed kubelet available-space thresholds to the retained reserve."""
    from decimal import Decimal, ROUND_CEILING
    from corral_perf import capacity_receipt
    node=kubectl(context,'get','node',node_name,'-o','json',deadline=deadline)
    stats=kubectl(context,'get','--raw','/api/v1/nodes/'+node_name+'/proxy/stats/summary',deadline=deadline)
    config=kubectl(context,'get','--raw','/api/v1/nodes/'+node_name+'/proxy/configz',deadline=deadline)
    settings=config.get('kubeletconfig',{})
    hard=settings.get('evictionHard');soft=settings.get('evictionSoft',{})
    if not isinstance(hard,dict) or not isinstance(soft,dict) or not {'nodefs.available','imagefs.available'}<=set(hard):
        raise ValueError('actual kubelet disk thresholds unavailable')
    percentage=Decimal(15)
    for key,value in list(hard.items())+list(soft.items()):
        if key.endswith('.available'):
            if not isinstance(value,str) or not re.fullmatch(r'(?:[0-9]|[1-9][0-9])(?:\.[0-9]+)?%',value):
                raise ValueError('unsupported actual kubelet available-space threshold')
            percentage=max(percentage,Decimal(value[:-1]))
    receipt=capacity_receipt(node,stats,estimated_peak_bytes=estimate)
    runtime=stats.get('node',{}).get('runtime',{})
    pools={'node':stats.get('node',{}).get('fs'),'image':runtime.get('imageFs'),'container':runtime.get('containerFs')}
    budgets=[]
    for name,pool in pools.items():
        if not pool or type(pool.get('availableBytes')) is not int or type(pool.get('capacityBytes')) is not int:
            raise ValueError('actual gzip filesystem pool missing')
        reserve=int((Decimal(pool['capacityBytes'])*percentage/100).to_integral_value(rounding=ROUND_CEILING))+GIB
        budgets.append(max(0,pool['availableBytes']-reserve))
    receipt['minimum_available_disk_headroom_bytes']=min(budgets)
    receipt['configured_thresholds']={'evictionHard':hard,'evictionSoft':soft,'reserved_percent':str(percentage)}
    receipt['reserve']['configured_thresholds_verified']=True
    if estimate>min(budgets):
        receipt['ready_for_disk_admission']=False
        receipt['reasons'].append('actual-kubelet-threshold-headroom-insufficient')
    return node,receipt
