#!/usr/bin/env python3
"""Unit tests for tuna-polkit-helper-stub.py protocol and authentication decisions."""
import io
import os
from pathlib import Path
import runpy
import sys
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
STUB_PATH = ROOT / "scripts" / "lib" / "tuna-polkit-helper-stub.py"


class PolkitHelperStubTests(unittest.TestCase):
    def execute_stub(self, args, stdin_text="", env=None):
        fake_stdin = io.StringIO(stdin_text)
        fake_stdout = io.StringIO()
        merged_env = os.environ.copy()
        if env is not None:
            merged_env.update(env)

        argv = [str(STUB_PATH)] + args
        exit_code = 0
        with patch.dict(os.environ, merged_env, clear=True), \
             patch("sys.argv", argv), \
             patch("sys.stdin", fake_stdin), \
             patch("sys.stdout", fake_stdout):
            try:
                runpy.run_path(str(STUB_PATH), run_name="__main__")
            except SystemExit as exc:
                exit_code = exc.code

        return exit_code, fake_stdout.getvalue()

    def test_missing_user_argument_exits_code_2(self):
        code, stdout = self.execute_stub([])
        self.assertEqual(code, 2)

    def test_excess_arguments_exits_code_2(self):
        code, stdout = self.execute_stub(["user1", "user2"])
        self.assertEqual(code, 2)

    def test_valid_default_password_authenticates_success(self):
        code, stdout = self.execute_stub(["testuser"], stdin_text="valid-cookie\ntuna-proof\n")
        self.assertEqual(code, 0)
        self.assertIn("PAM_PROMPT_ECHO_OFF Password: ", stdout)
        self.assertTrue(stdout.endswith("SUCCESS\n"))

    def test_wrong_password_returns_failure(self):
        code, stdout = self.execute_stub(["testuser"], stdin_text="valid-cookie\nwrong-pass\n")
        self.assertEqual(code, 0)
        self.assertIn("PAM_PROMPT_ECHO_OFF Password: ", stdout)
        self.assertTrue(stdout.endswith("FAILURE\n"))

    def test_empty_cookie_returns_failure(self):
        code, stdout = self.execute_stub(["testuser"], stdin_text="\ntuna-proof\n")
        self.assertEqual(code, 0)
        self.assertTrue(stdout.endswith("FAILURE\n"))

    def test_whitespace_only_cookie_returns_failure(self):
        code, stdout = self.execute_stub(["testuser"], stdin_text="   \t  \ntuna-proof\n")
        self.assertEqual(code, 0)
        self.assertTrue(stdout.endswith("FAILURE\n"))

    def test_custom_password_env_succeeds(self):
        code, stdout = self.execute_stub(
            ["testuser"],
            stdin_text="valid-cookie\nsecret-pass-42\n",
            env={"TUNA_POLKIT_TEST_PASSWORD": "secret-pass-42"},
        )
        self.assertEqual(code, 0)
        self.assertTrue(stdout.endswith("SUCCESS\n"))

    def test_custom_password_env_rejects_default_password(self):
        code, stdout = self.execute_stub(
            ["testuser"],
            stdin_text="valid-cookie\ntuna-proof\n",
            env={"TUNA_POLKIT_TEST_PASSWORD": "secret-pass-42"},
        )
        self.assertEqual(code, 0)
        self.assertTrue(stdout.endswith("FAILURE\n"))


if __name__ == "__main__":
    unittest.main()
