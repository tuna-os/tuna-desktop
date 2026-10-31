#!/usr/bin/python3
"""Temporary diagnostic only: passive debugger overhead invalidates acceptance."""
import ctypes
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
import uuid

ROOT = Path(sys.argv[1]).resolve()
WORK = Path(sys.argv[2]).resolve()
WORK.mkdir()
OLD = 'da1b7c40c7cc69f5800970e61cffa3d4e95c3f65'
HARNESS = '8eebec1308374d78b819c7f2fec48c03f7fc6d55'
loader = importlib.machinery.SourceFileLoader('original_comparison', str(ROOT / 'scripts/tuna-grid-cancel-comparison'))
spec = importlib.util.spec_from_loader(loader.name, loader)
comparison = importlib.util.module_from_spec(spec)
loader.exec_module(comparison)
assert comparison.contract.source_tuple(ROOT)['sha'] == HARNESS
subprocess.run(['git', 'fetch', '--no-tags', 'origin', OLD], cwd=ROOT, check=True, timeout=60)
source = WORK / 'old-source'
subprocess.run(['git', 'worktree', 'add', '--detach', str(source), OLD], cwd=ROOT, check=True, timeout=60)
assert comparison.contract.source_tuple(source)['sha'] == OLD
admission_path, admission, binaries = comparison.build(source, 'old', WORK, {
    'diagnostic_only': True, 'original_run': 41210529702, 'original_job': 124571621678,
    'original_harness': HARNESS, 'observer': 'external sudo gdb attach; not acceptance',
})
assert ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) == 0
home = WORK / 'home'
home.mkdir(mode=0o700)
for name in ('config', 'cache', 'data'):
    (home / name).mkdir(mode=0o700)
token = str(uuid.uuid4())
env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / 'config'),
           XDG_CACHE_HOME=str(home / 'cache'), XDG_DATA_HOME=str(home / 'data'),
           TUNA_PROOF_BIN_DIR=str(binaries), TUNA_PROOF_COMPANIONS=str(binaries / 'examples'),
           TUNA_PROOF_BINARY_SOURCE_COMMIT=OLD, TUNA_GRID_COMPARISON_OWNER=token,
           PYTHONDONTWRITEBYTECODE='1')
for name in ('DISPLAY', 'WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'TUNA_PROOF_SHELL_BIN',
             'TUNA_SHELL_BIN', 'TUNA_PROOF_PRIVATE_BUS', 'AT_SPI_BUS_ADDRESS',
             'DBUS_SESSION_BUS_ADDRESS', 'XDG_RUNTIME_DIR'):
    env.pop(name, None)
output = WORK / 'proof-output.txt'
record = {'scope': 'DIAGNOSTIC ONLY; debugger observer overhead; not GRID acceptance',
          'source': admission['source'], 'harness': admission['harness'],
          'original_failed_run': 41210529702, 'original_failed_job': 124571621678,
          'original_failed_receipt_retained_at': '/tmp/brightness973-failures/grid-cancel-comparison/receipt-old.json'}
debugger = None
child = None
boundary = None
try:
    with output.open('w') as out, (WORK / 'debugger.txt').open('w') as debug_out:
        child = subprocess.Popen([str(ROOT / 'scripts/tuna-gtk-shell-proof'), '--artifacts', str(WORK / 'runtime')],
                                 cwd=ROOT, env=env, stdin=subprocess.DEVNULL, stdout=out,
                                 stderr=subprocess.STDOUT, start_new_session=True)
        deadline = time.monotonic() + 600
        expected = (binaries / 'tuna-shell-gtk').resolve()
        while child.poll() is None and time.monotonic() < deadline:
            if debugger is None:
                for pid, start in comparison.owned_processes(token):
                    proc = Path('/proc') / str(pid)
                    try:
                        if (proc / 'exe').resolve() != expected:
                            continue
                        digest = hashlib.sha256((proc / 'exe').read_bytes()).hexdigest()
                        if digest != admission['files']['tuna-shell-gtk']['sha256']:
                            raise RuntimeError('live shell bytes differ from original build')
                        if (proc / 'stat').read_text().rsplit(')', 1)[1].split()[19] != start:
                            raise RuntimeError('live shell replaced during debugger admission')
                    except (FileNotFoundError, ProcessLookupError, PermissionError):
                        continue
                    record['observed_shell'] = {'pid': pid, 'start': start, 'sha256': digest}
                    args = ['sudo', '-n', 'gdb', '--nx', '--batch', '-ex', 'set pagination off',
                            '-ex', 'set print thread-events off', '-ex', 'set debuginfod enabled off',
                            '-ex', 'handle SIGPIPE nostop noprint pass', '-ex', f'attach {pid}',
                            '-ex', 'continue', '-ex', 'info program', '-ex', 'thread apply all bt',
                            '-ex', f'generate-core-file {WORK / "shell.core"}', '-ex', 'detach']
                    debugger = subprocess.Popen(args, stdout=debug_out, stderr=subprocess.STDOUT,
                                                stdin=subprocess.DEVNULL, start_new_session=True)
                    record['debugger_argv'] = args
                    break
            text = output.read_text(errors='replace')
            boundary = next((line for line in text.splitlines() if 'G-OV-LAUNCH pass:' in line or 'G-OV-LAUNCH FAIL:' in line), None)
            if boundary:
                break
            time.sleep(.05)
        record['proof_exit_before_cleanup'] = child.poll()
        record['boundary'] = boundary
        record['runtime_deadline_reached'] = time.monotonic() >= deadline
        if debugger is not None and debugger.poll() is None:
            os.killpg(debugger.pid, signal.SIGINT)
            try:
                debugger.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(debugger.pid, signal.SIGTERM)
                try:
                    debugger.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(debugger.pid, signal.SIGKILL)
                    debugger.wait(timeout=3)
        record['debugger_exit'] = debugger.returncode if debugger else None
finally:
    if child is not None:
        record['owned_cleanup'] = comparison.cleanup_owned(child, token)
    for name, item in admission['files'].items():
        assert comparison.contract.digest(item['admitted_executable']) == item['sha256'], name
    record['proof_output_sha256'] = comparison.contract.digest(output)
    (WORK / 'diagnostic-receipt.json').write_text(json.dumps(record, indent=2) + '\n')
print(json.dumps(record, indent=2))
