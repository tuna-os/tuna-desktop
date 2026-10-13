"""Synthetic node staging, never a full-image admission receipt."""
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile
import stat
import io
from urllib.parse import urlsplit
import sys
import time
import uuid
import importlib.machinery
import importlib.util

from corral_perf import GIB, capacity_receipt
import corral_node_stage as stage
import corral_node_worker as worker

MAX_ZIP = 2*1024*1024
START = b'TUNA_NODE_STAGE_SMOKE'+b'\xff'*65536
MIDDLE = b'NONZERO_MIDDLE'
MIDDLE_OFFSET = 8*GIB


def expected_raw_sha(virtual=worker.VIRTUAL, middle=MIDDLE_OFFSET, deadline=None):
    """Hash known sparse content in 1 MiB blocks; never create/download a RAW."""
    if not len(START)<=middle or middle+len(MIDDLE)>virtual:
        raise ValueError('nonoverlapping bounded known pattern required')
    h=hashlib.sha256();h.update(START)
    zero=b'\0'*(1024*1024)
    def zeros(count):
        while count:
            if deadline is not None and time.monotonic()>=deadline:raise TimeoutError('pattern SHA deadline')
            n=min(count,len(zero));h.update(zero[:n]);count-=n
    zeros(middle-len(START));h.update(MIDDLE);zeros(virtual-middle-len(MIDDLE))
    return h.hexdigest()


def tiny_storage(closure):
    tool=stage.tool_contract(closure,require_published=False)
    return {'stage_pvc_bytes':GIB,'target_pvc_bytes':GIB,'host_zip_limit_bytes':MAX_ZIP,
            'tool_closure_allowance_bytes':tool,'guest_write_allowance_bytes':GIB//2,
            'estimated_peak_bytes':2*GIB+tool+GIB//2,
            'logical_fixture_bytes':worker.VIRTUAL,'container_memory_limit_bytes':512*1024*1024,
            'node_available_memory_required_bytes':GIB+512*1024*1024,'full_20gib_reservation_qualified':False,
            'dense_decoder_memory_qualified':False,'full_node_method_qualified':False}


def digest_negative(data, length, expected, path, receiver=worker.receive):
    """Only a complete known archive can establish the wrong-digest control."""
    if not 0 < length <= MAX_ZIP or len(data) != length or hashlib.sha256(data).hexdigest() != expected:
        raise ValueError('negative input is truncated or differs from original archive')
    try:
        receiver(io.BytesIO(data), path, length, '0'*64)
    except ValueError as error:
        if str(error) != 'original archive length or SHA differs':raise
        if Path(path).stat().st_size != length or worker.digest(path) != expected:
            raise ValueError('negative receiver did not retain complete known bytes')
        return {'negative_control':'wrong-archive-digest','rejected':True}
    raise ValueError('wrong digest admitted')


