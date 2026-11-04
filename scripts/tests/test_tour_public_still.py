"""Tour public-still error evidence keeps the causal message, not just the type."""
import ast
import importlib.machinery
import importlib.util
import io
import json
from contextlib import redirect_stdout
from pathlib import Path
from types import SimpleNamespace
import tempfile
import traceback
import unittest


source = Path(__file__).resolve().parents[1] / "tuna-vm-lane"
loader = importlib.machinery.SourceFileLoader("tour_public_still_lane", str(source))
spec = importlib.util.spec_from_loader(loader.name, loader)
lane = importlib.util.module_from_spec(spec)
loader.exec_module(lane)


class PublicStillErrorEvidence(unittest.TestCase):
    def recorder(self):
        out = tempfile.mkdtemp(prefix="tour-public-still-")
        rec = lane.Recorder(SimpleNamespace(cmd=lambda *a, **k: None), out, timeout=5.0)
        rec.frames.append({"sha256": "0" * 64, "requested_monotonic_ns": 1,
                           "completed_monotonic_ns": 2})
        rec.still = lambda card, delay=0.0: rec.stills.__setitem__(card, 0)
        return rec

    def test_observer_error_carries_type_and_message(self):
        rec = self.recorder()
        confirmed = rec.public_still("probe-card", observe=_raise)
        self.assertFalse(confirmed)
        self.assertIn("probe-card", rec.unconfirmed)
        evidence = rec.still_confirmations["probe-card"]["evidence"]
        for side in ("before", "after"):
            self.assertEqual(evidence[side]["observation_error"], "RuntimeError")
            self.assertEqual(evidence[side]["observation_error_detail"], "probe-detail-xyz")

    def test_successful_snapshot_has_no_error_keys(self):
        rec = self.recorder()
        rec.public_still("probe-card", observe=lambda remaining: None)
        evidence = rec.still_confirmations["probe-card"]["evidence"]
        for side in ("before", "after"):
            self.assertIsNone(evidence[side]["observation"])
            self.assertNotIn("observation_error", evidence[side])
            self.assertNotIn("observation_error_detail", evidence[side])


def _raise(remaining):
    raise RuntimeError("probe-detail-xyz")


class TourObserverFailureRecord(unittest.TestCase):
    def guest(self):
        path = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(path.read_text())
        wanted = [node for node in tree.body
                  if isinstance(node, ast.FunctionDef)
                  and node.name in {"tour_public_ui", "_tour_public_ui", "inventory_error",
                                    "_tour_shell_candidates"}
                  or isinstance(node, ast.Assign)
                  and any(isinstance(target, ast.Name) and target.id == "INVENTORY_STAGES"
                          for target in node.targets)]
        import os
        import pathlib
        scope = {"json": json, "traceback": traceback, "os": os, "pathlib": pathlib}
        exec(compile(ast.Module(body=wanted, type_ignores=[]), str(path), "exec"), scope)
        return scope

    def test_observer_crash_prints_bounded_record_and_exits_one(self):
        scope = self.guest()

        def crash():
            raise RuntimeError("private boom")

        scope["_tour_public_ui"] = crash
        output = io.StringIO()
        with redirect_stdout(output), self.assertRaises(SystemExit) as caught:
            scope["tour_public_ui"]()
        self.assertEqual(caught.exception.code, 1)
        root = json.loads(output.getvalue())
        self.assertEqual(set(root), {"tour_public_ui_error"})
        record = root["tour_public_ui_error"]
        self.assertEqual({key: record[key] for key in ("stage", "exception_type", "code")},
                         {"stage": "unstaged", "exception_type": "RuntimeError",
                          "code": "observation-exception"})
        self.assertNotIn("private", output.getvalue())
        self.assertNotIn("Traceback", output.getvalue())

    def test_unreadable_owned_processes_are_skipped_not_fatal(self):
        import os
        import pathlib
        import tempfile
        from unittest import mock
        scope = self.guest()
        root = pathlib.Path(tempfile.mkdtemp(prefix='tour-proc-'))
        owner = os.geteuid()

        def process(pid, exe):
            entry = root / str(pid)
            entry.mkdir()
            entry.joinpath('exe').symlink_to(exe)
            fields = ' '.join(['S'] + [str(index) for index in range(1, 22)])
            entry.joinpath('stat').write_text(f'{pid} (proc) {fields}\n')
            entry.joinpath('environ').write_bytes(b'WAYLAND_DISPLAY=wayland-0\0XDG_SESSION_ID=c3\0')

        process(4242, '/usr/bin/tuna-shell-gtk')
        process(5150, '/usr/bin/bwrap')
        process(6161, '/usr/bin/other')
        real_readlink = os.readlink

        def readlink(path):
            if str(path).endswith('5150/exe'):
                raise PermissionError(13, 'Permission denied', str(path))
            return real_readlink(path)

        fake_os = mock.Mock(wraps=os)
        fake_os.readlink = readlink
        scope['os'] = fake_os
        candidates = scope['_tour_shell_candidates'](owner, root)
        self.assertEqual(candidates, [{'pid': 4242, 'uid': owner, 'start_ticks': 19,
                                       'executable': '/usr/bin/tuna-shell-gtk',
                                       'display': 'wayland-0', 'session_id': 'c3'}])

    def test_no_owned_shell_leaves_uniqueness_to_the_caller(self):
        import os
        import pathlib
        import tempfile
        scope = self.guest()
        root = pathlib.Path(tempfile.mkdtemp(prefix='tour-proc-empty-'))
        self.assertEqual(scope['_tour_shell_candidates'](os.geteuid(), root), [])


if __name__ == "__main__":
    unittest.main()
