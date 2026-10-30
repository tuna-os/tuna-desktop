"""Exercise the exact CI freeze program without Cargo or runtime claims."""
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
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

    def test_ci_freezes_before_live_accessibility_creates_artifacts(self):
        checkout = Path(__file__).resolve().parents[2]
        workflow = (checkout / ".github/workflows/ci.yml").read_text()
        freeze = workflow.index("Prove source-matched native minimize and restore in LTR and RTL")
        accessibility = workflow.index("Prove live accessibility widgets and focus timers")
        self.assertLess(freeze, accessibility)
        self.assertIn('scripts/tuna-minimize-ci-proof "$RUNNER_TEMP/native-minimize"', workflow)


if __name__ == "__main__":
    unittest.main()
