#!/usr/bin/python3
"""Temporary source-pinned launch prefix; diagnostic only, never acceptance."""
import ctypes
import difflib
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import resource
import shlex
import shutil
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
    'original_harness': HARNESS, 'observer': 'passive bounded core capture; not acceptance',
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
record = {'scope': 'STANDALONE DIAGNOSTIC PREFIX ONLY; not comparison execution, admission or acceptance',
          'source': admission['source'], 'harness': admission['harness'],
          'original_failed_run': 41210529702, 'original_failed_job': 124571621678,
          'original_failed_receipt_retained_at': '/tmp/brightness973-failures/grid-cancel-comparison/receipt-old.json'}
original_script = ROOT / 'scripts/tuna-gtk-shell-proof'
original = original_script.read_text()
lines = original.splitlines(keepends=True)
assert lines[2160].startswith('pass G-OV-LAUNCH ')
assert lines[374].startswith('ROOT=')
assert ''.join(lines[400:407]).startswith('if [ "$GRID_CANCEL_ONLY" = 1 ]; then')
prefix_lines = lines[:2161]
# Standalone diagnostic extraction, not a comparison/admission entrypoint.
# Preserve the original grid-only control-flow through the launch boundary.
prefix_lines[382] = 'GRID_CANCEL_ONLY=1 # diagnostic prefix selects original grid-only setup only\n'
prefix_lines[374] = 'ROOT=' + shlex.quote(str(ROOT)) + '\n'
prefix_lines[400:407] = ['# No comparison flags/admission are executed by this standalone diagnostic.\n']
extracted = ''.join(prefix_lines) + '\nexit 0\n'
prefix = WORK / 'standalone-launch-prefix.sh'
prefix.write_text(extracted)
prefix.chmod(0o700)
(WORK / 'prefix.diff').write_text(''.join(difflib.unified_diff(
    original.splitlines(keepends=True), extracted.splitlines(keepends=True),
    fromfile='immutable-8eebec13-full-proof', tofile='standalone-diagnostic-launch-prefix')))
record['extraction'] = {
    'source_path': str(original_script), 'source_sha256': hashlib.sha256(original.encode()).hexdigest(),
    'original_lines_inclusive': [1, 2161], 'boundary': lines[2160].strip(),
    'extracted_sha256': hashlib.sha256(extracted.encode()).hexdigest(),
    'differences': ['explicit pinned ROOT for relocated artifact', 'select original grid-only setup',
                    'remove comparison argument/admission guard; no comparison API or accepted tuple call',
                    'truncate after unchanged G-OV-LAUNCH assertion/pass; standalone exit'],
}
soft, hard = resource.getrlimit(resource.RLIMIT_CORE)
core_limit = 256 * 1024 * 1024
if hard != resource.RLIM_INFINITY:
    core_limit = min(core_limit, hard)
resource.setrlimit(resource.RLIMIT_CORE, (core_limit, hard))
record['core_capture'] = {'prior_limit': [soft, hard], 'active_limit': [core_limit, hard],
                          'core_pattern': Path('/proc/sys/kernel/core_pattern').read_text(),
                          'host_sysctl_modified': False, 'ptrace_used': False}
before_cores = set(ROOT.glob('core*'))
child = None
boundary = None
try:
    with output.open('w') as out:
        argv = [str(prefix), '--artifacts', str(WORK / 'runtime')]
        record['runtime_argv'] = argv
        record['runtime_cwd'] = str(ROOT)
        child = subprocess.Popen(argv, cwd=ROOT, env=env, stdin=subprocess.DEVNULL,
                                 stdout=out, stderr=subprocess.STDOUT, start_new_session=True)
        deadline = time.monotonic() + 600
        expected = (binaries / 'tuna-shell-gtk').resolve()
        while child.poll() is None and time.monotonic() < deadline:
            if 'observed_shell' not in record:
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
                    break
            text = output.read_text(errors='replace')
            boundary = next((line for line in text.splitlines() if 'G-OV-LAUNCH pass:' in line or 'G-OV-LAUNCH FAIL:' in line), None)
            if boundary:
                break
            time.sleep(.05)
        record['proof_exit_before_cleanup'] = child.poll()
        record['boundary'] = boundary
        record['runtime_deadline_reached'] = time.monotonic() >= deadline
finally:
    if child is not None:
        record['owned_cleanup'] = comparison.cleanup_owned(child, token)
    record['retained_cores'] = []
    for core in sorted(set(ROOT.glob('core*')) - before_cores):
        if core.is_file() and core.stat().st_size <= core_limit:
            saved = WORK / ('retained-' + core.name)
            shutil.copy2(core, saved)
            record['retained_cores'].append({'path': saved.name, 'sha256': comparison.contract.digest(saved)})
            with (WORK / (saved.name + '.backtrace.txt')).open('w') as trace:
                subprocess.run(['gdb', '--nx', '--batch', '-ex', 'set pagination off',
                                '-ex', 'info program', '-ex', 'thread apply all bt',
                                str(binaries / 'tuna-shell-gtk'), str(saved)],
                               stdout=trace, stderr=subprocess.STDOUT, timeout=30)
    for name, item in admission['files'].items():
        assert comparison.contract.digest(item['admitted_executable']) == item['sha256'], name
    record['proof_output_sha256'] = comparison.contract.digest(output)
    (WORK / 'diagnostic-receipt.json').write_text(json.dumps(record, indent=2) + '\n')
print(json.dumps(record, indent=2))
