"""V-UNLOCK-WAKE's judgement and its drive against a scripted session; no VM."""
from pathlib import Path
import json
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import vm_unlock_wake as gate


def snap(step, locked, mode, frames, elapsed=0.0):
    return {"step": step, "elapsed": elapsed, "locked": locked, "power_save_mode": mode, "frames": frames}


UNLOCK = {"step": gate.UNLOCK, "elapsed": 3.0}


def passing():
    return [snap("lock", False, 0, 10), snap("lock", True, 0, 12), snap("blank", True, 0, 13),
            snap("blank", True, 3, 14), snap("blank", True, 3, 14), UNLOCK,
            snap("wake", True, 3, 14), snap("wake", False, 0, 14), snap("frames", False, 0, 16)]


class Problems(unittest.TestCase):
    def test_a_blanked_lock_that_wakes_and_draws_on_unlock_passes(self):
        self.assertEqual(gate.problems(passing()), [])

    def test_frames_already_advanced_at_the_wake_pass(self):
        samples = passing()[:-2] + [snap("wake", False, 0, 15)]
        self.assertEqual(gate.problems(samples), [])

    def test_an_unlock_that_leaves_the_screen_blanked_fails(self):
        # Before #1194: logind's Unlock ended the lock without WakeUpScreen,
        # so gsd-power never woke the output it blanked.
        samples = passing()[:6] + [snap("wake", False, 3, 14)] * 3
        found = gate.problems(samples)
        self.assertEqual(len(found), 1)
        self.assertIn("locked=False at PowerSaveMode 3", found[0])

    def test_an_unlock_that_never_lands_fails(self):
        found = gate.problems(passing()[:6] + [snap("wake", True, 3, 14)])
        self.assertIn("locked=True at PowerSaveMode 3", found[0])

    def test_nothing_observed_after_the_unlock_fails(self):
        self.assertIn("unobserved", gate.problems(passing()[:6])[0])

    def test_a_wake_without_new_frames_fails(self):
        samples = passing()[:-1] + [snap("frames", False, 0, 14)]
        found = gate.problems(samples)
        self.assertEqual(len(found), 1)
        self.assertIn("did not advance past the blanked output's 14", found[0])

    def test_missing_frame_counts_never_count_as_progress(self):
        samples = [dict(value, frames=None) if "frames" in value else value for value in passing()]
        self.assertIn("did not advance", gate.problems(samples)[0])

    def test_a_lock_gsd_power_never_blanked_fails(self):
        samples = [snap("lock", True, 0, 12), snap("blank", True, 0, 20), UNLOCK,
                   snap("wake", False, 0, 22), snap("frames", False, 0, 25)]
        found = gate.problems(samples)
        self.assertEqual(len(found), 1)
        self.assertIn("never blanked", found[0])

    def test_an_old_guest_without_power_save_mode_fails(self):
        samples = [dict(value, power_save_mode=None) if "frames" in value else value for value in passing()]
        found = gate.problems(samples)
        self.assertIn("never blanked", found[0])
        self.assertIn("PowerSaveMode None", found[1])

    def test_a_screen_woken_before_the_unlock_fails(self):
        # Input (or anything else) lit the screen before unlock-session ran:
        # the wake would not be the unlock's.
        samples = passing()
        samples.insert(5, snap("blank", True, 0, 15))
        found = gate.problems(samples)
        self.assertEqual(len(found), 1)
        self.assertIn("no longer locked and blanked", found[0])

    def test_super_l_that_never_locks_fails(self):
        samples = [snap("lock", False, 0, 10), UNLOCK, snap("wake", False, 0, 11)]
        found = gate.problems(samples)
        self.assertIn("never locked", found[0])

    def test_the_unlock_runs_exactly_once(self):
        self.assertEqual(gate.problems(passing()[:5]), ["loginctl unlock-session never ran"])
        twice = passing()
        twice.insert(7, UNLOCK)
        self.assertIn("2 times", gate.problems(twice)[0])


