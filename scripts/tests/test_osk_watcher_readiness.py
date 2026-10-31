#!/usr/bin/env python3
"""No-GPU regression tests for the OSK proof's asynchronous lock watcher."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest


HELPER = Path(__file__).resolve().parents[1] / "lib/tuna-osk-proof.py"
SPEC = importlib.util.spec_from_file_location("tuna_osk_proof", HELPER)
PROOF = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROOF)
MAPPED = [0, 500, 1280, 300]
LOCKED = 286419.453
HIDDEN = 286419.655
UNLOCKED = 286427.362


def change(t, locked, osk):
    return {"t": t, "locked": locked, "osk": osk,
            "text_input": None, "focused_app_id": None}


class OskWatcherReadiness(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.watch = Path(self.tmp.name) / "watch.jsonl"
        self.state = Path(self.tmp.name) / "state.json"
        self.watch.touch()

    def append(self, *records):
        with self.watch.open("a", encoding="utf-8") as stream:
            for record in records:
                stream.write(json.dumps(record) + "\n")

    def call(self, command, *args):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            status = PROOF.main([str(HELPER), command, str(self.watch), *map(str, args)])
        return status, out.getvalue()

    def test_hidden_compositor_waits_for_delayed_watcher_sample(self):
        self.state.write_text(json.dumps({"locked": True, "layers": []}), encoding="utf-8")
        self.append(change(LOCKED - 2, False, None),
                    change(LOCKED - 1, False, MAPPED),
                    change(LOCKED, True, MAPPED))
        # The live compositor has hidden the layer, but its watcher has not.
        self.assertEqual(PROOF.sample(self.state)["osk"], None)
        self.assertIs(PROOF.sample(self.state)["locked"], True)
        self.assertEqual(self.call("lock-times", LOCKED - 0.1), (1, ""))

        self.append(change(HIDDEN, True, None))
        status, times = self.call("lock-times", LOCKED - 0.1)
        self.assertEqual(status, 0)
        lock_t, hidden_t = map(float, times.split())
        self.assertEqual((lock_t, hidden_t), (LOCKED, HIDDEN))

        # Unlocking must also reach the watcher before passing a timestamp
        # to never; neither pending transition may produce literal "null".
        self.assertEqual(self.call("unlock-time", lock_t), (1, ""))
        self.append(change(UNLOCKED, False, None))
        status, stamp = self.call("unlock-time", lock_t)
        self.assertEqual(status, 0)
        self.assertEqual(float(stamp), UNLOCKED)
        self.assertEqual(self.call("never", hidden_t, float(stamp)), (0, ""))

    def test_prior_lock_and_unlocked_hidden_samples_cannot_qualify(self):
        self.append(change(LOCKED - 3, True, MAPPED),
                    change(LOCKED - 2, True, None),
                    change(LOCKED - 1, False, None),
                    change(LOCKED, False, None))
        self.assertEqual(self.call("lock-times", LOCKED - 0.1), (1, ""))
        self.append(change(LOCKED + 0.1, True, MAPPED))
        self.assertEqual(self.call("lock-times", LOCKED - 0.1), (1, ""))
        self.append(change(HIDDEN, True, None))
        self.assertEqual(self.call("lock-times", LOCKED - 0.1),
                         (0, f"{LOCKED + 0.1!r} {HIDDEN!r}\n"))

    def test_unlock_before_hidden_cannot_be_replaced_by_another_lock(self):
        self.append(change(LOCKED, True, MAPPED),
                    change(HIDDEN, False, None),
                    change(UNLOCKED, True, None))
        self.assertEqual(self.call("lock-times", LOCKED - 0.1), (1, ""))

    def test_one_sample_can_record_both_lock_and_hide(self):
        self.append(change(HIDDEN, True, None))
        self.assertEqual(self.call("lock-times", LOCKED),
                         (0, f"{HIDDEN!r} {HIDDEN!r}\n"))

    def test_real_remap_is_not_hidden_by_a_later_hidden_sample(self):
        remap = HIDDEN + 0.1
        self.append(change(LOCKED, True, MAPPED),
                    change(HIDDEN, True, None),
                    change(remap, True, MAPPED),
                    change(HIDDEN + 0.2, True, None),
                    change(UNLOCKED, False, None))
        status, times = self.call("lock-times", LOCKED - 0.1)
        self.assertEqual(status, 0)
        lock_t, hidden_t = map(float, times.split())
        self.assertEqual(hidden_t, HIDDEN)
        status, stamp = self.call("unlock-time", lock_t)
        self.assertEqual(status, 0)
        status, failure = self.call("never", hidden_t, float(stamp))
        self.assertEqual(status, 1)
        self.assertIn(f"mapped at {remap:.6f}", failure)

    def test_submicrosecond_remaps_at_interval_boundaries_are_rejected(self):
        intervals = (
            ("after hiding", HIDDEN + 0.0000006, HIDDEN + 0.0000007, UNLOCKED),
            ("before unlocking", HIDDEN, UNLOCKED + 0.0000003, UNLOCKED + 0.0000004),
        )
        locked = LOCKED + 0.0000004
        for boundary, hidden, remap, unlocked in intervals:
            with self.subTest(boundary=boundary):
                self.watch.write_text("", encoding="utf-8")
                self.append(change(locked, True, MAPPED),
                            change(hidden, True, None),
                            change(remap, True, MAPPED),
                            change(unlocked, False, None))
                status, times = self.call("lock-times", LOCKED - 0.1)
                self.assertEqual(status, 0)
                lock_t, hidden_t = map(float, times.split())
                self.assertEqual((lock_t, hidden_t), (locked, hidden))
                status, stamp = self.call("unlock-time", lock_t)
                self.assertEqual(status, 0)
                self.assertEqual(float(stamp), unlocked)
                status, failure = self.call("never", hidden_t, float(stamp))
                self.assertEqual(status, 1)
                self.assertIn(f"mapped at {remap:.6f}", failure)

    def test_prior_unlock_does_not_qualify_current_unlock(self):
        self.append(change(LOCKED - 1, False, None),
                    change(LOCKED, True, None))
        self.assertEqual(self.call("unlock-time", LOCKED), (1, ""))

    def test_null_lock_flag_cannot_qualify(self):
        self.append(change(HIDDEN, None, None))
        self.assertEqual(self.call("lock-times", LOCKED), (1, ""))
        self.assertEqual(self.call("unlock-time", LOCKED), (1, ""))

    def test_null_timestamp_is_not_emitted_or_suppressed(self):
        self.append(change(None, True, None))
        for command in ("lock-times", "unlock-time"):
            with self.subTest(command=command):
                with self.assertRaises(TypeError):
                    self.call(command, LOCKED)

    def test_malformed_watcher_record_is_not_suppressed(self):
        self.watch.write_text("not JSON\n", encoding="utf-8")
        with self.assertRaises(json.JSONDecodeError):
            self.call("lock-times", LOCKED)


if __name__ == "__main__":
    unittest.main()
