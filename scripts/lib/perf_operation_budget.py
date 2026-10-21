"""Local disposable-VM trace-start supervision; termination is not D-Bus completion.

Only fixed 20s work + 15s cleanup budgets are used in production. The caller
must own a live disposable VM and retire it after any failed operation.
"""
import json
import math
import os
import select
import signal
import subprocess
import sys
import time

WORK_SECONDS = 20
CLEANUP_SECONDS = 15
MAX_OUTPUT = 65536
STAGES = {'provenance', 'kernel-start', 'profiler', 'complete'}
_active = None
_progress = None

class Budget:
    def __init__(self, deadline, now=time.monotonic):
        if type(deadline) not in (int, float) or not math.isfinite(deadline):
            raise ValueError('finite deadline required')
        self.deadline, self.now = deadline, now
    def remaining(self, cap):
        value = self.deadline - self.now()
        if value <= 0:
            raise TimeoutError('operation budget exhausted')
        return min(cap, value)

def remaining(cap):
    return _active.remaining(cap) if _active is not None else cap

def stage(value):
    if value not in STAGES:
        raise ValueError('fixed operation stage required')
    if _progress is not None:
        os.write(_progress, (value + '\n').encode())
    remaining(WORK_SECONDS)

def profiler_environment():
    return [] if _active is None else ['TUNA_TRACE_WORK_DEADLINE=' + repr(_active.deadline)]

def adopt_profiler_budget():
    global _active
    raw = os.environ.get('TUNA_TRACE_WORK_DEADLINE')
    if raw is not None:
        _active = Budget(float(raw))
        _active.remaining(WORK_SECONDS)

class OperationFailure(RuntimeError):
    def __init__(self, receipt):
        self.receipt = receipt
        super().__init__('supervised trace start failed; disposable VM retirement required')

def receipt(stage_name, outcome, helper_retired, kernel_entered, marker_present):
    return {'trace_failure': {'schema': 2, 'action': 'start', 'stage': stage_name,
        'outcome': outcome, 'helper_retirement': 'confirmed' if helper_retired else 'unconfirmed',
        'kernel_state': ('uncertain' if stage_name == 'unknown' else 'not-entered' if not kernel_entered else
                         'entered-with-marker' if marker_present else 'entered-without-marker'),
        # The profiler stage includes validation before Start. Without a terminal
        # success receipt, conservatively treat possible acceptance as uncertain.
        'start_acceptance': 'uncertain' if stage_name in {'profiler', 'complete', 'unknown'} else 'not-sent',
        'cleanup': 'helpers-retired' if helper_retired else 'expired',
        'vm_retirement_required': True, 'performance_qualified': False}}

