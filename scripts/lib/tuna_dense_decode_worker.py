"""Exact-image diagnostic only; never an import/export admission entry point."""
import hashlib
import json
import os
from pathlib import Path
import signal
import sys
import time

GIB=1024**3
VIRTUAL_BYTES=18*GIB
MEMORY_BYTES=512*1024**2
OUTPUT_RESERVE=20*GIB
SAFETY_BYTES=2*GIB+128*1024**2
QEMU_SHA='250b2b4ee552fdd96e1078525d12bb0b1f2557f33f153c282bfaea1ece5b3fad'


def digest(path,deadline=None):
 h=hashlib.sha256()
 with Path(path).open('rb') as stream:
  while True:
   if deadline is not None and time.monotonic()>=deadline:raise TimeoutError('readback deadline')
   block=stream.read(1024*1024)
   if not block:break
   h.update(block)
 return h.hexdigest()


def own_cgroup():
 rows=Path('/proc/self/cgroup').read_text().splitlines()
 unified=[row[3:] for row in rows if row.startswith('0::')]
 if len(unified)!=1 or '..' in Path(unified[0]).parts:raise ValueError('one cgroup v2 identity required')
 # A cgroup namespace may expose the process root as /; never guess another group.
 group=Path('/sys/fs/cgroup')/unified[0].lstrip('/')
 if not group.is_dir():raise ValueError('own cgroup path unavailable')
 return group


def memory(group):
 def number(name):
  text=(group/name).read_text().strip()
  if not text.isdecimal():raise ValueError('measured finite cgroup limit required')
  return int(text)
 return {'current_bytes':number('memory.current'),'peak_bytes':number('memory.peak'),
         'max_bytes':number('memory.max'),'swap_max_bytes':number('memory.swap.max'),
         'events':{k:int(v) for k,v in (line.split() for line in (group/'memory.events').read_text().splitlines())}}


def validate_memory(before,after):
 if before['max_bytes']!=MEMORY_BYTES or after['max_bytes']!=MEMORY_BYTES or before['swap_max_bytes']!=0 or after['swap_max_bytes']!=0:
  raise ValueError('actual 512Mi memory limit required')
 if after['peak_bytes']>MEMORY_BYTES:raise ValueError('measured cgroup charge exceeded limit')
 for name in ('oom','oom_kill'):
  if name not in before['events'] or name not in after['events'] or after['events'][name]!=before['events'][name]:
   raise ValueError('OOM or missing event observation')


def owned_children():
 # The private PID namespace init adopts every orphaned descendant. A held
 # WNOWAIT leader prevents PID/group recycling until all children are gone.
 if os.getpid()!=1:raise ValueError('private PID namespace init required')
 return tuple(int(value) for value in Path('/proc/self/task/1/children').read_text().split())


def retire_decoder(child,receipt,deadline):
 """Reap only a terminal leader with no surviving adopted descendants."""
 while time.monotonic()<deadline:
  observed=os.waitid(os.P_PID,child.pid,os.WEXITED|os.WNOHANG|os.WNOWAIT)
  children=owned_children()
  if observed is not None and children==(child.pid,):
   # No signal is needed for this zombie. In particular, a denied killpg
   # must not mask a successful decoder or destroy the diagnostic receipt.
   _,status,usage=os.wait4(child.pid,0)
   child.returncode=os.waitstatus_to_exitcode(status)
   receipt.update(decoder_exit=child.returncode,
                  qemu_peak_rss_bytes=int(usage.ru_maxrss)*1024,
                  decoder_rss_measured=True,decoder_descendants_absent=True)
   return True
  try:os.killpg(child.pid,signal.SIGKILL)
  except ProcessLookupError:pass
  except PermissionError as error:
   receipt['decoder_signal_denied']=str(error)[:1024]
  time.sleep(.02)
 receipt.update(qualified=False,decoder_retirement_unconfirmed=True)
 return False


