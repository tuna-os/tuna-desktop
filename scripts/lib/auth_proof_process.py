"""Owned Linux proof process retirement; copied from reviewed fingerprint wrapper."""
import ctypes
import os
from pathlib import Path
import signal
import subprocess
import time

def bounded_command(argv, path, deadline, *, limit=1024*1024, env=None):
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
    if deadline-time.monotonic()<=0:raise TimeoutError('absolute phase deadline')
    libc=ctypes.CDLL(None,use_errno=True)
    prior=ctypes.c_int()
    if libc.prctl(37,ctypes.byref(prior),0,0,0)!=0 or libc.prctl(36,1,0,0,0)!=0:
        raise OSError('command subreaper setup failed')
    proc=None;lingered=False;retired=False;primary=None;timed_out=False;exit_status=None
    try:
        with path.open('wb') as f:
            proc=subprocess.Popen(argv,stdin=subprocess.DEVNULL,
                                  stdout=f,stderr=subprocess.STDOUT,start_new_session=True,env=env)
            while True:
                # WNOWAIT preserves the owned leader PID until group retirement.
                status=os.waitid(os.P_PID,proc.pid,os.WEXITED|os.WNOHANG|os.WNOWAIT)
                if status is not None:break
                if time.monotonic()>=deadline or f.tell()>limit:
                    timed_out=True
                    raise TimeoutError('phase child bound')
                time.sleep(.02)
            exit_status = status.si_status if status.si_code == os.CLD_EXITED else -status.si_status
            if exit_status != 0 or f.tell()>limit:
                raise RuntimeError('phase child failed')
    except BaseException as error:
        primary=error
    finally:
        if proc is not None:
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
            primary=RuntimeError('command retirement unconfirmed; phase wrapper must abort')
    return {'pid':proc.pid if proc is not None else None,
            'exit':124 if timed_out else exit_status, 'owned_cleanup':retired,
            'adopted_descendants_observed':lingered,
            'command_error':str(primary) if primary is not None else None,
            'ownership_method':'exclusive-single-thread-subreaper-held-leader-pidfd'}

