"""Private digital notification audio capture; no physical output claim."""
import array
import hashlib
import importlib.util
import json
import math
from pathlib import Path
import subprocess
import time
import wave


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


HERE = Path(__file__).resolve().parent
helpers = load('notification_audio_helpers', HERE/'tuna-sound-consumers-proof.py')
dump = load('notification_pipewire_dump', HERE/'tuna-pipewire-dump.py')


class AudioProof:
    def __init__(self, artifacts, runtime, env, spawn, manifest):
        self.artifacts = artifacts
        self.env = env
        self.spawn = spawn
        self.manifest = manifest
        self.active = None
        self.processes = []
        manifest['audio_cases'] = []
        manifest['audio_helper_sha256'] = {
            p.name: hashlib.sha256(p.read_bytes()).hexdigest()
            for p in (Path(__file__), HERE/'tuna-sound-consumers-proof.py',
                      HERE/'tuna-pipewire-dump.py')}
        manifest['audio_output'] = 'private PipeWire null sink, recorded digital samples'
        manifest['physical_audio_proof'] = False
        manifest['real_player_sha256'] = hashlib.sha256(
            Path('/usr/bin/canberra-gtk-play').read_bytes()).hexdigest()
        version = helpers.run(['wireplumber', '--version'], env)
        manifest['wireplumber_version'] = version
        assert '0.4.' in version, 'this isolated audio configuration requires WirePlumber 0.4'
        # Disable physical device discovery before starting any session manager.
        for name, body in (
            ('main.lua.d', 'alsa_monitor.enabled = false\nv4l2_monitor.enabled = false\nlibcamera_monitor.enabled = false\n'),
            ('bluetooth.lua.d', 'bluez_monitor.enabled = false\nbluez_midi_monitor.enabled = false\n')):
            folder = artifacts/'config/wireplumber'/name
            folder.mkdir(parents=True)
            (folder/'51-tuna-notification-proof.lua').write_text(body)
        env['PULSE_SERVER'] = 'unix:'+str(runtime/'pulse/native')
        env['PIPEWIRE_REMOTE'] = 'pipewire-0'
        self.processes.append(spawn(['pipewire'], 'pipewire'))
        helpers.wait_until(lambda: (runtime/'pipewire-0').exists())
        self.processes.append(spawn(['wireplumber'], 'wireplumber'))
        self.processes.append(spawn(['pipewire-pulse'], 'pulse'))
        helpers.wait_until(lambda: helpers.run(['pactl', 'info'], env))
        helpers.run(['pactl', 'load-module', 'module-null-sink',
                     'sink_name=tuna-notification-audio', 'rate=48000', 'channels=2'], env)
        # PipeWire-Pulse acknowledges module loading before the new sink is
        # always visible to subsequent pactl commands. Wait for the exact
        # named sink instead of racing set-default-sink against registration.
        def sink_ready():
            sinks = helpers.run(['pactl', 'list', 'short', 'sinks'], env)
            return any(line.split()[1:2] == ['tuna-notification-audio']
                       for line in sinks.splitlines())

        try:
            helpers.wait_until(sink_ready, seconds=8)
        except RuntimeError as error:
            sinks = helpers.run(['pactl', 'list', 'short', 'sinks'], env)
            raise RuntimeError(f'{error}; pactl sinks: {sinks!r}') from error
        graph = self.graph()
        sink_ids = [str(o['id']) for o in graph
                    if o.get('info', {}).get('props', {}).get('node.name')
                    == 'tuna-notification-audio'
                    and o.get('info', {}).get('props', {}).get('media.class')
                    == 'Audio/Sink']
        if len(sink_ids) != 1:
            raise RuntimeError(
                'expected one PipeWire Audio/Sink named tuna-notification-audio; '
                f'found IDs {sink_ids!r}')
        try:
            helpers.run(['wpctl', 'set-default', sink_ids[0]], env)
            selected = helpers.run(['wpctl', 'inspect', '@DEFAULT_AUDIO_SINK@'], env)
            if 'node.name = "tuna-notification-audio"' not in selected:
                raise RuntimeError(
                    f'WirePlumber default sink did not resolve to the private sink: {selected!r}')
        except (subprocess.CalledProcessError, RuntimeError) as error:
            sinks = helpers.run(['pactl', 'list', 'short', 'sinks'], env)
            raise RuntimeError(
                f'WirePlumber could not select the private default sink: {error}; '
                f'pactl sinks: {sinks!r}') from error
        helpers.run(['pactl', 'set-sink-volume', 'tuna-notification-audio', '100%'], env)
        graph = self.graph()
        assert not any(o.get('info', {}).get('props', {}).get('device.api')
                       in ('alsa', 'bluez5', 'v4l2', 'libcamera') for o in graph)
        (artifacts/'private-audio-graph.json').write_text(json.dumps(graph, indent=2))
        silence = artifacts/'silence.wav'
        with wave.open(str(silence), 'wb') as stream:
            stream.setnchannels(2)
            stream.setsampwidth(2)
            stream.setframerate(48000)
            for _ in range(180):
                stream.writeframesraw(bytes(192000))
        self.silence = spawn(['pw-play', '--target', 'tuna-notification-audio',
                             '-P', 'node.name=tuna-notification-silence', str(silence)],
                            'silent-graph-driver')
        time.sleep(.5)
        self.file = artifacts/'local-event.wav'
        self.tone(self.file, 880, 8000, .25)

    def graph(self):
        return dump.dump_objects(subprocess.check_output(['pw-dump'], env=self.env, timeout=8))

    @staticmethod
    def tone(path, frequency, amplitude, seconds):
        samples = array.array('h')
        for i in range(int(48000*seconds)):
            value = round(amplitude*math.sin(2*math.pi*frequency*i/48000))
            samples.extend((value, value))
        with wave.open(str(path), 'wb') as stream:
            stream.setnchannels(2)
            stream.setsampwidth(2)
            stream.setframerate(48000)
            stream.writeframes(samples.tobytes())

    def begin(self, case):
        assert self.active is None
        assert self.silence.poll() is None, 'capture graph keepalive exited'
        path = self.artifacts/(case+'.wav')
        recorder = self.spawn(['pw-record', '--target', 'tuna-notification-audio',
                               '--rate', '48000', '--channels', '2', '--format', 's16',
                               '--latency', '1024', '-P',
                               'stream.capture.sink=true node.passive=true node.name=tuna-notification-recorder',
                               str(path)], case+'-record')
        def linked():
            graph = self.graph()
            ids = {o['id'] for o in graph if o.get('info', {}).get('props', {}).get('node.name')
                   == 'tuna-notification-recorder'}
            return any(o.get('info', {}).get('input-node-id') in ids for o in graph)
        helpers.wait_until(linked)
        time.sleep(.2)
        self.active = case, path, recorder

    def finish(self, sound, media=False):
        case, path, recorder = self.active
        time.sleep(1.5)
        helpers.stop(recorder)
        observed = helpers.measure(path)
        self.active = None
        assert observed['frames'] > 4800, ('empty capture', case, observed)
        if media:
            assert self.media.poll() is None, 'notification stopped media fixture'
            assert observed['rms'] > 40, ('media carrier missing', case, observed)
            assert (observed['sine_residual_rms'] > 20 if sound
                    else observed['sine_residual_rms'] < 5), (case, sound, observed)
        else:
            assert (observed['peak'] > 20 if sound else observed['peak'] == 0), (case, sound, observed)
        self.manifest['audio_cases'].append(dict(case=case, expected_sound=sound,
                                                media_playing=media, **observed))

    def start_media(self):
        path = self.artifacts/'playing-media.wav'
        self.tone(path, 440, 160, 90)
        self.media = self.spawn(['pw-play', '--target', 'tuna-notification-audio', '-P',
                                 'node.name=tuna-notification-media media.role=Music', str(path)],
                                'playing-media')
        helpers.wait_until(lambda: any(o.get('info', {}).get('props', {}).get('node.name')
                                      == 'tuna-notification-media' for o in self.graph()))
        time.sleep(.5)

    def exited(self):
        return all(process.poll() is not None for process in self.processes)
