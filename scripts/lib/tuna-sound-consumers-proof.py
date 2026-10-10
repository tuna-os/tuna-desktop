#!/usr/bin/env python3
"""Scoped native audio-consumer proof with private daemons and a null sink.

Requires frozen source-owned example binaries, WirePlumber 0.4, Xvfb,
libcanberra's GTK player/Pulse backend, and the freedesktop sound theme.
Does not qualify physical audio, GTK key delivery, notification/input-click
sounds, visual presentation, Xwayland bells, or a GNOME performance win.
"""
import argparse
import array
import hashlib
import json
import math
import os
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
    while time.monotonic() < end:
        try:
            result = check()
            if result:
                return result
        except (OSError, subprocess.SubprocessError):
            pass
        time.sleep(.05)
    raise RuntimeError('private service readiness timed out')


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


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--volume-client', type=Path, required=True)
    parser.add_argument('--bell-client', type=Path, required=True)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--source-revision', required=True)
    parser.add_argument('--identity', type=Path, required=True)
    args = parser.parse_args()
    identity = json.loads(args.identity.read_text())
    assert identity['source_revision'] == args.source_revision, 'frozen source identity mismatch'
    for binary in (args.volume_client, args.bell_client):
        assert binary.resolve().parent == args.identity.resolve().parent, 'client outside frozen bundle'
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == identity['binary_sha256'][binary.name], 'frozen client byte mismatch'
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
    for key in ('WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'DISPLAY', 'DBUS_SESSION_BUS_ADDRESS',
                'PIPEWIRE_REMOTE', 'WIREPLUMBER_CONFIG_DIR', 'WIREPLUMBER_CONFIG_PATH'):
        env.pop(key, None)
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
              'source_revision': args.source_revision, 'cases': {},
              'frozen_identity_sha256': hashlib.sha256(args.identity.read_bytes()).hexdigest(),
              'player': {'path': '/usr/bin/canberra-gtk-play', 'sha256': hashlib.sha256(Path('/usr/bin/canberra-gtk-play').read_bytes()).hexdigest()},
              'binaries': {str(p.resolve()): hashlib.sha256(p.read_bytes()).hexdigest()
                           for p in (args.volume_client, args.bell_client)}}
    try:
        bus_socket = runtime / 'bus'
        spawn(['dbus-daemon', '--session', '--nofork', '--address=unix:path=' + str(bus_socket)], 'bus')
        wait_until(bus_socket.exists)
        env['DBUS_SESSION_BUS_ADDRESS'] = 'unix:path=' + str(bus_socket)
        env['DBUS_SYSTEM_BUS_ADDRESS'] = env['DBUS_SESSION_BUS_ADDRESS']
        display_read, display_write = os.pipe()
        spawn(['Xvfb', '-displayfd', str(display_write), '-screen', '0', '800x600x24', '-nolisten', 'tcp'],
              'xvfb', pass_fds=(display_write,))
        os.close(display_write)
        assert select.select([display_read], [], [], 8)[0], 'private display unavailable'
        env['DISPLAY'] = ':' + os.read(display_read, 32).decode().strip()
        os.close(display_read)
        spawn(['pipewire'], 'pipewire')
        wait_until((runtime / 'pipewire-0').exists)
        spawn(['wireplumber'], 'wireplumber')
        spawn(['pipewire-pulse'], 'pulse')
        wait_until(lambda: run(['pactl', 'info'], env))
        run(['pactl', 'load-module', 'module-null-sink', 'sink_name=tuna-sound-proof',
             'rate=48000', 'channels=2'], env)
        run(['pactl', 'set-default-sink', 'tuna-sound-proof'], env)
        wait_until(lambda: 'tuna-sound-proof' in run(['wpctl', 'inspect', '@DEFAULT_AUDIO_SINK@'], env))
        graph = json.loads(run(['pw-dump'], env))
        assert not any(o.get('info', {}).get('props', {}).get('device.api') in ('alsa', 'bluez5')
                       for o in graph), 'hardware monitor escaped isolation'
        (artifacts / 'initial-graph.json').write_text(json.dumps(graph, indent=2))
        report['services'] = {name: run([name, '--version'], env) for name in ('pipewire', 'wireplumber', 'pactl')}
        report['assets'] = {}
        for name in ('audio-volume-change.oga', 'bell.oga'):
            asset = Path('/usr/share/sounds/freedesktop/stereo') / name
            report['assets'][str(asset)] = hashlib.sha256(asset.read_bytes()).hexdigest()

        # Positive control checks native libcanberra bell-window-system fallback
        # to the shipped bell.oga before qualifying any silent case.
        for case in ('up', 'quiet', 'event-disabled', 'muted', 'clamp',
                     'bell-audible', 'bell-silent', 'bell-event-disabled'):
            run(['wpctl', 'set-mute', '@DEFAULT_AUDIO_SINK@', '0'], env)
            run(['wpctl', 'set-volume', '@DEFAULT_AUDIO_SINK@', '1.40' if case == 'clamp' else '0.40'], env)
            if case == 'muted':
                run(['wpctl', 'set-mute', '@DEFAULT_AUDIO_SINK@', '1'], env)
            wait_until(lambda: any(o.get('info', {}).get('props', {}).get('node.name') == 'tuna-sound-proof'
                                  and o['info'].get('state') in ('idle', 'suspended')
                                  for o in json.loads(run(['pw-dump'], env))))
            audio = artifacts / (case + '.wav')
            recorder = spawn(['pw-record', '--target', 'tuna-sound-proof', '--rate', '48000', '--channels', '2',
                              '--format', 's16', '-P', 'stream.capture.sink=true node.passive=true', str(audio)], case + '-record')
            time.sleep(.2)
            if case.startswith('bell-'):
                command = [str(args.bell_client.resolve()), case.removeprefix('bell-')]
            else:
                command = [str(args.volume_client.resolve()), case]
            try:
                (artifacts / (case + '-client.log')).write_text(run(command, env, timeout=10))
            finally:
                stop(recorder)
            observed = measure(audio)
            observed['backend'] = run(['wpctl', 'get-volume', '@DEFAULT_AUDIO_SINK@'], env)
            if case in ('up', 'bell-audible'):
                assert observed['frames'] > 1000 and observed['peak'] > 20, (case, observed)
            else:
                assert observed['peak'] == 0, (case, observed)
            if case == 'clamp':
                assert float(observed['backend'].split()[1]) <= 1.0, observed
            report['cases'][case] = observed
    finally:
        for child in reversed(children):
            stop(child)
        for log in logs:
            log.close()
        report['cleanup'] = all(child.poll() is not None for child in children)
    assert report['cleanup'] and len(report['cases']) == 8
    (artifacts / 'receipt.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))


if __name__ == '__main__':
    main()
