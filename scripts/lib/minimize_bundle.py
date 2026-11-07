"""Validate the one native minimize freeze before execution and after cleanup."""
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess

ROLES = ("tuna-compositor", "tuna-shell-gtk", "tuna-modal-dim-client")


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def validate(root, bundle_root, expected=None):
    root, bundle_root = Path(root).resolve(), Path(bundle_root).resolve()
    if bundle_root == root or root in bundle_root.parents:
        raise ValueError("bundle must remain outside the source checkout")
    frozen = bundle_root / "frozen"
    if frozen.is_symlink() or not frozen.is_dir():
        raise ValueError("owned frozen directory required")
    names = (*ROLES, "Cargo.lock", "SOURCE_COMMIT", "manifest.json")
    for name in names:
        path = frozen / name
        if not stat.S_ISREG(path.lstat().st_mode):
            raise ValueError(f"regular frozen member required: {name}")
        if name in ROLES and not os.access(path, os.X_OK):
            raise ValueError(f"executable frozen role required: {name}")
    hashes = {name: digest(frozen / name) for name in names}
    manifest = json.loads((frozen / "manifest.json").read_text())
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    status = subprocess.check_output(["git", "status", "--porcelain"], cwd=root, text=True).strip()
    if status:
        raise ValueError("proof must use clean source")
    if not revision == (frozen / "SOURCE_COMMIT").read_text().strip() == manifest["source_commit"]:
        raise ValueError("frozen source revision differs from proof source")
    if not digest(root / "Cargo.lock") == hashes["Cargo.lock"] == manifest["cargo_lock_sha256"]:
        raise ValueError("frozen lock differs from proof source")
    if manifest["sha256"] != {name: hashes[name] for name in ROLES}:
        raise ValueError("frozen role hashes differ from manifest")
    snapshot = {"source_commit": revision, "sha256": hashes}
    if expected is not None and snapshot != expected:
        raise ValueError("frozen bundle changed during proof")
    return snapshot


def mode_artifacts(bundle_root, rtl):
    """A mode owns a fresh directory; never reuse receipts or another mode's state."""
    art = Path(bundle_root).resolve() / ("rtl" if rtl else "ltr")
    art.mkdir(exist_ok=False)
    return art
