#!/usr/bin/python3
"""Batch journey bookkeeping beside the unchanged native portal requesters.

This is test orchestration, not a portal provider. Consent remains the real
AT-SPI action, frames use the real frontend PipeWire FD, and lock input stays
on the host's noVNC console. No requester deadline is changed here.
"""
import base64
import datetime
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec='milliseconds')


def session_state(phase):
    from gi.repository import Gio
    try:
        bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        reply = bus.call_sync('org.gnome.ScreenSaver', '/org/gnome/ScreenSaver',
                              'org.gnome.ScreenSaver', 'GetActive', None, None,
                              Gio.DBusCallFlags.NONE, 5000, None).unpack()
        if reply not in ((False,), (True,)):
            raise RuntimeError('unexpected ScreenSaver reply: ' + repr(reply))
        return {'phase': phase, 'at': now(), 'exit': 0,
                'stdout': '(true,)' if reply[0] else '(false,)', 'stderr': ''}
    except Exception as error:
        return {'phase': phase, 'at': now(), 'exit': 1, 'stdout': '',
                'stderr': f'{type(error).__name__}: {error}'}


def unlocked(state):
    return state['exit'] == 0 and state['stdout'] == '(false,)'


def wait_for_files(paths, seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if any(path.exists() for path in paths):
            return True
        time.sleep(.01)
    return any(path.exists() for path in paths)


def paths(root):
    return {name: root / name for name in ('frame.png', 'frame.waiting', 'frame.ready',
            'frame.active', 'frame.revoke', 'frame.response.json', 'frame.revoked.json',
            'requester.exit', 'requester.log', 'consent.json', 'worker.json')}


def snapshot(root):
    p = paths(root)
    result = {'root': str(root), 'requester_exit': None, 'log': '', 'response': None}
    if p['requester.exit'].exists():
        result['requester_exit'] = p['requester.exit'].read_text().strip()
    if p['requester.log'].exists():
        result['log'] = p['requester.log'].read_text()
    if p['frame.response.json'].exists():
        result['response'] = json.loads(p['frame.response.json'].read_text())
    return result


def process_start(pid):
    try:
        return Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19]
    except FileNotFoundError:
        return None


def stop_requester(root):
    """Retire only this requester's still-identical process group on failure."""
    p = paths(root)
    if p['requester.exit'].exists() or not p['worker.json'].exists():
        return False
    worker = json.loads(p['worker.json'].read_text())
    if not worker['start'] or process_start(worker['pid']) != worker['start']:
        return False
    try:
        os.killpg(worker['pid'], signal.SIGTERM)
    except ProcessLookupError:
        return False
    return True


