import hashlib,json,os,pathlib,subprocess,sys,tempfile,unittest
from unittest.mock import patch
sys.path.insert(0,str(pathlib.Path(__file__).resolve().parents[1]/'lib'))
import corral_ssh as m
class Tests(unittest.TestCase):
 def test_private_context_projection_replaces_ambient_and_is_deleted(self):
  config={'current-context':'selected','contexts':[{'name':'selected','context':{'cluster':'c'}}],'clusters':[{'name':'c','cluster':{'server':'https://example.invalid'}}],'users':[{'name':'u','user':{'token':'synthetic-private-test-token'}}]}
  for fails in (False,True):
   with patch.object(m.subprocess,'check_output',return_value=json.dumps(config).encode()) as call,patch.dict(os.environ,{'KUBECONFIG':'ambient-other-context'}):
    try:
     with m.projected_kubeconfig('selected') as env:
      path=pathlib.Path(env['KUBECONFIG']);self.assertNotEqual(str(path),os.environ['KUBECONFIG']);self.assertEqual(path.stat().st_mode&0o777,0o600);self.assertEqual(json.loads(path.read_text())['current-context'],'selected')
      self.assertEqual(path.parent.stat().st_mode&0o777,0o700)
      if fails:raise RuntimeError('controlled SSH failure')
    except RuntimeError:
     if not fails:raise
    self.assertFalse(path.exists());self.assertEqual(os.environ['KUBECONFIG'],'ambient-other-context')
   self.assertIn('selected',call.call_args.args[0])
 def test_projection_rejects_other_context(self):
  with patch.object(m.subprocess,'check_output',return_value=b'{"current-context":"ambient","contexts":[],"clusters":[]}'):
   with self.assertRaises(ValueError):
    with m.projected_kubeconfig('selected'):self.fail('must refuse')
 def setup(self,d):
  key=pathlib.Path(d)/'key';subprocess.run(['ssh-keygen','-q','-t','ed25519','-N','','-f',str(key)],check=True)
  pub=pathlib.Path(str(key)+'.pub');fp=subprocess.check_output(['ssh-keygen','-lf',str(pub)]).decode().split()[1]
  manifest={'ssh':{'user':'root','password_authentication':False,'authorized_keys_path':'/etc/ssh/authorized_keys/root','public_key_sha256':hashlib.sha256(pub.read_bytes()).hexdigest(),'public_key_fingerprint':fp}}
  return key,pub,fp,manifest
 def test_actual_key_pair_binds_original_export(self):
  with tempfile.TemporaryDirectory() as d:
   key,pub,fp,manifest=self.setup(d);r=m.client_key(manifest,key,pub,'root');self.assertEqual(r['public_key_fingerprint'],fp);self.assertFalse(r['private_key_recorded'])
 def test_wrong_public_bytes_or_user_refused(self):
  with tempfile.TemporaryDirectory() as d:
   key,pub,fp,manifest=self.setup(d)
   with self.assertRaises(ValueError):m.client_key(manifest,key,pub,'other')
   pub.write_text(pub.read_text()+'\n')
   with self.assertRaises(ValueError):m.client_key(manifest,key,pub,'root')
 def test_host_key_is_measured_and_matches_known_hosts(self):
  with tempfile.TemporaryDirectory() as d:
   key,pub,fp,manifest=self.setup(d);known=pathlib.Path(d)/'known';known.write_text('vm.bench.ns '+pub.read_text())
   value={'boot_id':'boot','keys':[{'path':'/etc/ssh/ssh_host_ed25519_key.pub','fingerprint':fp}]}
   receipt=m.host_receipt(value,known,'boot');self.assertTrue(receipt['measured_after_boot']);self.assertEqual(receipt['matched_known_hosts_fingerprints'],[fp])
   with self.assertRaises(ValueError):m.host_receipt(value,known,'another-boot')
   value['keys'][0]['fingerprint']='SHA256:'+'a'*43
   with self.assertRaises(ValueError):m.host_receipt(value,known,'boot')
 def test_password_export_refused(self):
  with self.assertRaises(ValueError):m.export_policy({'ssh':{'password_authentication':True}})
if __name__=='__main__':unittest.main()
