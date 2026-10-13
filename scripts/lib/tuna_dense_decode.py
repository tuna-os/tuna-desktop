"""Optional dense decoder observation; existing export storage guards remain authoritative."""
import json
import os
from pathlib import Path
import shutil
import sys
import time
import uuid
from tuna_benchmark_disk import digest, require, VIRTUAL_BYTES
from tuna_dense_process import DenseCommands, bounded_command

GIB=1024**3
TOOL='ghcr.io/tuna-os/benchmark-node-tools@sha256:e6c52c9653c1406acc685063d571d28222251bb46eefbda34692a9b7ad197893'
TOOL_ALLOWANCE=1572877129
OUTPUT_RESERVE=20*GIB
SAFETY_BYTES=2*GIB+128*1024**2


def capacity_observation(paths,*,before_pull):
 """Statvfs already excludes existing source and already-pulled tool layers."""
 observations={}
 for role,path in paths.items():
  path=Path(path).resolve();s=path.stat();v=os.statvfs(path)
  observations[role]={'device':s.st_dev,'available_bytes':v.f_bavail*v.f_frsize,'path':str(path)}
 reserve=OUTPUT_RESERVE+SAFETY_BYTES+(TOOL_ALLOWANCE if before_pull else 0)
 # Current CI layout must have all three roles on one filesystem. No implicit copy.
 devices={r['device'] for r in observations.values()}
 require(len(devices)==1,'separate source/target/graphroot capacity model not admitted')
 free=min(r['available_bytes'] for r in observations.values())
 return {'before_pull':before_pull,'required_additional_bytes':reserve,'available_bytes':free,
         'admitted':free>=reserve,'observations':observations,
         'existing_tool_charged_again':False if not before_pull else None}



def query_capacity(commands,paths,*,before_pull):
 # Rootful graphroot may be private. Query only these three already-owned roles.
 script="import json,os,sys; paths=json.loads(sys.argv[1]); result={};\nfor role,path in paths.items():\n s=os.stat(path);v=os.statvfs(path);result[role]={'device':s.st_dev,'available_bytes':v.f_bavail*v.f_frsize,'path':os.path.realpath(path)}\nprint(json.dumps(result))"
 _,text=commands.call(['sudo','python3','-c',script,json.dumps({k:str(v) for k,v in paths.items()})],timeout=30)
 observations=json.loads(text)
 require(set(observations)==set(paths),'capacity roles missing')
 require(all(type(row.get('device')) is int and type(row.get('available_bytes')) is int and row['available_bytes']>=0 for row in observations.values()),'actual filesystem capacity required')
 require(len({r['device'] for r in observations.values()})==1,'separate source/target/graphroot capacity model not admitted')
 reserve=OUTPUT_RESERVE+SAFETY_BYTES+(TOOL_ALLOWANCE if before_pull else 0)
 free=min(r['available_bytes'] for r in observations.values())
 return {'before_pull':before_pull,'required_additional_bytes':reserve,'available_bytes':free,
         'admitted':free>=reserve,'observations':observations,
         'existing_tool_charged_again':False if not before_pull else None}

def write_json(path,value):
 Path(path).write_text(json.dumps(value,indent=2)+'\n')


