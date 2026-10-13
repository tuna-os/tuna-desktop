"""Source-owned tiny orchestration negatives; no Kubernetes execution."""
import copy
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest import mock
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'lib'))
import corral_node_probe as probe
import corral_node_stage as stage
import test_corral_node_stage as fixtures


class Tests(unittest.TestCase):
    def closure(self):
        result=fixtures.Tests().closure()
        result.update(registry_published=False,raw_sha256='a'*64,publication_evidence={'actual_publication':True})
        return result

    def args(self,root):
        return types.SimpleNamespace(out=root,context='explicit',namespace='corral-vms',node='node')

    def test_known_pattern_independent_hash(self):
        size=128*1024;middle=96*1024
        data=bytearray(size);data[:len(probe.START)]=probe.START;data[middle:middle+len(probe.MIDDLE)]=probe.MIDDLE
        self.assertEqual(probe.expected_raw_sha(size,middle),hashlib.sha256(data).hexdigest())
        with self.assertRaises(ValueError):probe.expected_raw_sha(10,5)
        with self.assertRaises(TimeoutError):probe.expected_raw_sha(size,middle,deadline=0)

    def test_full_tiny_reservation_does_not_admit_dense_or20gib(self):
        plan=probe.tiny_storage(self.closure())
        self.assertEqual(plan['stage_pvc_bytes'],probe.GIB)
        self.assertEqual(plan['target_pvc_bytes'],probe.GIB)
        self.assertGreater(plan['estimated_peak_bytes'],2*probe.GIB)
        for key in ('full_20gib_reservation_qualified','dense_decoder_memory_qualified','full_node_method_qualified'):
            self.assertIs(plan[key],False)

    def test_runtime_identity_library_negatives(self):
        closure=self.closure()
        value={'uid':107,'worker_sha256':closure['worker_sha256'],'qemu_sha256':stage.QEMU_SHA,
               'qemu_libraries_before':closure['qemu_libraries'],'qemu_libraries_after':closure['qemu_libraries'],'result':{'actual':True},
               'python_executable':closure['python_executable'],'python_libraries_observed':closure['python_libraries'],
               'memory_before':{'status':'unsupported','reason':'fixture'},'memory_after':{'status':'unsupported','reason':'fixture'}}
        self.assertEqual(probe.runtime_receipt(value,closure),{'actual':True})
        for key,bad in [('uid',0),('worker_sha256','0'*64),('qemu_sha256','0'*64),('qemu_libraries_after',{})]:
            wrong=copy.deepcopy(value);wrong[key]=bad
            with self.assertRaises(ValueError):probe.runtime_receipt(wrong,closure)

    def test_actual_create_ambiguous_response_records_only_matching_nonce(self):
        for outcome in ('timeout','empty','foreign'):
            with tempfile.TemporaryDirectory() as d:
                actor=probe.Probe(self.args(Path(d)))
                obj={'metadata':{'name':'own','namespace':'corral-vms'}}
                restored={'metadata':{'name':'own','uid':'actual-uid','labels':{'tuna-perf-nonce':actor.nonce if outcome!='foreign' else 'other'}}}
                calls=[TimeoutError('ambiguous') if outcome!='empty' else None,restored]
                def api(*a,**kw):
                    value=calls.pop(0)
                    if isinstance(value,Exception):raise value
                    return value
                with mock.patch.object(actor,'api',side_effect=api),self.assertRaises((TimeoutError,ValueError)):
                    actor.create('pvc',obj)
                self.assertEqual(actor.resources,[] if outcome=='foreign' else [('pvc','own','actual-uid')])

    def actor(self,root,fail=None):
        actor=probe.Probe(self.args(root));actor.events=[]
        node={'metadata':{'name':'node','uid':'node-uid'}}
        config={'setup':'#!/bin/sh\nset -eu\nmkdir -m 0777 -p "$VOL_DIR"'}
        def api(*argv,**kwargs):
            if argv[:2]==('get','node'):return node
            if argv[:2]==('get','namespace'):return {'metadata':{'uid':argv[2]+'-uid'}}
            if argv[:2]==('get','configmap'):return {'data':config}
            raise AssertionError(argv)
        actor.api=api;actor.capacity=lambda:actor.events.append('capacity')
        def create(kind,obj):
            actor.events.append('create-'+kind)
            obj['metadata']['uid']=obj['metadata']['name']+'-uid'
            actor.register(kind,obj['metadata']['name'],obj['metadata']['uid']);return obj
        actor.create=create;actor.wait=lambda pod:actor.events.append('wait')
        data=b'bounded fixture'
        def remote(pod,operation,*args,**kwargs):
            actor.events.append(operation)
            if operation==fail:raise ValueError('actual boundary failed')
            if operation=='fixture':return {'raw_sha256':'a'*64,'zip_bytes':len(data),'zip_sha256':hashlib.sha256(data).hexdigest(),'disk_sha256':'b'*64}
            if operation=='download':return data
            if operation=='receive-negative':return {'negative_control':'wrong-archive-digest','rejected':True}
            if operation=='receive':return {'zip_sha256':hashlib.sha256(data).hexdigest(),'disk_sha256':'b'*64,'manifest':{'raw_sha256':'a'*64}}
            if operation in ('convert','readback'):return {'raw_sha256':'a'*64,'logical_bytes':probe.worker.VIRTUAL,'allocated_bytes':65536}
            raise AssertionError(operation)
        actor.remote=remote
        return actor

    def test_actual_orchestrator_order_scoped_receipt_and_cleanup(self):
        with tempfile.TemporaryDirectory() as d:
            actor=self.actor(Path(d));calls=[]
            def cleanup(*args,**kw):calls.append(args[0]);return {'all_owned_resources_absent':True}
            with mock.patch.object(probe,'expected_raw_sha',return_value='a'*64),mock.patch.object(actor,'cleanup',side_effect=cleanup):
                receipt=actor.execute(self.closure())
            self.assertIs(receipt['completed'],True)
            self.assertEqual(receipt['method'],'node-stage-tiny-v1')
            self.assertIs(receipt['full_node_method_qualified'],False)
            self.assertIs(receipt['dense_decoder_memory_qualified'],False)
            self.assertIs(receipt['cleanup']['owned_pvs_absent'],True)
            self.assertLess(actor.events.index('receive-negative'),actor.events.index('receive'))
            self.assertLess(actor.events.index('convert'),actor.events.index('readback'))
            self.assertTrue(any(q[0]=='pvc' for q in calls[-1]))
            self.assertTrue(all('readback' not in q[1] for q in calls[0]))

    def test_actual_orchestrator_decoder_failure_cleanup_no_completion(self):
        with tempfile.TemporaryDirectory() as d:
            actor=self.actor(Path(d),fail='convert')
            with mock.patch.object(probe,'expected_raw_sha',return_value='a'*64),mock.patch.object(actor,'cleanup',return_value={'all_owned_resources_absent':True}) as cleanup,self.assertRaises(ValueError):
                actor.execute(self.closure())
            self.assertTrue(cleanup.called)
            self.assertIs(json.loads((Path(d)/'receipt.json').read_text())['completed'],False)

    def test_cleanup_failure_never_becomes_completion(self):
        with tempfile.TemporaryDirectory() as d:
            actor=self.actor(Path(d))
            # First temporary worker cleanup succeeds; final cleanup fails.
            with mock.patch.object(probe,'expected_raw_sha',return_value='a'*64),mock.patch.object(actor,'cleanup',side_effect=[{'all_owned_resources_absent':True},TimeoutError('still live')]),self.assertRaises(RuntimeError):
                actor.execute(self.closure())
            receipt=json.loads((Path(d)/'receipt.json').read_text())
            self.assertIs(receipt['completed'],False)
            self.assertEqual(receipt['cleanup_error_type'],'TimeoutError')

    def test_uid_image_node_pv_replacements_fail_real_observe(self):
        for defect in ('none','pod-uid','image','node','pvc-uid','pv-node','pv-reclaim','pull-secret','writable-readback','init','ephemeral','privileged','cap-add','group','seccomp','swap','subpath'):
            with self.subTest(defect=defect),tempfile.TemporaryDirectory() as d:
                actor=probe.Probe(self.args(Path(d)));actor.closure=self.closure();actor.capacity=lambda:None
                pod=stage.pod_spec('own','corral-vms','node','stage','disk',actor.closure['image'],actor.closure['worker_sha256'])
                pod['metadata']['uid']='pod';pod['spec']['nodeName']='node'
                pod['status']={'containerStatuses':[{'imageID':actor.closure['image'],'ready':True}]}
                pvc={'metadata':{'uid':'pvc','name':'disk'},'status':{'phase':'Bound'},'spec':{'volumeName':'pv-disk','resources':{'requests':{'storage':'1Gi'}},'storageClassName':'local-path','volumeMode':'Filesystem','accessModes':['ReadWriteOnce']}}
                pv={'metadata':{'uid':'pv-disk','name':'pv-disk'},'spec':{'claimRef':{'uid':'pvc','namespace':'corral-vms'},'persistentVolumeReclaimPolicy':'Delete','nodeAffinity':{'required':{'nodeSelectorTerms':[{'matchExpressions':[{'key':'kubernetes.io/hostname','operator':'In','values':['node']}]}]}}}}
                expected=copy.deepcopy(pod);extra=copy.deepcopy(pvc);extra['metadata']={'uid':'stage','name':'stage'};extra['spec']['volumeName']='pv-stage';actor.claims=[extra,copy.deepcopy(pvc)]
                if defect=='pod-uid':pod['metadata']['uid']='new'
                if defect=='image':pod['status']['containerStatuses'][0]['imageID']='other'
                if defect=='node':pod['spec']['nodeName']='other'
                if defect=='pvc-uid':pvc['metadata']['uid']='new'
                if defect=='pv-node':pv['spec']['nodeAffinity']['required']['nodeSelectorTerms'][0]['matchExpressions'][0]['values']=['other']
                if defect=='pv-reclaim':pv['spec']['persistentVolumeReclaimPolicy']='Retain'
                if defect=='pull-secret':pod['spec']['imagePullSecrets']=[{'name':'unexpected'}]
                if defect=='writable-readback':actor.readonly.add('pod')
                if defect=='init':pod['spec']['initContainers']=[{'name':'extra'}]
                if defect=='ephemeral':pod['spec']['ephemeralContainers']=[{'name':'extra'}]
                if defect=='privileged':pod['spec']['containers'][0]['securityContext']['privileged']=True
                if defect=='cap-add':pod['spec']['containers'][0]['securityContext']['capabilities']['add']=['SYS_ADMIN']
                if defect=='group':pod['spec']['securityContext']['runAsGroup']=0
                if defect=='seccomp':pod['spec']['securityContext']['seccompProfile']={'type':'Unconfined'}
                if defect=='swap':
                    for v in pod['spec']['volumes']:v['persistentVolumeClaim']['claimName']='disk' if v['name']=='stage' else 'stage'
                if defect=='subpath':pod['spec']['containers'][0]['volumeMounts'][0]['subPath']='other'

                stage_pv=copy.deepcopy(pv);stage_pv['metadata']={'uid':'pv-stage','name':'pv-stage'};stage_pv['spec']['claimRef']['uid']='stage'
                def api(*argv,**kw):
                    if argv[1]=='pvc':return extra if argv[2]=='stage' else pvc
                    if argv[1]=='pv':return stage_pv if argv[2]=='pv-stage' else pv
                    return pod
                actor.api=api
                if defect=='none':actor.observe(expected,'phase')
                else:
                    with self.assertRaises(ValueError):actor.observe(expected,'phase')

    def test_complete_digest_negative_and_truncated_input(self):
        data=b'known archive';sha=hashlib.sha256(data).hexdigest()
        with tempfile.TemporaryDirectory() as d:
            path=Path(d)/'negative'
            self.assertIs(probe.digest_negative(data,len(data),sha,path)['rejected'],True)
            for bad in (data[:-1],data+b'extra',b'wrong'):
                with self.assertRaisesRegex(ValueError,'truncated or differs'):
                    probe.digest_negative(bad,len(data),sha,Path(d)/'unused')
            with self.assertRaisesRegex(ValueError,'unrelated'):
                probe.digest_negative(data,len(data),sha,Path(d)/'unused',receiver=mock.Mock(side_effect=ValueError('unrelated')))

    def test_cleanup_early_bound_pv_and_atomic_uid_delete(self):
        with tempfile.TemporaryDirectory() as d:
            actor=probe.Probe(self.args(Path(d)));actor.receipt.update(cluster_uid='cluster',namespace_uid='namespace')
            actor.resources=[('pvc','disk','claim')]
            claim={'metadata':{'uid':'claim'},'spec':{'volumeName':'volume'}}
            pv={'metadata':{'uid':'volume-uid','name':'volume'},'spec':{'claimRef':{'uid':'claim','namespace':'corral-vms'},'persistentVolumeReclaimPolicy':'Delete'}}
            deleted=False;calls=[]
            def api(*a,**kw):
                nonlocal deleted
                calls.append(a)
                if a[:2]==('get','namespace'):return {'metadata':{'uid':'cluster' if a[2]=='kube-system' else 'namespace'}}
                if a[:2]==('get','pvc'):return None if deleted else claim
                if a[:2]==('get','pv'):
                    if '-o'==a[2]:return {'items':[] if deleted else [pv]}
                    return None if deleted else pv
                if a[0]=='delete':
                    body=json.loads(kw['input_path'].read_text())
                    self.assertEqual(body['preconditions'],{'uid':'claim'});deleted=True;return {}
                raise AssertionError(a)
            actor.api=api
            result=actor.cleanup(actor.resources,include_pvs=True)
            self.assertTrue(result['owned_pvs_absent']);self.assertEqual(actor.pvs,{'volume':'volume-uid'})
            self.assertLess(calls.index(('get','pv','volume','-o','json')),next(i for i,a in enumerate(calls) if a[0]=='delete'))

    def test_cleanup_cluster_replacement_refuses_delete(self):
        with tempfile.TemporaryDirectory() as d:
            actor=probe.Probe(self.args(Path(d)));actor.receipt.update(cluster_uid='original',namespace_uid='namespace')
            def api(*a,**kw):
                if a[1]=='pvc':return {'metadata':{'uid':'claim'}}
                if a[1]=='namespace':return {'metadata':{'uid':'foreign'}}
                self.fail('destructive command after identity mismatch')
            actor.api=api
            with self.assertRaisesRegex(ValueError,'identity changed'):
                actor.cleanup([('pvc','disk','claim')])

    def test_cleanup_unknown_pv_discovery_is_not_confirmed(self):
        with tempfile.TemporaryDirectory() as d:
            actor=probe.Probe(self.args(Path(d)));actor.resources=[('pvc','disk','claim')]
            def api(*a,**kw):
                if a[1]=='pvc':return None
                raise TimeoutError('PV inventory unavailable')
            actor.api=api
            with self.assertRaises(TimeoutError):actor.cleanup(actor.resources,include_pvs=True)

    def test_projection_is_private_fixed_and_removed(self):
        with tempfile.TemporaryDirectory() as d:
            actor=probe.Probe(self.args(Path(d)))
            value={'current-context':'explicit','contexts':[{'name':'explicit','context':{'cluster':'c','user':'u'}}],
                   'clusters':[{'name':'c','cluster':{'server':'https://example.test','certificate-authority-data':'private'}}],
                   'users':[{'name':'u','user':{'token':'private'}}]}
            def bounded(argv,path,deadline,**kw):
                self.assertEqual(path.stat().st_mode&0o777,0o600)
                self.assertNotEqual(kw['env'].get('KUBECONFIG'),str(path))
                path.write_text(json.dumps(value))
            actor.bounded=bounded
            actor.project_routing();directory=actor.private
            self.assertEqual(directory.stat().st_mode&0o777,0o700)
            self.assertNotIn('certificate-authority-data',json.dumps(actor.receipt))
            self.assertNotIn('token',json.dumps(actor.receipt))
            self.assertEqual(actor.kube_argv('get','pods')[2],str(directory/'config'))
            actor.close_routing();self.assertFalse(directory.exists())

    def test_cleanup_and_api_use_supervised_fixed_routing(self):
        with tempfile.TemporaryDirectory() as d:
            actor=probe.Probe(self.args(Path(d)));actor.private=Path(d)/'private';actor.private.mkdir(mode=0o700)
            config=actor.private/'config';config.write_bytes(b'fixed');config.chmod(0o600);actor.routing_sha=probe.stage.sha(config)
            seen=[]
            def bounded(argv,path,deadline,**kw):
                seen.append((argv,deadline,kw['env']['KUBECONFIG']));path.write_bytes(b'{}')
            actor.bounded=bounded
            actor.api('get','pods','-o','json')
            self.assertEqual(seen[0][0][:5],['kubectl','--kubeconfig',str(config),'--context','explicit'])
            self.assertLessEqual(seen[0][1],actor.deadline)
            config.write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError,'routing changed'):actor.api('get','pods')

    def test_source_preserves_original_publisher_identity_and_no_implicit_import_admission(self):
        root=Path(__file__).resolve().parents[2]
        # Original successful publication's exact three source SHA256 values;
        # no network or shallow Git-history dependency in focused CI.
        original={'scripts/tuna-corral-node-tools-publish': '6e540ae53646d5d92d8a7a23fe7b07954a52504e5e434a221f72044373670270', 'scripts/lib/corral_node_publish.py': 'a4f93860aeaaed49014aa9c02eb06527dbbaad535c72b8e8f35ec84e551d81b2', '.github/workflows/corral-visual-tools.yml': '6bd307d1eb05d0dfc90677855ae551dea42464b14bd276ba4d2a8ae285768f68'}
        for name,digest in original.items():
            self.assertEqual(hashlib.sha256((root/name).read_bytes()).hexdigest(),digest)
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);source=root/'source';source.mkdir()
            for name in ('corral_node_stage.py','corral_node_worker.py'):(source/name).write_bytes((Path(stage.__file__).parent/name).read_bytes())
            sources={name:hashlib.sha256((source/name).read_bytes()).hexdigest() for name in ('corral_node_stage.py','corral_node_worker.py')}
            receipt={'method':'node-stage-tiny-v1','completed':True,'source_sha256':sources,
                     'context':'explicit','namespace':'corral-vms','node_uid':'node-uid','image':self.closure()['image'],
                     'cleanup':{'all_owned_resources_absent':True},'independent_readback_matched':True,'full_image_import_qualified':False}
            data=json.dumps(receipt).encode();(root/'receipt.json').write_bytes(data)
            digests={'receipt.json':hashlib.sha256(data).hexdigest(),**{'source/'+name:value for name,value in sources.items()}}
            expected=hashlib.sha256(json.dumps(digests,sort_keys=True).encode()).hexdigest()
            # All byte/source/context flags match; the distinct method alone fails.
            with self.assertRaisesRegex(ValueError,'separate actual tiny node-stage qualification'):
                stage.method_proof(root,expected,self.closure(),'node-uid','explicit','corral-vms')



if __name__=='__main__':unittest.main()
