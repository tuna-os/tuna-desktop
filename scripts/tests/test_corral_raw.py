import hashlib,json,pathlib,sys,tempfile,unittest
from unittest.mock import patch
sys.path.insert(0,str(pathlib.Path(__file__).resolve().parents[1]/'lib'))
import corral_raw as m
import corral_artifact
class Tests(unittest.TestCase):
 def setup(self,d):
  p=pathlib.Path(d);(p/'source').mkdir();helpers={}
  for name in ('corral_session.py','corral_owned.py','corral_perf.py'):
   (p/'source'/name).write_bytes(b'helper');helpers[name]=hashlib.sha256(b'helper').hexdigest()
  (p/'source/tuna-corral-raw-upload-probe').write_bytes(b'probe');(p/'client').write_bytes(b'client')
  data={'setup':'#!/bin/sh\nset -eu\nmkdir -m 0777 -p "$VOL_DIR"'}
  r={'source_sha256':hashlib.sha256(b'probe').hexdigest(),'context':'ctx','completed':True,'cleanup':{'all_owned_resources_absent':True},'helper_sha256':helpers,'node_uid':'n','pvc_uid':'p','raw_sha256':'x','readback':{'matched':True,'prefix_bytes':64*1024*1024,'sha256':'x','node':'node','pvc_uid':'p'},'virtctl_sha256':hashlib.sha256(b'client').hexdigest(),'local_path_config_sha256':hashlib.sha256(json.dumps(data,sort_keys=True).encode()).hexdigest()}
  observations=[{'phase':'Running','directory_usage_returncode':0,'node':'node','directory_usage':'0\t/scratch\n4\t/data','container_statuses':[{'imageID':m.UPLOAD_IMAGE}]}]
  (p/'receipt.json').write_text(json.dumps(r));(p/'upload-observations.json').write_text(json.dumps(observations))
  results=[{'metadata':{'uid':'n'}},{'data':data},{'items':[{'status':{k:'v1.66.1' for k in ('targetVersion','observedVersion','operatorVersion')}}]}]
  return p,r,observations,results
 def verify(self,p,r,results):
  with patch.object(m,'PROBE_SHA',r['source_sha256']),patch.object(m,'kubectl',side_effect=results),patch.object(m.shutil,'which',return_value=str(p/'client')):return m.proof(p,'ctx','node',m.evidence_sha(p))
 def test_exact_backend_accepts_only_as_method(self):
  with tempfile.TemporaryDirectory() as d:
   p,r,o,results=self.setup(d);proof=self.verify(p,r,results)
   plan=corral_artifact.disk_plan({'storage':{'virtual_bytes':18*m.GIB,'transfer_bytes':m.GIB},'requested_pvc':'20Gi','firmware':'uefi'},method='raw-upload',raw_proof=proof)
   self.assertEqual(plan['estimated_peak_bytes'],22*m.GIB+m.GIB//2);self.assertEqual(plan['scratch_pvc_bytes'],20*m.GIB);self.assertFalse(plan['full_image_import_qualified'])
 def test_absent_readback_refused(self):
  with tempfile.TemporaryDirectory() as d:
   p,r,o,results=self.setup(d);r['readback']['matched']=False;(p/'receipt.json').write_text(json.dumps(r))
   with self.assertRaises(ValueError):self.verify(p,r,results)
 def test_backend_node_version_and_config_changes_refused(self):
  for position,replacement in [(0,{'metadata':{'uid':'replaced'}}),(1,{'data':{}}),(2,{'items':[{'status':{'observedVersion':'v1.67'}}]})]:
   with tempfile.TemporaryDirectory() as d:
    p,r,o,results=self.setup(d);results[position]=replacement
    with self.assertRaises(ValueError):self.verify(p,r,results)
 def test_nonzero_scratch_or_no_actual_image_refused(self):
  for usage,image in [('10\t/scratch',m.UPLOAD_IMAGE),('0\t/scratch','')]:
   with tempfile.TemporaryDirectory() as d:
    p,r,o,results=self.setup(d);o[0]['directory_usage']=usage;o[0]['container_statuses'][0]['imageID']=image;(p/'upload-observations.json').write_text(json.dumps(o))
    with self.assertRaises(ValueError):self.verify(p,r,results)
 def test_evidence_mutation_changes_bound_hash(self):
  with tempfile.TemporaryDirectory() as d:
   p,r,o,results=self.setup(d);sha=m.evidence_sha(p);(p/'upload-observations.json').write_text('[]')
   with self.assertRaises(ValueError):m.proof(p,'ctx','node',sha)
 def test_raw_requires_probe(self):
  with self.assertRaises(ValueError):corral_artifact.disk_plan({'storage':{'virtual_bytes':18*m.GIB,'transfer_bytes':m.GIB},'requested_pvc':'20Gi','firmware':'uefi'},method='raw-upload')
if __name__=='__main__':unittest.main()