def _observe(commands,artifacts,scratch,disk,raw_hash,info,source):
 """Never modifies source or changes export admission. Small receipt even on refusal."""
 artifacts,scratch,disk=map(Path,(artifacts,scratch,disk))
 result={'schema':1,'qualified':False,'tool':TOOL,'source_provenance':source,
         'export_admission_changed':False,'node_or_vm_qualified':False}
 receipt=artifacts/'dense-decode.json';worker=Path(__file__).with_name('tuna_dense_decode_worker.py')
 directory=None;container=None;target=None;owned=None
 previous_log=commands.log;commands.log=artifacts/'dense-decode-host.log'
 try:
  require(VIRTUAL_BYTES==18*GIB and info['virtual-size']==VIRTUAL_BYTES,'exact 18Gi source required')
  require(info.get('format')=='qcow2' and not info.get('backing-filename'),'standalone QCOW source required')
  require(not disk.is_symlink() and disk.is_file(),'owned regular source disk required')
  _,text=commands.call(['sudo','podman','info','--format','{{.Store.GraphRoot}}'],timeout=60)
  graphroot=Path(text.strip());require(graphroot.is_absolute(),'actual Podman graphroot required')
  paths={'source':disk.parent,'target':scratch,'graphroot':graphroot}
  result['capacity_before_pull']=query_capacity(commands,paths,before_pull=True)
  if not result['capacity_before_pull']['admitted']:
   result['status']='capacity-refused-before-pull';raise ValueError('capacity refused before tool pull')
  commands.call(['sudo','podman','pull',TOOL],timeout=1800,max_output=4*1024*1024)
  _,text=commands.call(['sudo','podman','image','inspect',TOOL,'--format','{{.Id}}\n{{json .RepoDigests}}'],timeout=60)
  image,digests=text.splitlines();require(TOOL in json.loads(digests),'pinned public tool digest missing')
  result['tool_image_id']=image
  result['capacity_after_pull']=query_capacity(commands,paths,before_pull=False)
  if not result['capacity_after_pull']['admitted']:
   result['status']='capacity-refused-after-pull';raise ValueError('capacity refused after tool pull')
  nonce=uuid.uuid4().hex
  directory=scratch/('dense-decode-'+nonce);directory.mkdir(mode=0o755)
  target=directory/'output';target.mkdir(mode=0o755)
  owned=(target.stat().st_dev,target.stat().st_ino)
  request={'virtual_bytes':VIRTUAL_BYTES,'qcow_sha256':digest(disk),'raw_sha256':raw_hash,
           'observer_sha':source['observer_source_commit'],'candidate_sha':source['candidate_source_commit'],
           'package_run':source['package_selection']['run_id'],'worker_sha256':digest(worker),'tool':TOOL,
           'operation_deadline_monotonic':commands.deadline,'tool_identity_sha256':digest(worker.with_name('tuna_dense_tool_identity.json'))}
  result['request']=request
  write_json(directory/'request.json',request)
  # UID/name/inode bounds are checked again before deleting any output.
  commands.call(['sudo','chown','107:107',str(target)],timeout=30)
  container='tuna-dense-decode-'+nonce
  code,_=commands.call(['sudo','podman','container','exists',container],check=False,timeout=30)
  require(code==1,'fresh owned diagnostic container name required')
  argv=['sudo','podman','create','--name',container,'--label','io.tuna.dense-owner='+nonce,
        '--user','107:107','--read-only','--network','none','--cap-drop','ALL',
        '--security-opt','no-new-privileges','--pids-limit','32','--memory','512m',
        '--memory-swap','512m','--pull','never','--entrypoint','/opt/tuna-tools/python',
        '-v',str(disk.resolve())+':/input/disk.qcow2:ro',
        '-v',str(worker.resolve())+':/observer/worker.py:ro',
        '-v',str(worker.with_name('tuna_dense_tool_identity.json').resolve())+':/observer/tool-identity.json:ro',
        '-v',str((directory/'request.json').resolve())+':/observer/request.json:ro',
        '-v',str(target.resolve())+':/target:rw',image,'/observer/worker.py']
  commands.call(argv,timeout=60)
  _,text=commands.call(['sudo','podman','inspect',container],timeout=60)
  inspect=json.loads(text);require(len(inspect)==1,'owned container inspect required')
  config=inspect[0]
  require(config['Name'].lstrip('/')==container and config['Config']['Labels'].get('io.tuna.dense-owner')==nonce,'owned container name/label mismatch')
  require(config['Config']['User']=='107:107' and config['HostConfig']['Memory']==512*1024**2,
          'actual user/container memory configuration required')
  result['container']={'id':config['Id'],'name':container,'uid_gid':config['Config']['User'],
                       'memory_bytes':config['HostConfig']['Memory'],'worker_sha256':digest(worker)}
  code,_=commands.call(['sudo','podman','start','--attach',container],check=False,timeout=max(.01,commands.deadline-time.monotonic()),max_output=1024*1024)
  result['host_start_exit']=code
  _,text=commands.call(['sudo','podman','inspect',container,'--format','{{json .State}}'],timeout=60)
  state=json.loads(text)
  result['container_state']={key:state.get(key) for key in ('Status','Running','ExitCode','OOMKilled','Error')}
  require(not state.get('Running'),'owned container still running')
  if (target/'receipt.json').is_file():
   value=(target/'receipt.json').stat();require(value.st_uid==107 and value.st_size<=1024*1024,'bounded owned worker receipt required')
   result['worker']=json.loads((target/'receipt.json').read_text())
   require(result['worker'].get('request')==request,'worker/source request identity mismatch')
  if (target/'decoder.log').is_file():
   value=(target/'decoder.log').stat();require(value.st_uid==107 and value.st_size<=1024*1024,'bounded owned decoder log required')
   shutil.copyfile(target/'decoder.log',artifacts/'dense-decoder.log')
   result['decoder_log_sha256']=digest(artifacts/'dense-decoder.log')
  result['source_unchanged']=digest(disk)==request['qcow_sha256']
  require(code==0 and state.get('ExitCode')==0 and state.get('OOMKilled') is False and
          result.get('worker',{}).get('qualified') is True and result['source_unchanged'],
          'actual dense decoder/readback/limit observation did not qualify')
  result.update(qualified=True,status='decoder-readback-qualified')
 except Exception as error:
  result.update(qualified=False,error=str(error)[:2048])
 finally:
  try:
   commands.deadline=time.monotonic()+30 # independent exact owned cleanup budget
   if container:
    code,text=commands.call(['sudo','podman','inspect',container],check=False,timeout=30)
    if code==0:
     current=json.loads(text)
     require(len(current)==1 and current[0]['Name'].lstrip('/')==container and current[0]['Config']['Labels'].get('io.tuna.dense-owner')==nonce and current[0]['Image'].removeprefix('sha256:')==image.removeprefix('sha256:') and current[0]['Config']['User']=='107:107','cleanup owner changed; retain directory')
     exact_id=current[0]['Id']
     if result.get('container'):require(exact_id==result['container']['id'],'cleanup container ID changed')
     commands.call(['sudo','podman','rm','--force','--ignore',exact_id],timeout=15)
    else:
     code,_=commands.call(['sudo','podman','container','exists',container],check=False,timeout=30)
     require(code==1,'cleanup owner status unknown')

    require(not commands.process_cleanup_uncertain,'container/process retirement unconfirmed')
    # exists exits1 only for absence; use returncode rather than error text.
    code,_=commands.call(['sudo','podman','container','exists',container],check=False,timeout=30)
    require(code==1,'owned container absence unconfirmed')
   if target:
    value=target.lstat()
    require(not target.is_symlink() and (value.st_dev,value.st_ino)==owned and value.st_uid==107,'output directory identity changed')
    for child in target.iterdir():
     require(child.name in ('disk.raw','decoder.log','receipt.json') and not child.is_symlink() and child.is_file() and child.stat().st_uid==107,'unowned output child; retain directory')
    commands.call(['sudo','rm','--',*[str(p) for p in target.iterdir()]],timeout=30) if list(target.iterdir()) else None
    commands.call(['sudo','rmdir','--',str(target)],timeout=30)
    (directory/'request.json').unlink();directory.rmdir()
   result['owned_cleanup']=True
  except Exception as error:result.update(qualified=False,owned_cleanup=False,cleanup_error=str(error)[:2048])
  result['command_events']=commands.events if hasattr(commands,'events') else []
  write_json(receipt,result);commands.log=previous_log
 return {'file':receipt.name,'sha256':digest(receipt),'qualified':result['qualified'],
         'export_admission_changed':False,'owned_cleanup':result.get('owned_cleanup',False)}


