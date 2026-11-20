#!/usr/bin/env python3
"""snapshot_state copies a state file that a writer keeps renaming over.

The compositor publishes TUNA_COMPOSITOR_STATE by renaming a fresh file over
the path. Plain cp refuses that race ("replaced while being copied") and
failed a journey proof step; every snapshot must be one complete version.
"""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "scripts/lib/tuna-state-snapshot.sh"
# The copy command under test; `cp` here reproduces the original failure.
COPY = "snapshot_state"
# Plain cp lost this race in about 0.2-1% of copies on coreutils 9.4 and 9.7.
COPIES = 5000
# Spans more than one page, so a torn read shows as a short pad.
PAD = 4096

# Publishes like crates/compositor/src/state_snapshot.rs: write a temporary
# file, then rename it over the path, as fast as possible.
WRITER = r'''
import json, os, sys
path, pad = sys.argv[1], int(sys.argv[2])
tmp, serial = path + ".tmp", 0
while True:
    serial += 1
    with open(tmp, "w") as f:
        json.dump({"serial": serial, "pad": "x" * pad, "end": serial}, f)
    os.rename(tmp, path)
'''

COPY_LOOP = r'''set -eu
. "$1"
i=0
while [ "$i" -lt "$4" ]; do
    $COPY "$2" "$3/$i.json"
    i=$((i + 1))
done
'''


def run_copy(src, dst, count=1):
    return subprocess.run(["sh", "-c", COPY_LOOP, "copy", str(HELPER), str(src), str(dst), str(count)],
                          env={"PATH": "/usr/bin:/bin", "COPY": COPY},
                          capture_output=True, text=True, timeout=300)


class SnapshotState(unittest.TestCase):
    def test_every_copy_under_tight_renames_is_one_complete_version(self):
        with tempfile.TemporaryDirectory() as tmp:
            src, out = Path(tmp) / "state.json", Path(tmp) / "out"
            out.mkdir()
            writer = subprocess.Popen([sys.executable, "-c", WRITER, str(src), str(PAD)])
            try:
                while not src.exists():
                    self.assertIsNone(writer.poll(), "writer died")
                    time.sleep(0.01)
                result = run_copy(src, out, COPIES)
            finally:
                writer.kill()
                writer.wait()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stderr, "")
            copies = sorted(out.iterdir())
            self.assertEqual(len(copies), COPIES, [p.name for p in copies if not p.name.endswith(".json")])
            serials = set()
            for path in copies:
                state = json.loads(path.read_text())
                self.assertEqual(state["serial"], state["end"], path.name)
                self.assertEqual(state["pad"], "x" * PAD, path.name)
                serials.add(state["serial"])
            # The writer really replaced the file while the copies ran.
            self.assertGreater(len(serials), 1)

    def test_missing_source_fails_and_leaves_no_destination(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            out.mkdir()
            result = run_copy(Path(tmp) / "missing.json", out)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("missing.json", result.stderr)
            self.assertEqual(list(out.iterdir()), [])

    def test_existing_destination_is_replaced_whole(self):
        with tempfile.TemporaryDirectory() as tmp:
            src, out = Path(tmp) / "state.json", Path(tmp) / "out"
            out.mkdir()
            (out / "0.json").write_text("old " * 100)
            src.write_text('{"serial": 2}')
            result = run_copy(src, out)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((out / "0.json").read_text(), '{"serial": 2}')
            self.assertEqual(sorted(p.name for p in out.iterdir()), ["0.json"])


if __name__ == "__main__":
    unittest.main()