REMOTE = r'''
import hashlib,importlib.util,json,os,subprocess,sys,time,zipfile,io
from pathlib import Path
sys.path.insert(0,'/opt/tuna-tools')
import corral_node_tools as tools
import corral_node_worker as worker
operation=sys.argv[1]
if os.getuid()!=107:raise ValueError('actual runtime UID differs')
qemu=worker.digest('/usr/bin/qemu-img')
if qemu!='250b2b4ee552fdd96e1078525d12bb0b1f2557f33f153c282bfaea1ece5b3fad':raise ValueError('qemu bytes differ')
before=tools.qemu_libraries();tools.same_qemu_libraries(tools.baseline_libraries('/opt/tuna-tools/qemu-before-libraries.sha256'),before)
def memory():
 p=Path('/sys/fs/cgroup/memory.peak');limit=Path('/sys/fs/cgroup/memory.max')
 if not p.is_file() or not limit.is_file():return {'status':'unsupported','reason':'cgroup-v2 peak/limit unavailable'}
 peak=p.read_text().strip();bound=limit.read_text().strip()
 if not peak.isdecimal() or not bound.isdecimal():raise ValueError('missing numeric cgroup limit/peak')
 if int(bound)!=512*1024*1024 or not 0<int(peak)<=int(bound):raise ValueError('actual memory limit/peak differs')
 return {'status':'observed','peak_bytes':int(peak),'limit_bytes':int(bound),'scope':'Whole owned container since start, including fixture setup; not standalone decoder RSS.'}
pre_memory=memory()
if operation=='fixture':
 root=Path('/stage/fixture');root.mkdir(mode=0o700,exist_ok=False)
 raw=root/'original.raw'
 with raw.open('xb') as f:
  f.write(b'TUNA_NODE_STAGE_SMOKE'+b'\xff'*65536);f.seek(8*1024**3);f.write(b'NONZERO_MIDDLE');f.truncate(worker.VIRTUAL)
 if raw.stat().st_blocks*512>1024*1024:raise ValueError('synthetic RAW allocation exceeds bound')
 qcow=root/'disk.qcow2'
 subprocess.run(['qemu-img','convert','-f','raw','-O','qcow2',str(raw),str(qcow)],check=True,timeout=120)
 if qcow.stat().st_size>1024*1024:raise ValueError('synthetic QCOW exceeds tiny bound')
 manifest={'disk_sha256':worker.digest(qcow),'raw_sha256':worker.digest(raw),
           'evidence':{'fixture.json':hashlib.sha256(b'{"kind":"synthetic-known-pattern"}').hexdigest()},
           'scope':'Synthetic transport only; not an installed image.'}
 (root/'manifest.json').write_text(json.dumps(manifest));(root/'fixture.json').write_bytes(b'{"kind":"synthetic-known-pattern"}')
 archive=Path('/stage/fixture.zip')
 with zipfile.ZipFile(archive,'x',zipfile.ZIP_DEFLATED) as z:
  for name in ('disk.qcow2','manifest.json','fixture.json'):z.write(root/name,name)
 if archive.stat().st_size>2*1024*1024:raise ValueError('synthetic ZIP exceeds bound')
 result={'zip_bytes':archive.stat().st_size,'zip_sha256':worker.digest(archive),
         'fixture_raw_allocated_bytes':raw.stat().st_blocks*512,'qcow_bytes':qcow.stat().st_size,
         'qcow_allocated_bytes':qcow.stat().st_blocks*512,**manifest}
elif operation=='download':
 p=Path('/stage/fixture.zip')
 if p.stat().st_size>2*1024*1024:raise ValueError('ZIP exceeds transport limit')
 with p.open('rb') as f:
  while data:=f.read(65536):sys.stdout.buffer.write(data)
 sys.stdout.buffer.flush();sys.exit(0)
elif operation=='receive-negative':
 length=int(sys.argv[2]);expected=sys.argv[3]
 if not 0<length<=2*1024*1024:raise ValueError('negative archive size invalid')
 data=sys.stdin.buffer.read(length+1)
 if len(data)!=length or hashlib.sha256(data).hexdigest()!=expected:raise ValueError('negative input is truncated or differs from original archive')
 try:worker.receive(io.BytesIO(data),'/stage/negative.zip',length,'0'*64)
 except ValueError as error:
  if str(error)!='original archive length or SHA differs':raise
  if Path('/stage/negative.zip').stat().st_size!=length or worker.digest('/stage/negative.zip')!=expected:raise ValueError('negative retained bytes differ')
  result={'negative_control':'wrong-archive-digest','rejected':True}
 else:raise ValueError('wrong digest admitted')
elif operation=='receive':
 worker.receive(sys.stdin.buffer,'/stage/original-artifact.zip',int(sys.argv[2]),sys.argv[3]);worker.extract('/stage/original-artifact.zip','/stage/export')
 root=Path('/stage/export');manifest=json.loads((root/'manifest.json').read_text())
 if worker.digest(root/'fixture.json')!=manifest['evidence']['fixture.json']:raise ValueError('fixture evidence differs')
 result={'manifest':manifest,'zip_sha256':worker.digest('/stage/original-artifact.zip'),'disk_sha256':worker.digest(root/'disk.qcow2')}
elif operation=='convert':
 result=worker.convert('/stage/export','/target/disk.img',time.monotonic()+float(sys.argv[2]))
 if result['allocated_bytes']>16*1024*1024:raise ValueError('synthetic RAW allocated beyond tiny control')
elif operation=='readback':
 p=Path('/target/disk.img')
 if p.stat().st_size!=worker.VIRTUAL:raise ValueError('readback length differs')
 result={'raw_sha256':worker.digest(p),'logical_bytes':p.stat().st_size,'allocated_bytes':p.stat().st_blocks*512}
elif operation=='usage':result=worker.usage(['/stage','/target'])
else:raise ValueError('unknown tiny operation')
after=tools.qemu_libraries();tools.same_qemu_libraries(before,after)
print(json.dumps({'uid':os.getuid(),'worker_sha256':worker.digest('/opt/tuna-tools/corral_node_worker.py'),
                  'qemu_sha256':qemu,'qemu_libraries_before':before,'qemu_libraries_after':after,
                  'python_executable':str(Path(sys.executable).resolve()),
                  'python_libraries_observed':{str(p):tools.sha(p) for p in tools.private_elf_paths(Path('/proc/self/maps').read_text())},
                  'memory_before':pre_memory,'memory_after':memory(),'result':result}))
'''


