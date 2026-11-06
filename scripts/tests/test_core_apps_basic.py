"""Host font-metadata matching regressions; no VM qualification."""
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts' / 'lib'))
import core_apps_basic


def label(path, ancestors, bounds):
    return {'role': 'label', 'text': 'Noto Sans CJK JP', 'showing': True,
            'bounds': bounds, 'path': path,
            'ancestors': [{'role': role} for role in ancestors]}


class FontMetadataLabel(unittest.TestCase):
    def details(self, *labels):
        frame = {'role': 'frame', 'showing': True, 'bounds': [0, 0, 1200, 800],
                 'path': [0], 'name': 'Fonts'}
        return {'nodes': [frame, *labels]}

    def test_metadata_value_beats_header_title(self):
        value = label([0, 0, 1], ('application', 'frame', 'panel', 'list', 'list item', 'panel'),
                      [100, 100, 200, 30])
        title = label([0, 0, 0], ('application', 'frame', 'panel', 'panel', 'grouping'),
                      [100, 10, 200, 30])
        self.assertIs(core_apps_basic.metadata_label(self.details(value, title), 'Noto Sans CJK JP'), value)

    def test_title_alone_is_not_metadata(self):
        title = label([0, 0, 0], ('application', 'frame', 'panel', 'panel', 'grouping'),
                      [100, 10, 200, 30])
        with self.assertRaises(RuntimeError):
            core_apps_basic.metadata_label(self.details(title), 'Noto Sans CJK JP')


class FormattedArithmeticResult(unittest.TestCase):
    def test_space_grouped_result_matches(self):
        self.assertTrue(core_apps_basic.formatted_result_matches('56 088', 56088, True))

    def test_comma_grouped_result_matches(self):
        self.assertTrue(core_apps_basic.formatted_result_matches('56,088', 56088, True))

    def test_ungrouped_result_rejected_when_separators_desired(self):
        self.assertFalse(core_apps_basic.formatted_result_matches('56088', 56088, True))

    def test_plain_result_matches_when_separators_off(self):
        self.assertTrue(core_apps_basic.formatted_result_matches('56088', 56088, False))

    def test_grouped_result_rejected_when_separators_off(self):
        self.assertFalse(core_apps_basic.formatted_result_matches('56 088', 56088, False))

    def test_wrong_digits_rejected(self):
        self.assertFalse(core_apps_basic.formatted_result_matches('56 089', 56088, True))

    def test_small_result_needs_no_separator(self):
        self.assertTrue(core_apps_basic.formatted_result_matches('703', 703, True))

    def test_raw_digit_clipboard_matches(self):
        self.assertTrue(core_apps_basic.clipboard_result_matches('56088', 56088))

    def test_grouped_clipboard_matches(self):
        self.assertTrue(core_apps_basic.clipboard_result_matches('56 088', 56088))

    def test_wrong_digit_clipboard_rejected(self):
        self.assertFalse(core_apps_basic.clipboard_result_matches('56089', 56088))


if __name__ == '__main__':
    unittest.main()
