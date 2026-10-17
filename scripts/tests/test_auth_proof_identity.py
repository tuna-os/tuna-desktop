#!/usr/bin/python3
"""Production API identity matcher negatives; no network required."""
import copy
import json
import importlib.util
from pathlib import Path
import sys
import unittest
sys.dont_write_bytecode=True
spec=importlib.util.spec_from_file_location('identity',Path(__file__).resolve().parents[1]/'lib/auth_proof_identity.py')
i=importlib.util.module_from_spec(spec);spec.loader.exec_module(i)
class Identity(unittest.TestCase):
 def fixtures(self):
  run=dict(id=17,run_attempt=2,repository={'full_name':i.REPO},head_sha='a'*40,path=i.WORKFLOW,event='pull_request',status='in_progress',conclusion=None)
  jobs=[dict(id=1,name='gtk-shell',status='in_progress',conclusion=None,head_sha='a'*40,run_id=17,run_attempt=2),dict(id=2,name='check',status='completed',conclusion='success',head_sha='a'*40,run_id=17,run_attempt=2)]
  return run,jobs,dict(sha='a'*40,tree={'sha':'b'*40})
 def validate(self,run,jobs,commit):return i.validate_identity(run,jobs,commit,'a'*40,17,2,'pull_request')
 def test_exact_pr_head_with_green_same_attempt(self):self.assertEqual(self.validate(*self.fixtures())['tree'],'b'*40)
 def test_wrong_run_head_attempt_event_path_repository_or_tree_source(self):
  for key,value in [('id',18),('head_sha','c'*40),('run_attempt',3),('event','push'),('path','other'),('repository',{'full_name':'other'})]:
   with self.subTest(key=key):
    r,j,c=self.fixtures();r[key]=value
    with self.assertRaises(ValueError):self.validate(r,j,c)
  r,j,c=self.fixtures();c['sha']='c'*40
  with self.assertRaises(ValueError):self.validate(r,j,c)
 def test_missing_duplicate_red_stale_prerequisite_or_current(self):
  for index in (0,1):
   for key,value in [('head_sha','c'*40),('run_attempt',1),('run_id',19)]:
    r,j,c=self.fixtures();j[index][key]=value
    with self.assertRaises(ValueError):self.validate(r,j,c)
  for mode in ('missing','duplicate','red'):
   r,j,c=self.fixtures()
   if mode=='missing':j.pop()
   elif mode=='duplicate':j.append(copy.deepcopy(j[1]))
   else:j[1]['conclusion']='failure'
   with self.assertRaises(ValueError):self.validate(r,j,c)
 def test_queued_run_requires_exact_running_job_and_green_check(self):
  r,j,c=self.fixtures();r['status']='queued'
  self.assertEqual(self.validate(r,j,c)['job_id'],1)
  for index,key,value in [(0,'status','queued'),(0,'status','completed'),(0,'conclusion','success'),(1,'status','queued'),(1,'conclusion',None),(1,'conclusion','failure')]:
   with self.subTest(index=index,key=key,value=value):
    jobs=copy.deepcopy(j);jobs[index][key]=value
    with self.assertRaises(ValueError):self.validate(r,jobs,c)
  for status in ('completed','waiting','pending','requested',None):
   with self.subTest(status=status),self.assertRaises(ValueError):self.validate(dict(r,status=status),j,c)
 def test_mismatch_preserves_observed_public_fields_without_extra_payload(self):
  r,j,c=self.fixtures();r.update(status='completed',private_payload='excluded fixture value')
  with self.assertRaises(ValueError) as error:self.validate(r,j,c)
  receipt=json.loads(str(error.exception).split(': ',1)[1])
  self.assertEqual(receipt['expected']['status'],['in_progress','queued'])
  self.assertEqual(receipt['observed']['status'],'completed')
  self.assertEqual(receipt['observed']['commit_sha'],'a'*40)
  self.assertNotIn('private_payload',receipt['observed'])
 def test_schedule_event_admitted(self):
  r,j,c=self.fixtures()
  r['event']='schedule'
  self.assertEqual(i.validate_identity(r,j,c,'a'*40,17,2,'schedule')['tree'],'b'*40)
 def test_unsupported_event_rejected(self):
  r,j,c=self.fixtures()
  r['event']='fork'
  with self.assertRaises(ValueError):i.validate_identity(r,j,c,'a'*40,17,2,'fork')
if __name__=='__main__':unittest.main()