def runtime_receipt(value, closure):
    if (value.get('uid')!=107 or value.get('worker_sha256')!=closure['worker_sha256']
            or value.get('qemu_sha256')!=stage.QEMU_SHA
            or value.get('qemu_libraries_before')!=closure['qemu_libraries']
            or value.get('qemu_libraries_after')!=closure['qemu_libraries']):
        raise ValueError('actual tool runtime source/UID/ELF closure differs')
    python=value.get('python_libraries_observed',{})
    if (value.get('python_executable')!=closure['python_executable'] or not python
            or any(closure['python_libraries'].get(name)!=digest for name,digest in python.items())):
        raise ValueError('actual private Python closure differs')
    for key in ('memory_before','memory_after'):
        memory=value.get(key,{})
        if memory.get('status')=='unsupported':
            if not memory.get('reason'):raise ValueError('missing memory observability reason')
        elif (memory.get('status')!='observed' or type(memory.get('peak_bytes')) is not int
              or memory.get('limit_bytes')!=512*1024*1024 or not 0<memory['peak_bytes']<=memory['limit_bytes']):
            raise ValueError('actual container cgroup memory observation differs')
    return value['result']


class Probe:
    def __init__(self,args):
        self.args=args;self.out=args.out;self.deadline=time.monotonic()+900
        self.cleanup_until=self.deadline+180
        self.nonce=uuid.uuid4().hex;self.prefix='tuna-node-tiny-'+self.nonce[:12]
        self.resources=[];self.claims=[];self.pvs={};self.observations=[];self.readonly=set();self.unresolved=[]
        self.receipt={'schema':1,'method':'node-stage-tiny-v1','completed':False,
                      'context':args.context,'namespace':args.namespace,'node':args.node,
                      'full_node_method_qualified':False,'full_image_import_qualified':False,
                      'dense_decoder_memory_qualified':False,'performance_qualified':False}
        loader=importlib.machinery.SourceFileLoader('owned_command',str(Path(__file__).resolve().parents[1]/'tuna-corral-node-tools-publish'))
        spec=importlib.util.spec_from_loader(loader.name,loader);module=importlib.util.module_from_spec(spec);loader.exec_module(module)
        self.bounded=module.bounded_command
        self.private=None

    def remaining(self):
        value=self.deadline-time.monotonic()
        if value<=0:raise TimeoutError('absolute tiny operation deadline')
        return value

    def record(self,name,value):
        (self.out/name).write_text(json.dumps(value,indent=2)+'\n')

    def command(self,argv, *, input_path=None, limit=4*1024*1024, deadline=None, credentials=False, output_path=None):
        # Reuse the reviewed whole-command supervisor without modifying it.
        if input_path:
            argv=[sys.executable,'-c','import json,os,sys;f=os.open(sys.argv[1],os.O_RDONLY|os.O_NOFOLLOW);os.dup2(f,0);os.close(f);a=json.loads(sys.argv[2]);os.execvp(a[0],a)',str(input_path),json.dumps(argv)]
        path=output_path or self.out/'last-command-output'
        env=dict(os.environ) if credentials else {k:v for k,v in os.environ.items() if k not in ('GH_TOKEN','GITHUB_TOKEN','NODE_TOOL_TOKEN')}
        if self.private and hasattr(self,'routing_sha'):env['KUBECONFIG']=str(self.private/'config')
        self.bounded(argv,path,deadline or self.deadline,limit=limit,env=env)
        return path.read_bytes()

    def project_routing(self):
        self.private=Path(tempfile.mkdtemp(prefix='tuna-node-routing-'))
        path=self.private/'config'
        path.touch(mode=0o600)
        raw=self.command(['kubectl','--context',self.args.context,'config','view','--raw','--minify','--flatten','-o','json'],output_path=path)
        value=json.loads(raw)
        if any(len(value.get(k,[]))!=1 for k in ('contexts','clusters','users')):
            raise ValueError('private routing must contain exactly one context/cluster/user')
        context=value['contexts'][0];cluster=value['clusters'][0];user=value['users'][0]
        endpoint=urlsplit(cluster['cluster'].get('server',''))
        if (context['name']!=self.args.context or value.get('current-context')!=self.args.context
                or context['context'].get('cluster')!=cluster['name'] or context['context'].get('user')!=user['name']
                or endpoint.scheme!='https' or not endpoint.hostname or endpoint.username or endpoint.password
                or cluster['cluster'].get('certificate-authority')
                or any(user.get('user',{}).get(k) for k in ('client-certificate','client-key','tokenFile'))
                or cluster['cluster'].get('insecure-skip-tls-verify')
                or user.get('user',{}).get('exec') or user.get('user',{}).get('auth-provider')):
            raise ValueError('private routing is not a fixed authenticated TLS projection')
        self.routing_sha=hashlib.sha256(raw).hexdigest()
        self.receipt['private_routing_sha256']=self.routing_sha

    def close_routing(self):
        if self.private:
            (self.private/'config').unlink(missing_ok=True);self.private.rmdir();self.private=None

    def kube_argv(self,*argv):
        if not self.private:raise ValueError('private routing unavailable')
        path=self.private/'config'
        if (path.is_symlink() or not stat.S_ISREG(path.stat().st_mode) or path.stat().st_uid!=os.getuid()
                or path.stat().st_mode&0o777!=0o600 or self.private.stat().st_mode&0o777!=0o700
                or self.private.stat().st_uid!=os.getuid() or stage.sha(path)!=self.routing_sha):
            raise ValueError('private routing changed')
        return ['kubectl','--kubeconfig',str(path),'--context',self.args.context,*argv]

    def api(self,*argv,input_path=None,recover=False):
        deadline=min(self.deadline,self.cleanup_until,time.monotonic()+30) if recover else self.deadline
        raw=self.command(self.kube_argv(*argv),input_path=input_path,deadline=deadline)
        return json.loads(raw) if raw.strip() else None

    def routing_identity(self):
        for name,key in (('kube-system','cluster_uid'),(self.args.namespace,'namespace_uid')):
            if self.api('get','namespace',name,'-o','json')['metadata']['uid']!=self.receipt[key]:
                raise ValueError('cluster/namespace identity changed before deletion')

    def discover_pvs(self):
        claims={uid for kind,name,uid in self.resources if kind=='pvc'}
        for kind,name,uid in self.resources:
            if kind!='pvc':continue
            claim=self.api('get','pvc',name,'-n',self.args.namespace,'--ignore-not-found','-o','json')
            if claim and claim['metadata']['uid']!=uid:raise ValueError('foreign PVC replacement during cleanup')
            if claim and claim.get('spec',{}).get('volumeName'):
                pv=self.api('get','pv',claim['spec']['volumeName'],'-o','json')
                if pv['spec'].get('claimRef',{}).get('uid')!=uid:raise ValueError('bound PV claim identity differs')
                self.retain_pv(pv)
        for pv in self.api('get','pv','-o','json')['items']:
            ref=pv.get('spec',{}).get('claimRef',{})
            if ref.get('uid') in claims:
                if ref.get('namespace')!=self.args.namespace:raise ValueError('PV claim namespace differs')
                self.retain_pv(pv)
        self.record('owned-pvs.json',self.pvs)

    def retain_pv(self,pv):
        if pv['spec'].get('persistentVolumeReclaimPolicy')!='Delete':raise ValueError('PV reclamation unconfirmed')
        name=pv['metadata']['name'];uid=pv['metadata']['uid']
        if self.pvs.setdefault(name,uid)!=uid:raise ValueError('foreign PV replacement')

    def cleanup(self,objects,include_pvs=False):
        if include_pvs:self.discover_pvs()
        for kind,name,uid in objects:
            actual=self.api('get',kind,name,'-n',self.args.namespace,'--ignore-not-found','-o','json')
            if not actual:continue
            if actual['metadata']['uid']!=uid:raise ValueError('foreign replacement; not deleted')
            self.routing_identity()
            path=self.out/'delete-options.json'
            self.record(path.name,{'apiVersion':'v1','kind':'DeleteOptions','preconditions':{'uid':uid},'propagationPolicy':'Foreground'})
            plural={'pod':'pods','pvc':'persistentvolumeclaims'}[kind]
            self.api('delete','--raw','/api/v1/namespaces/'+self.args.namespace+'/'+plural+'/'+name,input_path=path)
        while True:
            self.remaining();absent=True
            if include_pvs:self.discover_pvs()
            for kind,name,uid in objects:
                actual=self.api('get',kind,name,'-n',self.args.namespace,'--ignore-not-found','-o','json')
                if actual:
                    if actual['metadata']['uid']!=uid:raise ValueError('foreign replacement after deletion')
                    absent=False
            if include_pvs:
                for name,uid in list(self.pvs.items()):
                    actual=self.api('get','pv',name,'--ignore-not-found','-o','json')
                    if actual:
                        if actual['metadata']['uid']!=uid:raise ValueError('foreign PV replacement; not deleted')
                        absent=False
            if absent:return {'all_owned_resources_absent':True,'owned_pvs_absent':include_pvs,'scope':'API object absence; not physical filesystem reclamation.'}
            time.sleep(min(.5,self.remaining()))

    def register(self,kind,name,uid):
        if (kind,name,uid) not in self.resources:self.resources.append((kind,name,uid))
        if (kind,name) in self.unresolved:self.unresolved.remove((kind,name))
        self.record('owned-objects.json',self.resources)

    def create(self,kind,obj):
        obj['metadata'].setdefault('labels',{})['tuna-perf-nonce']=self.nonce
        name=obj['metadata']['name'];path=self.out/(name+'.json');self.record(path.name,obj)
        self.unresolved.append((kind,name));self.record('attempted-creates.json',{'nonce':self.nonce,'names':self.unresolved})
        try:
            actual=self.api('create','-f','-','-o','json',input_path=path)
            if not isinstance(actual,dict) or not actual.get('metadata',{}).get('uid'):
                raise ValueError('ambiguous create response lacks UID')
        except BaseException:
            result=self.api('get',kind,name,'-n',self.args.namespace,'--ignore-not-found','-o','json',recover=True)
            if result:
                if result['metadata'].get('labels',{}).get('tuna-perf-nonce')!=self.nonce:raise ValueError('ambiguous create is foreign')
                self.register(kind,name,result['metadata']['uid'])
            raise
        if actual['metadata'].get('labels',{}).get('tuna-perf-nonce')!=self.nonce:raise ValueError('create nonce differs')
        self.register(kind,name,actual['metadata']['uid']);return actual

    def capacity(self):
        node=self.api('get','node',self.args.node,'-o','json')
        cluster=self.api('get','namespace','kube-system','-o','json')['metadata']['uid']
        namespace=self.api('get','namespace',self.args.namespace,'-o','json')['metadata']['uid']
        config=self.api('get','configmap','local-path-config','-n','local-path-storage','-o','json')['data']
        if cluster!=self.receipt['cluster_uid'] or namespace!=self.receipt['namespace_uid'] or node['metadata']['uid']!=self.receipt['node_uid'] or hashlib.sha256(json.dumps(config,sort_keys=True).encode()).hexdigest()!=self.receipt['local_path_config_sha256']:
            raise ValueError('node/storage backend changed')
        stats=self.api('get','--raw','/api/v1/nodes/'+self.args.node+'/proxy/stats/summary')
        value=capacity_receipt(node,stats,estimated_peak_bytes=self.plan['estimated_peak_bytes'])
        available=stats.get('node',{}).get('memory',{}).get('availableBytes')
        if type(available) is not int or available<self.plan['node_available_memory_required_bytes']:
            raise ValueError('actual node memory headroom unknown/insufficient for tiny pod')
        self.record('capacity-latest.json',{'node':node,'stats':stats,'admission':value})
        if not value['ready_for_disk_admission']:raise ValueError('full tiny storage reservation refused')

    def observe(self,pod,phase):
        actual=self.api('get','pod',pod['metadata']['name'],'-n',self.args.namespace,'-o','json')
        if actual['metadata']['uid']!=pod['metadata']['uid'] or actual['spec'].get('nodeName')!=self.args.node or actual['spec'].get('automountServiceAccountToken') is not False:
            raise ValueError('tool pod identity/placement/token policy differs')
        spec=actual['spec'];containers=spec.get('containers',[])
        if (spec.get('imagePullSecrets') or any(spec.get(k,False) for k in ('hostPID','hostIPC','hostNetwork'))
                or spec.get('initContainers') or spec.get('ephemeralContainers')
                or len(containers)!=1 or containers[0].get('image')!=self.closure['image']
                or containers[0].get('name')!='stage'
                or containers[0].get('resources')!={'requests':{'cpu':'100m','memory':'256Mi'},'limits':{'cpu':'1','memory':'512Mi'}}
                or any(spec.get('securityContext',{}).get(k)!=v for k,v in {'runAsUser':107,'runAsGroup':107,'fsGroup':107,'runAsNonRoot':True,'seccompProfile':{'type':'RuntimeDefault'}}.items())
                or containers[0].get('securityContext',{}).get('privileged',False)
                or any(containers[0].get('securityContext',{}).get(k,v)!=v for k,v in {'runAsUser':107,'runAsGroup':107,'runAsNonRoot':True,'seccompProfile':{'type':'RuntimeDefault'}}.items())
                or containers[0].get('securityContext',{}).get('capabilities',{}).get('add')
                or containers[0].get('securityContext',{}).get('allowPrivilegeEscalation') is not False
                or containers[0].get('securityContext',{}).get('readOnlyRootFilesystem') is not True
                or containers[0].get('securityContext',{}).get('capabilities',{}).get('drop')!=['ALL']
                or len(spec.get('volumes',[]))!=2 or any(set(v)!={'name','persistentVolumeClaim'} for v in spec['volumes'])):
            raise ValueError('actual unprivileged tool resource/volume policy differs')
        if {v['name']:v['persistentVolumeClaim'].get('claimName') for v in spec['volumes']}!={'stage':self.claims[0]['metadata']['name'],'target':self.claims[1]['metadata']['name']}:
            raise ValueError('tool mounts differ from owned claims')
        mounts=containers[0].get('volumeMounts',[])
        if {(m.get('name'),m.get('mountPath')) for m in mounts}!={('stage','/stage'),('target','/target')} or len(mounts)!=2 or any(m.get('subPath') or m.get('subPathExpr') for m in mounts):
            raise ValueError('owned mount geometry differs')
        if actual['metadata']['uid'] in self.readonly and (any(m.get('readOnly') is not True for m in mounts) or any(v['persistentVolumeClaim'].get('readOnly') is not True for v in spec['volumes'])):
            raise ValueError('independent readback mounts are not read-only')
        statuses=actual['status'].get('containerStatuses',[])
        if len(statuses)!=1 or statuses[0].get('imageID')!=self.closure['image'] or not statuses[0].get('ready'):
            raise ValueError('actual immutable ready tool image differs')
        entries=[]
        for expected in self.claims:
            claim=self.api('get','pvc',expected['metadata']['name'],'-n',self.args.namespace,'-o','json')
            if (claim['metadata']['uid']!=expected['metadata']['uid'] or claim['status'].get('phase')!='Bound'
                    or claim['spec'].get('resources',{}).get('requests',{}).get('storage')!='1Gi'
                    or claim['spec'].get('storageClassName')!='local-path' or claim['spec'].get('volumeMode')!='Filesystem'
                    or claim['spec'].get('accessModes')!=['ReadWriteOnce']):raise ValueError('PVC replaced/unbound/reservation differs')
            pv=self.api('get','pv',claim['spec']['volumeName'],'-o','json')
            ref=pv['spec'].get('claimRef',{})
            terms=pv['spec'].get('nodeAffinity',{}).get('required',{}).get('nodeSelectorTerms',[])
            if (ref.get('uid')!=claim['metadata']['uid'] or ref.get('namespace')!=self.args.namespace
                    or not terms or any(not any(e.get('key')=='kubernetes.io/hostname' and e.get('operator')=='In' and e.get('values')==[self.args.node] for e in t.get('matchExpressions',[])) for t in terms)
                    or pv['spec'].get('persistentVolumeReclaimPolicy')!='Delete'):
                raise ValueError('exclusive PV placement/reclaim differs')
            old=self.pvs.setdefault(pv['metadata']['name'],pv['metadata']['uid'])
            if old!=pv['metadata']['uid']:raise ValueError('PV replaced')
            entries.append({'pvc':claim,'pv':pv})
        self.observations.append({'phase':phase,'pod':actual,'claims':entries})
        self.record('identity-observations.json',self.observations);self.capacity()

    def wait(self,pod):
        while True:
            self.remaining()
            actual=self.api('get','pod',pod['metadata']['name'],'-n',self.args.namespace,'-o','json')
            if actual['metadata']['uid']!=pod['metadata']['uid']:raise ValueError('pod replaced during startup')
            if actual['status'].get('phase')=='Running':return self.observe(pod,'started')
            if actual['status'].get('phase') in ('Failed','Succeeded'):raise ValueError('pod exited before proof')
            time.sleep(min(.5,self.remaining()))

    def remote(self,pod,operation,*values,input_path=None,raw=False):
        self.observe(pod,'before-'+operation)
        argv=self.kube_argv('exec','-n',self.args.namespace,pod['metadata']['name'],'-c','stage')
        if input_path:argv.append('-i')
        argv+=['--','/opt/tuna-tools/python','-c',REMOTE,operation,*map(str,values)]
        data=self.command(argv,input_path=input_path,limit=MAX_ZIP if raw else 1024*1024)
        self.observe(pod,'after-'+operation)
        if raw:return data
        value=json.loads(data);runtime_receipt(value,self.closure)
        self.record(operation+'-result.json',value);return value['result']

    def pod(self,name,readonly=False):
        spec=stage.pod_spec(name,self.args.namespace,self.args.node,self.claims[0]['metadata']['name'],self.claims[1]['metadata']['name'],self.closure['image'],self.closure['worker_sha256'])
        if readonly:
            for m in spec['spec']['containers'][0]['volumeMounts']:m['readOnly']=True
            for v in spec['spec']['volumes']:v['persistentVolumeClaim']['readOnly']=True
        pod=self.create('pod',spec)
        if readonly:self.readonly.add(pod['metadata']['uid'])
        return pod

    def execute(self,closure):
        self.closure=closure;self.plan=tiny_storage(closure);self.record('source-sha256.json',{name:stage.sha(Path(__file__).with_name(name)) for name in ('corral_node_probe.py','corral_node_stage.py','corral_node_worker.py')});self.record('storage-plan.json',self.plan)
        node=self.api('get','node',self.args.node,'-o','json')
        cluster=self.api('get','namespace','kube-system','-o','json')['metadata']['uid']
        namespace=self.api('get','namespace',self.args.namespace,'-o','json')['metadata']['uid']
        config=self.api('get','configmap','local-path-config','-n','local-path-storage','-o','json')['data']
        if config.get('setup')!='#!/bin/sh\nset -eu\nmkdir -m 0777 -p "$VOL_DIR"':raise ValueError('qualified directory-only backend required')
        self.receipt.update(node_uid=node['metadata']['uid'],cluster_uid=cluster,namespace_uid=namespace,image=closure['image'],publication=closure['publication_evidence'],
            local_path_config_sha256=hashlib.sha256(json.dumps(config,sort_keys=True).encode()).hexdigest())
        self.capacity()
        expected=expected_raw_sha(deadline=self.deadline)
        if expected!=closure['raw_sha256']:raise ValueError('independent known pattern differs from original CI fixture')
        try:
            for suffix in ('stage','disk'):
                obj={'apiVersion':'v1','kind':'PersistentVolumeClaim','metadata':{'name':self.prefix+'-'+suffix,'namespace':self.args.namespace},
                     'spec':{'storageClassName':'local-path','volumeMode':'Filesystem','accessModes':['ReadWriteOnce'],'resources':{'requests':{'storage':'1Gi'}}}}
                self.claims.append(self.create('pvc',obj))
            pod=self.pod(self.prefix+'-worker');self.wait(pod)
            fixture=self.remote(pod,'fixture')
            if fixture['raw_sha256']!=expected or not 0<fixture['zip_bytes']<=MAX_ZIP:raise ValueError('fixture bytes differ')
            archive=self.remote(pod,'download',raw=True)
            if len(archive)!=fixture['zip_bytes'] or hashlib.sha256(archive).hexdigest()!=fixture['zip_sha256']:raise ValueError('downloaded tiny ZIP differs')
            path=self.out/'fixture.zip';path.write_bytes(archive)
            rejected=self.remote(pod,'receive-negative',len(archive),fixture['zip_sha256'],input_path=path)
            if rejected!={'negative_control':'wrong-archive-digest','rejected':True}:raise ValueError('actual digest negative control missing')
            received=self.remote(pod,'receive',len(archive),fixture['zip_sha256'],input_path=path)
            if received['zip_sha256']!=fixture['zip_sha256'] or received['disk_sha256']!=fixture['disk_sha256'] or received['manifest']['raw_sha256']!=expected:raise ValueError('real host-to-node archive receipt differs')
            converted=self.remote(pod,'convert',min(180,self.remaining()))
            if converted['raw_sha256']!=expected or converted['logical_bytes']!=worker.VIRTUAL:raise ValueError('actual decoder differs')
            self.cleanup([('pod',pod['metadata']['name'],pod['metadata']['uid'])])
            self.resources.remove(('pod',pod['metadata']['name'],pod['metadata']['uid']))
            readback=self.pod(self.prefix+'-readback',True);self.wait(readback)
            result=self.remote(readback,'readback')
            if result['raw_sha256']!=expected or result['logical_bytes']!=worker.VIRTUAL:raise ValueError('independent RO readback differs')
            self.receipt.update(independent_readback_matched=True,independent_readback={'pod_uid':readback['metadata']['uid'],**result},tiny_transport_decoder_succeeded=True)
        except BaseException as error:
            self.receipt['error_type']=type(error).__name__;raise
        finally:
            primary=sys.exc_info()[1];cleanup_failure=None
            self.deadline=min(self.cleanup_until,time.monotonic()+180)
            try:
                for kind,name in list(self.unresolved):
                    actual=self.api('get',kind,name,'-n',self.args.namespace,'--ignore-not-found','-o','json',recover=True)
                    if actual is not None and actual['metadata'].get('labels',{}).get('tuna-perf-nonce')==self.nonce:
                        self.register(kind,name,actual['metadata']['uid'])
                self.receipt['cleanup']=self.cleanup(self.resources,include_pvs=True)
                if self.unresolved:
                    self.receipt['unresolved_creates']=list(self.unresolved)
                    raise RuntimeError('ambiguous server create ownership unconfirmed')
                self.receipt['cleanup']['owned_pvs_absent']=True
                self.capacity()
                if self.receipt.get('tiny_transport_decoder_succeeded'):self.receipt['completed']=True
            except BaseException as error:
                self.receipt.update(completed=False,cleanup_error_type=type(error).__name__);cleanup_failure=error
            self.record('receipt.json',self.receipt)
            if primary is None and cleanup_failure is not None:raise RuntimeError('tiny owned cleanup unconfirmed') from None
        return self.receipt
