"""Actual FFmpeg frame readiness and retained-byte timeline validation; no UI proof."""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import threading
import time

def probe_frames(path, timeout, first_only=False):
    argv = ['ffprobe', '-v', 'error', '-select_streams', 'v:0', '-show_frames',
            '-show_entries', 'frame=best_effort_timestamp,pkt_pos,pkt_size:stream=time_base',
            '-show_streams', '-of', 'json']
    if first_only:
        argv += ['-read_intervals', '%+#1']
    return json.loads(subprocess.check_output(argv + [str(path)], text=True, timeout=timeout))


def recording_command(path, input_args):
    return ['ffmpeg', '-nostdin', '-hide_banner', '-loglevel', 'info', '-nostats',
            '-copyts', *input_args, '-vf', 'settb=AVTB,trim=duration=12,showinfo,setpts=PTS-STARTPTS',
            '-t', '12', '-fps_mode', 'passthrough', '-c:v', 'libx264', '-preset', 'veryfast',
            '-tune', 'zerolatency', '-bf', '0', '-pix_fmt', 'yuv420p', '-enc_time_base', '1:1000000',
            '-video_track_timescale', '1000000', '-movflags', '+frag_every_frame+empty_moov+default_base_moof',
            '-flush_packets', '1', '-stats_period', '0.05', '-progress', 'pipe:2', str(path)]


