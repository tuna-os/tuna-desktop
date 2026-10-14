#!/usr/bin/env python3
"""Pin the DELAY due-poll re-arm shape (journey runs 38489411792/38494022555).

Both misses polled a shell that never left Disabled: the session log
shows no break-state transition for the DELAY enable (contrast the
Active/BreakDue pairs of the BREAK and TAKE gates in the same runs),
and the final poll dump is a quiet 21-node tree (a caught due banner
is 37 nodes). A missed poll therefore means a lost enable, not a
straddled banner fuse, so the poll re-drives the enable once
(disable/re-enable restarts the interval at now) instead of failing.

The Delay measurement is unaffected: the due epoch is recorded when
the banner is caught, in either pass, and the redue poll, the
delay-seconds move assertion and the stale-press check are unchanged.

This test pins that shape in the harness text. It is a tripwire, not a
behavior proof: the live side is the DELAY gate itself in both the
gtk-shell and journey jobs. No cargo, no compositor required.
"""
import os
import unittest
from pathlib import Path

ROOT = Path(os.environ.get('WB_DELAY_POLL_PIN_ROOT', Path(__file__).resolve().parents[2]))
HARNESS = ROOT / 'scripts' / 'tuna-gtk-shell-proof'


class DelayDuePoll(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        harness = HARNESS.read_text()
        start = harness.index('wb_gate_begin G-WELLBEING-ACTION-DELAY')
        end = harness.index('wb_gate_begin G-WELLBEING-ACTION-SKIP')
        cls.delay = harness[start:end]

    def test_single_rearm_reselects(self):
        self.assertIn('could not deselect breaks for re-arm', self.delay)
        self.assertIn('could not re-select the eyesight break', self.delay)
        # Exactly one re-arm: the initial select plus the re-select.
        self.assertEqual(self.delay.count('selected-breaks "[\'eyesight\']"'), 2)

    def test_poll_pass_reused(self):
        self.assertIn('wb_delay_poll_due() {', self.delay)
        # One definition plus the initial call and the re-arm call.
        self.assertEqual(self.delay.count('wb_delay_poll_due'), 3)

    def test_due_epoch_recorded_at_catch(self):
        self.assertIn('delay_due_epoch="$(date +%s)"', self.delay)


if __name__ == '__main__':
    unittest.main()
