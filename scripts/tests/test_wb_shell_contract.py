#!/usr/bin/env python3
"""Structural contract for the WB shell proof's frozen-clock discipline.

Bracketed captures (owned-interior/verify) require identical typed
manual-clock samples, and real time reports animation_clock_ms null:
every direct bracketed capture site must run under a frozen clock (an
anim_clock freeze with no intervening clock-file removal), and every
freeze must be observed manual before its capture. Ramp/stepped
captures inherit their gate freeze via wb_step_ramp (unchanged) and
are out of scope here. Fail guards must use explicit if blocks: an
`A && B || fail` chain trips CI shellcheck SC2015.
"""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROOF = ROOT / 'scripts' / 'tuna-gtk-shell-proof'

BRACKETED = ['gray-baseline', 'fade-baseline', 'both-baseline',
             'reset-baseline', 'take-fade-baseline', 'take-fade-settled']

RM_CLOCK = 'rm -f "$TUNA_ANIMATION_CLOCK"'


class FrozenClockOrder(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.lines = PROOF.read_text().splitlines()

    def capture_index(self, label):
        found = [i for i, line in enumerate(self.lines)
                 if re.match(r'\s*wb_capture\s+%s\s*$' % label, line)]
        self.assertEqual(len(found), 1, f'{label}: capture site not unique')
        return found[0]

    def nearest_clock_op(self, index):
        # Gate-scoped: a freeze only counts inside the capture's own
        # gate (function bodies above the gate do not execute in order).
        for i in range(index - 1, -1, -1):
            stripped = self.lines[i].strip()
            if re.match(r'anim_clock\s', stripped):
                return ('freeze', i)
            if RM_CLOCK in stripped:
                return ('remove', i)
            if re.match(r'wb_gate_begin\b', stripped):
                return ('gate-start', i)
        return (None, -1)

    def test_bracketed_captures_run_frozen(self):
        for label in BRACKETED:
            with self.subTest(label=label):
                idx = self.capture_index(label)
                op, at = self.nearest_clock_op(idx)
                self.assertEqual(
                    op, 'freeze',
                    f'{label}: nearest clock op is {op} (line {at + 1}); '
                    f'want a freeze with no intervening clock removal')

    def test_freeze_observed_manual_before_capture(self):
        for label in BRACKETED:
            with self.subTest(label=label):
                idx = self.capture_index(label)
                op, at = self.nearest_clock_op(idx)
                self.assertEqual(op, 'freeze')
                window = self.lines[at:idx]
                self.assertTrue(
                    any("cstate '.animation_clock_manual'" in line
                        for line in window),
                    f'{label}: freeze at line {at + 1} never observed manual')


class FailChainShape(unittest.TestCase):
    def test_no_and_or_fail_chains(self):
        text = PROOF.read_text()
        # Join shell continuations into logical lines, keeping any
        # list operator: a line ending in && or || continues below.
        logical = re.sub(r'\\\n[ \t]*', '', text)
        logical = re.sub(r'(\|\||&&)\n[ \t]*', r'\1 ', logical)
        bad = []
        for line in logical.splitlines():
            # Single-quoted operators are not shell lists (awk/jq
            # programs); strip per line so stray apostrophes cannot
            # pair across lines.
            code = re.sub(r"'[^'\n]*'", "''", line)
            # One and-or list only: no separator may intervene.
            if re.search(r'&&[^;\n]*\|\|[^;\n]*\bfail\b', code):
                bad.append(line.strip())
        self.assertEqual(bad, [])


if __name__ == '__main__':
    unittest.main()
