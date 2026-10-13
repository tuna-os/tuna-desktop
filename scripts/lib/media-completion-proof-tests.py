#!/usr/bin/env python3
import importlib.machinery
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
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
    def test_actual_posix_shell_owner_selection_requires_one_pid(self):
        proof=(path.parent/'tuna-gtk-shell-proof').read_text()
        start=proof.index('MEDIA_SHELL_PID="$(pgrep -P "$COMPOSITOR_PID" -x tuna-shell-gtk)"')
        end=proof.index('"$PY" - "$MEDIA_SHELL_PID"',start)
        actual=proof[start:end]
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            pgrep=root/'pgrep'
            pgrep.write_text('#!/bin/sh\nprintf "%s" "$PGREP_OUTPUT"\nexit "$PGREP_STATUS"\n')
            pgrep.chmod(0o700)
            command='fail() { exit 77; }; COMPOSITOR_PID=123; '+actual+'\nprintf "%s" "$MEDIA_SHELL_PID"\n'
            for output,status,expected in [('456\n',0,0),('456\n789\n',0,77),('',1,77),('',0,77),('bad',0,77),('0',0,77)]:
                with self.subTest(output=output,status=status):
                    result=subprocess.run(['/bin/sh','-c',command],env={**os.environ,'PATH':str(root)+os.pathsep+os.environ['PATH'],'PGREP_OUTPUT':output,'PGREP_STATUS':str(status)},capture_output=True,text=True,timeout=2)
                    self.assertEqual(result.returncode,expected,result.stderr)
                    if expected==0:self.assertEqual(result.stdout,'456')


if __name__=='__main__':unittest.main()
