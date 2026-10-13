"""Owned KubeVirt benchmark session transport, never a VM builder.

SSH executes fixed probes with an independent guest timeout and bounded output.
Native renderer/reference qualification remains the shared runner's responsibility.
"""
import base64
import json
import os
from pathlib import Path
import re
import shlex
import signal
import socket
import struct
import subprocess
import tempfile
import time
import zlib
from corral_perf import Rfb, kubectl, vm_identity, verify_identity

MAX_OUTPUT = 8*1024*1024
ACTIONS = frozenset(('clock','notify','frames','reference-image','stock-reference',
 'gnome-trace-start','gnome-trace-stop','gnome-trace-read','gnome-trace-failed-stop',
 'gnome-trace-failed-stop-read','kernel-trace-stop','kernel-trace-read',
 'kernel-formats-read','kernel-discovery-read','tuna-overview-start','tuna-overview-stop'))

# Host timeout is backed by a guest-owned timeout, so losing SSH cannot leave an
# unbounded phase child behind. stdout/stderr go to bounded temporary files via
# readers; truncation is explicitly refused by the existing shared validator.
PHASE = '''import base64,json,os,pathlib,selectors,signal,subprocess,sys,time
args=json.loads(sys.argv[1]);limit=8388608
seconds=30 if args[0] in ('gnome-trace-stop','gnome-trace-failed-stop') else 100 if args[0]=='stock-reference' else 20
p=subprocess.Popen(['/usr/bin/timeout','--signal=TERM','--kill-after=2s',str(seconds)+'s','/usr/libexec/tuna-perf-phase',*args],stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True)
selector=selectors.DefaultSelector();selector.register(p.stdout,selectors.EVENT_READ,'out');selector.register(p.stderr,selectors.EVENT_READ,'err')
data={'out':bytearray(),'err':bytearray()};truncated={'out':False,'err':False};deadline=time.monotonic()+seconds+3
try:
 while selector.get_map():
  if time.monotonic()>deadline:raise TimeoutError('guest phase timeout')
  for key,_ in selector.select(.1):
   chunk=os.read(key.fileobj.fileno(),65536)
   if not chunk:selector.unregister(key.fileobj);continue
   buf=data[key.data];remaining=limit-len(buf);buf.extend(chunk[:remaining])
   if len(chunk)>remaining:truncated[key.data]=True;raise ValueError('guest phase output limit')
 p.wait(timeout=2)
finally:
 if p.poll() is None:
  os.killpg(p.pid,signal.SIGKILL);p.wait(timeout=3)
 selector.close();p.stdout.close();p.stderr.close()
print(json.dumps({'exited':True,'exitcode':p.returncode,'out-data':base64.b64encode(data['out']).decode(),'err-data':base64.b64encode(data['err']).decode(),'out-truncated':truncated['out'],'err-truncated':truncated['err'],'transport_boot_id':pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()}))
'''
JOURNAL = '''import json,pathlib,subprocess,sys
boot=pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()
p=subprocess.run(['journalctl','--no-pager','-b','-u','tuna-perf.service','-o','json','--lines=30000'],stdout=subprocess.PIPE,timeout=20,check=True)
if len(p.stdout)>8388608:raise ValueError('sample journal exceeds bound')
rows=[]
for line in p.stdout.splitlines():
 row=json.loads(line)
 if row.get('_BOOT_ID')!=boot.replace('-','') or row.get('_SYSTEMD_UNIT')!='tuna-perf.service':raise ValueError('journal attribution mismatch')
 message=row.get('MESSAGE')
 if not isinstance(message,str):raise ValueError('invalid journal message')
 rows.append(message)
print(json.dumps({'boot_id':boot,'messages':rows}))
'''


