"""Exercise the exact CI freeze program without Cargo or runtime claims."""
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import sys
import unittest


ROOT = Path(__file__).resolve().parents[2]
sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT / "scripts/lib"))
from minimize_bundle import mode_artifacts, validate
FREEZE = (ROOT / "scripts/tuna-minimize-ci-proof").read_text().split(
    "<<'PY'\n", 1
)[1].split("\nPY\n", 1)[0]
MEMBERS = ("tuna-compositor", "tuna-shell-gtk", "tuna-modal-dim-client")


class MinimizeFreezeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)
        self.root = self.base / "source"
        self.root.mkdir()
        (self.root / "Cargo.lock").write_text("fixture lock bytes\n")
        subprocess.run(["git", "init", "--quiet", str(self.root)], check=True, timeout=60)
        subprocess.run(["git", "add", "Cargo.lock"], cwd=self.root, check=True, timeout=60)
        subprocess.run([
            "git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
            "commit", "--quiet", "-m", "fixture",
        ], cwd=self.root, check=True, timeout=60)
        self.art = self.base / "artifacts"
        self.art.mkdir()
        self.messages = []
        for name in MEMBERS:
            executable = self.base / name
            executable.write_bytes(b"controlled fixture, not a native runtime binary: " + name.encode())
            executable.chmod(0o755)
            self.messages.append({
                "reason": "compiler-artifact", "target": {"name": name},
                "executable": str(executable),
            })

    def run_freeze(self):
        (self.art / "build.json").write_text(
            "\n".join(json.dumps(message) for message in self.messages)
        )
        return subprocess.run([
            "/usr/bin/python3", "-c", FREEZE, str(self.root), str(self.art),
        ], capture_output=True, text=True, timeout=60)

    def test_exact_source_lock_and_member_bytes_are_bound(self):
        result = self.run_freeze()
        self.assertEqual(result.returncode, 0, result.stderr)
        frozen = self.art / "frozen"
        manifest = json.loads((frozen / "manifest.json").read_text())
        revision = subprocess.check_output([
            "git", "rev-parse", "HEAD",
        ], cwd=self.root, text=True, timeout=60).strip()
        self.assertEqual(manifest["source_commit"], revision)
        self.assertEqual((frozen / "SOURCE_COMMIT").read_text().strip(), revision)
        self.assertEqual(manifest["cargo_lock_sha256"], hashlib.sha256(
            (self.root / "Cargo.lock").read_bytes()).hexdigest())
        for name in MEMBERS:
            self.assertEqual((frozen / name).read_bytes(), (self.base / name).read_bytes())
            self.assertEqual(manifest["sha256"][name], hashlib.sha256(
                (frozen / name).read_bytes()).hexdigest())

    def test_missing_member_is_rejected_without_manifest(self):
        self.messages.pop()
        self.assertNotEqual(self.run_freeze().returncode, 0)
        self.assertFalse((self.art / "frozen/manifest.json").exists())

    def test_duplicate_member_is_rejected_without_manifest(self):
        self.messages.append(self.messages[0])
        self.assertNotEqual(self.run_freeze().returncode, 0)
        self.assertFalse((self.art / "frozen/manifest.json").exists())

    def test_dirty_source_is_rejected_without_manifest(self):
        (self.root / "Cargo.lock").write_text("changed during build\n")
        self.assertNotEqual(self.run_freeze().returncode, 0)
        self.assertFalse((self.art / "frozen/manifest.json").exists())

    def test_existing_bundle_cannot_be_reused_or_overwritten(self):
        self.assertEqual(self.run_freeze().returncode, 0)
        original = (self.art / "frozen/manifest.json").read_bytes()
        self.assertNotEqual(self.run_freeze().returncode, 0)
        self.assertEqual((self.art / "frozen/manifest.json").read_bytes(), original)

    def test_generated_checkout_artifacts_are_rejected(self):
        generated = self.root / "gtk-shell-artifacts/a11y/bin/gtk-tests"
        generated.parent.mkdir(parents=True)
        generated.write_bytes(b"generated accessibility executable")
        self.assertNotEqual(self.run_freeze().returncode, 0)
        self.assertFalse((self.art / "frozen/manifest.json").exists())

    def frozen_bundle(self):
        self.assertEqual(self.run_freeze().returncode, 0)
        return self.art / "frozen"

    def test_consumer_rejects_missing_role(self):
        frozen = self.frozen_bundle()
        (frozen / MEMBERS[0]).unlink()
        with self.assertRaises(FileNotFoundError):
            validate(self.root, self.art)

    def test_consumer_rejects_mutated_role(self):
        frozen = self.frozen_bundle()
        (frozen / MEMBERS[0]).write_bytes(b"another worktree executable")
        with self.assertRaisesRegex(ValueError, "role hashes"):
            validate(self.root, self.art)

    def test_consumer_rejects_nonexecutable_role(self):
        frozen = self.frozen_bundle()
        (frozen / MEMBERS[0]).chmod(0o644)
        with self.assertRaisesRegex(ValueError, "executable frozen role"):
            validate(self.root, self.art)

    def test_consumer_rejects_unowned_symlink_role(self):
        frozen = self.frozen_bundle()
        role = frozen / MEMBERS[0]
        role.unlink()
        role.symlink_to(self.base / MEMBERS[0])
        with self.assertRaisesRegex(ValueError, "regular frozen member"):
            validate(self.root, self.art)

    def test_consumer_rejects_unowned_symlink_freeze(self):
        frozen = self.frozen_bundle()
        frozen.rename(self.art / "other-freeze")
        frozen.symlink_to(self.art / "other-freeze", target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "owned frozen directory"):
            validate(self.root, self.art)

    def test_consumer_rejects_mismatched_source_commit(self):
        frozen = self.frozen_bundle()
        (frozen / "SOURCE_COMMIT").write_text("0" * 40 + "\n")
        with self.assertRaisesRegex(ValueError, "source revision"):
            validate(self.root, self.art)

    def test_consumer_rejects_stale_source_even_with_clean_checkout(self):
        self.frozen_bundle()
        subprocess.run(["git", "-c", "user.name=Fixture", "-c",
                        "user.email=fixture@example.invalid", "commit", "--quiet",
                        "--allow-empty", "-m", "different source"], cwd=self.root, check=True)
        with self.assertRaisesRegex(ValueError, "source revision"):
            validate(self.root, self.art)

    def test_consumer_rejects_dirty_source(self):
        self.frozen_bundle()
        (self.root / "untracked").write_text("uncommitted source")
        with self.assertRaisesRegex(ValueError, "clean source"):
            validate(self.root, self.art)

    def test_consumer_rejects_frozen_lock_mutation(self):
        frozen = self.frozen_bundle()
        (frozen / "Cargo.lock").write_text("foreign lock")
        with self.assertRaisesRegex(ValueError, "frozen lock"):
            validate(self.root, self.art)

    def test_consumer_rejects_lock_mismatched_to_clean_source(self):
        frozen = self.frozen_bundle()
        (frozen / "Cargo.lock").write_text("foreign lock")
        manifest_path = frozen / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["cargo_lock_sha256"] = hashlib.sha256(b"foreign lock").hexdigest()
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "frozen lock"):
            validate(self.root, self.art)

    def test_post_cleanup_check_rejects_coherently_rewritten_role_and_manifest(self):
        frozen = self.frozen_bundle()
        before = validate(self.root, self.art)
        (frozen / MEMBERS[0]).write_bytes(b"changed during live session")
        manifest_path = frozen / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["sha256"][MEMBERS[0]] = hashlib.sha256(b"changed during live session").hexdigest()
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "changed during proof"):
            validate(self.root, self.art, before)

    def test_post_cleanup_check_rejects_manifest_only_change(self):
        frozen = self.frozen_bundle()
        before = validate(self.root, self.art)
        path = frozen / "manifest.json"
        path.write_text(path.read_text() + "\n")
        with self.assertRaisesRegex(ValueError, "changed during proof"):
            validate(self.root, self.art, before)

    def test_modes_own_separate_fresh_state_and_refuse_existing_receipts(self):
        ltr = mode_artifacts(self.art, False)
        (ltr / "state.json").write_text("LTR state")
        (ltr / "assertions.txt").write_text("earlier receipt")
        rtl = mode_artifacts(self.art, True)
        self.assertEqual(ltr, self.art / "ltr")
        self.assertEqual(rtl, self.art / "rtl")
        self.assertEqual(list(rtl.iterdir()), [])
        with self.assertRaises(FileExistsError):
            mode_artifacts(self.art, False)
        self.assertEqual((ltr / "state.json").read_text(), "LTR state")
        self.assertEqual((ltr / "assertions.txt").read_text(), "earlier receipt")



if __name__ == "__main__":
    unittest.main()
