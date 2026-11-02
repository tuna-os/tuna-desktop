#!/usr/bin/env python3
"""Bind canonical nested GTK media to the real clean build and launched ELFs.

Called only at the full, ordinary source proof's build/launch seam. Partial,
installed and comparison/override proofs deliberately cannot emit this receipt.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from maintained_media import assertions, digest, inventory


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def source(root):
    subprocess.run(["git", "-C", str(root), "diff", "--quiet", "HEAD", "--"], check=True)
    return {"merged_commit": os.environ["GITHUB_SHA"],
            "built_commit": git(root, "rev-parse", "HEAD"),
            "built_tree": git(root, "rev-parse", "HEAD^{tree}"),
            "run_id": os.environ["GITHUB_RUN_ID"], "run_attempt": os.environ["GITHUB_RUN_ATTEMPT"]}


def elf(path):
    blob = path.read_bytes()
    if not blob.startswith(b"\x7fELF"):
        raise ValueError("launched media producer is not an ELF")
    return hashlib.sha256(blob).hexdigest()


def process(pid, expected):
    proc = Path("/proc") / str(pid)
    executable = proc / "exe"
    if executable.resolve() != expected.resolve() or elf(executable) != elf(expected):
        raise ValueError("actual producer process differs from current built ELF")
    # comm can contain spaces/parentheses; fields after its closing ')' are stable.
    fields = (proc / "stat").read_text().rsplit(")", 1)[1].split()
    return {"pid": int(pid), "start_ticks": int(fields[19]), "sha256": elf(executable)}


def main():
    command, root, artifacts, *tail = sys.argv[1:]
    root, artifacts = Path(root), Path(artifacts)
    if artifacts.resolve().is_relative_to(root.resolve() / "docs"):
        raise ValueError("proof media provenance cannot overwrite historical documentation")
    if command == "invalidate":
        for filename in ("media-source.json", "media-build-source.json", "media-capture.json", "media-receipt.json"):
            (artifacts / filename).unlink(missing_ok=True)
        return
    build = artifacts / "media-build-source.json"
    if command == "begin":
        (artifacts / "media-source.json").unlink(missing_ok=True)
        (artifacts / "media-capture.json").unlink(missing_ok=True)
        # Every canonical frame must be newly captured by this invocation;
        # an absent capture cannot inherit a previous run's PNG.
        for filename, _ in inventory()["gtk-walkthrough"]:
            (artifacts / filename).unlink(missing_ok=True)
        build.write_text(json.dumps(source(root), sort_keys=True) + "\n")
        return
    if command == "finish":
        expected = json.loads(build.read_text())
        if source(root) != expected:
            raise ValueError("source changed during the actual media proof")
        source_bytes = (artifacts / "media-source.json").read_bytes()
        outcomes = (artifacts / "assertions.txt").read_bytes()
        assertions(outcomes)
        captures = {}
        for filename, _ in inventory()["gtk-walkthrough"]:
            blob = (artifacts / filename).read_bytes()
            captures[filename] = {"sha256": digest(blob), "bytes": len(blob)}
        capture = {"schema": 1, "status": "ok", "exit_code": 0,
                   "source_sha256": digest(source_bytes), "outcomes_sha256": digest(outcomes),
                   "files": captures}
        (artifacts / "media-capture.json").write_text(json.dumps(capture, sort_keys=True, indent=2) + "\n")
        return
    if command != "launch" or len(tail) != 1:
        raise ValueError("expected begin|finish ROOT ARTIFACTS or launch ROOT ARTIFACTS COMPOSITOR_PID")
    expected = json.loads(build.read_text())
    if source(root) != expected:
        raise ValueError("source changed between actual media build and launch")
    compositor = int(tail[0])
    binaries = root / "target/debug"
    record = process(compositor, binaries / "tuna-compositor")
    deadline = time.monotonic() + 30
    while True:
        children = (Path("/proc") / str(compositor) / "task" / str(compositor) / "children").read_text().split()
        shell = [int(pid) for pid in children if (Path("/proc") / pid / "exe").resolve() ==
                 (binaries / "tuna-shell-gtk").resolve()]
        if len(shell) == 1:
            break
        if time.monotonic() >= deadline:
            raise ValueError("original compositor did not launch its current built GTK shell")
        time.sleep(0.1)
    expected["launched_elfs"] = {"tuna-compositor": record,
                                 "tuna-shell-gtk": process(shell[0], binaries / "tuna-shell-gtk")}
    expected["environment"] = "nested-gtk"
    expected["tracked_source_clean"] = True
    (artifacts / "media-source.json").write_text(json.dumps(expected, sort_keys=True, indent=2) + "\n")


if __name__ == "__main__":
    main()
