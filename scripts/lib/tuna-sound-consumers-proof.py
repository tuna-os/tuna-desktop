#!/usr/bin/env python3
"""Scoped native audio-consumer proof with private daemons and a null sink.

Requires frozen source-owned example binaries, WirePlumber 0.4, Xvfb,
libcanberra's GTK player/Pulse backend, and the freedesktop sound theme.
Does not qualify physical audio, GTK key delivery, notification/input-click
sounds, visual presentation, Xwayland bells, or a GNOME performance win.
"""
import argparse
import array
import ast
import hashlib
import json
import math
import os
import re
import shutil
from pathlib import Path
import select
import signal
import subprocess
import time
import wave


def run(argv, env, timeout=8):
    return subprocess.check_output(argv, env=env, text=True, stderr=subprocess.STDOUT,
                                   timeout=timeout).strip()


def wait_until(check, seconds=8):
    end = time.monotonic() + seconds
    last_error = None
    while time.monotonic() < end:
        try:
            result = check()
            if result:
                return result
        except (OSError, subprocess.SubprocessError) as error:
            last_error = getattr(error, 'output', None) or str(error)
        time.sleep(.05)
    raise RuntimeError('private service readiness timed out: ' + str(last_error))


def stop(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=2)
    # Also retire descendants if their process group outlived its leader.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def retire_private_processes(runtime):
    """Retire bus-activated helpers outside our direct process groups by pidfd."""
    handles = []
    marker = ('XDG_RUNTIME_DIR=' + str(runtime)).encode()
    for proc in Path('/proc').iterdir():
        if not proc.name.isdecimal() or int(proc.name) == os.getpid():
            continue
        try:
            if marker not in (proc / 'environ').read_bytes().split(b'\0'):
                continue
            fd = os.pidfd_open(int(proc.name))
            # Recheck after acquiring the handle to reject a recycled PID.
            if marker not in (proc / 'environ').read_bytes().split(b'\0'):
                os.close(fd)
                continue
            handles.append(fd)
            signal.pidfd_send_signal(fd, signal.SIGTERM)
        except (OSError, ProcessLookupError):
            continue
    try:
        for fd in handles:
            if not select.select([fd], [], [], 2)[0]:
                signal.pidfd_send_signal(fd, signal.SIGKILL)
            assert select.select([fd], [], [], 2)[0], 'private descendant survived cleanup'
    finally:
        for fd in handles:
            os.close(fd)
    return len(handles)


def measure(path):
    with wave.open(str(path), 'rb') as stream:
        assert stream.getsampwidth() == 2 and stream.getframerate() == 48000
        samples = array.array('h', stream.readframes(stream.getnframes()))
        channels = stream.getnchannels()
    mono = samples[::channels]
    peak = max((abs(x) for x in mono), default=0)
    rms = math.sqrt(sum(x * x for x in mono) / max(1, len(mono)))
    # The playing fixture is a low-amplitude 440 Hz sine. Residual energy
    # outside its fitted sine/cosine pair detects an additional event sound.
    residual = 0.0
    for start in range(0, len(mono) - 2400, 2400):
        block = mono[start:start + 2400]
        sine = [math.sin(2 * math.pi * 440 * i / 48000) for i in range(2400)]
        cosine = [math.cos(2 * math.pi * 440 * i / 48000) for i in range(2400)]
        a = sum(x * y for x, y in zip(block, sine)) / 1200
        b = sum(x * y for x, y in zip(block, cosine)) / 1200
        energy = sum((x - a * s - b * c) ** 2 for x, s, c in zip(block, sine, cosine)) / 2400
        residual = max(residual, math.sqrt(energy))
    return dict(frames=len(mono), peak=peak, rms=rms, sine_residual_rms=residual)


def validate_identity(identity_path, revision, volume_client, bell_client, delivered=False):
    identity = json.loads(identity_path.read_text())
    assert identity['source_revision'] == revision, 'frozen source identity mismatch'
    binaries = [volume_client, bell_client]
    if delivered:
        binaries += [identity_path.parent / name for name in ('tuna-compositor', 'tuna-shell-gtk', 'tuna-shell-host')]
    for binary in binaries:
        assert binary.resolve().parent == identity_path.resolve().parent, 'client outside frozen bundle'
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == identity['binary_sha256'][binary.name], 'frozen client byte mismatch'
    return identity, binaries


