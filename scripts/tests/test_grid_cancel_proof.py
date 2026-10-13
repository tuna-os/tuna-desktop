import copy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("grid_cancel", Path(__file__).parents[1] / "lib/tuna_grid_cancel_proof.py")
proof = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proof)


class CancelEvidence(unittest.TestCase):
    def setUp(self):
        self.before = dict(locked=False, overview_open=True, overview_app_grid=True,
                           client_dnd_active=True, rendered_frames=10)
        self.after = dict(self.before, client_dnd_active=False, rendered_frames=11)

    def test_actual_positive_shape(self):
        proof.validate(self.before, self.after, b"[]\n", b"[]\n")

    def test_old_escape_close_is_not_cancellation(self):
        self.after.update(overview_open=False, client_dnd_active=True)
        with self.assertRaises(ValueError):
            proof.validate(self.before, self.after, b"[]\n", b"[]\n")

    def test_bad_states_fail_closed(self):
        for name, value in (("locked", True), ("overview_open", False),
                            ("overview_app_grid", False), ("client_dnd_active", True),
                            ("rendered_frames", 10), ("rendered_frames", True),
                            ("rendered_frames", None)):
            with self.subTest(name=name, value=value):
                after = dict(self.after, **{name: value})
                with self.assertRaises(ValueError):
                    proof.validate(self.before, after, b"[]\n", b"[]\n")

    def test_missing_fields_fail_closed(self):
        for key in self.after:
            after = copy.copy(self.after)
            del after[key]
            with self.subTest(key=key), self.assertRaises(ValueError):
                proof.validate(self.before, after, b"[]\n", b"[]\n")

    def test_empty_invalid_or_changed_layout_fails(self):
        for a, b in ((b"", b""), (b" ", b" "), (None, None), (b"[]\n", b"[changed]\n")):
            with self.subTest(layout=a), self.assertRaises(ValueError):
                proof.validate(self.before, self.after, a, b)

    def test_missing_active_before_fails(self):
        self.before["client_dnd_active"] = False
        with self.assertRaises(ValueError):
            proof.validate(self.before, self.after, b"[]\n", b"[]\n")


if __name__ == "__main__":
    unittest.main()