def bounded_run(command, timeout, env=None):
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        process = subprocess.Popen(command,stdout=stdout,stderr=stderr,start_new_session=True,env=env)
        try:
            deadline=time.monotonic()+timeout
            while process.poll() is None:
                if stdout.tell()>MAX_OUTPUT*3 or stderr.tell()>MAX_OUTPUT or time.monotonic()>deadline:
                    raise TimeoutError('bounded SSH acquisition exceeded limit')
                time.sleep(.02)
            if stdout.tell()>MAX_OUTPUT*3 or stderr.tell()>MAX_OUTPUT:
                raise ValueError('SSH acquisition output exceeds limit')
            stdout.seek(0);stderr.seek(0)
            output,error=stdout.read(),stderr.read()
            if process.returncode:
                raise RuntimeError('SSH acquisition failed: '+error[:4096].decode(errors='replace'))
            return output
        finally:
            if process.poll() is None:
                os.killpg(process.pid,signal.SIGKILL);process.wait(timeout=5)


def ssh_args(context,namespace,user,key,known_hosts):
    known=str(Path(known_hosts).resolve())
    return ['virtctl','--context',context,'ssh','--namespace',namespace,'--username',user,
            '--identity-file',str(Path(key).resolve()),'--known-hosts',known,
            '--local-ssh-opts=-o UserKnownHostsFile='+known,
            '--local-ssh-opts=-o GlobalKnownHostsFile=/dev/null',
            '--local-ssh-opts=-o StrictHostKeyChecking=accept-new','--local-ssh-opts=-o BatchMode=yes',
            '--local-ssh-opts=-o ConnectTimeout=10']