def pipewire_graph(text):
    """Fold consecutive complete pw-dump arrays; reject malformed input."""
    decoder = json.JSONDecoder()
    objects = {}
    offset = 0
    while offset < len(text):
        while offset < len(text) and text[offset].isspace():
            offset += 1
        if offset == len(text):
            break
        batch, offset = decoder.raw_decode(text, offset)
        assert isinstance(batch, list), 'PipeWire event must be an array'
        for item in batch:
            assert isinstance(item, dict) and type(item.get('id')) is int
            if 'info' in item and item['info'] is None:
                objects.pop(item['id'], None)
            else:
                objects[item['id']] = item
    return list(objects.values())


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--volume-client', type=Path, required=True)
    parser.add_argument('--bell-client', type=Path, required=True)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--source-revision', required=True)
    parser.add_argument('--identity', type=Path, required=True)
    parser.add_argument('--delivered', action='store_true', help='exercise frozen GTK/compositor through genuine nested keys and Wayland bell requests')
    parser.add_argument('--trace-processes', action='store_true', help='diagnose owned GTK audio subprocesses; not a performance run')
    parser.add_argument('--diagnose-player', action='store_true', help='log real canberra invocations and stderr through an exec wrapper; not a performance run')
    args = parser.parse_args()
    identity, binaries = validate_identity(args.identity, args.source_revision, args.volume_client, args.bell_client, args.delivered)
    artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=False)
    runtime = artifacts / 'runtime'
    runtime.mkdir(mode=0o700)
    config = artifacts / 'config'
    for folder, body in [
        ('main.lua.d', 'alsa_monitor.enabled = false\nv4l2_monitor.enabled = false\nlibcamera_monitor.enabled = false\n'),
        ('bluetooth.lua.d', 'bluez_monitor.enabled = false\nbluez_midi_monitor.enabled = false\n'),
    ]:
        folder = config / 'wireplumber' / folder
        folder.mkdir(parents=True)
        (folder / '51-tuna-private-proof.lua').write_text(body)
    env = os.environ.copy()
    env.update(XDG_RUNTIME_DIR=str(runtime), PIPEWIRE_RUNTIME_DIR=str(runtime),
               XDG_CONFIG_HOME=str(config), GSETTINGS_BACKEND='memory',
               PULSE_SERVER='unix:' + str(runtime / 'pulse/native'), CANBERRA_DRIVER='pulse')
    # Never use an inherited host display/session endpoint.
    for key in ('WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'AT_SPI_BUS_ADDRESS',
                'PIPEWIRE_REMOTE', 'WIREPLUMBER_CONFIG_DIR', 'WIREPLUMBER_CONFIG_PATH'):
        env.pop(key, None)
    player_diagnostic = None
    if args.diagnose_player:
        player_path = shutil.which('canberra-gtk-play', path=env.get('PATH'))
        assert player_path, 'canberra player unavailable'
        real_player = Path(player_path).resolve()
        diagnostic_bin = artifacts / 'diagnostic-bin'
        diagnostic_bin.mkdir(mode=0o700)
        wrapper = diagnostic_bin / 'canberra-gtk-play'
        player_log = artifacts / 'canberra-player.log'
        wrapper.write_text('#!/usr/bin/python3\nimport os,sys,time\n'
                           + 'fd=os.open(' + repr(str(player_log)) + ',os.O_WRONLY|os.O_CREAT|os.O_APPEND,0o600)\n'
                           + 'os.write(fd,(repr((time.monotonic(),os.getpid(),sys.argv))+"\\n").encode())\n'
                           + 'os.dup2(fd,2);os.close(fd)\n'
                           + 'os.execv(' + repr(str(real_player)) + ',[' + repr(str(real_player)) + ']+sys.argv[1:])\n')
        wrapper.chmod(0o700)
        env['PATH'] = str(diagnostic_bin) + os.pathsep + env.get('PATH', '')
        player_diagnostic = {'real_player': str(real_player), 'real_player_sha256': hashlib.sha256(real_player.read_bytes()).hexdigest(),
                             'exec_wrapper_sha256': hashlib.sha256(wrapper.read_bytes()).hexdigest(), 'log': str(player_log)}
    assert '0.4.' in run(['wireplumber', '--version'], env), 'fixture supports WirePlumber0.4 only'
    children = []
    logs = []

    def spawn(argv, label, **kwargs):
        log = (artifacts / (label + '.log')).open('wb')
        logs.append(log)
        child = subprocess.Popen(argv, env=env, stdout=log, stderr=log,
                                 start_new_session=True, **kwargs)
        children.append(child)
        return child

    report = {'scope': 'native source audio consumers on private null sink',
              'source_revision': args.source_revision, 'cases': {}, 'passed': False,
              'proof_script_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              'frozen_identity_sha256': hashlib.sha256(args.identity.read_bytes()).hexdigest(),
              'player': {'path': '/usr/bin/canberra-gtk-play', 'sha256': hashlib.sha256(Path('/usr/bin/canberra-gtk-play').read_bytes()).hexdigest()},
              'binaries': {str(p.resolve()): hashlib.sha256(p.read_bytes()).hexdigest()
                           for p in binaries}}
    report['player_diagnostic'] = player_diagnostic
    try:
        bus_socket = runtime / 'bus'
        spawn(['dbus-daemon', '--session', '--nofork', '--address=unix:path=' + str(bus_socket)], 'bus')
        wait_until(bus_socket.exists)
        env['DBUS_SESSION_BUS_ADDRESS'] = 'unix:path=' + str(bus_socket)
        env['DBUS_SYSTEM_BUS_ADDRESS'] = env['DBUS_SESSION_BUS_ADDRESS']
        spawn(['/usr/libexec/at-spi-bus-launcher', '--launch-immediately'], 'accessibility-launcher')
        wait_until(lambda: run(['gdbus', 'call', '--session', '--dest', 'org.freedesktop.DBus',
                              '--object-path', '/org/freedesktop/DBus', '--method',
                              'org.freedesktop.DBus.GetNameOwner', 'org.a11y.Bus'], env))
        address, = ast.literal_eval(run(['gdbus', 'call', '--session', '--dest', 'org.a11y.Bus',
                                        '--object-path', '/org/a11y/bus', '--method',
                                        'org.a11y.Bus.GetAddress'], env))
        assert str(runtime) in address, 'accessibility bus escaped private runtime'
        env['AT_SPI_BUS_ADDRESS'] = address
        report['session_isolation'] = {
            'runtime_directory': str(runtime), 'runtime_mode': oct(runtime.stat().st_mode & 0o777),
            'session_bus': env['DBUS_SESSION_BUS_ADDRESS'], 'accessibility_bus': address,
        }
        display_read, display_write = os.pipe()
        spawn(['Xvfb', '-displayfd', str(display_write), '-screen', '0', '1280x800x24', '-nolisten', 'tcp', '-noreset'],
              'xvfb', pass_fds=(display_write,))
        os.close(display_write)
        display_number = b''
        display_deadline = time.monotonic() + 8
        while not display_number.endswith(b'\n'):
            assert select.select([display_read], [], [], max(0, display_deadline - time.monotonic()))[0], 'private display unavailable'
            chunk = os.read(display_read, 32)
            assert chunk, 'private display closed before readiness'
            display_number += chunk
            assert len(display_number) <= 32
        assert display_number.strip().isdigit()
        env['DISPLAY'] = ':' + display_number.decode().strip()
        os.close(display_read)
        spawn(['pipewire'], 'pipewire')
        wait_until((runtime / 'pipewire-0').exists)
        spawn(['wireplumber'], 'wireplumber')
        spawn(['pipewire-pulse'], 'pulse')
        wait_until(lambda: run(['pactl', 'info'], env))
        run(['pactl', 'load-module', 'module-null-sink', 'sink_name=tuna-sound-proof',
             'rate=48000', 'channels=2'], env)
        wait_until(lambda: 'tuna-sound-proof' in run(['pactl', 'list', 'short', 'sinks'], env))
        # The node may appear before Pulse publishes a selectable sink.
        # Retry only this private graph readiness step and verify the selection.
        wait_until(lambda: (run(['pactl', 'set-default-sink', 'tuna-sound-proof'], env),
                            run(['pactl', 'get-default-sink'], env) == 'tuna-sound-proof')[1])
        wait_until(lambda: 'tuna-sound-proof' in run(['wpctl', 'inspect', '@DEFAULT_AUDIO_SINK@'], env))
        graph = pipewire_graph(run(['pw-dump'], env))
        assert not any(o.get('info', {}).get('props', {}).get('device.api') in ('alsa', 'bluez5')
                       for o in graph), 'hardware monitor escaped isolation'
        (artifacts / 'initial-graph.json').write_text(json.dumps(graph, indent=2))
        report['services'] = {name: run([name, '--version'], env) for name in ('pipewire', 'wireplumber', 'pactl')}
        report['assets'] = {}
        for name in ('audio-volume-change.oga', 'bell.oga'):
            asset = Path('/usr/share/sounds/freedesktop/stereo') / name
            report['assets'][str(asset)] = hashlib.sha256(asset.read_bytes()).hexdigest()

        if args.delivered:
            env['GSETTINGS_BACKEND'] = 'dconf'
            env['XDG_DATA_HOME'] = str(artifacts / 'data')
            env['TUNA_COMPOSITOR_STATE'] = str(artifacts / 'compositor-state.json')
            env['LIBGL_ALWAYS_SOFTWARE'] = '1'
            run(['gsettings', 'set', 'org.gnome.desktop.sound', 'event-sounds', 'true'], env)
            run(['gsettings', 'set', 'org.gnome.desktop.wm.preferences', 'audible-bell', 'true'], env)
            run(['gsettings', 'set', 'org.gnome.desktop.wm.preferences', 'visual-bell', 'false'], env)
            xml = Path('/usr/share/wayland-protocols/staging/xdg-system-bell/xdg-system-bell-v1.xml')
            source = Path(__file__).with_name('tuna-xdg-bell-proof.c')
            run(['wayland-scanner', 'client-header', str(xml), str(artifacts / 'xdg-bell-client.h')], env)
            run(['wayland-scanner', 'private-code', str(xml), str(artifacts / 'xdg-bell-protocol.c')], env)
            bell_request = artifacts / 'xdg-bell-client'
            run(['cc', '-Wall', '-Wextra', '-Werror', '-I' + str(artifacts), str(source),
                 str(artifacts / 'xdg-bell-protocol.c'), '-lwayland-client', '-o', str(bell_request)], env)
            report['protocol_client'] = {'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
                                          'xml_sha256': hashlib.sha256(xml.read_bytes()).hexdigest(),
                                          'binary_sha256': hashlib.sha256(bell_request.read_bytes()).hexdigest()}
            core = spawn([str(args.identity.parent / 'tuna-compositor'), '--backend', 'winit',
                          '--socket', 'tuna-sound-delivered', '--shell-bin', str(args.identity.parent / 'tuna-shell-gtk')], 'desktop')
            window = wait_until(lambda: run(['xdotool', 'search', '--pid', str(core.pid), '--name', 'Smithay'], env))
            assert len(window.splitlines()) == 1
            run(['xdotool', 'windowfocus', '--sync', window], env)
            run(['xdotool', 'mousemove', '--window', window, '400', '300'], env)
            wait_until(lambda: (runtime / 'tuna-sound-delivered').exists())
            def shell_service_ready():
                assert core.poll() is None, 'compositor exited before shell readiness'
                return run(['gdbus', 'call', '--session', '--dest', 'org.gnome.Shell', '--object-path', '/org/gnome/Shell', '--method', 'org.freedesktop.DBus.Peer.Ping'], env)
            # Match the existing native GTK rendering fixture's readiness
            # bound. This precondition is separate from event observation.
            wait_until(shell_service_ready, seconds=25)
            owner = run(['gdbus', 'call', '--session', '--dest', 'org.freedesktop.DBus',
                         '--object-path', '/org/freedesktop/DBus', '--method',
                         'org.freedesktop.DBus.GetConnectionUnixProcessID', 'org.gnome.Shell'], env)
            shell_pid = int(re.fullmatch(r'\(uint32 (\d+),\)', owner)[1])
            def desktop_identity():
                assert core.poll() is None, 'original compositor exited'
                observed = {}
                for name, pid in [('tuna-compositor', core.pid), ('tuna-shell-gtk', shell_pid)]:
                    exe = Path('/proc', str(pid), 'exe')
                    assert exe.resolve() == (args.identity.parent / name).resolve()
                    digest = hashlib.sha256(exe.read_bytes()).hexdigest()
                    assert digest == identity['binary_sha256'][name], 'running desktop byte mismatch'
                    observed[name] = {'pid': pid, 'sha256': digest, 'exe': str(exe.resolve())}
                return observed
            report['running_identity_start'] = desktop_identity()
            if args.trace_processes:
                spawn(['strace', '-f', '-e', 'trace=process', '-p', str(shell_pid),
                       '-o', str(artifacts / 'gtk-process-trace.log')], 'gtk-process-tracer')
            time.sleep(2)
            env['WAYLAND_DISPLAY'] = 'tuna-sound-delivered'
            report['scope'] = 'delivered nested media keys and real Wayland bell requests on private null sink'
            report['desktop'] = {'pid': core.pid, 'host_window': window, 'exe': str(Path('/proc', str(core.pid), 'exe').resolve())}

        # A low-level sine keeps the real sink Running without hiding an event sound.
        tone = artifacts / 'playing-tone.wav'
        with wave.open(str(tone), 'wb') as stream:
            stream.setnchannels(2)
            stream.setsampwidth(2)
            stream.setframerate(48000)
            samples = array.array('h')
            for i in range(48000 * 10):
                value = round(160 * math.sin(2 * math.pi * 440 * i / 48000))
                samples.extend((value, value))
            stream.writeframes(samples.tobytes())
        report['playing_fixture'] = {'sha256': hashlib.sha256(tone.read_bytes()).hexdigest(),
                                     'frequency_hz': 440, 'sample_peak': 160}
        # Positive cases qualify the same backend used by suppression cases.
        for case in ('up', 'quiet', 'event-disabled', 'muted', 'clamp', 'playing',
                     'bell-audible', 'bell-silent', 'bell-event-disabled'):
            if args.delivered:
                run(['gsettings', 'set', 'org.gnome.desktop.sound', 'event-sounds', 'false' if case in ('event-disabled', 'bell-event-disabled') else 'true'], env)
                run(['gsettings', 'set', 'org.gnome.desktop.wm.preferences', 'audible-bell', 'false' if case == 'bell-silent' else 'true'], env)
                run(['gsettings', 'set', 'org.gnome.desktop.sound', 'allow-volume-above-100-percent', 'true' if case == 'clamp' else 'false'], env)
                time.sleep(.5)
            run(['wpctl', 'set-mute', '@DEFAULT_AUDIO_SINK@', '0'], env)
            run(['wpctl', 'set-volume', '@DEFAULT_AUDIO_SINK@', '1.40' if case == 'clamp' else '0.40'], env)
            if case == 'muted':
                run(['wpctl', 'set-mute', '@DEFAULT_AUDIO_SINK@', '1'], env)
            time.sleep(.5)
            wait_until(lambda: any(o.get('info', {}).get('props', {}).get('node.name') == 'tuna-sound-proof'
                                  and o['info'].get('state') in ('idle', 'suspended')
                                  for o in pipewire_graph(run(['pw-dump'], env))))
            player = None
            if case == 'playing':
                player = spawn(['pw-play', '--target', 'tuna-sound-proof', str(tone)], 'playing-tone')
                wait_until(lambda: any(o.get('info', {}).get('props', {}).get('node.name') == 'tuna-sound-proof'
                                      and o['info'].get('state') == 'running'
                                      for o in pipewire_graph(run(['pw-dump'], env))))
            audio = artifacts / (case + '.wav')
            recorder = spawn(['pw-record', '--target', 'tuna-sound-proof', '--rate', '48000', '--channels', '2',
                              '--format', 's16', '--latency', '256', '-P', 'stream.capture.sink=true node.passive=true node.name=tuna-sound-proof-recorder', str(audio)], case + '-record')
            def recorder_linked():
                graph = pipewire_graph(run(['pw-dump'], env))
                nodes = {o['id'] for o in graph if o.get('info', {}).get('props', {}).get('node.name') == 'tuna-sound-proof-recorder'}
                return any(o.get('info', {}).get('input-node-id') in nodes for o in graph)
            wait_until(recorder_linked)
            time.sleep(.2)
            graph_before = pipewire_graph(run(['pw-dump'], env))
            sink_before = [o for o in graph_before if o.get('info', {}).get('props', {}).get('node.name') == 'tuna-sound-proof']
            report['active_case'] = {'name': case, 'sink_before_command': sink_before,
                                     'command_started_monotonic': time.monotonic()}
            if args.delivered:
                if case.startswith('bell-'):
                    command = [str(bell_request)]
                elif case == 'clamp':
                    command = ['gsettings', 'set', 'org.gnome.desktop.sound', 'allow-volume-above-100-percent', 'false']
                else:
                    key = 'alt+XF86AudioRaiseVolume' if case == 'quiet' else ('XF86AudioLowerVolume' if case == 'muted' else 'XF86AudioRaiseVolume')
                    command = ['xdotool', 'key', key]
                    assert run(['xdotool', 'getwindowfocus'], env) == window
            elif case.startswith('bell-'):
                command = [str(args.bell_client.resolve()), case.removeprefix('bell-')]
            else:
                command = [str(args.volume_client.resolve()), case]
            try:
                (artifacts / (case + '-client.log')).write_text(run(command, env, timeout=10))
                if args.delivered:
                    # Production players have a five-second deadline. Keep
                    # the passive recorder linked through that whole lifetime,
                    # including cold GTK/player initialization under load.
                    # This is an audibility check, not an input latency claim.
                    time.sleep(6)
            finally:
                stop(recorder)
                if player is not None:
                    stop(player)
            observed = measure(audio)
            observed['backend'] = run(['wpctl', 'get-volume', '@DEFAULT_AUDIO_SINK@'], env)
            observed['sink_before_command'] = sink_before
            observed['recorder_latency_samples'] = 256
            observed['observation_seconds'] = time.monotonic() - report['active_case']['command_started_monotonic']
            if case in ('up', 'bell-audible'):
                assert observed['frames'] > 1000 and observed['peak'] > 20, (case, observed)
            elif case == 'playing':
                # Volume0.46 scales the160-unit fixture to <20 units on this
                # real backend. Event feedback positive control peaks>400.
                assert observed['frames'] > 1000 and 0 < observed['peak'] < 40, (case, observed)
                assert observed['sine_residual_rms'] < 5, (case, observed)
            else:
                assert observed['peak'] == 0, (case, observed)
            if args.delivered and not case.startswith('bell-') and case != 'clamp':
                assert abs(float(observed['backend'].split()[1]) - (.34 if case == 'muted' else .46)) < .01, (case, observed)
            if case == 'clamp':
                assert float(observed['backend'].split()[1]) <= 1.0, observed
            report['cases'][case] = observed
            if args.delivered:
                observed['command'] = command
                observed['desktop_identity'] = desktop_identity()
        report['passed'] = True
        report.pop('active_case', None)
    except BaseException as error:
        report['failure'] = {'type': type(error).__name__, 'message': str(error)}
        if isinstance(error, subprocess.CalledProcessError):
            report['failure']['command_output'] = error.output
        report['failure']['private_audio_diagnostics'] = {}
        for label, command in [('pulse-info', ['pactl', 'info']),
                               ('pulse-sinks', ['pactl', 'list', 'short', 'sinks']),
                               ('pipewire-graph', ['pw-dump'])]:
            try:
                report['failure']['private_audio_diagnostics'][label] = run(command, env)
            except (OSError, subprocess.SubprocessError) as diagnostic_error:
                report['failure']['private_audio_diagnostics'][label] = str(diagnostic_error)
        raise
    finally:
        for child in reversed(children):
            stop(child)
        for log in logs:
            log.close()
        report['retired_bus_helpers'] = retire_private_processes(runtime)
        report['cleanup'] = all(child.poll() is not None for child in children)
        report['passed'] = report['passed'] and report['cleanup'] and len(report['cases']) == 9
        (artifacts / 'receipt.json').write_text(json.dumps(report, indent=2) + '\n')
    assert report['passed']
    print(json.dumps(report))


if __name__ == '__main__':
    main()
