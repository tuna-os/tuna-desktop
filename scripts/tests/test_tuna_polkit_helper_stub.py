#!/usr/bin/env python3
"""Behaviour of the polkit-agent-helper-1 stand-in used by the GTK shell
proof and visual capture ($TUNA_POLKIT_HELPER).

The shell's polkit agent drives the helper exactly like the real
polkit-agent-helper-1 (polkit src/polkitagent/polkitagenthelper-pam.c):
USER on argv, the cookie on the first stdin line, a
"PAM_PROMPT_ECHO_OFF <prompt>" request, the answer on the next stdin
line, then a final SUCCESS/FAILURE line. A stub that answers SUCCESS too
eagerly would let the proof pass with a broken password path, so the
rejection branches matter as much as the happy path.
"""
import os
import subprocess
import sys
import threading
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
STUB = ROOT / 'scripts' / 'lib' / 'tuna-polkit-helper-stub.py'
PROMPT = 'PAM_PROMPT_ECHO_OFF Password: '
COOKIE = '1-6c3b2b5e-0a4f-4f6a-9d0b-6f1c2a1e9f00-42\n'


def run(argv, stdin, password_env=None):
    env = {k: v for k, v in os.environ.items() if k != 'TUNA_POLKIT_TEST_PASSWORD'}
    if password_env is not None:
        env['TUNA_POLKIT_TEST_PASSWORD'] = password_env
    return subprocess.run([sys.executable, str(STUB), *argv], input=stdin, env=env,
                          capture_output=True, text=True, timeout=30)


class Arguments(unittest.TestCase):
    def test_missing_user_is_usage_error(self):
        result = run([], COOKIE + 'tuna-proof\n')
        self.assertEqual(result.returncode, 2)
        self.assertNotIn('SUCCESS', result.stdout)

    def test_excess_arguments_are_usage_error(self):
        result = run(['alice', 'extra'], COOKIE + 'tuna-proof\n')
        self.assertEqual(result.returncode, 2)
        self.assertNotIn('SUCCESS', result.stdout)


class Conversation(unittest.TestCase):
    def outcome(self, stdin, password_env=None):
        result = run(['alice'], stdin, password_env)
        self.assertEqual(result.returncode, 0, result.stderr)
        lines = result.stdout.splitlines()
        self.assertEqual(lines[0], PROMPT, 'the agent needs exactly one PAM prompt first')
        self.assertEqual(len(lines), 2, lines)
        return lines[1]

    def test_default_password_succeeds(self):
        self.assertEqual(self.outcome(COOKIE + 'tuna-proof\n'), 'SUCCESS')

    def test_wrong_password_fails(self):
        self.assertEqual(self.outcome(COOKIE + 'wrong\n'), 'FAILURE')

    def test_password_is_matched_exactly(self):
        for answer in ('tuna-proof \n', ' tuna-proof\n', 'TUNA-PROOF\n', 'tuna-proo\n',
                       'tuna-prooff\n', '\n'):
            with self.subTest(answer=answer):
                self.assertEqual(self.outcome(COOKIE + answer), 'FAILURE')

    def test_empty_cookie_fails_even_with_right_password(self):
        self.assertEqual(self.outcome('\ntuna-proof\n'), 'FAILURE')

    def test_whitespace_cookie_fails_even_with_right_password(self):
        self.assertEqual(self.outcome(' \t \ntuna-proof\n'), 'FAILURE')

    def test_closed_stdin_before_answer_fails(self):
        # The agent dialog was dismissed: no answer line ever arrives.
        self.assertEqual(self.outcome(COOKIE), 'FAILURE')
        self.assertEqual(self.outcome(''), 'FAILURE')

    def test_answer_without_trailing_newline_is_accepted(self):
        self.assertEqual(self.outcome(COOKIE + 'tuna-proof'), 'SUCCESS')

    def test_custom_password_succeeds(self):
        self.assertEqual(self.outcome(COOKIE + 'hunter2\n', 'hunter2'), 'SUCCESS')

    def test_custom_password_rejects_default(self):
        self.assertEqual(self.outcome(COOKIE + 'tuna-proof\n', 'hunter2'), 'FAILURE')


class Interleaving(unittest.TestCase):
    def test_prompt_follows_cookie_and_result_follows_answer(self):
        # A line-buffered agent writes the cookie, waits for the prompt,
        # and only then answers; a stub that blocked on output buffering
        # or answered before reading the password would deadlock or lie.
        env = {k: v for k, v in os.environ.items() if k != 'TUNA_POLKIT_TEST_PASSWORD'}
        child = subprocess.Popen([sys.executable, str(STUB), 'alice'], env=env, text=True,
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        # readline() has no deadline of its own; a hung stub is killed.
        watchdog = threading.Timer(30, child.kill)
        watchdog.start()
        try:
            child.stdin.write(COOKIE)
            child.stdin.flush()
            self.assertEqual(child.stdout.readline(), PROMPT + '\n')
            child.stdin.write('tuna-proof\n')
            child.stdin.flush()
            self.assertEqual(child.stdout.readline(), 'SUCCESS\n')
            child.stdin.close()
            self.assertEqual(child.wait(timeout=30), 0)
        finally:
            watchdog.cancel()
            if child.poll() is None:
                child.kill()
                child.wait(timeout=30)
            child.stdout.close()


if __name__ == '__main__':
    unittest.main()