def supervise(call, marker_exists, *, work_seconds=WORK_SECONDS, cleanup_seconds=CLEANUP_SECONDS):
    """Fork a process group, boundedly collect output, and retire helpers on failure.

    A waitid WNOWAIT observation keeps the leader unreaped until after killpg,
    so its original process-group number cannot be recycled before the signal.
    Detached observers/server operations are explicitly NOT retired by killpg.
    """
    global _active, _progress
    deadline = time.monotonic() + work_seconds
    read_fd, write_fd = os.pipe()
    progress_read, progress_write = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(read_fd); os.close(progress_read)
        try:
            os.setsid()
            _active, _progress = Budget(deadline), progress_write
            # Existing helper diagnostics may print. Keep them off the protocol
            # pipe; the bounded supervisor emits its own strict v2 failure.
            null = os.open(os.devnull, os.O_WRONLY)
            os.dup2(null, 1); os.dup2(null, 2); os.close(null)
            result = call()
            stage('complete')
            raw = json.dumps(result, separators=(',', ':')).encode()
            if len(raw) > MAX_OUTPUT:
                raise ValueError('operation output bound')
            view = memoryview(raw)
            while view:
                count = os.write(write_fd, view)
                if count <= 0:raise OSError('operation output write failed')
                view = view[count:]
            os._exit(0)
        except BaseException as error:
            if isinstance(error, (TimeoutError, subprocess.TimeoutExpired)):
                os.write(progress_write, b'work-timeout\n')
            os._exit(1)
    os.close(write_fd); os.close(progress_write)
    for fd in (read_fd, progress_read):os.set_blocking(fd, False)
    output = bytearray(); progress = bytearray(); last_stage = 'provenance'
    kernel_entered = False; observed = None; outcome = 'expired'; worker_timeout = False
    progress_valid = True; progress_bytes = 0

    def accept_progress(data):
        nonlocal last_stage, kernel_entered, worker_timeout, progress_valid, progress_bytes
        progress_bytes += len(data)
        if progress_bytes > 4096:
            progress_valid = False
            return
        progress.extend(data)
        while b'\n' in progress:
            line, _, tail = progress.partition(b'\n');progress[:] = tail
            try:value = line.decode('ascii')
            except UnicodeDecodeError:
                progress_valid = False
                continue
            if value == 'work-timeout':
                worker_timeout = True
            elif value in STAGES:
                last_stage = value
                kernel_entered |= value == 'kernel-start'
            else:progress_valid = False

    def drain_progress():
        nonlocal progress_valid
        # Nonblocking and byte-bounded. EOF is not required: a detached process
        # could inherit an FD, but cannot expand the bounded retirement wait.
        try:
            while progress_bytes <= 4096:
                try:data = os.read(progress_read, 4097)
                except BlockingIOError:break
                if not data:break
                accept_progress(data)
        except OSError:progress_valid = False

    try:
        while time.monotonic() < deadline:
            ready, _, _ = select.select([read_fd, progress_read], [], [], min(.02, max(0, deadline-time.monotonic())))
            for fd in ready:
                data = os.read(fd, MAX_OUTPUT + 1)
                if fd == read_fd:
                    output.extend(data)
                else:accept_progress(data)
            if len(output) > MAX_OUTPUT or not progress_valid:
                outcome = 'failed'
                break
            observed = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            if observed is not None:
                try:output.extend(os.read(read_fd, MAX_OUTPUT + 1))
                except BlockingIOError:pass
                drain_progress()
                if (observed.si_code == os.CLD_EXITED and observed.si_status == 0
                        and len(output) <= MAX_OUTPUT and progress_valid and not progress):
                    result = json.loads(output)
                    os.waitpid(pid, 0)
                    os.close(read_fd); os.close(progress_read)
                    return result
                outcome = 'expired' if worker_timeout else 'failed'
                break
    except BaseException:
        outcome = 'failed'
    cleanup_deadline = time.monotonic() + cleanup_seconds
    retired = False

    def signal_original():
        # Query the still-unreaped leader now, not before a potentially blocking
        # select. It may have called setsid since the previous observation.
        try:
            if os.getpgid(pid) == pid:os.killpg(pid, signal.SIGKILL)
            else:os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:pass

    try:
        signal_original()
        while time.monotonic() < cleanup_deadline:
            observed = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
            if observed is not None:
                # Recheck after leader termination too: setsid may race the
                # earlier query/direct-child signal. Its PID still cannot recycle.
                signal_original()
                drain_progress()
                retired = group_departed(pid, cleanup_deadline)
                if retired:break
            time.sleep(min(.01, max(0, cleanup_deadline-time.monotonic())))
        drain_progress()
        if observed is not None:os.waitpid(pid, 0)
    finally:
        os.close(read_fd);os.close(progress_read)
    try:marker_present = bool(marker_exists())
    except OSError:marker_present = False;progress_valid = False
    # A live leader, lost/malformed progress, or contradictory marker means
    # effects may have occurred. Never infer not-entered/not-sent from silence.
    if observed is None or not progress_valid or progress or (marker_present and not kernel_entered):
        last_stage = 'unknown'
    if worker_timeout:outcome = 'expired'
    raise OperationFailure(receipt(last_stage, outcome, retired, kernel_entered, marker_present))


def group_departed(group, deadline):
    try:
        entries = os.listdir('/proc')
        if len(entries) > 65536:return False
        for name in entries:
            if time.monotonic() >= deadline:return False
            if not name.isdecimal():continue
            try:
                with open('/proc/' + name + '/stat') as stream:raw = stream.read(8193)
                if len(raw)>8192:return False
                fields = raw.rsplit(')', 1)[1].split()
                if int(fields[2]) == group and fields[0] not in {'Z', 'X'}:return False
            except (FileNotFoundError, ProcessLookupError):continue
        return True
    except (OSError, ValueError, IndexError):return False
