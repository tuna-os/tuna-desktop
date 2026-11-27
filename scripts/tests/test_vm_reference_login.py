"""Controlled login-admission policy tests, not native GNOME qualification."""
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import sys
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import Mock, patch


source = Path(__file__).resolve().parents[1] / "tuna-vm-lane"
loader = importlib.machinery.SourceFileLoader("vm_reference_login_lane", str(source))
spec = importlib.util.spec_from_loader(loader.name, loader)
lane = importlib.util.module_from_spec(spec)
loader.exec_module(lane)


class ReferenceLoginAdmission(unittest.TestCase):
    def admission(self, observations):
        return SimpleNamespace(session_status=Mock(side_effect=observations))

    def test_started_but_unadmitted_session_waits_without_password_retry(self):
        observed = {"ready": False, "session": {"started": True}, "missing": ["native-owner"]}
        observer = self.admission([observed])
        agent = object()
        with patch.dict(sys.modules, {"vm_gnome_reference": observer}), \
                patch.object(lane.time, "monotonic", side_effect=[0, 10]), \
                patch.object(lane.time, "sleep") as sleep:
            self.assertEqual(lane.await_reference_login(agent, 10, profile="gnome"), (False, observed))

    def test_real_session_admission_not_greeter_readiness_controls_success(self):
        observed = {"ready": True, "gdm": {"ready": False}, "session": {"started": True, "ready": True}}
        observer = self.admission([observed])
        with patch.dict(sys.modules, {"vm_gnome_reference": observer}), \
                patch.object(lane.time, "monotonic", return_value=0):
            self.assertEqual(lane.await_reference_login(object(), 10, profile="tuna"), (True, observed))

    def test_starting_native_session_can_finish_admission_within_same_login(self):
        starting = {"ready": False, "session": {"started": True}}
        ready = {"ready": True, "session": {"started": True, "ready": True}}
        observer = self.admission([starting, ready])
        with patch.dict(sys.modules, {"vm_gnome_reference": observer}), \
                patch.object(lane.time, "monotonic", side_effect=[0, 1]), \
                patch.object(lane.time, "sleep"):
            self.assertEqual(lane.await_reference_login(object(), 10, profile="gnome"), (True, ready))

    def test_no_session_waits_but_expired_deadline_is_not_admission(self):
        observed = {"ready": False, "session": {"started": False}}
        observer = self.admission([observed])
        with patch.dict(sys.modules, {"vm_gnome_reference": observer}), \
                patch.object(lane.time, "monotonic", side_effect=[0, 10]), \
                patch.object(lane.time, "sleep") as sleep:
            self.assertEqual(lane.await_reference_login(object(), 10, profile="gnome"), (False, None))

    def test_failed_greeter_retains_last_native_admission_and_transport_error(self):
        observed = {"gdm": {"ready": False}, "missing": ["payload_integrity: mismatched original ELF"]}
        observer = self.admission([observed, RuntimeError("original observer disconnected")])
        with tempfile.TemporaryDirectory() as directory, \
                patch.dict(sys.modules, {"vm_gnome_reference": observer}), \
                patch.object(lane.guest_agent, "GuestAgent", return_value=Mock()), \
                patch.object(lane.time, "monotonic", side_effect=[0, 1, 2, 120]), \
                patch.object(lane.time, "sleep"):
            with self.assertRaisesRegex(RuntimeError, "shipped GDM greeter did not become ready"):
                lane.gdm_login(object(), "unused.sock", directory, Mock(), reference_profile="gnome")
            receipt = json.loads((Path(directory) / "greeter-before-login.json").read_text())
        self.assertEqual(receipt, {"last_observation": observed,
                                   "last_error": "original observer disconnected"})


class _Admitted(Exception):
    pass


def _ppm(painted):
    width, height = 16, 80
    pixels = bytes(value for y in range(height) for x in range(width)
                   for value in ((x * 16, y * 3, 0) if painted else (0, 0, 0)))
    return f"P6\n{width} {height}\n255\n".encode() + pixels


class GreeterReferenceFrame(unittest.TestCase):
    """The logout check compares against this frame, so it must be the painted greeter."""

    def capture(self, frames, monotonic):
        dumps = iter(frames)
        screendumps = []

        def cmd(command, **args):
            self.assertEqual(command, "screendump")
            screendumps.append(args["filename"])
            Path(args["filename"]).write_bytes(_ppm(next(dumps)))

        qmp = SimpleNamespace(cmd=cmd, frame=Mock())
        observer = SimpleNamespace(session_status=Mock(return_value={"gdm": {"ready": True}}))
        directory = self.enterContext(tempfile.TemporaryDirectory())
        with patch.dict(sys.modules, {"vm_gnome_reference": observer}), \
                patch.object(lane.guest_agent, "GuestAgent", return_value=Mock()), \
                patch.object(lane.time, "monotonic", side_effect=monotonic), \
                patch.object(lane.time, "sleep"):
            try:
                lane.gdm_login(qmp, "unused.sock", directory, Mock(side_effect=_Admitted()),
                               reference_profile="gnome")
            except _Admitted:
                pass
        return Path(directory, "greeter-before-login.ppm").read_bytes(), screendumps, qmp.frame

    def test_black_scanout_after_readiness_is_not_the_reference(self):
        reference, screendumps, frame = self.capture([False, False, True, True], lambda: 0)
        self.assertEqual(reference, _ppm(True))
        self.assertEqual(len(screendumps), 4)
        frame.assert_called_once()

    def test_greeter_that_never_paints_fails_at_the_unchanged_deadline(self):
        clock = iter(range(0, 1000, 30))
        with self.assertRaisesRegex(RuntimeError, "did not paint a settled frame"):
            self.capture([False] * 10, lambda: next(clock))


if __name__ == "__main__":
    unittest.main()
