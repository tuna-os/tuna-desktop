#!/usr/bin/env python3
import importlib.machinery
import importlib.util
from pathlib import Path
import unittest

path=Path(__file__).resolve().parents[1]/'tuna-media-completion-proof'
loader=importlib.machinery.SourceFileLoader('media_wrapper',str(path))
spec=importlib.util.spec_from_loader(loader.name,loader)
wrapper=importlib.util.module_from_spec(spec);loader.exec_module(wrapper)
OK='running 1 test\nMEDIA-COMPLETION pass old=0.4800 guarded=0.4200 callback-scheduler failure timeout\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;\n'


class Verdict(unittest.TestCase):
    def test_explicit_actual_filter_receipt(self):self.assertTrue(wrapper.verdict(0,True,OK))
    def test_nonzero_and_watchdog(self):
        for status in [1,23,124,-9]:self.assertFalse(wrapper.verdict(status,True,OK))
    def test_binary_changed(self):self.assertFalse(wrapper.verdict(0,False,OK))
    def test_missing_marker(self):self.assertFalse(wrapper.verdict(0,True,OK.replace('MEDIA-COMPLETION pass','wrong-marker')))
    def test_zero_ran(self):self.assertFalse(wrapper.verdict(0,True,OK.replace('running 1 test','running 0 tests')))
    def test_ignored(self):self.assertFalse(wrapper.verdict(0,True,OK.replace('0 ignored','1 ignored')))
    def test_test_failed(self):self.assertFalse(wrapper.verdict(0,True,OK.replace('0 failed','1 failed')))
    def test_wrong_fixture_values(self):self.assertFalse(wrapper.verdict(0,True,OK.replace('old=0.4800','old=0.4200')))
    def test_wrong_filter_is_not_public_filter(self):self.assertEqual(wrapper.FILTER,'media_keys::completion_tests::actual_media_keys_clamp_key_completion_barrier')


if __name__=='__main__':unittest.main()
