"""Execute the actual small OCI closure parser on synthetic metadata controls."""
import gzip
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest


class Tests(unittest.TestCase):
    def generate(self, bad=None):
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup)
        root=Path(temporary.name);blobs={}
        def blob(data):
            digest='sha256:'+hashlib.sha256(data).hexdigest();blobs['blobs/sha256/'+digest[7:]]=data
            return digest
        raw=b'synthetic layer contents';compressed=gzip.compress(raw);layer=blob(compressed)
        diff='sha256:'+hashlib.sha256(raw).hexdigest()
        config=blob(json.dumps({'architecture':'amd64','os':'linux','rootfs':{'diff_ids': [] if bad=='count' else ['sha256:'+'0'*64 if bad=='diff' else diff]}}).encode())
        manifest=blob(json.dumps({'config':{'digest':config},'layers':[{'digest':layer,'size':len(compressed),'mediaType':'application/vnd.oci.image.layer.v1.tar+gzip'}]}).encode())
        blobs['index.json']=json.dumps({'manifests':[{'digest':manifest}]}).encode()
        archive=root/'tools.oci.tar'
        with tarfile.open(archive,'w') as t:
            for name,data in blobs.items():
                info=tarfile.TarInfo(name);info.size=len(data);t.addfile(info,io.BytesIO(data))
        smoke=root/'smoke.json';smoke.write_text(json.dumps({'schema':1,'smoke_passed':True,'uid':107}))
        script=Path(__file__).resolve().parents[1]/'lib/corral-node-tool-closure.py'
        result=subprocess.run([sys.executable,str(script),str(archive),str(smoke),str(root/'closure'),'--source','a'*40],capture_output=True,timeout=10)
        return root,result

    def test_build_only_honest_false_and_exact_source_hashes(self):
        root,result=self.generate();self.assertEqual(result.returncode,0,result.stderr.decode())
        closure=json.loads((root/'closure/tool-closure.json').read_text())
        self.assertFalse(closure['registry_published']);self.assertEqual(len(closure['source_files_sha256']),8)
        self.assertEqual(closure['layers'][0]['uncompressed_bytes'],len(b'synthetic layer contents'))
        self.assertEqual(closure['layers'][0]['diff_id'],'sha256:'+hashlib.sha256(b'synthetic layer contents').hexdigest())

    def test_wrong_diff_id_is_not_a_tool_closure(self):
        root,result=self.generate('diff');self.assertNotEqual(result.returncode,0)
        self.assertFalse((root/'closure/tool-closure.json').exists())

    def test_wrong_layer_count_is_not_a_tool_closure(self):
        root,result=self.generate('count');self.assertNotEqual(result.returncode,0)
        self.assertFalse((root/'closure/tool-closure.json').exists())


if __name__=='__main__':unittest.main()
