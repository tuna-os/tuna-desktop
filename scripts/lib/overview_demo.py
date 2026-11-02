"""Record the README's real select-Beta/reopen/dismiss nested UI journey."""
import hashlib
import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from maintained_media import required_source_inputs
from overview_recording import FrameRecorder, bind_recorded_interval, recording_command


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def save(path, value):
    Path(path).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def run(root, out, state_path, window, compositor_pid, binaries):
    root, out, state_path, binaries = map(Path, (root, out, state_path, binaries))
    def command(*argv):
        return subprocess.check_output(list(map(str, argv)), text=True, timeout=10).strip()
    commit = command('git', '-C', root, 'rev-parse', 'HEAD')
    tree = command('git', '-C', root, 'rev-parse', 'HEAD^{tree}')
    subprocess.run(['git', '-C', str(root), 'diff', '--quiet', 'HEAD', '--'], check=True, timeout=10)
    run_id, attempt = os.environ.get('GITHUB_RUN_ID', ''), os.environ.get('GITHUB_RUN_ATTEMPT', '')
    if not all(re.fullmatch('[0-9a-f]{40}', value) for value in (commit, tree)) or not all(
            re.fullmatch('[1-9][0-9]*', value) for value in (run_id, attempt)):
        raise ValueError('exact clean CI source and run/attempt required for maintained overview capture')
    if os.environ.get('GITHUB_SHA') != commit:
        raise ValueError('actual built checkout is not the workflow source')
    proc = Path('/proc') / str(compositor_pid)
    start = (proc / 'stat').read_text().rsplit(')', 1)[1].split()[19]
    principal = {'pid': compositor_pid, 'uid': proc.stat().st_uid, 'start_ticks': int(start),
                 'exe_sha256': sha(proc / 'exe'), 'clock_domain': 'Linux /proc starttime ticks since boot'}
    if principal['exe_sha256'] != sha(binaries / 'tuna-compositor'):
        raise ValueError('actual nested compositor is not the just-built current source executable')
    if command('xdotool', 'getwindowpid', window) != str(compositor_pid):
        raise ValueError('recorded host window does not belong to the original compositor')
    shells = []
    expected_shell = sha(binaries / 'tuna-shell-gtk')
    for candidate in Path('/proc').iterdir():
        if not candidate.name.isdigit():
            continue
        try:
            fields = (candidate / 'stat').read_text().rsplit(')', 1)[1].split()
            if (int(fields[1]) == compositor_pid and candidate.stat().st_uid == principal['uid']
                    and sha(candidate / 'exe') == expected_shell):
                shells.append({'pid': int(candidate.name), 'start_ticks': int(fields[19]),
                               'sha256': expected_shell})
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
    if len(shells) != 1:
        raise ValueError('original current-source GTK shell process is absent or ambiguous')
    source = {'merged_commit': commit, 'built_commit': commit, 'built_tree': tree,
              'run_id': run_id, 'run_attempt': attempt, 'environment': 'nested-gtk',
              'tracked_source_clean': True,
              'required_source_inputs_sha256': required_source_inputs(root),
              'launched_elfs': {'tuna-compositor': {'pid': compositor_pid, 'start_ticks': int(start),
                                                  'sha256': principal['exe_sha256']},
                               'tuna-shell-gtk': shells[0]},
              'binaries': {name: sha(binaries / name) for name in ('tuna-compositor', 'tuna-shell-gtk')},
              'compositor': principal}
    save(out / 'media-source.json', source)
    actions = {'schema': 1, 'scope': 'actual nested three-fixture-window README journey, not installed-native proof',
               'clock_domain': 'host CLOCK_MONOTONIC', 'source': source, 'window': int(window),
               'events': [], 'snapshots': {}, 'complete': False}
    recorder = None
    journey_begin_ns = None
    def original():
        if ((proc / 'stat').read_text().rsplit(')', 1)[1].split()[19] != start
                or sha(proc / 'exe') != principal['exe_sha256']):
            raise RuntimeError('original nested compositor changed during recording')
    def observe(label, predicate, deadline_offset):
        end = min(journey_begin_ns + int(min(10.5, deadline_offset) * 1e9),
                  time.monotonic_ns() + 3_000_000_000)
        while time.monotonic_ns() < end:
            original()
            lower = time.monotonic_ns()
            value = json.loads(state_path.read_bytes())
            upper = time.monotonic_ns()
            if predicate(value) and upper < end:
                actions['snapshots'][label] = {
                    'observed_host_monotonic': upper / 1e9, 'state': value,
                    'observed_host_monotonic_lower_ns': lower, 'observed_host_monotonic_upper_ns': upper}
                return value
            time.sleep(.05)
        raise RuntimeError('real overview state did not arrive inside the recording: ' + label)
    def input_action(label, *argv):
        original()
        if recorder.ready_ack is None or recorder.process.poll() is not None:
            raise RuntimeError('physical input requires the original live recorded-frame acknowledgement')
        lower = time.monotonic_ns()
        event = {'action': label, 'begin': lower / 1e9, 'begin_host_monotonic_ns': lower, 'argv': list(argv)}
        actions['events'].append(event)
        command('xdotool', *argv)
        upper = time.monotonic_ns()
        event.update(end=upper / 1e9, end_host_monotonic_ns=upper)
    def pause_until(offset):
        remaining = journey_begin_ns + int(offset * 1e9) - time.monotonic_ns()
        if remaining <= 0:
            raise RuntimeError('recording readiness/UI observation missed the original journey schedule')
        time.sleep(remaining / 1e9)
    try:
        for name in ('overview-demo.raw.mp4', 'overview-demo.mp4', 'overview-demo.gif'):
            if (out / name).exists():
                raise ValueError('refusing to overwrite a previous overview recording')
        journey_begin_ns = time.monotonic_ns()
        raw_path = out / 'overview-demo.raw.mp4'
        recorder = FrameRecorder(recording_command(raw_path,
            ['-f', 'x11grab', '-framerate', '15', '-video_size', '1280x800', '-i', os.environ['DISPLAY']]),
            raw_path, out / 'overview-demo-ffmpeg.log')
        recorder.wait_ready(journey_begin_ns + 1_000_000_000)
        expected = {'tuna-test-alpha', 'tuna-test-beta', 'tuna-test-gamma'}
        observe('before', lambda s: not s['overview_open'] and not s['locked']
                and len(s['windows']) == 3 and {w['app_id'] for w in s['windows']} == expected, 1)
        pause_until(1)
        input_action('open-overview', 'key', 'Super_L')
        opened = observe('opened', lambda s: s['overview_progress'] == 1 and len(s['previews']) == 3
                         and {w['app_id'] for w in s['previews']} == expected, 3)
        pause_until(3)
        beta = [w for w in opened['previews'] if w['app_id'] == 'tuna-test-beta']
        if len(beta) != 1:
            raise RuntimeError('actual Beta preview is not unique')
        x, y, width, height = beta[0]['rect']
        if not all(isinstance(n, (int, float)) and math.isfinite(n) for n in (x, y, width, height)):
            raise RuntimeError('actual Beta preview rectangle is invalid')
        input_action('select-beta', 'mousemove', '--window', window,
                     str(math.floor(x + width / 2)), str(math.floor(y + height / 2)), 'click', '1')
        observe('selected-beta', lambda s: not s['overview_open'] and s['focused_app_id'] == 'tuna-test-beta', 5)
        pause_until(5)
        input_action('reopen-overview', 'key', 'Super_L')
        observe('reopened', lambda s: s['overview_progress'] == 1 and len(s['previews']) == 3, 7)
        pause_until(7)
        input_action('dismiss-overview', 'key', 'Escape')
        observe('dismissed', lambda s: not s['overview_open'] and s['focused_app_id'] == 'tuna-test-beta', 10.5)
        recorder.finish()
        bind_recorded_interval(actions, recorder.receipt(journey_begin_ns))
        command('ffmpeg', '-hide_banner', '-loglevel', 'error', '-i', out / 'overview-demo.raw.mp4',
                '-c', 'copy', '-movflags', '+faststart', out / 'overview-demo.mp4')
        command('ffmpeg', '-hide_banner', '-loglevel', 'error', '-i', out / 'overview-demo.mp4',
                '-filter_complex', 'fps=8,scale=640:400:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse',
                '-loop', '0', out / 'overview-demo.gif')
        media = json.loads(command('ffprobe', '-v', 'error', '-show_streams', '-show_format',
                                   '-of', 'json', out / 'overview-demo.mp4'))
        stream = media['streams'][0]
        if (stream.get('codec_name') != 'h264' or (stream.get('width'), stream.get('height')) != (1280, 800)
                or not 11.8 <= float(media['format']['duration']) <= 12.2):
            raise RuntimeError('actual recorded overview video has wrong duration/codec/size')
        actions.update(complete=True, video={'file': 'overview-demo.mp4', 'sha256': sha(out / 'overview-demo.mp4'),
                        'raw_sha256': sha(out / 'overview-demo.raw.mp4'), 'inspection': media},
                       derivative={'file': 'overview-demo.gif', 'sha256': sha(out / 'overview-demo.gif'),
                        'input_sha256': sha(out / 'overview-demo.mp4'), 'fps': 8, 'size': [640, 400]})
        with (out / 'assertions.txt').open('a') as assertions:
            for name in ('OPEN', 'SELECT-BETA', 'REOPEN', 'DISMISS', 'VIDEO', 'GIF'):
                assertions.write('C-README-' + name + ' pass actual recorded three-window journey\n')
    finally:
        try:
            if recorder is not None:
                recorder.stop()
        finally:
            save(out / 'overview-demo-actions.json', actions)


if __name__ == '__main__':
    if len(sys.argv) != 8:
        raise SystemExit('usage: overview_demo.py ROOT OUT STATE WINDOW COMPOSITOR_PID BINARIES PRIVATE_DISPLAY_PID')
    if not sys.argv[7].isdigit() or int(sys.argv[7]) <= 0:
        raise ValueError('owned private Xvfb process required')
    run(*sys.argv[1:5], int(sys.argv[5]), sys.argv[6])