class Session:
    def __init__(self,context,namespace,name,user,key,known_hosts,out):
        for value in (namespace,name):
            if not re.fullmatch(r'[a-z0-9][a-z0-9.-]{0,251}',value):raise ValueError('invalid Kubernetes session name')
        if not re.fullmatch(r'[a-z_][a-z0-9_-]*',user):raise ValueError('invalid SSH user')
        self.context,self.namespace,self.name=context,namespace,name
        self.user=user
        self.out=Path(out);self.boot_id=None;self.counter=0;self.responses={};self.proxy=None;self.display=None
        self.identity=vm_identity(*self.objects())
        if self.identity['migration_state'] is not None:raise ValueError('migrating guest rejected')
        self.ssh=ssh_args(context,namespace,user,key,known_hosts)
        self.metadata={ 'backend':'corral-kubevirt','identity':self.identity,
          'gpu':{'status':'unsupported','qualified':False},'total_power':{'status':'unsupported','qualified':False},
          'cpu_hardware_scope':'AWS virtual node/guest; no physical-host counters',
          'whole_session_cpu_qualified':False,'performance_qualified':False }
        spec=self.objects()[1]['spec'];self.metadata['guest_domain']=spec['domain']
        node=kubectl(context,'get','node',self.identity['node'],'-o','json')
        self.metadata['node_uid']=node['metadata']['uid'];self.metadata['node_info']=node['status'].get('nodeInfo',{})
        if not any('devices.kubevirt.io/kvm' in c.get('resources',{}).get('limits',{}) for c in self.launcher()['spec']['containers']):
            raise ValueError('launcher does not request advertised KVM device')
        self.check_boot(self.remote('cat /proc/sys/kernel/random/boot_id').decode().strip())
        from corral_ssh import HOST_KEYS,host_receipt
        self.metadata['ssh_host_identity']=host_receipt(self.python(HOST_KEYS),known_hosts,self.boot_id)
        self.out.joinpath('corral-session.json').write_text(json.dumps(self.metadata,indent=2))

    def objects(self):
        return tuple(kubectl(self.context,'get',kind,self.name,'-n',self.namespace,'-o','json') for kind in ('vm','vmi'))

    def launcher(self):
        pods=kubectl(self.context,'get','pods','-n',self.namespace,'-l','kubevirt.io/created-by='+self.identity['vmi_uid'],'-o','json')['items']
        owned=[p for p in pods if any(o.get('uid')==self.identity['vmi_uid'] and o.get('kind')=='VirtualMachineInstance' and o.get('controller') is True for o in p['metadata'].get('ownerReferences',[])) and not p['metadata'].get('deletionTimestamp')]
        if len(owned)!=1 or owned[0]['spec'].get('nodeName')!=self.identity['node']:raise ValueError('exact owned launcher on identified node required')
        return owned[0]

    def verify(self):
        vm,vmi=self.objects();result=verify_identity(self.identity,vm,vmi)
        if 'guest_domain' in self.metadata and vmi['spec']['domain']!=self.metadata['guest_domain']:
            raise ValueError('guest hardware/resources changed')
        if 'node_uid' in self.metadata and kubectl(self.context,'get','node',self.identity['node'],'-o','json')['metadata']['uid']!=self.metadata['node_uid']:
            raise ValueError('node replaced during acquisition')
        return result

    def remote(self,command,timeout=130):
        from corral_ssh import projected_kubeconfig
        deadline=time.monotonic()+timeout
        with projected_kubeconfig(self.context,deadline=deadline) as env:
            self.metadata['ssh_kubernetes_context']={'explicit_context':self.context,'child_ambient_context_isolated':True,'credential_file_retained':False}
            remaining=deadline-time.monotonic()
            if remaining<=0:raise TimeoutError('SSH acquisition deadline')
            return bounded_run([*self.ssh,'--command',command,'vm/'+self.name],remaining,env=env)

    def python(self,source,argument=None):
        command=('python3 -c ' if self.user=='root' else 'sudo -n python3 -c ')+shlex.quote(source)
        if argument is not None:command+=' '+shlex.quote(json.dumps(argument))
        return json.loads(self.remote(command))

    def check_boot(self,value):
        if not re.fullmatch(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}',value):raise ValueError('invalid guest boot identity')
        if self.boot_id is not None and value!=self.boot_id:raise ValueError('guest rebooted during measurement')
        self.boot_id=value;self.metadata['boot_id']=value

    def command(self,name,**arguments):
        # The numeric token is an adapter handle, never a claimed guest PID.
        if name=='guest-exec':
            args=arguments.get('arg',[])
            if arguments.get('path')!='/usr/libexec/tuna-perf-phase' or not args or args[0] not in ACTIONS or arguments.get('capture-output') is not True:
                raise ValueError('only fixed performance helper is allowed')
            if len(args)>1 and not (len(args)==3 and args[1]=='--index' and re.fullmatch(r'\d{1,4}',args[2])):
                raise ValueError('invalid fixed phase arguments')
            status=self.python(PHASE,args);self.check_boot(status.pop('transport_boot_id'))
            self.counter+=1;self.responses[self.counter]=status
            return {'pid':self.counter}
        if name=='guest-exec-status':
            return self.responses.pop(arguments['pid'])
        raise ValueError('unsupported SSH guest-agent operation')

    def samples(self):
        result=self.python(JOURNAL);self.check_boot(result['boot_id'])
        return '\n'.join(result['messages'])+'\n'

    def connect_display(self):
        log=self.out/'corral-vnc-proxy.log';self.proxy_log=log.open('wb')
        self.proxy=subprocess.Popen(['virtctl','--context',self.context,'vnc',self.name,'-n',self.namespace,'--proxy-only','--port','0','--address','127.0.0.1'],stdout=self.proxy_log,stderr=self.proxy_log,start_new_session=True)
        deadline=time.monotonic()+20
        while time.monotonic()<deadline and self.proxy.poll() is None:
            text=log.read_text()
            for line in text.splitlines():
                try:port=json.loads(line).get('port')
                except (ValueError,AttributeError):continue
                if type(port) is int and 0<port<65536:
                    if not owned_listener(self.proxy.pid,port):raise ValueError('display endpoint not owned by exact proxy')
                    self.display=Display(Rfb(socket.create_connection(('127.0.0.1',port),timeout=10)))
                    return self.display
            time.sleep(.05)
        raise RuntimeError('owned virtctl display tunnel unavailable')

    def desktop(self,selected):
        result=self.python(DESKTOP,selected);self.check_boot(result['boot_id'])
        return result

    def select(self,selected):
        self.verify();result=self.python(SELECT,selected)
        if result['previous_boot_id']!=self.boot_id:raise ValueError('selection boot identity changed')
        return result

    def close(self):
        try:self.verify()
        finally:
            if self.display:self.display.rfb.close()
            if self.proxy and self.proxy.poll() is None:
                os.killpg(self.proxy.pid,signal.SIGTERM)
                try:self.proxy.wait(timeout=5)
                except subprocess.TimeoutExpired:os.killpg(self.proxy.pid,signal.SIGKILL);self.proxy.wait(timeout=5)
            if hasattr(self,'proxy_log'):self.proxy_log.close()


