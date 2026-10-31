#!/usr/bin/env python3
"""scripts/tuna-reaper stops only slot holders past their timeout."""
import os
from pathlib import Path
import select
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
REAPER = ROOT / "scripts/tuna-reaper"
SEMAPHORE = ROOT / "scripts/lib/tuna-semaphore"
TIMEOUT = ROOT / "scripts/lib/tuna-timeout"


def alive(pid: int) -> bool:
    try:
        state = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0]
    except FileNotFoundError:
        return False
    return state != "Z"


def children_named(marker: str) -> list[int]:
    out = subprocess.run(["pgrep", "-f", marker], capture_output=True, text=True).stdout
    return [int(p) for p in out.split()]


class Reaper(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="reaper-test."))
        self.tag = "tuna-" + self.dir.name
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.dir)])
        self.env = dict(os.environ,
                        TUNA_BUILD_SLOT_BASE=str(self.dir / "build.slot"),
                        TUNA_PROOF_SLOT_BASE=str(self.dir / "proof.slot"),
                        TUNA_PROOF_LOCK=str(self.dir / "proof.lock"),
                        TUNA_BUILD_TIMEOUT="60", TUNA_PROOF_TIMEOUT="45")

    def hold(self, base: str, tag: str, slot_minutes=None) -> subprocess.Popen:
        """A build/proof holding a slot, its work in a tuna-timeout group."""
        started = self.dir / f"{tag}.started"
        holder = subprocess.Popen(
            [str(SEMAPHORE), str(self.dir / base), "2", str(TIMEOUT), "600", tag,
             "bash", "-c", f"sleep 600 & echo $! >{started}.tmp; mv {started}.tmp {started}; exec -a {tag} sleep 600"],
            env=dict(self.env, **({"TUNA_SLOT_TIMEOUT": str(slot_minutes)}
                                 if slot_minutes is not None else {})), start_new_session=True,
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(holder.wait)
        self.addCleanup(lambda: subprocess.run(["pkill", "-KILL", "-f", tag]))
        self.addCleanup(holder.kill)
        end = time.time() + 60
        while time.time() < end:
            if started.exists():
                try:
                    child = int(started.read_text().strip())
                except ValueError:
                    time.sleep(0.05)
                    continue
                self.addCleanup(lambda: subprocess.run(["kill", "-KILL", str(child)]))
                holder.child = child
                return holder
            time.sleep(0.05)
        out, err = (holder.stdout.read() if holder.stdout else ""), (holder.stderr.read() if holder.stderr else "")
        self.fail(f"{tag} never started after 60s (rc={holder.poll()} out={out!r} err={err!r})")

    def test_stuck_holder_is_stopped_fresh_one_kept(self):
        stuck = self.hold("build.slot", self.tag + "-stuck", slot_minutes=1)
        fresh = self.hold("build.slot", self.tag + "-fresh")
        proof = self.hold("proof.slot", self.tag + "-proof")
        # Its own recorded deadline is 1 min, not the configured default 60.
        old = time.time() - 20 * 60  # past 1 + 15, but not past 60 + 15
        os.utime(self.dir / "build.slot.0", (old, old))
        # The proof took 40 minutes: within 45 + 15.
        recent = time.time() - 40 * 60
        os.utime(self.dir / "proof.slot.0", (recent, recent))

        out = subprocess.run([str(REAPER)], env=self.env, check=True,
                             capture_output=True, text=True, timeout=60).stdout

        stuck.wait(timeout=5)
        self.assertEqual(children_named(self.tag + "-stuck"), [], out)
        # Its work ran in tuna-timeout's own process group; that went too.
        self.assertFalse(alive(stuck.child), out)
        for kept in (fresh, proof):
            self.assertTrue(alive(kept.pid), out)
            self.assertTrue(alive(kept.child), out)

    def test_detached_proof_marker_is_reaped(self):
        self.env["TUNA_PROOF_RUN"] = str(self.dir / "owned-proof")
        holder = self.hold("proof.slot", self.tag + "-marked")
        detached = subprocess.Popen(
            ["sleep", "600"], env=self.env, start_new_session=True,
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.addCleanup(detached.wait)
        self.addCleanup(detached.kill)
        old = time.time() - 65 * 60  # proof's 45 min timeout + 15 min grace
        os.utime(self.dir / "proof.slot.0", (old, old))
        reaper_env = dict(self.env)
        del reaper_env["TUNA_PROOF_RUN"]
        out = subprocess.run([str(REAPER)], env=reaper_env, check=True,
                             capture_output=True, text=True, timeout=60).stdout
        holder.wait(timeout=5)
        detached.wait(timeout=5)
        self.assertFalse(alive(detached.pid), out)

    def test_stale_unlocked_open_file_does_not_kill_reader(self):
        slot = self.dir / "build.slot.0"
        slot.write_text("1\n")
        old = time.time() - 20 * 60
        os.utime(slot, (old, old))
        reader = subprocess.Popen(
            ["bash", "-c", 'exec 9<"$1"; echo ready; exec sleep 600', "reader", str(slot)],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, text=True)
        try:
            if not select.select([reader.stdout], [], [], 60)[0]:
                self.fail("unlocked reader did not start within its fixture deadline")
            self.assertEqual(reader.stdout.readline().strip(), "ready")
            subprocess.run([str(REAPER)], env=self.env, check=True,
                           capture_output=True, text=True, timeout=60)
            self.assertIsNone(reader.poll(), "an unlocked reader is not a stale slot holder")
        finally:
            reader.kill()
            reader.wait(timeout=5)
            reader.stdout.close()

    def test_nothing_held_nothing_done(self):
        out = subprocess.run([str(REAPER)], env=self.env, check=True,
                             capture_output=True, text=True, timeout=30).stdout
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()