def observe(_legacy_commands,artifacts,scratch,disk,raw_hash,info,source):
 """Private root scope; the old exporter executor never owns diagnostic commands."""
 artifacts,scratch,disk=map(Path,(artifacts,scratch,disk))
 request_path=artifacts/'dense-root-request.json'
 request={'artifacts':str(artifacts.resolve()),'scratch':str(scratch.resolve()),'disk':str(disk.resolve()),
          'raw_hash':raw_hash,'info':info,'source':source,'owner_uid':os.getuid(),
          'deadline':time.monotonic()+1800}
 with request_path.open('x') as file:json.dump(request,file)
 os.chmod(request_path,0o600)
 launcher=bounded_command(['sudo','-n','/usr/bin/python3',str(Path(__file__).resolve()),'owned-observer',str(request_path.resolve())],
                          artifacts/'dense-root-launcher.log',request['deadline']+35,limit=1024*1024,cleanup_seconds=10)
 receipt=artifacts/'dense-decode.json'
 if receipt.is_file():result=json.loads(receipt.read_text())
 else:result={'qualified':False,'owned_cleanup':False,'error':'root observer receipt missing'}
 result['root_launcher']=launcher
 if not launcher['owned_cleanup'] or launcher['command_error'] or launcher['exit']!=0:
  result.update(qualified=False,owned_cleanup=False)
 if not result.get('owned_cleanup',False):
  # Retain original backing/source if any diagnostic owner is uncertain.
  _legacy_commands.process_cleanup_uncertain=True
 # Root receipt has mode644. Parent writes a separate launcher receipt.
 outer=artifacts/'dense-launcher-receipt.json';write_json(outer,result)
 return {'file':outer.name,'sha256':digest(outer),'qualified':result.get('qualified',False),
         'owned_cleanup':result.get('owned_cleanup',False),'export_admission_changed':False}


if __name__=='__main__':
 if os.geteuid()!=0 or len(sys.argv)!=3 or sys.argv[1]!='owned-observer':raise SystemExit('private owned root observer only')
 path=Path(sys.argv[2]);require(not path.is_symlink() and path.is_file() and path.stat().st_size<=1024*1024,'owned bounded root request required')
 request=json.loads(path.read_text())
 require(path.stat().st_uid==request['owner_uid'] and path.stat().st_mode&0o777==0o600,'request owner/mode changed')
 for key in ('artifacts','scratch','disk'):
  value=Path(request[key]);require(value.is_absolute() and not value.is_symlink() and value.stat().st_uid==request['owner_uid'],'owned absolute diagnostic path required')
 require(path.parent==Path(request['artifacts']) and Path(request['disk']).parent==path.parent,'source/request artifact directory mismatch')
 require(0<request['deadline']-time.monotonic()<=1800,'bounded absolute operation deadline required')
 commands=DenseCommands(Path(request['artifacts'])/'dense-decode-host.log',request['deadline'])
 _observe(commands,request['artifacts'],request['scratch'],request['disk'],request['raw_hash'],request['info'],request['source'])
 raise SystemExit(0)