def main():
 target=Path('/target');request=json.loads(Path('/observer/request.json').read_text())
 receipt={'schema':1,'qualified':False,'scope':'dense CI decoder/readback only; no export/import/VM qualification',
          'request':request,'virtual_bytes':VIRTUAL_BYTES,'decoder_rss_measured':False}
 child=None;reaped=False
 try:
  if os.getuid()!=107 or os.getgid()!=107:raise ValueError('owned UID/GID107 required')
  if os.getpid()!=1:raise ValueError('private PID namespace init required')
  if request['virtual_bytes']!=VIRTUAL_BYTES:raise ValueError('exact 18Gi source required')
  if sys.version_info[:3]!=(3,12,15):raise ValueError('admitted private Python version required')
  operation_deadline=request['operation_deadline_monotonic']
  if type(operation_deadline) not in (int,float) or operation_deadline<=time.monotonic():raise ValueError('absolute operation budget required')
  if digest('/observer/tool-identity.json')!=request['tool_identity_sha256'] or digest('/observer/worker.py')!=request['worker_sha256']:raise ValueError('observer source bytes changed')
  identity=json.loads(Path('/observer/tool-identity.json').read_text())
  if identity['image']!=request['tool'] or identity['qemu_after_sha256']!=QEMU_SHA:raise ValueError('admitted tool custody mismatch')
  # Check every original closure file; observed Python ELF maps must stay private
  # and match original custody. Other original modules need not all be loaded.
  for name,expected in {**identity['python_libraries'],**identity['qemu_libraries']}.items():
   if digest(name,min(operation_deadline,time.monotonic()+30))!=expected:raise ValueError('tool closure file changed')
  mapped={}
  for line in Path('/proc/self/maps').read_text().splitlines():
   fields=line.split(None,5)
   if len(fields)!=6 or not fields[5].startswith('/'):continue
   if fields[5].endswith(' (deleted)'):raise ValueError('deleted Python mapping')
   actual=Path(fields[5]).resolve(strict=True)
   with actual.open('rb') as file:is_elf=file.read(4)==b'\x7fELF'
   if is_elf:
    if not actual.is_relative_to('/opt/tuna-python'):raise ValueError('Python mapped global ELF')
    name=str(actual);mapped[name]=digest(actual,min(operation_deadline,time.monotonic()+30))
  if not mapped or any(identity['python_libraries'].get(name)!=sha for name,sha in mapped.items()):raise ValueError('Python mapping differs from admitted custody')
  if str(Path(sys.executable).resolve())!=identity['python_executable']:raise ValueError('private Python executable changed')
  receipt['python_elf_mappings_sha256']=mapped
  import subprocess
  listing=subprocess.run(['/lib64/ld-linux-x86-64.so.2','--list','/usr/bin/qemu-img'],capture_output=True,timeout=min(10,max(.01,operation_deadline-time.monotonic())),check=True,text=True)
  if len(listing.stdout)>65536:raise ValueError('QEMU loader output bound')
  libraries={token:digest(token,min(operation_deadline,time.monotonic()+30)) for line in listing.stdout.splitlines() for token in line.split() if token.startswith('/')}
  if libraries!=identity['qemu_libraries']:raise ValueError('QEMU actual required library closure changed')
  receipt['qemu_required_libraries_sha256']=libraries
  qemu=Path('/usr/bin/qemu-img')
  if digest(qemu)!=QEMU_SHA:raise ValueError('admitted QEMU executable required')
  receipt['qemu_executable_sha256']=QEMU_SHA
  source=Path('/input/disk.qcow2');before_stat=source.stat()
  if digest(source,min(operation_deadline,time.monotonic()+900))!=request['qcow_sha256']:raise ValueError('source hash mismatch')
  group=own_cgroup();receipt['cgroup_path']=str(group);receipt['memory_before']=memory(group)
  if receipt['memory_before']['max_bytes']!=MEMORY_BYTES:raise ValueError('memory limit not enforced')
  if list(target.iterdir()):raise ValueError('new empty owned output directory required')
  output=target/'disk.raw';log=target/'decoder.log'
  if Path('/proc/self/task/'+str(os.getpid())+'/children').read_text().strip():raise ValueError('exclusive child ownership required')
  import subprocess
  started=time.monotonic();deadline=min(operation_deadline,started+900)
  with log.open('xb') as stream:
   child=subprocess.Popen([str(qemu),'convert','-f','qcow2','-O','raw','-S','4k',str(source),str(output)],
                          stdin=subprocess.DEVNULL,stdout=stream,stderr=subprocess.STDOUT,start_new_session=True)
   receipt['decoder_pid']=child.pid
   while True:
    if time.monotonic()>=deadline:raise TimeoutError('decoder deadline')
    if stream.tell()>1024*1024:raise ValueError('decoder log bound')
    written=output.stat().st_blocks*512 if output.exists() else 0
    fs=os.statvfs(target)
    if fs.f_bavail*fs.f_frsize<SAFETY_BYTES+max(0,OUTPUT_RESERVE-written):raise ValueError('remaining output capacity refused')
    status=os.waitid(os.P_PID,child.pid,os.WEXITED|os.WNOHANG|os.WNOWAIT)
    if status is not None:break
    time.sleep(.05)
   reaped=retire_decoder(child,receipt,time.monotonic()+5)
   if not reaped:raise ValueError('decoder retirement unconfirmed')
   receipt['decoder_elapsed_seconds']=time.monotonic()-started
   if child.returncode!=0:raise ValueError('decoder failed')
   if stream.tell()>1024*1024:raise ValueError('decoder log bound')
  value=output.stat()
  if value.st_size!=VIRTUAL_BYTES:raise ValueError('raw virtual size mismatch')
  receipt['output_stat']={'virtual_bytes':value.st_size,'allocated_bytes':value.st_blocks*512,'uid':value.st_uid,'gid':value.st_gid}
  if value.st_uid!=107 or value.st_gid!=107:raise ValueError('output ownership changed')
  receipt['raw_sha256']=digest(output,min(operation_deadline,time.monotonic()+900))
  if receipt['raw_sha256']!=request['raw_sha256']:raise ValueError('full readback hash mismatch')
  current=source.stat()
  if digest(source,min(operation_deadline,time.monotonic()+900))!=request['qcow_sha256'] or any(getattr(current,k)!=getattr(before_stat,k) for k in ('st_dev','st_ino','st_size','st_mtime_ns')):
   raise ValueError('source changed')
  receipt['memory_after']=memory(group);validate_memory(receipt['memory_before'],receipt['memory_after'])
  receipt['qualified']=True
 except BaseException as error:
  receipt['error']=str(error)[:2048]
 finally:
  if child is not None and not reaped:
   try:
    reaped=retire_decoder(child,receipt,time.monotonic()+5)
   except Exception as error:
    receipt.update(qualified=False,decoder_retirement_unconfirmed=True,
                   decoder_cleanup_error=str(error)[:1024])
  receipt['owned_decoder_retired']=child is None or reaped
  try:
   receipt['memory_final']=memory(own_cgroup())
  except Exception as error:receipt['memory_observation_error']=str(error)[:1024];receipt['qualified']=False
  (target/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
 return 0 if receipt['qualified'] else 1

if __name__=='__main__':raise SystemExit(main())
