"""Exclusive command ownership for this diagnostic only; no exporter executor changes."""
import ctypes
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def identity(pid):
 text=Path('/proc',str(pid),'stat').read_text().rsplit(')',1)[1].split()
 return {'parent':int(text[1]),'group':int(text[2]),'start':int(text[19])}


def signal_owned(pid,owner,start,group=False):
 """Narrow privileged helper: exact direct child identity, no free-form command."""
 value=identity(pid)
 if value['parent']!=owner or value['start']!=start:raise ValueError('owned child identity mismatch')
 fd=os.pidfd_open(pid)
 try:
  if identity(pid)!=value:raise ValueError('owned child identity changed')
  if group:
   if value['group']!=pid:raise ValueError('owned reserved leader group required')
   os.killpg(pid,signal.SIGKILL)
  signal.pidfd_send_signal(fd,signal.SIGKILL)
 finally:os.close(fd)


def bounded_command(argv,path,deadline,*,limit=1024*1024,cleanup_seconds=5):
 """Hold leader, then retire group/adopted children. Unknown retirement is failure.

    Privileged commands run inside a root-owned diagnostic supervisor. Its outer
    sudo launcher uses the fixed identity-checked signal helper on permission
    denial. Helper scheduling/kernel stalls are not hard real-time guarantees;
    no success receipt is possible without every direct child being reaped.
    """
 def children():return [int(v) for v in Path('/proc/self/task',str(os.getpid()),'children').read_text().split()]
 if len(list(Path('/proc/self/task').iterdir()))!=1 or children():raise ValueError('exclusive child ownership required')
 if deadline<=time.monotonic():raise TimeoutError('operation deadline')
 libc=ctypes.CDLL(None,use_errno=True);prior=ctypes.c_int()
 if libc.prctl(37,ctypes.byref(prior),0,0,0) or libc.prctl(36,1,0,0,0):raise OSError('subreaper setup')
 proc=None;exit_code=None;error=None;retired=False;adopted=False;privileged_helpers=[]
 def kill(pid,group,cleanup_deadline):
  try:
   value=identity(pid)
   if value['parent']!=os.getpid():raise ValueError('child no longer owned')
   try:
    signal_owned(pid,os.getpid(),value['start'],group)
   except PermissionError:
    # Parent holds this child unreaped, so PID and group cannot be recycled.
    helper=subprocess.Popen(['sudo','-n',sys.executable,str(Path(__file__).resolve()),'kill-owned',str(pid),str(os.getpid()),str(value['start']),str(int(group))],
                            stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True)
    privileged_helpers.append(helper.pid)
    while time.monotonic()<cleanup_deadline:
     status=os.waitid(os.P_PID,helper.pid,os.WEXITED|os.WNOHANG|os.WNOWAIT)
     if status is not None:
      _,value=os.waitpid(helper.pid,0);helper.returncode=os.waitstatus_to_exitcode(value)
      if helper.returncode!=0:raise RuntimeError('privileged signal helper failed')
      return
     time.sleep(.01)
    # Do not reap a still-live helper or fabricate cleanup; fail closed.
    raise TimeoutError('privileged signal helper retirement unconfirmed')
  except ProcessLookupError:pass
 try:
  with Path(path).open('wb') as out:
   proc=subprocess.Popen(argv,stdin=subprocess.DEVNULL,stdout=out,stderr=subprocess.STDOUT,start_new_session=True)
   while True:
    if time.monotonic()>=deadline:raise TimeoutError('operation deadline')
    if out.tell()>limit:raise ValueError('command output bound')
    observed=os.waitid(os.P_PID,proc.pid,os.WEXITED|os.WNOHANG|os.WNOWAIT)
    if observed is not None:
     exit_code=observed.si_status if observed.si_code==os.CLD_EXITED else -observed.si_status
     break
    time.sleep(.02)
   if out.tell()>limit:raise ValueError('command output bound')
 except BaseException as failure:error=str(failure)[:2048]
 finally:
  cleanup_deadline=time.monotonic()+cleanup_seconds
  try:
   if proc is not None:kill(proc.pid,True,cleanup_deadline)
   while time.monotonic()<cleanup_deadline:
    direct=children()
    if not direct:retired=True;break
    for pid in direct:
     if pid!=proc.pid:adopted=True
     kill(pid,False,cleanup_deadline)
    while True:
     try:pid,status=os.waitpid(-1,os.WNOHANG)
     except ChildProcessError:break
     if pid==0:break
     if proc is not None and pid==proc.pid:proc.returncode=os.waitstatus_to_exitcode(status)
    time.sleep(.01)
   if not children():retired=True
  except BaseException as failure:error=(error or '')+'; '+str(failure)[:2048]
  if retired:
   if libc.prctl(36,prior.value,0,0,0):error=(error or '')+'; subreaper restore failed'
  else:error=(error or '')+'; owned retirement unconfirmed'
 return {'exit':exit_code,'command_error':error,'owned_cleanup':retired,'adopted_descendants_observed':adopted,
         'pid':proc.pid if proc else None,'privileged_signal_helpers':len(privileged_helpers),
         'ownership_method':'exclusive-subreaper-held-leader-pidfd-with-fixed-privileged-signal'}


class DenseCommands:
 def __init__(self,log,deadline):
  if os.geteuid()!=0:raise ValueError('isolated root diagnostic supervisor required')
  self.log=Path(log);self.deadline=deadline;self.events=[];self.process_cleanup_uncertain=False
 def call(self,args,check=True,timeout=60,max_output=4*1024*1024):
  args=list(args)
  if args[0]=='sudo':args=args[1:]
  if args[0] not in ('podman','python3','chown','rm','rmdir'):raise ValueError('diagnostic fixed command family required')
  with self.log.open('a') as file:file.write(json.dumps(args)+'\n')
  output=self.log.with_suffix('.command-output')
  event=bounded_command(args,output,min(self.deadline,time.monotonic()+timeout),limit=max_output)
  self.events.append(event)
  if not event['owned_cleanup']:self.process_cleanup_uncertain=True
  with self.log.open('a') as file:file.write(output.read_text(errors='replace'))
  if event['command_error'] or not event['owned_cleanup'] or check and event['exit']!=0:raise RuntimeError(str(event))
  return event['exit'],output.read_text(errors='replace')

if __name__=='__main__':
 if len(sys.argv)!=6 or sys.argv[1]!='kill-owned' or os.geteuid()!=0 or sys.argv[5] not in ('0','1'):raise SystemExit('private owned signal helper only')
 signal_owned(int(sys.argv[2]),int(sys.argv[3]),int(sys.argv[4]),bool(int(sys.argv[5])))
