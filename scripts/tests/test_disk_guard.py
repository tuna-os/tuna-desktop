#!/usr/bin/env python3
"""scripts/tuna-disk-guard removes only idle, unreferenced targets and scratch."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
GUARD = ROOT / "scripts/tuna-disk-guard"


def git(*args, cwd):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True)


def age(path: Path, minutes: int):
    """Backdate PATH and everything under it."""
    when = time.time() - minutes * 60
    for p in [path, *path.rglob("*")]:
        os.utime(p, (when, when))


def make_target(wt: Path, minutes: int) -> Path:
    target = wt / "target"
    (target / "debug/deps").mkdir(parents=True)
    (target / "debug/deps/libx.rlib").write_bytes(b"x")
    age(target, minutes)
    return target


class DiskGuard(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="disk-guard-test."))
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.dir)])
        self.main = self.dir / "main"
        self.main.mkdir()
        git("init", "-q", "-b", "main", cwd=self.main)
        git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q",
            "--allow-empty", "-m", "init", cwd=self.main)
        self.trees = {}
        for name in ("idle", "fresh", "busy", "running", "argref"):
            wt = self.dir / "wt" / name
            git("worktree", "add", "-q", "-b", name, str(wt), cwd=self.main)
            self.trees[name] = wt
        self.tmp = self.dir / "tmp"
        self.tmp.mkdir()
        self.ci = self.dir / "ci"
        self.ci.mkdir()

    def run_guard(self, *args, min_free="1000000"):
        env = dict(os.environ, TUNA_MIN_FREE_GB=min_free, TUNA_IDLE_MINUTES="45",
                   TUNA_CI_ARTIFACTS=str(self.ci), TMPDIR=str(self.tmp))
        return subprocess.run([str(GUARD), *args], cwd=self.main, env=env,
                              check=True, capture_output=True, text=True).stdout

    def test_removes_only_idle_unreferenced(self):
        main_target = make_target(self.main, 120)
        # A binary running from an old target, started from elsewhere and
        # named nowhere in its arguments, as a proof runs the shell.
        running = make_target(self.trees["running"], 120)
        exe = running / "debug/tuna-sleep"
        shutil.copy2("/bin/sleep", exe)
        age(running, 120)
        idle = make_target(self.trees["idle"], 120)
        fresh = make_target(self.trees["fresh"], 120)
        deep = fresh / "debug/a/b/c/d"
        deep.mkdir(parents=True)
        recently_written = deep / "object"
        recently_written.write_text("still building")
        age(fresh, 120)
        recently_written.touch()
        argref = make_target(self.trees["argref"], 120)
        busy = make_target(self.trees["busy"], 120)
        old_proof = self.tmp / "tuna-proof-gtk-shell-proof.old"
        new_proof = self.tmp / "tuna-proof-gtk-shell-proof.new"
        held_proof = self.tmp / "tuna-proof-gtk-shell-proof.held"
        other = self.tmp / "not-ours"
        old_ci, new_ci = self.ci / "run-1", self.ci / "run-2"
        for d, minutes in ((old_proof, 120), (new_proof, 30), (held_proof, 120),
                           (other, 120), (old_ci, 120), (new_ci, 30)):
            (d / "sub").mkdir(parents=True)
            (d / "sub/f").write_text("x")
            age(d, minutes)
        # Live processes: one working inside an old target, one inside an
        # old proof dir, one executing a binary from an old target.
        live = [subprocess.Popen(["sleep", "60"], cwd=busy / "debug"),
                subprocess.Popen(["sleep", "60"], cwd=held_proof),
                subprocess.Popen(["tuna-sleep", "60"], executable=exe, cwd=self.dir),
                subprocess.Popen(["python3", "-c", "import time; time.sleep(60)",
                                  f"--target-dir={argref}"], cwd=self.dir)]
        for p in live:
            self.addCleanup(p.wait)
            self.addCleanup(p.kill)

        out = self.run_guard()

        self.assertFalse(idle.exists(), out)
        self.assertFalse(old_proof.exists(), out)
        self.assertFalse(old_ci.exists(), out)
        for kept in (main_target, fresh, busy, running, argref, new_proof, held_proof, other, new_ci):
            self.assertTrue(kept.exists(), f"{kept} removed:\n{out}")
        self.assertIn(f"removed {idle}", out)

        self.run_guard("--include-main")
        self.assertFalse(main_target.exists())

    def test_nothing_removed_with_enough_space(self):
        idle = make_target(self.trees["idle"], 120)
        self.assertEqual(self.run_guard(min_free="0"), "")
        self.assertTrue(idle.exists())


if __name__ == "__main__":
    unittest.main()