class FrameRecorder:
    """Own FFmpeg's bounded reader and acknowledge decoded, byte-bound output."""

    def __init__(self, argv, path, log_path):
        self.path = Path(path)
        self.log_path = Path(log_path)
        self.condition = threading.Condition()
        self.frames = []
        self.progress = None
        self.progress_host_monotonic_ns = None
        self.ready_ack = None
        self.error = None
        self.offset_lower_ns = None
        self.offset_upper_ns = None
        self.clock_samples = []
        self.sample_clock()
        self.process = subprocess.Popen(argv, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        self.reader = threading.Thread(target=self.read_output, name='overview-ffmpeg-reader', daemon=True)
        self.reader.start()

    def sample_clock(self):
        before = time.monotonic_ns()
        realtime = time.time_ns()
        after = time.monotonic_ns()
        lower, upper = before - realtime, after - realtime
        if self.offset_lower_ns is not None:
            lower = max(lower, self.offset_lower_ns)
            upper = min(upper, self.offset_upper_ns)
        if lower > upper:
            raise RuntimeError('capture realtime/host monotonic clock bridge changed')
        self.offset_lower_ns, self.offset_upper_ns = lower, upper
        self.clock_samples.append({'host_monotonic_before_ns': before, 'realtime_ns': realtime,
                                   'host_monotonic_after_ns': after})

    def read_output(self):
        progress = {}
        try:
            with self.log_path.open('wb') as log:
                while line := self.process.stderr.readline(32769):
                    if len(line) > 32768:
                        raise RuntimeError('FFmpeg telemetry line exceeds owned reader bound')
                    log.write(line)
                    text = line.decode('utf-8', errors='replace').strip()
                    match = re.search(r'\[Parsed_showinfo_\d+ @ [^]]+\].*?\bn:\s*(\d+)\s+pts:\s*(-?\d+)\s', text)
                    with self.condition:
                        if match:
                            index, pts = map(int, match.groups())
                            if (index != len(self.frames) or len(self.frames) >= 512
                                    or (self.frames and pts <= self.frames[-1])):
                                raise RuntimeError('capture frame lineage is missing, reordered, or unbounded')
                            self.sample_clock()
                            self.frames.append(pts)
                        elif re.fullmatch(r'(frame|out_time_us|total_size)=-?\d+', text):
                            key, value = text.split('=')
                            progress[key] = int(value)
                        elif text in ('progress=continue', 'progress=end'):
                            self.progress = dict(progress)
                            self.progress_host_monotonic_ns = time.monotonic_ns()
                            progress.clear()
                        self.condition.notify_all()
        except Exception as error:
            with self.condition:
                self.error = error
                self.condition.notify_all()
        finally:
            self.process.stderr.close()
            with self.condition:
                self.condition.notify_all()

    def wait_ready(self, deadline_ns):
        """Never replace a recorded-frame acknowledgement with process liveness."""
        with self.condition:
            while True:
                if self.error:
                    raise self.error
                remaining = (deadline_ns - time.monotonic_ns()) / 1e9
                if remaining <= 0:
                    raise RuntimeError('encoded recording was not ready before the first journey input')
                if (self.progress and self.progress.get('frame', 0) >= 2
                        and self.progress.get('out_time_us', 0) > 0
                        and self.progress.get('total_size', 0) > 0 and len(self.frames) >= 2):
                    progress = dict(self.progress)
                    progress_received = self.progress_host_monotonic_ns
                    break
                if self.process.poll() is not None:
                    raise RuntimeError('actual screen recorder exited before recorded-frame readiness')
                self.condition.wait(remaining)
        prefix = self.path.read_bytes()
        remaining = (deadline_ns - time.monotonic_ns()) / 1e9
        if remaining <= 0:
            raise RuntimeError('encoded recording acknowledgement missed the first journey input')
        decoded = probe_frames(self.path, remaining, first_only=True)
        frames = decoded.get('frames', [])
        if len(frames) != 1 or decoded.get('streams', [{}])[0].get('time_base') != '1/1000000':
            raise RuntimeError('live original MP4 has no uniquely decoded first frame')
        frame = frames[0]
        pts, pos, size = (int(frame[key]) for key in ('best_effort_timestamp', 'pkt_pos', 'pkt_size'))
        acknowledged = time.monotonic_ns()
        if pts != 0 or pos < 0 or size <= 0 or pos + size > len(prefix) or acknowledged >= deadline_ns:
            raise RuntimeError('first encoded frame bytes/readiness missed the original journey deadline')
        self.ready_ack = {
            'host_monotonic_ns': acknowledged, 'progress': progress,
            'progress_host_monotonic_ns': progress_received,
            'decoded_frame': {'video_pts_us': pts, 'packet_pos': pos, 'packet_size': size},
            'file_prefix_bytes': len(prefix), 'file_prefix_sha256': hashlib.sha256(prefix).hexdigest()}
        return self.ready_ack

    def finish(self):
        if self.process.wait(timeout=15) != 0:
            raise RuntimeError('actual screen recording failed')
        self.reader.join(timeout=5)
        if self.reader.is_alive():
            raise RuntimeError('owned FFmpeg telemetry reader did not finish')
        if self.error:
            raise self.error

    def stop(self):
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        self.reader.join(timeout=5)
        if self.reader.is_alive():
            raise RuntimeError('owned FFmpeg telemetry reader did not stop')

    def receipt(self, journey_begin_ns):
        if self.ready_ack is None or self.process.poll() != 0 or self.reader.is_alive() or self.error:
            raise RuntimeError('recording has no successful completed encoded-frame acknowledgement')
        self.sample_clock()
        data = self.path.read_bytes()
        prefix_size = self.ready_ack['file_prefix_bytes']
        if hashlib.sha256(data[:prefix_size]).hexdigest() != self.ready_ack['file_prefix_sha256']:
            raise RuntimeError('acknowledged original encoded bytes changed before completion')
        decoded = probe_frames(self.path, 10)
        packets = json.loads(subprocess.check_output(
            ['ffprobe', '-v', 'error', '-select_streams', 'v:0', '-show_packets',
             '-show_entries', 'packet=pts,pos,size', '-of', 'json', str(self.path)],
            text=True, timeout=10)).get('packets', [])
        decoded_frames = decoded.get('frames', [])
        if (not self.frames or len(self.frames) != len(decoded_frames) or len(self.frames) != len(packets)
                or decoded.get('streams', [{}])[0].get('time_base') != '1/1000000'):
            raise RuntimeError('original capture/encoded packet/decoded frame counts differ')
        frames = []
        first_capture = self.frames[0]
        for index, (capture, frame, packet) in enumerate(zip(self.frames, decoded_frames, packets)):
            pts = capture - first_capture
            pos, size = int(packet['pos']), int(packet['size'])
            if (int(frame['best_effort_timestamp']) != pts or int(packet['pts']) != pts
                    or int(frame['pkt_pos']) != pos or int(frame['pkt_size']) != size
                    or pos < 0 or size <= 0 or pos + size > len(data)):
                raise RuntimeError('original captured timestamp has no matching encoded/decoded packet bytes')
            frames.append({
                'index': index, 'capture_realtime_us': capture, 'video_pts_us': pts,
                'host_monotonic_lower_ns': capture * 1000 + self.offset_lower_ns,
                'host_monotonic_upper_ns': capture * 1000 + 999 + self.offset_upper_ns,
                'packet_pos': pos, 'packet_size': size})
        first = self.ready_ack['decoded_frame']
        if any(first[key] != frames[0][key] for key in ('video_pts_us', 'packet_pos', 'packet_size')):
            raise RuntimeError('live decoded readiness frame differs from retained original video')
        return {
            'schema': 1, 'capture_clock_domain': 'FFmpeg x11grab CLOCK_REALTIME microseconds',
            'host_clock_domain': 'host CLOCK_MONOTONIC nanoseconds',
            'video_clock_domain': 'raw MP4 presentation timestamps, time_base 1/1000000',
            'journey_begin_host_monotonic_ns': journey_begin_ns,
            'clock_bridge': {'offset_lower_ns': self.offset_lower_ns, 'offset_upper_ns': self.offset_upper_ns,
                             'samples': self.clock_samples},
            'frames': frames, 'first_frame': frames[0], 'last_frame': frames[-1],
            'raw_file': self.path.name, 'raw_sha256': hashlib.sha256(data).hexdigest(), 'raw_bytes': len(data),
            'frame_count': len(frames), 'ready_ack': self.ready_ack}


def bind_recorded_interval(actions, recording):
    """Conservatively enclose every input/read between actual retained frames."""
    frames = recording['frames']
    actions.update(recording=recording, capture_begin=frames[0]['host_monotonic_upper_ns'] / 1e9,
                   capture_end=frames[-1]['host_monotonic_lower_ns'] / 1e9)
    intervals = [(event, event['begin_host_monotonic_ns'], event['end_host_monotonic_ns'])
                 for event in actions['events']]
    intervals += [(snapshot, snapshot['observed_host_monotonic_lower_ns'],
                   snapshot['observed_host_monotonic_upper_ns']) for snapshot in actions['snapshots'].values()]
    for value, lower, upper in intervals:
        before = [frame['index'] for frame in frames if frame['host_monotonic_upper_ns'] <= lower]
        after = [frame['index'] for frame in frames if frame['host_monotonic_lower_ns'] >= upper]
        if not before or not after:
            raise ValueError('physical input/UI observation lies outside actual recorded frame boundaries')
        value['recorded_frame_bracket'] = [before[-1], after[0]]
    validate_recording(actions)


def validate_recording(actions, raw_path=None):
    """Reject absent/late/out-of-frame proof; optionally replay exact original bytes."""
    try:
        _validate_recording(actions, raw_path)
    except (KeyError, TypeError, AttributeError, IndexError, StopIteration, OverflowError,
            OSError, subprocess.SubprocessError) as error:
        raise ValueError('missing, malformed, or unreadable actual recording evidence') from error


def _validate_recording(actions, raw_path):
    def require(condition, message):
        if not condition:
            raise ValueError(message)

    def integer(value, minimum=0):
        return type(value) is int and value >= minimum

    def digest(value):
        return isinstance(value, str) and re.fullmatch('[0-9a-f]{64}', value) is not None

    recording = actions['recording']
    require(type(recording['schema']) is int and recording['schema'] == 1 and
            recording['capture_clock_domain'] == 'FFmpeg x11grab CLOCK_REALTIME microseconds' and
            recording['host_clock_domain'] == 'host CLOCK_MONOTONIC nanoseconds' and
            recording['video_clock_domain'] == 'raw MP4 presentation timestamps, time_base 1/1000000',
            'actual capture/host/video clock domains are missing')
    origin = recording['journey_begin_host_monotonic_ns']
    require(integer(origin, 1), 'missing original journey scheduling origin')
    bridge = recording['clock_bridge']
    samples = bridge['samples']
    frames = recording['frames']
    require(isinstance(frames, list) and 2 <= len(frames) <= 512 and
            integer(recording['frame_count'], 2) and recording['frame_count'] == len(frames) and
            isinstance(samples, list) and len(samples) == len(frames) + 2,
            'actual capture frame/clock sample inventory is incomplete')
    previous = origin
    offsets = []
    for sample in samples:
        before, realtime, after = (sample[key] for key in
                                  ('host_monotonic_before_ns', 'realtime_ns', 'host_monotonic_after_ns'))
        require(all(integer(value, 1) for value in (before, realtime, after)) and
                previous <= before <= after, 'host/realtime bridge samples are unordered')
        offsets.append((before - realtime, after - realtime))
        previous = after
    lower, upper = max(value[0] for value in offsets), min(value[1] for value in offsets)
    require(lower <= upper and type(bridge['offset_lower_ns']) is int and type(bridge['offset_upper_ns']) is int
            and bridge['offset_lower_ns'] == lower and bridge['offset_upper_ns'] == upper,
            'capture realtime/host monotonic bridge has no exact stable intersection')
    require(recording['raw_file'] == 'overview-demo.raw.mp4' and digest(recording['raw_sha256']) and
            integer(recording['raw_bytes'], 1), 'original recording byte identity is absent')
    first_capture, previous_pts, previous_end = frames[0]['capture_realtime_us'], -1, 0
    for index, frame in enumerate(frames):
        capture, pts, pos, size = (frame[key] for key in
                                   ('capture_realtime_us', 'video_pts_us', 'packet_pos', 'packet_size'))
        require(type(frame['index']) is int and frame['index'] == index and
                all(integer(value) for value in (capture, pts, pos, size)) and capture > 0 and
                previous_pts < pts < 12_000_000 and pts == capture - first_capture and
                size > 0 and previous_end <= pos and pos + size <= recording['raw_bytes'],
                'captured/encoded frame timestamp or packet byte lineage is invalid')
        require(frame['host_monotonic_lower_ns'] == capture * 1000 + lower and
                frame['host_monotonic_upper_ns'] == capture * 1000 + 999 + upper and
                integer(frame['host_monotonic_lower_ns'], 1) and integer(frame['host_monotonic_upper_ns'], 1) and
                origin <= frame['host_monotonic_lower_ns'] <= frame['host_monotonic_upper_ns'] <=
                samples[index + 1]['host_monotonic_after_ns'],
                'captured frame has no conservative original host-clock bracket')
        previous_pts, previous_end = pts, pos + size
    require(frames[0]['video_pts_us'] == 0 and recording['first_frame'] == frames[0] and
            recording['last_frame'] == frames[-1], 'first/last original captured frames are missing')
    first, last = frames[0]['host_monotonic_upper_ns'], frames[-1]['host_monotonic_lower_ns']
    require(type(actions['capture_begin']) in (int, float) and type(actions['capture_end']) in (int, float) and
            actions['capture_begin'] == first / 1e9 and actions['capture_end'] == last / 1e9,
            'recording interval is not the actual conservative captured-frame interval')
    ack = recording['ready_ack']
    acknowledged, progressed = ack['host_monotonic_ns'], ack['progress_host_monotonic_ns']
    progress, decoded = ack['progress'], ack['decoded_frame']
    require(integer(acknowledged, 1) and integer(progressed, 1) and
            frames[1]['host_monotonic_upper_ns'] <= progressed <= acknowledged < origin + 1_000_000_000 and
            integer(progress['frame'], 2) and progress['frame'] <= len(frames) and
            integer(progress['out_time_us'], 1) and integer(progress['total_size'], 1) and
            integer(ack['file_prefix_bytes'], 1) and
            progress['total_size'] <= ack['file_prefix_bytes'] <= recording['raw_bytes'] and
            digest(ack['file_prefix_sha256']) and
            all(type(decoded[key]) is int and decoded[key] == frames[0][key]
                for key in ('video_pts_us', 'packet_pos', 'packet_size')) and
            decoded['packet_pos'] + decoded['packet_size'] <= ack['file_prefix_bytes'],
            'missing, late, or non-decoded original encoded-frame readiness acknowledgement')
    events, snapshots = actions['events'], actions['snapshots']
    labels = ('before', 'opened', 'selected-beta', 'reopened', 'dismissed')
    require(isinstance(events, list) and
            [event['action'] for event in events] ==
            ['open-overview', 'select-beta', 'reopen-overview', 'dismiss-overview'] and
            isinstance(snapshots, dict) and set(snapshots) == set(labels),
            'required actual journey action/read inventory is missing')
    intervals = []
    previous = acknowledged
    for index, (label, deadline_ns) in enumerate(zip(labels, (1_000_000_000, 3_000_000_000,
                                                           5_000_000_000, 7_000_000_000, 10_500_000_000))):
        snapshot = snapshots[label]
        observed_lower, observed_upper = (snapshot[key] for key in
                                         ('observed_host_monotonic_lower_ns', 'observed_host_monotonic_upper_ns'))
        require(integer(observed_lower, 1) and integer(observed_upper, 1) and
                previous <= observed_lower <= observed_upper < origin + deadline_ns and
                type(snapshot['observed_host_monotonic']) in (int, float) and
                snapshot['observed_host_monotonic'] == observed_upper / 1e9,
                'UI observation is unordered or missed the unchanged journey deadline')
        intervals.append((snapshot, observed_lower, observed_upper))
        if index < len(events):
            event = events[index]
            begin, end = event['begin_host_monotonic_ns'], event['end_host_monotonic_ns']
            next_deadline = (3_000_000_000, 5_000_000_000, 7_000_000_000, 10_500_000_000)[index]
            require(integer(begin, 1) and integer(end, 1) and
                    max(observed_upper, origin + deadline_ns) <= begin <= end < origin + next_deadline and
                    type(event['begin']) in (int, float) and type(event['end']) in (int, float) and
                    event['begin'] == begin / 1e9 and event['end'] == end / 1e9,
                    'physical input is unordered or missed the unchanged 1/3/5/7-second schedule')
            intervals.append((event, begin, end))
            previous = end
    for value, begin, end in intervals:
        bracket = value['recorded_frame_bracket']
        require(first <= acknowledged <= begin <= end <= last and isinstance(bracket, list) and
                len(bracket) == 2 and all(integer(value) and value < len(frames) for value in bracket) and
                bracket[0] <= bracket[1] and
                frames[bracket[0]]['host_monotonic_upper_ns'] <= begin <= end <=
                frames[bracket[1]]['host_monotonic_lower_ns'] and
                bracket[0] == max(frame['index'] for frame in frames if frame['host_monotonic_upper_ns'] <= begin) and
                bracket[1] == next(frame['index'] for frame in frames if frame['host_monotonic_lower_ns'] >= end),
                'required physical input/UI read is outside its actual retained frame bracket')
    if raw_path is not None:
        data = Path(raw_path).read_bytes()
        require(len(data) == recording['raw_bytes'] and hashlib.sha256(data).hexdigest() == recording['raw_sha256'] and
                hashlib.sha256(data[:ack['file_prefix_bytes']]).hexdigest() == ack['file_prefix_sha256'],
                'retained original recording/acknowledged encoded prefix bytes differ')
        decoded = probe_frames(raw_path, 10)
        packets = json.loads(subprocess.check_output(
            ['ffprobe', '-v', 'error', '-select_streams', 'v:0', '-show_packets',
             '-show_entries', 'packet=pts,pos,size', '-of', 'json', str(raw_path)],
            text=True, timeout=10)).get('packets', [])
        require(decoded.get('streams', [{}])[0].get('time_base') == '1/1000000' and
                len(decoded.get('frames', [])) == len(frames) == len(packets),
                'retained original video has different decoded/encoded frame inventory')
        for expected, actual, packet in zip(frames, decoded['frames'], packets):
            require(int(actual['best_effort_timestamp']) == int(packet['pts']) == expected['video_pts_us'] and
                    int(actual['pkt_pos']) == int(packet['pos']) == expected['packet_pos'] and
                    int(actual['pkt_size']) == int(packet['size']) == expected['packet_size'],
                    'retained original decoded frame/packet differs from captured-frame lineage')
