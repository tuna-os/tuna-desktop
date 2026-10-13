"""Probe-only owned Linux command supervisor, retained from reviewed publisher66c.

No registry publication logic. Scheduling/kernel stalls are not hard deadlines;
no success on unconfirmed child retirement. Ownership ends at process exit.
"""
import ctypes
import os
from pathlib import Path
import signal
import subprocess
import threading
import time

def bounded_command(argv, path, deadline, *, data=None, limit=4*1024*1024, env=None):
    """Linux single-owner command retirement, including orphaned descendants.

    Require one thread and no preexisting children. A temporary subreaper scope
    owns adopted descendants. Keep the session leader unreaped until group kill
    so its PID cannot be reused. Adopted children are killed via pidfds and reaped.
    Cleanup has a separate five-second budget; unconfirmed retirement fails closed.
    Subreaper ownership lasts only while this process lives, not after CLI exit.
    Spawn/kernel I/O stalls are not hard real-time guarantees.
    """
    def children():
        return [int(value) for value in Path('/proc/self/task/'+str(os.getpid())+'/children').read_text().split()]
    if len(list(Path('/proc/self/task').iterdir()))!=1 or children():
        raise ValueError('command supervisor requires exclusive single-thread child ownership')
    if deadline-time.monotonic()<=0:raise TimeoutError('absolute publisher deadline')
    if data is not None and len(data)>4096:raise ValueError('credential stdin exceeds bound')
    libc=ctypes.CDLL(None,use_errno=True)
    prior=ctypes.c_int()
    if libc.prctl(37,ctypes.byref(prior),0,0,0)!=0 or libc.prctl(36,1,0,0,0)!=0:
        raise OSError('command subreaper setup failed')
    proc=None;lingered=False;retired=False;primary=None
    try:
        with path.open('wb') as f:
            proc=subprocess.Popen(argv,stdin=subprocess.PIPE if data is not None else subprocess.DEVNULL,
                                  stdout=f,stderr=subprocess.DEVNULL,start_new_session=True,env=env)
            if data is not None:
                proc.stdin.write(data);proc.stdin.close()
            while True:
                # WNOWAIT preserves the owned leader PID until group retirement.
                status=os.waitid(os.P_PID,proc.pid,os.WEXITED|os.WNOHANG|os.WNOWAIT)
                if status is not None:break
                if time.monotonic()>=deadline or f.tell()>limit:raise TimeoutError('publisher command bound')
                time.sleep(.02)
            if status.si_code!=os.CLD_EXITED or status.si_status!=0 or f.tell()>limit:
                raise RuntimeError('publisher command failed: '+Path(argv[0]).name)
    except BaseException as error:
        primary=error
    finally:
        if proc is not None:
            if proc.stdin is not None and not proc.stdin.closed:proc.stdin.close()
            # The unreaped owned leader reserves the PGID even after exit.
            # Always kill the group: a successful leader can leave writers alive.
            try:
                os.killpg(proc.pid,signal.SIGKILL)
            except ProcessLookupError:
                pass
            cleanup_deadline=time.monotonic()+5
            while time.monotonic()<cleanup_deadline:
                direct=children()
                if not direct:
                    retired=True;break
                for pid in direct:
                    if pid!=proc.pid:lingered=True
                    try:
                        fd=os.pidfd_open(pid)
                    except ProcessLookupError:
                        continue
                    try:
                        # Direct children cannot be stolen/reused without our
                        # reaping; pidfd also binds the actual kernel process.
                        signal.pidfd_send_signal(fd,signal.SIGKILL)
                    finally:
                        os.close(fd)
                while True:
                    try:pid,status=os.waitpid(-1,os.WNOHANG)
                    except ChildProcessError:break
                    if pid==0:break
                    if pid==proc.pid:proc.returncode=os.waitstatus_to_exitcode(status)
                time.sleep(.01)
            if not retired and not children():retired=True
        else:
            retired=not children()
        if retired:
            if libc.prctl(36,prior.value,0,0,0)!=0:
                raise OSError('command subreaper restore failed')
        else:
            # Keep ownership while this process lives; an ensuing CLI exit is not
            # physical retirement or persistent quarantine proof.
            raise RuntimeError('command retirement unconfirmed; publisher must abort') from primary
    if primary is not None:raise primary
    if lingered:raise RuntimeError('command left adopted descendants; retired and refused')
