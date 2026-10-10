import io,pathlib,sys,unittest,zipfile
sys.path.insert(0,str(pathlib.Path(__file__).resolve().parents[1]/'lib'))
import corral_artifact as m
class Tests(unittest.TestCase):
 def run_receipt(self):return {'status':'completed','conclusion':'success','event':'workflow_dispatch','head_sha':'a'*40,'head_repository':{'full_name':m.REPO},'repository':{'full_name':m.REPO},'path':'.github/workflows/performance-baseline.yml'}
 def test_upload_attempt_not_zero(self):
  scripts=pathlib.Path(__file__).resolve().parents[1]
  for filename in ('tuna-corral-benchmark-import','tuna-corral-raw-upload-probe'):
   source=(scripts/filename).read_text();self.assertIn("'--retry','1'",source);self.assertNotIn("'--retry','0'",source)
 def test_original_run(self):m.original_run(self.run_receipt(),'a'*40,'.github/workflows/performance-baseline.yml')
 def test_wrong_identity_fails(self):
  for field,value in [('conclusion','failure'),('event','push'),('head_sha','b'*40),('path','.github/workflows/ci.yml'),('head_repository',{'full_name':'someone/fork'})]:
   run=self.run_receipt();run[field]=value
   with self.assertRaises(ValueError):m.original_run(run,'a'*40,'.github/workflows/performance-baseline.yml')
 def test_full_scratch_plan(self):
  manifest={'storage':{'virtual_bytes':18*m.GIB,'transfer_bytes':2*m.GIB},'requested_pvc':'20Gi','firmware':'uefi'}
  plan=m.disk_plan(manifest);self.assertEqual(plan['target_pvc_bytes'],20*m.GIB);self.assertEqual(plan['scratch_pvc_bytes'],20*m.GIB);self.assertGreater(plan['estimated_peak_bytes'],40*m.GIB)
  with self.assertRaises(ValueError):m.disk_plan(manifest,method='unproven-raw')
 def test_virtual_overhead(self):
  with self.assertRaises(ValueError):m.disk_plan({'storage':{'virtual_bytes':20*m.GIB,'transfer_bytes':m.GIB},'requested_pvc':'20Gi','firmware':'uefi'})
 def archive(self,names):
  stream=io.BytesIO()
  with zipfile.ZipFile(stream,'w') as z:
   for name in names:z.writestr(name,b'content')
  stream.seek(0);return zipfile.ZipFile(stream)
 def test_safe_archive(self):
  with self.archive(['manifest.json','disk.qcow2']) as z:self.assertEqual(len(m.safe_members(z)[0]),2)
 def test_archive_traversal(self):
  for name in ['../disk.qcow2','/disk.qcow2','x\\disk.qcow2']:
   with self.archive(['manifest.json',name]) as z:
    with self.assertRaises(ValueError):m.safe_members(z)
 def test_missing_artifact(self):
  with self.archive(['manifest.json']) as z:
   with self.assertRaises(ValueError):m.safe_members(z)
if __name__=='__main__':unittest.main()
