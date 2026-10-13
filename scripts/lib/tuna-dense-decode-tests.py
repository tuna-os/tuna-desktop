#!/usr/bin/python3
"""Dense diagnostic contracts only. Actual decoder/cgroup observation belongs to CI."""
import copy
import ast
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
   def __init__(self):self.log=None;self.calls=[];self.process_cleanup_uncertain=False
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
  compile(text,'actual-exporter','exec')
  self.assertLess(text.index("receipt['dense_decode'] = dense_decode"),text.index("receipt['storage'] = storage_receipt"))
  self.assertIn("receipt['storage'] = storage_receipt(info, mapping, disk.stat().st_size)",text)
  self.assertIn("require(receipt['dense_decode'].get('owned_cleanup', True)",text)

class ReceiptOwnership(unittest.TestCase):
 def setUp(self):
  self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
  self.base=Path(self.temp.name);self.artifacts=self.base/'artifacts';self.artifacts.mkdir();self.scratch=self.base/'scratch';self.scratch.mkdir()
  self.disk=self.artifacts/'disk.qcow2';self.disk.write_bytes(b'original')
  self.legacy=SimpleNamespace(process_cleanup_uncertain=False)
  self.source={'candidate_source_commit':'a'*40}
 def matching_receipt(self):
  return {'schema':1,'qualified':False,'owned_cleanup':True,'tool':host.TOOL,
          'root_request_sha256':host.digest(self.artifacts/'dense-root-request.json'),
          'source_provenance':self.source,'export_admission_changed':False,'node_or_vm_qualified':False}
 def invoke(self,launcher,receipt=None,mutate=None):
  def launch(*args,**kwargs):
   self.assertTrue(self.legacy.process_cleanup_uncertain,'latch precedes launcher')
   if isinstance(launcher,Exception):raise launcher
   if receipt=='matching':
    value=self.matching_receipt()
    if mutate:mutate(value)
    (self.artifacts/'dense-decode.json').write_text(json.dumps(value))
   elif receipt is not None:(self.artifacts/'dense-decode.json').write_text(receipt)
   return launcher
  with patch.object(host,'bounded_command',side_effect=launch):
   return host.observe(self.legacy,self.artifacts,self.scratch,self.disk,'b'*64,{},self.source)
 def exporter_discard(self):
  # Execute the actual production cleanup predicate without main/processes.
  path=Path(__file__).resolve().parents[1]/'tuna-benchmark-disk-export'
  tree=ast.parse(path.read_text());function=next(n for n in tree.body if isinstance(n,ast.FunctionDef) and n.name=='discard_failed_disk')
  namespace={};exec(compile(ast.Module(body=[function],type_ignores=[]),str(path),'exec'),namespace)
  namespace['discard_failed_disk'](self.legacy,self.disk)
 def test_confirmed_matching_receipt_clears_only_own_uncertainty(self):
  result=self.invoke({'owned_cleanup':True,'command_error':None,'exit':0},'matching')
  self.assertTrue(result['owned_cleanup']);self.assertFalse(self.legacy.process_cleanup_uncertain)
  self.assertFalse(result['qualified'],'capacity refusal may still be safely retired')
 def test_preexisting_uncertainty_cannot_be_cleared(self):
  self.legacy.process_cleanup_uncertain=True
  self.invoke({'owned_cleanup':True,'command_error':None,'exit':0},'matching')
  self.assertTrue(self.legacy.process_cleanup_uncertain);self.exporter_discard();self.assertEqual(self.disk.read_bytes(),b'original')
 def test_malformed_truncated_missing_and_stale_receipts_retain_backing(self):
  for receipt in ('{','[]',None,json.dumps({'schema':1,'qualified':True,'owned_cleanup':True,'root_request_sha256':'0'*64})):
   with self.subTest(receipt=receipt):
    self.setUp()
    try:self.invoke({'owned_cleanup':True,'command_error':None,'exit':0},receipt)
    except (ValueError,TypeError):pass
    self.assertTrue(self.legacy.process_cleanup_uncertain);self.exporter_discard();self.assertEqual(self.disk.read_bytes(),b'original')
 def test_launcher_exception_nonzero_or_unconfirmed_retirement_retain_backing(self):
  for launcher in (RuntimeError('lost acknowledgement'),{'owned_cleanup':False,'command_error':None,'exit':0},
                   {'owned_cleanup':True,'command_error':'timeout','exit':0},{'owned_cleanup':True,'command_error':None,'exit':1}):
   with self.subTest(launcher=launcher):
    self.setUp()
    try:self.invoke(launcher,'matching')
    except RuntimeError:pass
    self.assertTrue(self.legacy.process_cleanup_uncertain);self.exporter_discard();self.assertEqual(self.disk.read_bytes(),b'original')
 def test_altered_source_request_tool_or_cleanup_receipt_is_refused(self):
  for key,value in [('source_provenance',{}),('root_request_sha256','0'*64),('tool','other'),('owned_cleanup',1),('export_admission_changed',True)]:
   with self.subTest(key=key):
    self.setUp()
    with self.assertRaises(ValueError):self.invoke({'owned_cleanup':True,'command_error':None,'exit':0},'matching',lambda r:r.update({key:value}))
    self.assertTrue(self.legacy.process_cleanup_uncertain)
 def test_outer_receipt_write_failure_preserves_uncertainty(self):
  with patch.object(host,'write_json',side_effect=OSError('write refused')):
   with self.assertRaises(OSError):self.invoke({'owned_cleanup':True,'command_error':None,'exit':0},'matching')
  self.assertTrue(self.legacy.process_cleanup_uncertain);self.exporter_discard();self.assertEqual(self.disk.read_bytes(),b'original')
 def test_precontainer_command_uncertainty_cannot_claim_cleanup(self):
  class Commands:
   log=None;process_cleanup_uncertain=False;events=[]
   def call(self,*args,**kwargs):
    self.process_cleanup_uncertain=True
    raise RuntimeError('podman info owner retirement unconfirmed')
  result=host._observe(Commands(),self.artifacts,self.scratch,self.disk,'b'*64,{'virtual-size':host.VIRTUAL_BYTES,'format':'qcow2'},self.source)
  self.assertFalse(result['owned_cleanup']);self.assertEqual(self.disk.read_bytes(),b'original')
 def test_actual_exporter_deletion_allowed_only_without_uncertainty(self):
  self.legacy.process_cleanup_uncertain=True;self.exporter_discard();self.assertTrue(self.disk.exists())
  self.legacy.process_cleanup_uncertain=False;self.exporter_discard();self.assertFalse(self.disk.exists())

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
