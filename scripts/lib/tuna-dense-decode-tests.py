#!/usr/bin/python3
"""Dense diagnostic contracts only. Actual decoder/cgroup observation belongs to CI."""
import copy
import json
from pathlib import Path
import sys
import os
import subprocess
import time
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
sys.dont_write_bytecode=True
import tuna_dense_decode as host
import tuna_dense_decode_worker as worker
import tuna_dense_process as process

class Dense(unittest.TestCase):
 def test_exact_eighteen_gib(self):
  self.assertEqual(worker.VIRTUAL_BYTES,19327352832)
  self.assertEqual(host.VIRTUAL_BYTES,18*1024**3)
 def test_prepull_reserves_tool_but_postpull_does_not_double_charge(self):
  with tempfile.TemporaryDirectory() as d:
   paths={'source':d,'target':d,'graphroot':d}
   post=host.OUTPUT_RESERVE+host.SAFETY_BYTES
   with patch.object(host.os,'statvfs',return_value=SimpleNamespace(f_bavail=post,f_frsize=1)):
    self.assertFalse(host.capacity_observation(paths,before_pull=True)['admitted'])
    after=host.capacity_observation(paths,before_pull=False)
    self.assertTrue(after['admitted']);self.assertFalse(after['existing_tool_charged_again'])
    self.assertEqual(after['required_additional_bytes'],post)
   with patch.object(host.os,'statvfs',return_value=SimpleNamespace(f_bavail=post+host.TOOL_ALLOWANCE,f_frsize=1)):
    self.assertTrue(host.capacity_observation(paths,before_pull=True)['admitted'])
 def test_one_byte_missing_capacity_refused_in_both_phases(self):
  with tempfile.TemporaryDirectory() as d:
   for phase in (True,False):
    reserve=host.OUTPUT_RESERVE+host.SAFETY_BYTES+(host.TOOL_ALLOWANCE if phase else 0)
    with patch.object(host.os,'statvfs',return_value=SimpleNamespace(f_bavail=reserve-1,f_frsize=1)):
     self.assertFalse(host.capacity_observation({'source':d,'target':d,'graphroot':d},before_pull=phase)['admitted'])
 def test_oom_missing_events_wrong_limit_swap_and_over_limit_charge_refused(self):
  before={'max_bytes':worker.MEMORY_BYTES,'swap_max_bytes':0,'peak_bytes':123,'events':{'oom':0,'oom_kill':0}}
  worker.validate_memory(before,before)
  for patcher in (lambda x:x.update(max_bytes=worker.MEMORY_BYTES+1),
                  lambda x:x.update(swap_max_bytes=1),
                  lambda x:x.update(peak_bytes=worker.MEMORY_BYTES+1),
                  lambda x:x['events'].update(oom=1),lambda x:x['events'].update(oom_kill=1),
                  lambda x:x['events'].pop('oom')):
   after=copy.deepcopy(before);patcher(after)
   with self.assertRaises(ValueError):worker.validate_memory(before,after)
 def test_readback_streams_actual_all_bytes_and_deadline_refuses(self):
  with tempfile.TemporaryDirectory() as d:
   p=Path(d)/'bytes';p.write_bytes(b'owned-readback')
   self.assertEqual(worker.digest(p),host.digest(p))
   with self.assertRaises(TimeoutError):worker.digest(p,0)
 def test_capacity_refusal_never_pulls_or_mutates_source_and_writes_receipt(self):
  class Commands:
   def __init__(self):self.log=None;self.calls=[]
   def call(self,args,**kwargs):
    self.calls.append(args)
    if args[:2]==['sudo','python3']:
     return 0,json.dumps({role:{'device':1,'available_bytes':0,'path':str(directory)} for role in ('source','target','graphroot')})
    return 0,str(directory)+'\n' 
  with tempfile.TemporaryDirectory() as directory:
   directory=Path(directory);artifacts=directory/'artifacts';artifacts.mkdir();scratch=directory/'scratch';scratch.mkdir()
   disk=directory/'disk.qcow2';disk.write_bytes(b'unchanged')
   commands=Commands()
   with patch.object(host.os,'statvfs',return_value=SimpleNamespace(f_bavail=0,f_frsize=1)):
    result=host._observe(commands,artifacts,scratch,disk,'a'*64,{'virtual-size':host.VIRTUAL_BYTES,'format':'qcow2'}, {})
   self.assertFalse(result['qualified']);self.assertTrue(result['owned_cleanup'])
   self.assertEqual(disk.read_bytes(),b'unchanged')
   self.assertEqual(len(commands.calls),2);self.assertTrue(all('pull' not in args for args in commands.calls))
   self.assertEqual(json.loads((artifacts/'dense-decode.json').read_text())['status'],'capacity-refused-before-pull')
 def test_production_guard_is_after_observer_and_unchanged(self):
  root=Path(__file__).resolve().parents[2]
  text=(root/'scripts/tuna-benchmark-disk-export').read_text()
  self.assertLess(text.index("receipt['dense_decode'] = dense_decode"),text.index("receipt['storage'] = storage_receipt"))
  self.assertIn("receipt['storage'] = storage_receipt(info, mapping, disk.stat().st_size)",text)
  self.assertIn("require(receipt['dense_decode'].get('owned_cleanup', True)",text)

class ProcessOwnership(unittest.TestCase):
 def test_successful_leader_and_escaped_cleared_env_child_retire(self):
  with tempfile.TemporaryDirectory() as d:
   marker=Path(d)/'child'
   code="import os,time;p=os.fork();\nif p==0:\n os.setsid();os.environ.clear();open(%r,'w').write(str(os.getpid()));time.sleep(60)\nelse:\n while not os.path.exists(%r):time.sleep(.005)\n os._exit(0)"%(str(marker),str(marker))
   receipt=process.bounded_command([sys.executable,'-c',code],Path(d)/'out',time.monotonic()+2)
   self.assertEqual(receipt['exit'],0);self.assertTrue(receipt['owned_cleanup']);self.assertIsNone(receipt['command_error'])
   self.assertTrue(receipt['adopted_descendants_observed']);self.assertFalse(Path('/proc',marker.read_text()).exists())
 def test_timeout_has_actual_cleanup(self):
  with tempfile.TemporaryDirectory() as d:
   receipt=process.bounded_command([sys.executable,'-c','import time;time.sleep(60)'],Path(d)/'out',time.monotonic()+.1)
   self.assertTrue(receipt['owned_cleanup']);self.assertIsNotNone(receipt['command_error'])
 def test_unrelated_child_refuses_before_spawn_and_stays_alive(self):
  child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'])
  try:
   with tempfile.TemporaryDirectory() as d:
    with self.assertRaisesRegex(ValueError,'exclusive'):
     process.bounded_command([sys.executable,'-c','pass'],Path(d)/'out',time.monotonic()+1)
   self.assertIsNone(child.poll())
   value=process.identity(child.pid)
   for owner,start in ((os.getpid()+1,value['start']),(os.getpid(),value['start']+1)):
    with self.assertRaisesRegex(ValueError,'identity mismatch'):process.signal_owned(child.pid,owner,start)
   self.assertIsNone(child.poll())
  finally:child.kill();child.wait(timeout=2)
if __name__=='__main__':unittest.main()
