#!/usr/bin/env python3
"""Self-test for scripts/tuna-doc-refs (#1219): one fixture that must fail,
one that must pass, the allowlist, the changed-file scope and the CI wiring."""
import importlib.machinery
import importlib.util
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/tuna-doc-refs"
_loader = importlib.machinery.SourceFileLoader("tuna_doc_refs", str(SCRIPT))
_spec = importlib.util.spec_from_loader(_loader.name, _loader)
D = importlib.util.module_from_spec(_spec)
_loader.exec_module(D)

TOP = {"docs", "scripts", "crates", "README.md"}


def git(root, *argv):
    return subprocess.run(["git", "-C", str(root), *argv], check=True, capture_output=True, text=True)


class Fixture(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        for path in ("docs/real.md", "docs/sub/other.md", "docs/with space.md",
                     "scripts/real.sh", "crates/c/src/lib.rs", "README.md"):
            (self.root / path).parent.mkdir(parents=True, exist_ok=True)
            (self.root / path).write_text("x\n")

    def problems(self, text, allowlist=None):
        page = self.root / "docs/page.md"
        page.write_text(text)
        return D.find_problems(page, self.root, allowlist, TOP)

    def test_broken_links_and_paths_fail_naming_file_line_and_reference(self):
        self.assertEqual(self.problems(
            "intro\n"
            "[gone](missing.md) and [anchored](sub/gone.md#part)\n"
            "![image](img/none.png) and `scripts/missing.sh`\n"
            "[label]: ../nowhere.md\n"
            "`crates/c/src/gone.rs:12-40` `docs/real.md/extra`\n"), [
            "docs/page.md:2: link 'missing.md' does not resolve",
            "docs/page.md:2: link 'sub/gone.md#part' does not resolve",
            "docs/page.md:3: link 'img/none.png' does not resolve",
            "docs/page.md:3: path 'scripts/missing.sh' does not resolve",
            "docs/page.md:4: link '../nowhere.md' does not resolve",
            "docs/page.md:5: path 'crates/c/src/gone.rs:12-40' does not resolve",
            "docs/page.md:5: path 'docs/real.md/extra' does not resolve",
        ])

    def test_resolvable_and_non_path_references_pass(self):
        self.assertEqual(self.problems(
            "[a](real.md#x) [b](#top) [c](https://x.invalid/y) [d](mailto:a@b.c)\n"
            "[e](sub/other.md) [f](../README.md) [g](/scripts/real.sh) [h](<with space.md>)\n"
            "[i](with%20space.md \"title\") ![j](real.md) [k [nested]](real.md)\n"
            "[label]: sub/other.md\n"
            "[^1]: a footnote, not a link\n"
            "`scripts/real.sh` `scripts/real.sh:3` `crates/c/src/lib.rs:10-20`\n"
            "`crates/c/src/lib.rs::item` `docs/real.md#anchor` `docs/sub/`\n"
            "`README.md` `unknowndir/file` `target/debug/tuna-compositor` `src/lib.rs`\n"
            "`docs/*.md` `docs/marlin/<date>/` `$XDG_RUNTIME_DIR/x` `docs/{a,b}.md`\n"
            "`scripts/missing.sh --flag` `/usr/bin/x` `../x/y` `docs/.../x.md`\n"
            "`[gone](missing.md)` inside a code span is not a link\n"
            "```sh\n[ignored](nope.md) `scripts/nope.sh`\n```\n"
            "~~~\n[ignored](nope.md)\n~~~\n"), [])

    def test_allowlisted_references_pass(self):
        self.assertEqual(self.problems("[gone](missing.md) `scripts/built.sh`\n",
                                       {"missing.md": "example", "scripts/built.sh": "build time"}), [])

    def test_allowlist_entries_need_a_reason(self):
        allowlist = self.root / "allow.txt"
        allowlist.write_text("# comment\n\nok/path | made at run time\n")
        self.assertEqual(D.load_allowlist(allowlist), {"ok/path": "made at run time"})
        for bad in ("some/path\n", "some/path | \n", " | reason only\n"):
            allowlist.write_text(bad)
            with self.subTest(entry=bad), self.assertRaises(ValueError):
                D.load_allowlist(allowlist)

    def test_the_repository_allowlist_loads_and_every_entry_has_a_reason(self):
        entries = D.load_allowlist()
        self.assertTrue(entries)
        self.assertTrue(all(reason for reason in entries.values()))

    def test_evidence_and_history_trees_are_not_living_docs(self):
        for path in (".spektacular/x.md", "docs/releases/v1.md", "docs/research/a.md",
                     "third-party/smithay/README.md", "docs/marlin/2026-10-30-corral-journey/notes.md",
                     "docs/perf/2026-10-04-x/report.md"):
            self.assertFalse(D.living(path), path)
        for path in ("docs/v1.md", "README.md", "docs/marlin/README.md", "docs/perf/README.md"):
            self.assertTrue(D.living(path), path)


class ChangedFiles(unittest.TestCase):
    """The command checks only the Markdown a change touches."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / "scripts").mkdir()
        shutil.copy2(SCRIPT, self.root / "scripts/tuna-doc-refs")
        (self.root / "docs").mkdir()
        (self.root / "docs/old.md").write_text("[debt](gone.md)\n")
        (self.root / "docs/edited.md").write_text("fine\n")
        (self.root / "docs/doomed.md").write_text("[debt](gone.md)\n")
        git(self.root, "init", "-q", "-b", "main")
        git(self.root, "add", "-A")
        git(self.root, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "base")
        self.base = git(self.root, "rev-parse", "HEAD").stdout.strip()

    def commit(self):
        git(self.root, "add", "-A")
        git(self.root, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "change")

    def run_script(self, *argv):
        return subprocess.run([str(self.root / "scripts/tuna-doc-refs"), *argv], cwd=self.root,
                              capture_output=True, text=True, timeout=60,
                              env={**os.environ, "GIT_CONFIG_NOSYSTEM": "1"})

    def test_unchanged_debt_does_not_fail_a_change_and_a_deleted_file_is_skipped(self):
        (self.root / "docs/edited.md").write_text("still fine, see [old](old.md)\n")
        (self.root / "docs/doomed.md").unlink()
        self.commit()
        result = self.run_script("--base", self.base)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("1 Markdown file(s) checked, 0 unresolved", result.stderr)

    def test_uncommitted_and_added_files_count_locally(self):
        (self.root / "docs/edited.md").write_text("[moved](moved.md)\n")
        (self.root / "docs/new.md").write_text("`scripts/gone`\n")
        git(self.root, "add", "docs/new.md")
        result = self.run_script("--base", self.base)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout.splitlines(), [
            "docs/edited.md:1: link 'moved.md' does not resolve",
            "docs/new.md:1: path 'scripts/gone' does not resolve",
        ])

    def test_a_broken_reference_in_a_changed_file_fails(self):
        (self.root / "docs/edited.md").write_text("fine\n\n[moved](moved.md) `scripts/gone`\n")
        self.commit()
        result = self.run_script("--base", self.base)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout.splitlines(), [
            "docs/edited.md:3: link 'moved.md' does not resolve",
            "docs/edited.md:3: path 'scripts/gone' does not resolve",
        ])

    def test_named_files_are_checked_whether_changed_or_not(self):
        result = self.run_script("docs/old.md")
        self.assertEqual((result.returncode, result.stdout),
                         (1, "docs/old.md:1: link 'gone.md' does not resolve\n"))

    def test_an_unknown_base_is_an_error_not_a_pass(self):
        result = self.run_script("--base", "no-such-revision")
        self.assertEqual(result.returncode, 2)
        self.assertIn("no-such-revision", result.stderr)

    def test_untracked_top_level_entries_are_not_repository_paths(self):
        (self.root / "target").mkdir()
        self.assertNotIn("target", D.top_level(self.root))
        self.assertIn("docs", D.top_level(self.root))


class Wiring(unittest.TestCase):
    def test_check_job_runs_the_self_test_and_the_resolver_on_the_change(self):
        steps = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())["jobs"]["check"]["steps"]
        runs = [step.get("run", "").strip() for step in steps]
        self.assertIn("python3 scripts/lib/tuna-doc-refs-tests.py", runs)
        resolver = [step for step in steps if step.get("run", "").startswith("scripts/tuna-doc-refs")]
        self.assertEqual(len(resolver), 1)
        self.assertEqual(resolver[0]["run"].strip(), 'scripts/tuna-doc-refs --base "$DOC_REFS_BASE"')
        self.assertEqual(resolver[0]["env"]["DOC_REFS_BASE"],
                         "${{ github.event.pull_request.base.sha || github.event.before }}")

    def test_just_recipe_runs_the_resolver(self):
        self.assertIn("scripts/tuna-doc-refs --base", (ROOT / "justfile").read_text())


if __name__ == "__main__":
    unittest.main()
