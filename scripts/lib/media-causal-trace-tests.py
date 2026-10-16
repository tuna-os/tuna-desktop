#!/usr/bin/env python3
"""Fixture/source contracts only; no compositor or installed qualification."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
spec = importlib.util.spec_from_file_location('media_trace', HERE / 'media-causal-trace.py')
trace = importlib.util.module_from_spec(spec)
spec.loader.exec_module(trace)


class TraceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'volume').write_text('0.4200\n')

    def enabled(self):
        (self.root / 'media-diagnostics-enabled').touch()

    def test_disabled_does_not_create_log(self):
        self.assertIsNone(trace.record(self.root, 'marker', os.getpid(), ['begin']))
        self.assertFalse((self.root / 'media-causal-trace.jsonl').exists())

    def test_monotonic_before_after_and_actor_start(self):
        self.enabled()
        a = trace.record(self.root, 'wpctl-enter', os.getpid(), ['set-volume', '@DEFAULT_AUDIO_SINK@', '0.4800'])
        (self.root / 'volume').write_text('0.4800\n')
        b = trace.record(self.root, 'wpctl-exit', os.getpid(), ['set-volume', '@DEFAULT_AUDIO_SINK@', '0.4800'])
        self.assertEqual(a['fixture']['volume'], '0.4200')
        self.assertEqual(b['fixture']['volume'], '0.4800')
        self.assertLess(a['monotonic_ns'], b['monotonic_ns'])
        self.assertEqual(a['actor']['pid'], os.getpid())
        self.assertGreater(a['actor']['start_ticks'], 0)
        self.assertEqual(len((self.root / 'media-causal-trace.jsonl').read_text().splitlines()), 2)

    def test_unbounded_or_unrelated_arguments_rejected(self):
        self.enabled()
        for stage, args in [('marker', ['arbitrary']), ('wpctl-enter', ['set-volume', 'secret']),
                            ('wpctl-enter', ['set-volume', '@DEFAULT_AUDIO_SINK@', 'nan']),
                            ('wpctl-enter', ['set-volume', '@DEFAULT_AUDIO_SINK@', 'inf']),
                            ('wpctl-enter', ['status', '1', '2', '3']), ('unknown', ['status'])]:
            with self.subTest(args=args), self.assertRaises(ValueError):
                trace.record(self.root, stage, os.getpid(), args)
        self.assertFalse((self.root / 'media-causal-trace.jsonl').exists())

    def test_removing_gate_stops_observations(self):
        self.enabled()
        trace.record(self.root, 'marker', os.getpid(), ['begin'])
        (self.root / 'media-diagnostics-enabled').unlink()
        self.assertIsNone(trace.record(self.root, 'marker', os.getpid(), ['disabled-check']))
        self.assertEqual(len((self.root / 'media-causal-trace.jsonl').read_text().splitlines()), 1)

    def test_actual_generated_wpctl_quoting_and_entry_exit(self):
        self.enabled()
        (self.root / 'bin').mkdir()
        script = (ROOT / 'scripts/tuna-gtk-shell-proof').read_text()
        block = script.split('cat >"$ARTIFACTS/bin/wpctl" <<WPCTL\n', 1)[1].split('\nWPCTL', 1)[0]
        # Replace only the FIFO wake after the recorded mutation; no monitor needed.
        block = block.replace('"$PY" "$ROOT/scripts/lib/tuna-audio-fixture.py" "$ARTIFACTS" --event', ':')
        env = dict(os.environ, PY=sys.executable, ROOT=str(ROOT), ARTIFACTS=str(self.root),
                   VOLUME_FILE=str(self.root / 'volume'), MIC_FILE=str(self.root / 'mic-volume'),
                   SINK_FILE=str(self.root / 'default-sink'), VOLUME_MUTED_FILE=str(self.root / 'volume-muted'),
                   MIC_MUTED_FILE=str(self.root / 'mic-muted'), RECORDING_FILE=str(self.root / 'recording'))
        subprocess.run(['bash', '-c', 'cat >"$ARTIFACTS/bin/wpctl" <<WPCTL\n' + block + '\nWPCTL\n'],
                       env=env, check=True, timeout=5)
        subprocess.run(['sh', str(self.root / 'bin/wpctl'), 'set-volume', '@DEFAULT_AUDIO_SINK@', '0.4800'],
                       check=True, timeout=5)
        rows = [json.loads(x) for x in (self.root / 'media-causal-trace.jsonl').read_text().splitlines()]
        self.assertEqual([x['stage'] for x in rows], ['wpctl-enter', 'wpctl-exit'])
        self.assertEqual(rows[0]['actor']['pid'], rows[1]['actor']['pid'])
        self.assertNotEqual(rows[0]['actor']['pid'], os.getpid())
        self.assertEqual(rows[0]['fixture']['volume'], '0.4200')
        self.assertEqual(rows[1]['fixture']['volume'], '0.4800')
        self.assertEqual(rows[0]['args'], ['set-volume', '@DEFAULT_AUDIO_SINK@', '0.4800'])

    def test_timing_application_and_lock_contract(self):
        proof = (ROOT / 'scripts/tuna-gtk-shell-proof').read_text()
        section = proof.split('media_mark static-disable-before', 1)[1].split('gsettings set "$MEDIA_SCHEMA" volume-up "', 1)[0]
        self.assertEqual(section.count('sleep 0.7'), 2)
        self.assertEqual(section.count('xdotool key XF86AudioRaiseVolume'), 1)
        self.assertIn('[ "$(cat "$VOLUME_FILE")" = 0.4200 ] || fail G-MEDIA-VOLUME', section)
        self.assertNotIn('while ', section)
        runtime = (ROOT / 'crates/compositor/src/runtime.rs').read_text()
        self.assertIn('self.manager.set_accelerators(list);\n            self.manager.trace_media_accelerators(self.is_locked());', runtime)
        self.assertIn('rm -f "$TUNA_MEDIA_DIAGNOSTICS"\npass G-MEDIA-VOLUME', proof)
        self.assertIn('if locked || !trace_enabled()', (ROOT / 'crates/shell-gtk/src/media_keys.rs').read_text())


if __name__ == '__main__':
    unittest.main()
