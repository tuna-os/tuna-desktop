#!/usr/bin/python3
"""Actual owned child retirement; no GUI or compositor execution."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location('life',Path(__file__).resolve().parents[1]/'lib/auth_proof_process.py')
life=importlib.util.module_from_spec(spec);spec.loader.exec_module(life)

class Lifetime(unittest.TestCase):
 def test_successful_leader_retires_cleared_environment_setsid_descendant(self):
  with tempfile.TemporaryDirectory() as d:
   marker=Path(d)/'child'
   code="import os,time; p=os.fork();\nif p==0:\n os.setsid();os.environ.clear();open(%r,'w').write(str(os.getpid()));time.sleep(60)\nelse:\n while not os.path.exists(%r):time.sleep(.005)\n os._exit(0)"%(str(marker),str(marker))
   receipt=life.bounded_command([sys.executable,'-c',code],Path(d)/'out',time.monotonic()+3,env={})
   self.assertEqual(receipt['exit'],0);self.assertTrue(receipt['owned_cleanup']);self.assertIsNone(receipt['command_error'])
   self.assertTrue(receipt['adopted_descendants_observed']);self.assertFalse(Path('/proc',marker.read_text()).exists())
 def test_timeout_retires_owned_process(self):
  with tempfile.TemporaryDirectory() as d:
   receipt=life.bounded_command([sys.executable,'-c','import time;time.sleep(60)'],Path(d)/'out',time.monotonic()+.15)
   self.assertEqual(receipt['exit'],124);self.assertTrue(receipt['owned_cleanup']);self.assertIsNotNone(receipt['command_error'])
 def test_unrelated_child_refuses_before_spawn_and_preserves_child(self):
  child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'])
  try:
   with tempfile.TemporaryDirectory() as d:
    with self.assertRaisesRegex(ValueError,'exclusive'):
     life.bounded_command([sys.executable,'-c','pass'],Path(d)/'out',time.monotonic()+1)
   self.assertIsNone(child.poll())
  finally:child.kill();child.wait(timeout=2)
if __name__=='__main__':unittest.main()
