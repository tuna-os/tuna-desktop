#!/usr/bin/python3
import importlib.util
from pathlib import Path
import unittest
spec=importlib.util.spec_from_file_location('xkeycode',Path(__file__).with_name('xkeycode-tap.py'))
module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)

class Arguments(unittest.TestCase):
    def test_existing_keysym_tap_unchanged(self):
        self.assertEqual(module.arguments([':96','ffbe']),(':96',None,0xffbe,'tap'))
    def test_physical_press_release_probe_never_resolve_a_keysym(self):
        for action in ('tap','press','release','probe'):
            self.assertEqual(module.arguments([':96','--physical=37',action]),(':96',37,None,action))
    def test_invalid_codes_symbols_and_actions_refused(self):
        for values in [[':96','--physical=7','tap'],[':96','--physical=256','tap'],
                       [':96','--physical=invalid','tap'],[':96','--physical=37','unknown'],
                       [':96','bad-symbol'],[':96','0'],[':96','100000000'],[]]:
            with self.subTest(values=values),self.assertRaises(ValueError): module.arguments(values)
if __name__=='__main__':unittest.main()
