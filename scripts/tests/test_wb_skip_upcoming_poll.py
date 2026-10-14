#!/usr/bin/env python3
"""Pin the SKIP upcoming-poll robustness shape (run 38475780955 journey).

The upcoming notice is one-shot per (type, next-due) with a 4 s banner fuse
(`BreakDispatcher.upcoming_fired` in crates/shell-gtk/src/wellbeing.rs). The
installed-package journey run missed the post with 14 dumps over 33 s: a
slow or failed dump iteration opens a gap wider than the fuse and the poll's
`|| continue` silences it, so the poll must be gap-proof by construction:

* a warm-up dump before enabling (no AT-SPI activation latency on the
  first poll dump),
* tight poll cadence (sub-second sleeps, so the fuse falls between no two
  dumps),
* one disable/re-enable re-arm restarting the enable epoch when the first
  poll misses (the post is deterministic ~10 s after a fresh enable, so a
  miss means a straddled fuse, not a missing post; the Skip press still
  retires the upcoming notice with the due schedule undisturbed).

This test pins that shape in the harness text. It is a tripwire, not a
behavior proof: the live side is the SKIP gate itself in both the gtk-shell
and journey jobs. No cargo, no compositor required.
"""
import os
import re
import unittest
from pathlib import Path

ROOT = Path(os.environ.get('WB_SKIP_POLL_PIN_ROOT', Path(__file__).resolve().parents[2]))
HARNESS = ROOT / 'scripts' / 'tuna-gtk-shell-proof'


class SkipUpcomingPoll(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        harness = HARNESS.read_text()
        start = harness.index('wb_gate_begin G-WELLBEING-ACTION-SKIP')
        end = harness.index('wb_gate_begin G-WELLBEING-NEGATIVE')
        cls.skip = harness[start:end]

    def test_warm_dump_before_enable(self):
        warm = self.skip.index('a11y-wb-skip-warm.json')
        select = self.skip.index('selected-breaks "[\'eyesight\']"')
        self.assertLess(warm, select,
                        'the warm-up dump must precede the break enable')

    def test_tight_poll_cadence(self):
        fn = self.skip.index('wb_skip_poll_upcoming() {')
        body = self.skip[fn:self.skip.index('\n}\n', fn)]
        sleeps = re.findall(r'sleep ([0-9.]+)', body)
        self.assertTrue(sleeps, 'the upcoming poll must pace keep-alive with sleeps')
        for nap in sleeps:
            self.assertLess(float(nap), 1.0,
                            f'poll sleep {nap}s leaves a fuse-straddling gap')

    def test_single_rearm_restarts_epoch(self):
        self.assertIn('could not deselect breaks for re-arm', self.skip)
        rearm = self.skip.index('could not deselect breaks for re-arm')
        # The enable epoch restarts after the re-select so skip-undisturbed
        # still measures the Skip press against the live schedule.
        epoch = self.skip.index('skip_enable_epoch="$(date +%s)"', rearm)
        due = self.skip.index('skip_orig_due_epoch=$((skip_enable_epoch + 130))', epoch)
        self.assertLess(epoch, due)
        # Exactly one re-arm: two enable-epoch assignments in the gate.
        self.assertEqual(self.skip.count('skip_enable_epoch="$(date +%s)"'), 2)


if __name__ == '__main__':
    unittest.main()
