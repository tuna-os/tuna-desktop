#!/usr/bin/python3
import importlib.util
from pathlib import Path
import subprocess
import unittest

spec = importlib.util.spec_from_file_location("cjk_font", Path(__file__).with_name("tuna-visual-cjk-font.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class CjkFontAdmissionTests(unittest.TestCase):
    def runner(self, status="ii \t1:fixture", missing=None):
        def run(command, **kwargs):
            self.assertEqual(kwargs, {"text": True, "timeout": 10})
            if command[0] == "dpkg-query":
                self.assertEqual(command[-1], "fonts-noto-cjk")
                return status
            self.assertEqual(command[:2], ["fc-list", "--format=%{file}\n"])
            return "" if command[-1] == missing else "/fixture/NotoSansCJK.ttc\n"
        return run

    def test_installed_package_and_all_fixture_glyphs_are_required(self):
        receipt = module.check_fixture(self.runner())
        self.assertEqual(receipt["package"], "fonts-noto-cjk")
        self.assertEqual(set(receipt["glyph_fonts"]), {"U+4F60", "U+597D", "U+5C3C", "U+6CE5"})

    def test_removed_or_unconfigured_package_is_rejected(self):
        for status in ("rc \t1:fixture", "iU \t1:fixture", "ii \t"):
            with self.subTest(status=status), self.assertRaises((RuntimeError, ValueError)):
                module.check_fixture(self.runner(status))

    def test_installed_package_without_fontconfig_glyph_coverage_is_rejected(self):
        for missing in (":charset=4f60", ":charset=597d", ":charset=5c3c", ":charset=6ce5"):
            with self.subTest(glyph=missing), self.assertRaises(RuntimeError):
                module.check_fixture(self.runner(missing=missing))

    def test_missing_package_or_font_tool_error_cannot_pass(self):
        def unavailable(command, **_kwargs):
            raise subprocess.CalledProcessError(1, command)
        with self.assertRaises(subprocess.CalledProcessError):
            module.check_fixture(unavailable)

    def test_input_workflow_installs_font_and_checks_before_capture(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/visual-capture.yml").read_text()
        install = workflow.index("install -y --no-install-recommends fontconfig fonts-noto-cjk")
        preflight = workflow.index("scripts/lib/tuna-visual-cjk-font.py")
        capture = workflow.index('scripts/tuna-visual-capture --stage')
        self.assertLess(install, preflight)
        self.assertLess(preflight, capture)


if __name__ == "__main__":
    unittest.main()
