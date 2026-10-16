#!/usr/bin/env python3
"""State->button mapping pin for wellbeing break notices.

GNOME Shell 51.0 is the contract authority (js/misc/breakManager.js at
peeled tag commit 2177bdf9624b2d285de7c1d34274073d3769d6b8,
https://github.com/GNOME/gnome-shell/blob/2177bdf9624b2d285de7c1d34274073d3769d6b8/js/misc/breakManager.js):
the upcoming notice carries Delay+Skip without Take (lines 985-1023);
due/overdue carry Delay+Take without Skip (lines 1133-1162);
in-progress and interrupted carry no action buttons.

This test pins the product's notice constructors to that exact mapping by
reading their allow-flag triples. It is a tripwire, not a behavior proof:
if a constructor gains or loses a button (e.g. Skip on the due notice),
this fails hermetically with no compositor. The live side is pinned by
the `notice-buttons` helper wired into the SKIP gate (upcoming and due
banners). No cargo, no compositor required.
"""
import os
import re
import unittest
from pathlib import Path

ROOT = Path(os.environ.get('WB_MAPPING_PIN_ROOT', Path(__file__).resolve().parents[2]))
WELLBEING = ROOT / 'crates' / 'shell-gtk' / 'src' / 'wellbeing.rs'

GNOME_URL = ('https://github.com/GNOME/gnome-shell/blob/'
             '2177bdf9624b2d285de7c1d34274073d3769d6b8/js/misc/breakManager.js')

# constructor -> (allow_delay, allow_skip, allow_take, GNOME lines).
EXPECTED = {
    'upcoming_notice': (True, True, False, '985-1023'),
    'due_notice': (True, False, True, '1133-1162'),
    'overdue_notice': (True, False, True, '1133-1162'),
    'in_progress_notice': (False, False, False, 'in-progress: no buttons'),
    'interrupted_notice': (False, False, False, 'interrupted: no buttons'),
}


def constructor_flags(source, name):
    """Read the (allow_delay, allow_skip, allow_take) triple passed positionally
    after `urgent` in the constructor's break_notice(...) call."""
    start = source.index(f'pub fn {name}(')
    # The next top-level item starts the following constructor or section.
    rest = source[start:]
    end = rest.index('\n}\n', rest.index('break_notice('))
    body = rest[:end]
    call = body.index('break_notice(')
    tail = body[call:]
    match = re.search(
        r'urgent\s*,\s*(true|false)\s*,\s*(true|false)\s*,\s*(true|false)',
        tail)
    if match is None:
        raise AssertionError(
            f'{name}: break_notice call shape changed; re-pin by hand')
    return tuple(flag == 'true' for flag in match.groups())


class NoticeButtonMapping(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.source = WELLBEING.read_text()

    def test_each_constructor_matches_gnome_mapping(self):
        for name, (delay, skip, take, gnome_lines) in EXPECTED.items():
            with self.subTest(constructor=name):
                self.assertEqual(
                    constructor_flags(self.source, name),
                    (delay, skip, take),
                    f'{name}: (allow_delay, allow_skip, allow_take) drifted '
                    f'from GNOME 51 ({GNOME_URL} lines {gnome_lines})')

    def test_action_labels_stay_header_case(self):
        for key, label in (('take', 'Take'), ('delay', 'Delay'),
                           ('skip', 'Skip')):
            with self.subTest(key=key):
                self.assertIn(f'("{key}", "{label}")', self.source,
                              f'ACTION_{key.upper()} label drifted')


if __name__ == '__main__':
    unittest.main()