def launch(values):
    state = session_state('before-request')
    report = {'session_state': [state], 'ready': False, 'finished': False}
    if not unlocked(state):
        report['error'] = 'portal before-request requires an unlocked session'
        return report
    root = Path(os.environ['XDG_RUNTIME_DIR']) / values['directory']
    root.mkdir()
    p = paths(root)
    for name, data in values['sources'].items():
        (root / name).write_bytes(base64.b64decode(data))
    kind = values['kind']
    script = 'tuna-portal-screenshot-client.py' if kind == 'screenshot' else 'tuna-portal-capture-client.py'
    mode = 'allow' if kind == 'screenshot' else 'revoke'
    argv = [sys.executable, str(root / script), str(p['frame.png']), mode]
    # The worker survives this SSH exchange, records the exact native exit,
    # and owns a separate process group so failure cleanup cannot hit peers.
    worker_code = ('import pathlib,subprocess; '
                   f'r=subprocess.run({argv!r}); '
                   f'pathlib.Path({str(p["requester.exit"])!r}).write_text(str(r.returncode))')
    with p['requester.log'].open('wb') as log:
        worker = subprocess.Popen([sys.executable, '-c', worker_code], stdin=subprocess.DEVNULL,
                                  stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    p['worker.json'].write_text(json.dumps({'pid': worker.pid, 'start': process_start(worker.pid)}))
    ready = p['frame.waiting'] if kind == 'screenshot' else p['frame.ready']
    observed = wait_for_files((ready, p['requester.exit']), 45 if kind == 'screenshot' else 200)
    report.update(snapshot(root), ready=ready.exists(), finished=p['requester.exit'].exists(),
                  pid=worker.pid, argv=argv,
                  source_sha256={name: hashlib.sha256(base64.b64decode(data)).hexdigest()
                                 for name, data in values['sources'].items()})
    if not observed:
        report['error'] = 'native requester did not reach its consent boundary'
        report['requester_terminated'] = stop_requester(root)
    return report


def consent(values):
    root = Path(values['root'])
    p = paths(root)
    report = snapshot(root)
    report.update(active=False, observer_armed=False)
    if p['requester.exit'].exists():
        report['error'] = 'native requester exited before consent; no picker action attempted'
        return report
    try:
        argv = [sys.executable, str(root / 'tuna-portal-consent.py'),
                str(p['consent.json']), 'grant', '--external-capture',
                '--kind', values['kind'], '--request-exit', str(p['requester.exit'])]
        result = subprocess.run(argv, capture_output=True, text=True, timeout=60)
        report['input'] = {'argv': argv, 'kind': values['kind'],
                           'exit': result.returncode, 'stdout': result.stdout,
                           'stderr': result.stderr,
                           'accessible_tree': json.loads(p['consent.json'].read_text())
                           if p['consent.json'].exists() else None}
        if result.returncode:
            report['error'] = 'actual matching portal consent was not activated'
            report['requester_terminated'] = stop_requester(root)
            report.update(snapshot(root))
            return report
        if values['kind'] == 'screenshot':
            report.update(collect(values))
            report['session_state'] = [session_state('after-response')]
            return report
        wait_for_files((p['frame.active'], p['requester.exit']), 100)
        report.update(snapshot(root), active=p['frame.active'].exists() and not p['requester.exit'].exists())
        state = session_state('after-response')
        report['session_state'] = [state]
        if not report['active'] or not unlocked(state):
            report['error'] = 'native grant is not active in an unlocked session'
            report['requester_terminated'] = stop_requester(root)
            return report
        # This is the final guest operation before returning to the host's
        # actual Super+L. Never transfer PNGs or open another SSH session here.
        p['frame.revoke'].touch()
        report.update(observer_armed=True, armed_at=now())
        return report
    except subprocess.TimeoutExpired as error:
        report['input'] = {'argv': argv, 'kind': values['kind'], 'exit': 124,
                           'stdout': error.stdout.decode() if isinstance(error.stdout, bytes) else error.stdout,
                           'stderr': error.stderr.decode() if isinstance(error.stderr, bytes) else error.stderr}
        report['error'] = 'actual matching portal consent timed out'
        report['requester_terminated'] = stop_requester(root)
        report.update(snapshot(root))
        return report
    except Exception:
        stop_requester(root)
        raise


def collect(values):
    root = Path(values['root'])
    p = paths(root)
    wait_for_files((p['requester.exit'],), 100)
    report = snapshot(root)
    if p['frame.png'].exists():
        data = p['frame.png'].read_bytes()
        report['frame'] = {'base64': base64.b64encode(data).decode(),
                           'sha256': hashlib.sha256(data).hexdigest()}
    if p['frame.revoked.json'].exists():
        report['revoked'] = json.loads(p['frame.revoked.json'].read_text())
    return report


def main():
    operation, values = sys.argv[1], json.loads(sys.argv[2])
    try:
        report = {'launch': launch, 'consent': consent, 'collect': collect}[operation](values)
    except Exception as error:
        report = {'error': f'{type(error).__name__}: {error}'}
        if values.get('root'):
            report.update(snapshot(Path(values['root'])))
    print(json.dumps(report), flush=True)
    return 1 if report.get('error') else 0


if __name__ == '__main__':
    sys.exit(main())
