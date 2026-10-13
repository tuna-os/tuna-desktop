"""Source controls only; no actual gzip CDI operation or crypto/guest qualification."""
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock
import zlib

sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'lib'))
import tuna_raw_gzip as raw
import corral_gzip as consumer
import corral_artifact


def wrapper(data):
    encoder=zlib.compressobj(6,zlib.DEFLATED,16+zlib.MAX_WBITS)
    return encoder.compress(data)+encoder.flush()


class GzipContract(unittest.TestCase):
    def test_actual_single_member_stream_hash_length(self):
        data=b'TUNA'*(1024*16)
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)/'disk.raw.gz';p.write_bytes(wrapper(data))
            r=raw.check_gzip(p,len(data),hashlib.sha256(data).hexdigest())
            self.assertEqual(r['raw_bytes'],len(data))
            self.assertEqual(r['transfer_bytes'],p.stat().st_size)
            self.assertEqual(r['gzip_sha256'],hashlib.sha256(p.read_bytes()).hexdigest())

    def test_truncated_extra_member_extra_payload_and_wrong_hash_refused(self):
        data=b'TUNA'*1024;good=wrapper(data)
        cases=[good[:-1],good+wrapper(b'x'),good+b'x',wrapper(b'wrong'),b'not-gzip']
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)/'disk.raw.gz'
            for bad in cases:
                p.write_bytes(bad)
                with self.subTest(bad=bad[:10]),self.assertRaises((ValueError,zlib.error)):
                    raw.check_gzip(p,len(data),hashlib.sha256(data).hexdigest())
            p.write_bytes(good)
            with self.assertRaises(ValueError):
                raw.check_gzip(p,len(data)-1,hashlib.sha256(data).hexdigest())
            with self.assertRaises(ValueError):
                raw.check_gzip(p,len(data)+1,hashlib.sha256(data).hexdigest())

    def test_symlink_nonregular_and_expired_deadline_refused(self):
        data=b'TUNA';sha=hashlib.sha256(data).hexdigest()
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)/'disk.gz';p.write_bytes(wrapper(data))
            q=Path(tmp)/'link';q.symlink_to(p)
            with self.assertRaises(OSError):raw.check_gzip(q,4,sha)
            with self.assertRaises(ValueError):raw.check_gzip(Path(tmp),4,sha)
            with self.assertRaises(ValueError):raw.check_gzip(p,4,sha,timeout=0)

    def manifest(self):
        r={'method':raw.METHOD,'policy':raw.POLICY,'requested_pvc':'20Gi',
           'single_member_verified':True,'raw_bytes':raw.RAW_BYTES,'raw_sha256':'a'*64,
           'gzip_sha256':'b'*64,'transfer_bytes':3*raw.GIB}
        return {'raw_gzip':r,'storage':raw.storage(r),'requested_pvc':'20Gi',
                'disk_format':'raw-gzip','import_method':raw.METHOD,'firmware':'uefi',
                'raw_sha256':'a'*64,'disk_sha256':'b'*64}

    def proof(self):
        return {'method':raw.METHOD,'original_probe_evidence_sha256':'c'*64,
                'full_image_import_qualified':False}

    def test_distinct_20gi_plan_does_not_reinterpret_old_dense_28gi(self):
        plan=consumer.disk_plan(self.manifest(),self.proof())
        self.assertEqual(plan['estimated_peak_bytes'],24159191040)
        self.assertFalse(plan['node_peak_qualified'])
        self.assertEqual(plan['target_pvc_bytes'],20*raw.GIB)
        m=self.manifest();m['requested_pvc']='28Gi'
        with self.assertRaises(ValueError):consumer.disk_plan(m,self.proof())
        with self.assertRaises(ValueError):corral_artifact.disk_plan(self.manifest())

    def test_old_raw_or_missing_proof_and_mismatched_bytes_refused(self):
        m=self.manifest()
        for proof in (None,{},dict(self.proof(),method='raw-upload'),dict(self.proof(),full_image_import_qualified=True)):
            with self.subTest(proof=proof),self.assertRaises(ValueError):consumer.disk_plan(m,proof)
        for field in ('raw_sha256','disk_sha256','firmware','import_method','disk_format'):
            bad=dict(m);bad[field]='wrong'
            with self.subTest(field=field),self.assertRaises(ValueError):consumer.disk_plan(bad,self.proof())

    def test_actual_overhead_rounding_requires_full_raw_plus_margin(self):
        self.assertGreaterEqual(raw.usable_target(20*raw.GIB,.05),raw.RAW_BYTES+raw.GIB//2)
        for available,overhead in ((18*raw.GIB,.05),(20*raw.GIB,.09),(True,.05),(20*raw.GIB,float('nan')),(20*raw.GIB,None)):
            with self.subTest(available=available,overhead=overhead),self.assertRaises(ValueError):raw.usable_target(available,overhead)

    def test_usable_target_refuses_unknown_cdi_overhead_before_any_pod_create(self):
        claim={'metadata':{'uid':'owned'},'status':{'phase':'Bound'},'spec':{'storageClassName':'local-path','volumeMode':'Filesystem','resources':{'requests':{'storage':'20Gi'}}}}
        with mock.patch.object(consumer,'kubectl',side_effect=[claim,{}]),mock.patch('corral_owned.placement',return_value={}),mock.patch('corral_owned.create') as create:
            with self.assertRaises(ValueError):consumer.target_capacity('context','ns','claim','owned','node',Path('/unused'))
            create.assert_not_called()

    def test_actual_uploader_predicate_rejects_wrong_owner_image_node_restart_or_preallocation(self):
        import copy
        pod={'metadata':{'name':'upload','uid':'pod','ownerReferences':[{'uid':'claim'}]},
             'spec':{'nodeName':'node','containers':[{'env':[{'name':'PREALLOCATION','value':'false'}]}]},
             'status':{'phase':'Running','containerStatuses':[{'imageID':consumer.UPLOAD_IMAGE,'restartCount':0}]}}
        self.assertEqual(consumer.uploader_observation(pod,'claim','node')['preallocation'],'false')
        for mutation in ('owner','node','image','restart','preallocation','init','ephemeral','extra'):
            bad=copy.deepcopy(pod)
            if mutation=='owner':bad['metadata']['ownerReferences'][0]['uid']='foreign'
            elif mutation=='node':bad['spec']['nodeName']='foreign'
            elif mutation=='image':bad['status']['containerStatuses'][0]['imageID']='foreign'
            elif mutation=='restart':bad['status']['containerStatuses'][0]['restartCount']=1
            elif mutation=='preallocation':bad['spec']['containers'][0]['env'][0]['value']='true'
            elif mutation=='init':bad['spec']['initContainers']=[{}]
            elif mutation=='ephemeral':bad['spec']['ephemeralContainers']=[{}]
            elif mutation=='extra':bad['spec']['containers'].append({})
            with self.subTest(mutation=mutation),self.assertRaises(ValueError):consumer.uploader_observation(bad,'claim','node')

    def test_actual_compressor_and_wrong_sha_cleanup_with_small_fixture(self):
        # Only this isolated control changes the byte policy to avoid an 18 GiB
        # local write. Production CLI remains fixed to the complete 18 GiB RAW.
        data=b'TUNA'*(16*1024);sha=hashlib.sha256(data).hexdigest()
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);source=root/'input.raw';source.write_bytes(data)
            with mock.patch.object(raw,'RAW_BYTES',len(data)),mock.patch.object(raw.os,'statvfs',return_value=type('V',(),{'f_bavail':10*raw.GIB,'f_frsize':1})()):
                good=root/'disk.raw.gz'
                result=raw.compress(source,good,sha,timeout=5)
                self.assertEqual(result['raw_sha256'],sha)
                self.assertEqual(raw.check_gzip(good,len(data),sha)['raw_bytes'],len(data))
                bad=root/'bad.raw.gz'
                with self.assertRaises(ValueError):raw.compress(source,bad,'0'*64,timeout=5)
                self.assertFalse(bad.exists())
                with self.assertRaises(ValueError):raw.compress(source,good,sha,timeout=5)

    def test_actual_compressor_capacity_refusal_removes_only_owned_new_output(self):
        data=b'x'*4096
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);source=root/'source.raw';source.write_bytes(data);target=root/'disk.gz'
            with mock.patch.object(raw,'RAW_BYTES',len(data)),mock.patch.object(raw.os,'statvfs',return_value=type('V',(),{'f_bavail':1,'f_frsize':1})()):
                with self.assertRaises(ValueError):raw.compress(source,target,hashlib.sha256(data).hexdigest())
            self.assertFalse(target.exists())
            self.assertEqual(source.read_bytes(),data)

    def test_actual_ambiguous_probe_recovery_uid_and_foreign_nonce(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);nonce='a'*32
            (root/'pvc-create.json').write_text(json.dumps({'metadata':{'name':'owned','namespace':'ns','labels':{'tuna-perf-nonce':nonce}}}))
            current={'metadata':{'uid':'actual','labels':{'tuna-perf-nonce':nonce}}}
            with mock.patch.dict('os.environ',{'TUNA_GZIP_NAMESPACE_UID':'namespace'}),mock.patch.object(consumer,'kubectl',return_value={'metadata':{'uid':'namespace'}}),mock.patch('corral_owned.get',return_value=current),mock.patch('corral_owned.cleanup',return_value={'all_owned_resources_absent':True}) as cleanup:
                consumer.recover_probe_cleanup('context','ns',root)
                cleanup.assert_called_once_with('context','ns',[['pvc','owned','actual']],timeout=120)
            current['metadata']['labels']['tuna-perf-nonce']='foreign'
            with mock.patch.dict('os.environ',{'TUNA_GZIP_NAMESPACE_UID':'namespace'}),mock.patch.object(consumer,'kubectl',return_value={'metadata':{'uid':'namespace'}}),mock.patch('corral_owned.get',return_value=current),mock.patch('corral_owned.cleanup') as cleanup:
                with self.assertRaises(ValueError):consumer.recover_probe_cleanup('context','ns',root)
                cleanup.assert_not_called()

    def test_actual_kubelet_threshold_refusal_and_hard_soft_not_overwritten(self):
        node={'metadata':{'name':'node','uid':'uid'},'spec':{},'status':{'conditions':[{'type':'Ready','status':'True'},{'type':'DiskPressure','status':'False'}],
              'allocatable':{k:'1' for k in ('devices.kubevirt.io/kvm','devices.kubevirt.io/tun','devices.kubevirt.io/vhost-net')}}}
        fs={'capacityBytes':100*raw.GIB,'availableBytes':40*raw.GIB}
        stats={'node':{'fs':fs,'runtime':{'imageFs':fs,'containerFs':fs}}}
        config={'kubeletconfig':{'evictionHard':{'nodefs.available':'30%','imagefs.available':'15%'},'evictionSoft':{'nodefs.available':'10%'}}}
        with mock.patch.object(consumer,'kubectl',side_effect=[node,stats,config]):
            _,receipt=consumer.fresh_capacity('context','node',20*raw.GIB)
        self.assertFalse(receipt['ready_for_disk_admission'])
        self.assertEqual(receipt['configured_thresholds']['reserved_percent'],'30')
        for config in ({},{'kubeletconfig':{'evictionHard':{'nodefs.available':'1Gi','imagefs.available':'15%'}}}):
            with self.subTest(config=config),mock.patch.object(consumer,'kubectl',side_effect=[node,stats,config]),self.assertRaises(ValueError):
                consumer.fresh_capacity('context','node',20*raw.GIB)


if __name__=='__main__':
    unittest.main()
