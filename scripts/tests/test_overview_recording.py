"""Real FFmpeg byte/timestamp tests, not UI fixtures or graphical qualification."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest


SOURCE = Path(__file__).resolve().parents[1] / 'lib' / 'overview_recording.py'
SPEC = importlib.util.spec_from_file_location('overview_recording', SOURCE)
recording = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(recording)


def timing_read():
    # Only clock markers: deliberately no state/argv/source or UI-proof assertion.
    lower = time.monotonic_ns()
    upper = time.monotonic_ns()
    return {'observed_host_monotonic_lower_ns': lower, 'observed_host_monotonic_upper_ns': upper,
            'observed_host_monotonic': upper / 1e9}


class RealRecordingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.scratch = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.scratch.cleanup)
        cls.root = Path(cls.scratch.name)
        cls.raw = cls.root / 'overview-demo.raw.mp4'
        cls.origin = time.monotonic_ns()
        argv = recording.recording_command(cls.raw, [
            '-use_wallclock_as_timestamps', '1', '-f', 'lavfi', '-i',
            'testsrc2=size=32x32:rate=15:duration=12,realtime,settb=AVTB'])
        recorder = recording.FrameRecorder(argv, cls.raw, cls.root / 'ffmpeg.log')
        cls.addClassCleanup(recorder.stop)
        original_pid = recorder.process.pid
        recorder.wait_ready(cls.origin + 1_000_000_000)
        cls.actions = {'events': [], 'snapshots': {'before': timing_read()}}
        for offset, action, label in zip((1, 3, 5, 7),
                                        ('open-overview', 'select-beta', 'reopen-overview', 'dismiss-overview'),
                                        ('opened', 'selected-beta', 'reopened', 'dismissed')):
            time.sleep(max(0, (cls.origin + offset * 1_000_000_000 - time.monotonic_ns()) / 1e9))
            lower = time.monotonic_ns()
            upper = time.monotonic_ns()
            cls.actions['events'].append({'action': action, 'begin_host_monotonic_ns': lower,
                                          'end_host_monotonic_ns': upper, 'begin': lower / 1e9, 'end': upper / 1e9})
            cls.actions['snapshots'][label] = timing_read()
        recorder.finish()
        if recorder.process.pid != original_pid:
            raise AssertionError('recording process identity changed')
        recording.bind_recorded_interval(cls.actions, recorder.receipt(cls.origin))

    def test_real_first_decoded_frame_ack_is_bound_to_retained_prefix(self):
        receipt = self.actions['recording']
        ack = receipt['ready_ack']
        raw = self.raw.read_bytes()
        self.assertEqual(receipt['raw_bytes'], len(raw))
        self.assertEqual(receipt['raw_sha256'], hashlib.sha256(raw).hexdigest())
        self.assertEqual(ack['file_prefix_sha256'], hashlib.sha256(raw[:ack['file_prefix_bytes']]).hexdigest())
        self.assertGreaterEqual(ack['progress']['frame'], 2)
        self.assertLess(ack['host_monotonic_ns'], self.origin + 1_000_000_000)
        self.assertEqual(ack['decoded_frame'], {key: receipt['first_frame'][key]
                                              for key in ('video_pts_us', 'packet_pos', 'packet_size')})
        recording.validate_recording(self.actions, self.raw)

    def test_real_parser_retains_every_captured_and_decoded_frame_boundary(self):
        receipt = self.actions['recording']
        frames = receipt['frames']
        self.assertGreater(len(frames), 170)
        self.assertEqual(receipt['first_frame'], frames[0])
        self.assertEqual(receipt['last_frame'], frames[-1])
        self.assertEqual(self.actions['capture_begin'], frames[0]['host_monotonic_upper_ns'] / 1e9)
        self.assertEqual(self.actions['capture_end'], frames[-1]['host_monotonic_lower_ns'] / 1e9)
        self.assertGreater(frames[-1]['video_pts_us'], 11_800_000)
        media = json.loads(subprocess.check_output(
            ['ffprobe', '-v', 'error', '-show_format', '-show_streams', '-of', 'json', str(self.raw)], text=True))
        self.assertEqual(media['streams'][0]['codec_name'], 'h264')
        self.assertTrue(11.8 <= float(media['format']['duration']) <= 12.2)
        for value in [*self.actions['events'], *self.actions['snapshots'].values()]:
            before, after = value['recorded_frame_bracket']
            lower = value.get('begin_host_monotonic_ns', value.get('observed_host_monotonic_lower_ns'))
            upper = value.get('end_host_monotonic_ns', value.get('observed_host_monotonic_upper_ns'))
            self.assertLessEqual(frames[before]['host_monotonic_upper_ns'], lower)
            self.assertLessEqual(upper, frames[after]['host_monotonic_lower_ns'])

    def test_actual_frame_interval_rejects_pre_capture_and_post_capture_reads(self):
        receipt = self.actions['recording']
        for outside in (receipt['first_frame']['host_monotonic_upper_ns'] - 1,
                        receipt['last_frame']['host_monotonic_lower_ns'] + 1):
            with self.subTest(outside=outside):
                actions = copy.deepcopy(self.actions)
                read = actions['snapshots']['dismissed']
                read.update(observed_host_monotonic_lower_ns=outside, observed_host_monotonic_upper_ns=outside,
                            observed_host_monotonic=outside / 1e9)
                with self.assertRaisesRegex(ValueError, 'actual recorded frame boundaries'):
                    recording.bind_recorded_interval(actions, actions['recording'])

    def test_missing_ack_frame_proof_and_original_deadline_fail_closed(self):
        for key in ('recording',):
            actions = copy.deepcopy(self.actions)
            del actions[key]
            with self.assertRaises(ValueError):
                recording.validate_recording(actions)
        for key in ('ready_ack', 'first_frame', 'last_frame', 'clock_bridge'):
            with self.subTest(key=key):
                actions = copy.deepcopy(self.actions)
                del actions['recording'][key]
                with self.assertRaises(ValueError):
                    recording.validate_recording(actions)
        actions = copy.deepcopy(self.actions)
        actions['recording']['ready_ack']['host_monotonic_ns'] = self.origin + 1_000_000_000
        with self.assertRaises(ValueError):
            recording.validate_recording(actions)
        actions = copy.deepcopy(self.actions)
        actions['recording']['clock_bridge']['offset_lower_ns'] += 1
        with self.assertRaises(ValueError):
            recording.validate_recording(actions)

    def test_nominal_recording_span_cannot_replace_encoded_packet_lineage(self):
        actions = copy.deepcopy(self.actions)
        # A plausible in-bounds packet span is not the actual decoded packet.
        frame = actions['recording']['frames'][1]
        frame['packet_pos'] += 1
        frame['packet_size'] -= 1
        recording.validate_recording(actions)
        with self.assertRaisesRegex(ValueError, 'decoded frame/packet differs'):
            recording.validate_recording(actions, self.raw)

    def test_original_ack_prefix_is_independently_checked(self):
        actions = copy.deepcopy(self.actions)
        actions['recording']['ready_ack']['file_prefix_sha256'] = '0' * 64
        recording.validate_recording(actions)
        with self.assertRaisesRegex(ValueError, 'encoded prefix bytes differ'):
            recording.validate_recording(actions, self.raw)

    def test_required_read_brackets_and_deadlines_cannot_be_missing_or_extended(self):
        actions = copy.deepcopy(self.actions)
        del actions['snapshots']['opened']['recorded_frame_bracket']
        with self.assertRaises(ValueError):
            recording.validate_recording(actions)
        actions = copy.deepcopy(self.actions)
        read = actions['snapshots']['dismissed']
        late = self.origin + 10_500_000_000
        read.update(observed_host_monotonic_lower_ns=late, observed_host_monotonic_upper_ns=late,
                    observed_host_monotonic=late / 1e9)
        with self.assertRaisesRegex(ValueError, 'unchanged journey deadline'):
            recording.bind_recorded_interval(actions, actions['recording'])


class DelayedRealProcessTests(unittest.TestCase):
    def test_alive_ffmpeg_without_captured_encoded_frames_never_acknowledges(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            fifo = root / 'not-yet-producing.rgb'
            os.mkfifo(fifo)
            raw = root / 'overview-demo.raw.mp4'
            origin = time.monotonic_ns()
            recorder = recording.FrameRecorder(recording.recording_command(raw, [
                '-use_wallclock_as_timestamps', '1', '-f', 'rawvideo', '-pixel_format', 'rgb24',
                '-video_size', '32x32', '-framerate', '15', '-i', str(fifo)]), raw, root / 'ffmpeg.log')
            try:
                self.assertIsNone(recorder.process.poll())
                with self.assertRaisesRegex(RuntimeError, 'before the first journey input'):
                    recorder.wait_ready(origin + 1_000_000_000)
                self.assertIsNone(recorder.process.poll())
                self.assertIsNone(recorder.ready_ack)
                self.assertEqual(recorder.frames, [])
            finally:
                recorder.stop()
            self.assertFalse(recorder.reader.is_alive())
            self.assertIsNotNone(recorder.process.poll())


if __name__ == '__main__':
    unittest.main()
