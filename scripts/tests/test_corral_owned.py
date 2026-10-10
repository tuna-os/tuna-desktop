import json,pathlib,subprocess,sys,tempfile,unittest
from unittest.mock import patch
sys.path.insert(0,str(pathlib.Path(__file__).resolve().parents[1]/'lib'))
import corral_owned as m
class Tests(unittest.TestCase):
 def test_delete_uid_in_server_body(self):
  with patch.object(m.subprocess,'run',return_value=subprocess.CompletedProcess([],0)) as run:
   m.delete('ctx','ns','pvc','disk','original')
  args,kw=run.call_args;self.assertIn('--raw',args[0]);self.assertEqual(json.loads(kw['input'])['preconditions'],{'uid':'original'})
 def test_conflict_replacement_not_deleted(self):
  with patch.object(m.subprocess,'run',return_value=subprocess.CompletedProcess([],1,stderr=b'Conflict')),patch.object(m,'get',return_value={'metadata':{'uid':'replacement'}}):
   with self.assertRaises(RuntimeError):m.delete('ctx','ns','vm','vm','old')
 def test_absent_delete_idempotent(self):
  with patch.object(m.subprocess,'run',return_value=subprocess.CompletedProcess([],1,stderr=b'NotFound')),patch.object(m,'get',return_value=None):m.delete('ctx','ns','dv','disk','old')
 def test_ambiguous_creation_recovers_own_nonce(self):
  registered=[];obj={'metadata':{'name':'new'}}
  with tempfile.TemporaryDirectory() as d,patch.object(m.subprocess,'check_output',side_effect=subprocess.TimeoutExpired('create',45)),patch.object(m,'get',return_value={'metadata':{'uid':'committed','labels':{'tuna-perf-nonce':'nonce'}}}):
   with self.assertRaises(subprocess.TimeoutExpired):m.create('ctx','ns','vm',obj,pathlib.Path(d)/'create.json','nonce',lambda *x:registered.append(x))
  self.assertEqual(registered,[('vm','new','committed')])
 def test_ambiguous_creation_foreign_not_registered(self):
  registered=[]
  with tempfile.TemporaryDirectory() as d,patch.object(m.subprocess,'check_output',side_effect=ValueError('invalid JSON')),patch.object(m,'get',return_value={'metadata':{'uid':'other','labels':{}}}):
   with self.assertRaises(ValueError):m.create('ctx','ns','vm',{'metadata':{'name':'new'}},pathlib.Path(d)/'c','nonce',lambda *x:registered.append(x))
  self.assertEqual(registered,[])
 def test_cleanup_missing_dv_continues_to_vm(self):
  with patch.object(m,'delete') as delete,patch.object(m.subprocess,'check_output',return_value=b'{"items":[]}'):
   result=m.cleanup('ctx','ns',[('vm','new','v'),('dv','disk','d')])
  self.assertEqual(delete.call_count,2);self.assertTrue(result['all_owned_resources_absent'])
 def test_cleanup_pending_is_not_success(self):
  with patch.object(m,'delete'),patch.object(m.time,'monotonic',side_effect=[0,0,2]),patch.object(m.time,'sleep'),patch.object(m.subprocess,'check_output',return_value=b'{"items":[{"metadata":{"name":"scratch","uid":"s","ownerReferences":[{"uid":"d"}]}}]}'):
   with self.assertRaises(TimeoutError):m.cleanup('ctx','ns',[('dv','disk','d')],timeout=1)
if __name__=='__main__':unittest.main()
