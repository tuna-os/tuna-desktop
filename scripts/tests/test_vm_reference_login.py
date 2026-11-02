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


if __name__ == "__main__":
    unittest.main()