class Session:
    """A guest whose gsd-power blanks two snapshots into the lock and wakes
    on input, and on unlock only when the shell emits WakeUpScreen."""

    def __init__(self, wakes_on_unlock=True, input_wakes=True):
        self.locked, self.mode, self.frames, self.dark_polls = False, 0, 100, 0
        self.wakes_on_unlock, self.input_wakes = wakes_on_unlock, input_wakes
        self.inputs, self.actions = [], []

    def keys(self, *keys):
        self.inputs.append(keys)
        if keys == ("meta_l", "l"):
            self.locked = True
        elif self.input_wakes:
            self.mode = 0

    def run(self, action):
        self.actions.append(action)
        if action == gate.UNLOCK:
            self.locked = False
            if self.wakes_on_unlock:
                self.mode = 0
            return {}
        assert action == "inventory", action
        if self.locked and self.mode == 0:
            self.dark_polls += 1
            if self.dark_polls >= 2:
                self.mode = 3
        if self.mode == 0:
            self.frames += 1
        return {"state": {"locked": self.locked, "power_save_mode": self.mode, "rendered_frames": self.frames}}

    def password_unlock(self):
        self.inputs.append(("password",))
        self.locked = False
        if self.input_wakes:
            self.mode = 0


@patch.multiple(gate, POLL=0, LOCK_TIMEOUT=0.2, BLANK_TIMEOUT=0.2, WAKE_TIMEOUT=0.2, FRAMES_TIMEOUT=0.2,
                RECOVER_TIMEOUT=0.2)
class Drive(unittest.TestCase):
    def drive(self, session):
        records = []
        with tempfile.TemporaryDirectory() as evidence:
            error = None
            try:
                gate.run(session, session, evidence, lambda *args: records.append(args), session.password_unlock)
            except RuntimeError as caught:
                error = caught
            written = json.loads(Path(evidence, "unlock-wake.json").read_text())
        return records, written, error

    def test_a_waking_unlock_passes_with_super_l_the_only_input(self):
        session = Session()
        records, written, error = self.drive(session)
        self.assertIsNone(error)
        self.assertEqual(len(records), 1)
        name, ok, detail = records[0]
        self.assertEqual((name, ok), ("V-UNLOCK-WAKE", True), detail)
        self.assertIn("PowerSaveMode was 0 again", detail)
        self.assertEqual(session.inputs, [("meta_l", "l")])
        self.assertEqual(session.actions.count(gate.UNLOCK), 1)
        self.assertEqual([value["step"] for value in written["samples"]].count(gate.UNLOCK), 1)
        self.assertIsNone(written["error"])

    def test_an_unlock_that_leaves_the_screen_dark_fails_then_input_recovers(self):
        session = Session(wakes_on_unlock=False)
        records, written, error = self.drive(session)
        self.assertIsNone(error)
        name, ok, detail = records[0]
        self.assertEqual((name, ok), ("V-UNLOCK-WAKE", False))
        self.assertIn("left the session locked=False at PowerSaveMode 3", detail)
        # Recovery input only after the verdict.
        self.assertEqual(session.inputs, [("meta_l", "l"), ("ctrl",)])
        self.assertEqual(written["samples"][-1]["step"], "wake")

    def test_a_lock_never_blanked_fails_without_unlock_and_recovers_by_password(self):
        session = Session()
        session.run = lambda action, run=session.run: (
            {"state": {"locked": session.locked, "power_save_mode": 0, "rendered_frames": 1}}
            if action == "inventory" else run(action))
        records, _, error = self.drive(session)
        self.assertIsNone(error)
        self.assertFalse(records[0][1])
        self.assertIn("never blanked", records[0][2])
        self.assertNotIn(gate.UNLOCK, session.actions)
        self.assertEqual(session.inputs, [("meta_l", "l"), ("password",)])

    def test_an_agent_failure_is_recorded_and_recovery_still_runs(self):
        session = Session()
        real = session.run

        def broken(action):
            if action == gate.UNLOCK:
                raise RuntimeError("guest agent lost")
            return real(action)
        session.run = broken
        records, written, error = self.drive(session)
        self.assertIsNone(error)
        self.assertFalse(records[0][1])
        self.assertIn("RuntimeError: guest agent lost", records[0][2])
        self.assertEqual(written["error"], "RuntimeError: guest agent lost")

    def test_a_session_that_stays_dark_after_recovery_raises(self):
        session = Session(wakes_on_unlock=False, input_wakes=False)
        records, _, error = self.drive(session)
        self.assertFalse(records[0][1])
        self.assertIsInstance(error, RuntimeError)
        self.assertIn("did not come back unlocked and lit", str(error))


if __name__ == "__main__":
    unittest.main()