class Display:
    KEY={'esc':0xff1b,'meta_l':0xffeb,'ret':0xff0d,'pgdn':0xff56,'pgup':0xff55}
    def __init__(self,rfb):self.rfb=rfb
    def keys(self,*keys):self.rfb.keys(*(self.KEY[k] for k in keys))
    def type_text(self,text):
        if not re.fullmatch(r'[a-z ]{1,64}',text):raise ValueError('fixed search text required')
        for char in text:self.rfb.keys(ord(char))
    def capture(self):
        w,h,p=self.rfb.snapshot()
        if (w,h)!=(1280,800):raise ValueError('matched1280x800 display required')
        return w,h,b''.join(bytes((p[i+2],p[i+1],p[i])) for i in range(0,len(p),4))
    def cmd(self,command,**args):
        if command!='screendump' or args.get('format')!='ppm':raise ValueError('unsupported display command')
        w,h,p=self.capture();Path(args['filename']).write_bytes(f'P6\n{w} {h}\n255\n'.encode()+p)
    def frame(self,path):
        w,h,p=self.capture()
        def chunk(kind,data):return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
        raw=b''.join(b'\0'+p[y*w*3:(y+1)*w*3] for y in range(h))
        Path(path).write_bytes(b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',w,h,8,2,0,0,0))+chunk(b'IDAT',zlib.compress(raw))+chunk(b'IEND',b''))

SELECT = r'''import json,os,pathlib,re,stat,subprocess,sys
selected=json.loads(sys.argv[1])
if selected not in ('gnome','tuna'):raise ValueError('fixed desktop required')
entry=pathlib.Path('/usr/share/wayland-sessions/'+selected+'.desktop')
if not entry.is_file():raise ValueError('matched desktop entry missing')
def protected(path):
 p=pathlib.Path(path);s=p.lstat()
 if not stat.S_ISREG(s.st_mode) or s.st_uid!=0 or s.st_mode&0o022 or s.st_size>65536:raise ValueError('untrusted session configuration')
 return p.read_text()
def replace(path,text,mode):
 p=pathlib.Path(path);p.parent.mkdir(parents=True,exist_ok=True)
 tmp=p.with_name(p.name+'.tuna-corral-tmp');fd=os.open(tmp,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,mode)
 try:
  with os.fdopen(fd,'w') as f:f.write(text);f.flush();os.fsync(f.fileno())
  os.replace(tmp,p)
 finally:
  if tmp.exists():tmp.unlink()
config=protected('/etc/gdm/custom.conf')
config,count=re.subn(r'(?m)^DefaultSession=(?:gnome|tuna)\.desktop$', 'DefaultSession='+selected+'.desktop',config)
if count!=1:raise ValueError('exact GDM default required')
source=protected('/usr/lib/tmpfiles.d/tuna-vm-lane.conf')
source,count=re.subn(r'Session=(?:gnome|tuna)\\n','Session='+selected+r'\\n',source)
if count!=1:raise ValueError('exact tmpfiles session required')
replace('/etc/gdm/custom.conf',config,0o644)
replace('/etc/tmpfiles.d/tuna-vm-lane.conf',source,0o644)
replace('/var/lib/AccountsService/users/tuna-test','[User]\nSession='+selected+'\nSystemAccount=false\n',0o600)
boot=pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()
subprocess.run(['systemd-run','--quiet','--collect','--unit=tuna-corral-reboot','--on-active=2s','/usr/bin/systemctl','reboot'],check=True,timeout=10)
print(json.dumps({'desktop':selected,'previous_boot_id':boot,'reboot_requested':True}))
'''
DESKTOP = r'''import hashlib,json,os,pathlib,pwd,sys
selected=json.loads(sys.argv[1]);uid=pwd.getpwnam('tuna-test').pw_uid
if selected not in ('gnome','tuna'):raise ValueError('fixed desktop required')
expected='/usr/bin/'+('gnome-shell' if selected=='gnome' else 'tuna-compositor');found=[];unexpected=[]
for p in pathlib.Path('/proc').iterdir():
 if not p.name.isdecimal():continue
 try:
  if p.stat().st_uid!=uid:continue
  exe=os.readlink(p/'exe')
  if exe==expected:
   fields=(p/'stat').read_text().rsplit(')',1)[1].split();found.append({'pid':int(p.name),'start_ticks':int(fields[19]),'exe':exe})
  elif exe in ('/usr/bin/gnome-shell','/usr/bin/tuna-compositor'):unexpected.append(exe)
 except (FileNotFoundError,ProcessLookupError):continue
if len(found)!=1 or unexpected:raise ValueError('exact selected compositor required')
found[0]['sha256']=hashlib.sha256(pathlib.Path(expected).read_bytes()).hexdigest()
print(json.dumps({'desktop':selected,'compositor':found[0],'boot_id':pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()}))
'''


def owned_listener(pid,port):
    inodes=set()
    for path in Path('/proc',str(pid),'fd').iterdir():
        try:target=os.readlink(path)
        except FileNotFoundError:continue
        match=re.fullmatch(r'socket:\[(\d+)\]',target)
        if match:inodes.add(match[1])
    for line in Path('/proc/net/tcp').read_text().splitlines()[1:]:
        fields=line.split()
        if fields[1]==f'0100007F:{port:04X}' and fields[3]=='0A' and fields[9] in inodes:return True
    return False


def admit_imported_disk(export_dir, receipt_path, context, namespace, vm_name, reference_path, payload_path, selection_path):
    """Require original exported bytes and still-owned successful CDI objects.

    The import receipt is retained by the original owned upload action; never
    synthesize one from an already running guest or a legacy unproven disk.
    """
    import hashlib
    directory=Path(export_dir);manifest_path=directory/'manifest.json'
    manifest=json.loads(manifest_path.read_text());receipt=json.loads(Path(receipt_path).read_text())
    if manifest.get('schema')!=1 or manifest.get('export_succeeded') is not True or manifest.get('paired_desktops')!=['gnome','tuna']:
        raise ValueError('successful paired disk export required')
    disk=directory/('disk.raw.gz' if receipt.get('method')=='raw-gzip-upload' else 'disk.qcow2')
    with disk.open('rb') as stream:digest=hashlib.file_digest(stream,'sha256').hexdigest()
    if digest!=manifest.get('disk_sha256') or receipt.get('disk_sha256')!=digest:
        raise ValueError('original transfer qcow2 bytes differ')
    if receipt.get('method') in ('raw-upload','raw-gzip-upload'):
        readback=receipt.get('raw_readback',{})
        if receipt.get('uploaded_raw_sha256')!=manifest.get('raw_sha256') or readback.get('sha256')!=manifest.get('raw_sha256') or readback.get('matched') is not True or readback.get('prefix_bytes')!=18*1024**3 or readback.get('pvc_uid')!=receipt.get('pvc_uid'):raise ValueError('actual imported raw byte proof differs')
    if receipt.get('method')=='raw-gzip-upload':
        from tuna_raw_gzip import usable_target, RAW_BYTES
        space=receipt.get('gzip_target_capacity', {})
        proof=receipt.get('raw_method_proof', {})
        if (manifest.get('import_method')!='raw-gzip-upload' or manifest.get('requested_pvc')!='20Gi'
                or proof.get('method')!='raw-gzip-upload' or proof.get('full_image_import_qualified') is not False
                or space.get('pvc_uid')!=receipt.get('pvc_uid') or space.get('observed_before_upload') is not True):
            raise ValueError('separate gzip method/target receipt required')
        if usable_target(min(space['available_bytes'],20*1024**3),float(space['filesystem_overhead']))!=space.get('usable_bytes'):
            raise ValueError('gzip actual usable target proof differs')
    manifest_sha=hashlib.sha256(manifest_path.read_bytes()).hexdigest()
    if receipt.get('manifest_sha256')!=manifest_sha or receipt.get('schema')!=1 or receipt.get('completed') is not True:
        raise ValueError('original completed upload receipt required')
    if (receipt.get('context'),receipt.get('namespace'),receipt.get('vm_name'))!=(context,namespace,vm_name):
        raise ValueError('upload target differs')
    reference=json.loads(Path(reference_path).read_text());payload=json.loads(Path(payload_path).read_text());selection=json.loads(Path(selection_path).read_text())
    if manifest.get('reference')!=reference or manifest.get('package_selection')!=selection or manifest.get('candidate_required_members')!=payload.get('required_members') or manifest.get('package_sha256')!=payload.get('package',{}).get('sha256'):
        raise ValueError('original source/image/package selection differs')
    from gnome_reference import validate_image
    validate_image(reference,manifest.get('gnome_profile'))
    if not re.fullmatch(r'[0-9a-f]{40}',selection.get('head_sha','')) or type(selection.get('run_id')) is not int or selection['run_id']<=0:
        raise ValueError('original selected CI source/run required')
    if not re.fullmatch(r'[0-9a-f]{64}',payload.get('package',{}).get('sha256','')):
        raise ValueError('original package bytes required')
    member=payload.get('required_members',{}).get('usr/bin/tuna-compositor',{})
    if not re.fullmatch(r'[0-9a-f]{64}',member.get('sha256','')):
        raise ValueError('original native compositor bytes required')
    fixture=manifest.get('fixture_image_id')
    if not re.fullmatch(r'sha256:[0-9a-f]{64}',fixture or ''):raise ValueError('original fixture image identity missing')
    pvc_name=receipt.get('pvc_name');dv_name=receipt.get('datavolume_name')
    for value in (pvc_name,dv_name):
        if not isinstance(value,str) or not re.fullmatch(r'[a-z0-9][a-z0-9.-]{0,251}',value):raise ValueError('owned upload name required')
    pvc=kubectl(context,'get','pvc',pvc_name,'-n',namespace,'-o','json')
    dv=kubectl(context,'get','dv',dv_name,'-n',namespace,'-o','json')
    vm=kubectl(context,'get','vm',vm_name,'-n',namespace,'-o','json')
    if pvc['metadata']['uid']!=receipt.get('pvc_uid') or dv['metadata']['uid']!=receipt.get('datavolume_uid') or vm['metadata']['uid']!=receipt.get('vm_uid'):
        raise ValueError('uploaded storage or VM replaced')
    if pvc['status'].get('phase')!='Bound' or dv['status'].get('phase')!='Succeeded':raise ValueError('CDI import has not completed')
    if not any(v.get('persistentVolumeClaim',{}).get('claimName')==pvc_name or v.get('dataVolume',{}).get('name')==dv_name for v in vm['spec']['template']['spec']['volumes']):
        raise ValueError('uploaded disk is not attached to owned VM')
    return {'fixture_image_id':fixture,'manifest_sha256':manifest_sha,'disk_sha256':digest,
            'vm_uid':receipt['vm_uid'],'pvc_uid':receipt['pvc_uid'],'datavolume_uid':receipt['datavolume_uid']}


def readback_prefix(context,namespace,pvc_name,pvc_uid,node,byte_count,expected_sha,out):
    """Hash the actual uploaded raw prefix from a new owned read-only reader pod."""
    import uuid
    if type(byte_count) is not int or byte_count<=0 or byte_count%(1024*1024) or not re.fullmatch(r'[0-9a-f]{64}',expected_sha):
        raise ValueError('bounded MiB raw prefix and exact SHA required')
    claim=kubectl(context,'get','pvc',pvc_name,'-n',namespace,'-o','json')
    if claim['metadata']['uid']!=pvc_uid or claim['status'].get('phase')!='Bound':raise ValueError('readback PVC identity/phase differs')
    name='tuna-perf-readback-'+uuid.uuid4().hex[:10];out=Path(out)
    owner={'apiVersion':'v1','kind':'PersistentVolumeClaim','name':pvc_name,'uid':pvc_uid,'controller':False}
    command=f'set -eu; set -o pipefail; test "$(stat -c%s /data/disk.img)" -ge {byte_count}; dd if=/data/disk.img bs=1048576 count={byte_count//(1024*1024)} 2>/dev/null | sha256sum; stat -c "size=%s blocks=%b" /data/disk.img'
    pod={'apiVersion':'v1','kind':'Pod','metadata':{'name':name,'namespace':namespace,'ownerReferences':[owner]},'spec':{'restartPolicy':'Never','automountServiceAccountToken':False,'nodeSelector':{'kubernetes.io/hostname':node},'securityContext':{'seccompProfile':{'type':'RuntimeDefault'},'runAsUser':107,'runAsGroup':107,'runAsNonRoot':True},'containers':[{'name':'reader','image':'alpine@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b','command':['sh','-c',command],'securityContext':{'allowPrivilegeEscalation':False,'capabilities':{'drop':['ALL']}},'resources':{'requests':{'cpu':'20m','memory':'32Mi'},'limits':{'memory':'128Mi'}},'volumeMounts':[{'name':'disk','mountPath':'/data','readOnly':True}]}],'volumes':[{'name':'disk','persistentVolumeClaim':{'claimName':pvc_name,'readOnly':True}}]}}
    path=out/'readback-pod-create.json';path.write_text(json.dumps(pod))
    import corral_owned
    owned=[]
    try:
        actual=corral_owned.create(context,namespace,'pod',pod,path,uuid.uuid4().hex,lambda *x:owned.append(x));uid=actual['metadata']['uid']
        deadline=time.monotonic()+180
        while time.monotonic()<deadline:
            actual=kubectl(context,'get','pod',name,'-n',namespace,'-o','json',deadline=deadline)
            if actual['metadata']['uid']!=uid:raise ValueError('readback pod replaced')
            if actual['status'].get('phase') in ('Succeeded','Failed'):break
            time.sleep(.5)
        else:raise TimeoutError('owned readback deadline')
        logs=subprocess.check_output(['kubectl','--context',context,'logs',name,'-n',namespace],timeout=min(20,max(.01,deadline-time.monotonic()))).decode()
        (out/'readback.log').write_text(logs)
        if actual['status'].get('phase')!='Succeeded' or logs.split()[0]!=expected_sha:raise ValueError('actual stored raw prefix differs')
        receipt={'pod_uid':uid,'pvc_uid':pvc_uid,'node':node,'prefix_bytes':byte_count,'sha256':expected_sha,'matched':True}
        (out/'raw-readback.json').write_text(json.dumps(receipt,indent=2));return receipt
    finally:
        if owned:corral_owned.cleanup(context,namespace,owned,timeout=60)
