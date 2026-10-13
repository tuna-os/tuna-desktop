import copy
import hashlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
import zipfile
from unittest import mock
sys.path.insert(0, str(Path(__file__).resolve().parents[1]/'lib'))
import corral_node_stage as stage
import corral_node_worker as worker


class Tests(unittest.TestCase):
    def closure(self):
        return {'schema': 1, 'base_image': stage.BASE_IMAGE, 'installer_image': stage.INSTALLER_IMAGE,
                'python_version': [3, 12, 0], 'qemu_libraries': {'/lib64/libc.so.6': 'c'*64},
                'qemu_libraries_before': {'/lib64/libc.so.6': 'c'*64}, 'qemu_libraries_after': {'/lib64/libc.so.6': 'c'*64},
                'python_executable': '/opt/tuna-python/usr/bin/python3.12',
                'python_libraries': {'/opt/tuna-python/usr/lib64/libc.so.6': 'd'*64}, 'qemu_before_sha256': stage.QEMU_SHA,
                'qemu_after_sha256': stage.QEMU_SHA, 'registry_published': True, 'smoke_passed': True,
                'uid': 107, 'image': 'ghcr.io/tuna-os/benchmark-node-tools@sha256:'+'a'*64,
                'worker_sha256': stage.sha(Path(worker.__file__)),
                'layers': [{'compressed_bytes': 1024, 'uncompressed_bytes': 2048, 'digest': 'sha256:'+'b'*64}]}

    def test_complete_tool_contract(self):
        self.assertEqual(stage.tool_contract(self.closure()), 3072+stage.GIB//2)

    def test_tool_contract_failures(self):
        for key, value in [('registry_published', False), ('smoke_passed', False), ('uid', 0),
                           ('installer_image', 'mutable:tag'), ('python_version', [3, 9]), ('qemu_libraries', {}), ('qemu_libraries_after', {}), ('python_executable', '/usr/bin/python3.12'), ('python_libraries', {'/lib/global.so': 'd'*64}), ('qemu_after_sha256', '0'*64), ('worker_sha256', '0'*64), ('image', 'mutable:tag')]:
            with self.subTest(key=key):
                closure = self.closure();closure[key] = value
                with self.assertRaises(ValueError):stage.tool_contract(closure)

    def test_missing_layer_sizes_not_zero(self):
        for value in (None, 0, -1, True, 5*stage.GIB):
            closure = self.closure();closure['layers'][0]['uncompressed_bytes'] = value
            with self.assertRaises(ValueError):stage.tool_contract(closure)

    def test_full_target_and_rounded_stage(self):
        result = stage.storage_plan({'size_in_bytes': 2*stage.GIB}, self.closure())
        self.assertEqual(result['target_pvc_bytes'], 20*stage.GIB)
        self.assertEqual(result['staging_pvc_bytes'], 6*stage.GIB)
        self.assertGreater(result['estimated_peak_bytes'], 26*stage.GIB)
        self.assertFalse(result['full_image_import_qualified'])

    def test_unbounded_archive_refused(self):
        for size in (0, True, 3*stage.GIB+1):
            with self.assertRaises(ValueError):stage.storage_plan({'size_in_bytes': size}, self.closure())

    def test_pod_security_and_placement(self):
        pod = stage.pod_spec('owned', 'corral-vms', 'node', 'stage', 'disk', self.closure()['image'], 'a'*64)
        spec = pod['spec'];self.assertFalse(spec['automountServiceAccountToken'])
        self.assertEqual(spec['nodeSelector'], {'kubernetes.io/hostname': 'node'})
        self.assertEqual(spec['securityContext']['runAsUser'], 107)
        self.assertTrue(spec['containers'][0]['securityContext']['readOnlyRootFilesystem'])
        self.assertEqual(spec['containers'][0]['securityContext']['capabilities'], {'drop': ['ALL']})
        self.assertTrue(all(set(v)=={'name', 'persistentVolumeClaim'} for v in spec['volumes']))

    def test_receive_exact_stream(self):
        with tempfile.TemporaryDirectory() as d:
            data = b'original bytes'
            worker.receive(io.BytesIO(data), Path(d)/'archive', len(data), hashlib.sha256(data).hexdigest())

    def test_receive_short_long_wrong_digest(self):
        for data, size, digest in [(b'ab', 3, '0'*64), (b'abcd', 3, '0'*64), (b'abc', 3, '0'*64)]:
            with tempfile.TemporaryDirectory() as d:
                with self.assertRaises(ValueError):worker.receive(io.BytesIO(data), Path(d)/'archive', size, digest)

    def archive(self, root, extra=None):
        path = root/'archive.zip'
        with zipfile.ZipFile(path, 'w') as z:
            z.writestr('manifest.json', '{}');z.writestr('disk.qcow2', b'qcow')
            if extra:
                import warnings
                with warnings.catch_warnings():
                    warnings.simplefilter('ignore', UserWarning)
                    z.writestr(extra, b'bad')
        return path

    def test_safe_extraction(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);worker.extract(self.archive(root), root/'export')
            self.assertEqual((root/'export/disk.qcow2').read_bytes(), b'qcow')

    def test_unsafe_members_rejected(self):
        for name in ('../escape', '/absolute', 'a\\b', 'x/../bad', 'disk.qcow2'):
            with tempfile.TemporaryDirectory() as d:
                root=Path(d)
                with self.assertRaises(ValueError):worker.extract(self.archive(root, name), root/'export')

    def test_backing_and_wrong_virtual_refused(self):
        for info in ({'format':'qcow2','virtual-size':worker.VIRTUAL,'backing-filename':'other'},
                     {'format':'qcow2','virtual-size':20*stage.GIB}, {'format':'raw','virtual-size':worker.VIRTUAL}):
            with mock.patch.object(worker.subprocess, 'run', return_value=mock.Mock(stdout=json.dumps(info).encode())):
                with self.assertRaises(ValueError):worker.inspect_qcow('disk', {}, __import__('time').monotonic()+1)

    def test_original_methods_untouched(self):
        import corral_artifact
        manifest={'storage':{'virtual_bytes':18*stage.GIB,'transfer_bytes':stage.GIB},'requested_pvc':'20Gi','firmware':'uefi'}
        self.assertGreater(corral_artifact.disk_plan(manifest)['estimated_peak_bytes'],40*stage.GIB)
        with self.assertRaises(ValueError):corral_artifact.disk_plan(manifest, method='node-stage')

    def test_missing_tool_closure_never_queries_cluster(self):
        with tempfile.TemporaryDirectory() as d:
            args = mock.Mock(out=Path(d), start=False, context='test-context', namespace='corral-vms')
            with mock.patch.object(stage, 'load_tool', side_effect=ValueError('unqualified')), mock.patch.object(stage.subprocess, 'run') as run:
                with self.assertRaises(ValueError):stage.execute(args)
                run.assert_not_called()
            receipt=json.loads((Path(d)/'node-stage-receipt.json').read_text())
            self.assertFalse(receipt['completed'])

    def test_usage_measures_allocated_blocks(self):
        with tempfile.TemporaryDirectory() as d:
            path=Path(d)/'sparse'
            with path.open('wb') as f:f.write(b'nonzero');f.truncate(16*1024*1024)
            measured=worker.usage([d])[d]
            self.assertEqual(measured,path.stat().st_blocks*512)
            self.assertLess(measured,path.stat().st_size)

    def test_no_start_or_fake_cdi_receipt(self):
        with self.assertRaises(ValueError):stage.execute(mock.Mock(out=Path('/unused'), start=True))
        source=Path(stage.__file__).read_text()
        self.assertIn("node_stage_completed=True", source)
        self.assertIn('vm_created=False', source)
        self.assertNotIn("datavolume_uid=", source)



class ExecuteTests(unittest.TestCase):
    """Exercise the actual orchestration with fake API/process boundaries."""
    def exercise(self, failure=None):
        import types
        import corral_owned
        import corral_ssh
        import gnome_reference
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup)
        out=Path(temporary.name)
        args=types.SimpleNamespace(out=out,start=False,context='context',namespace='corral-vms',node='node',
                                   node_tool_closure=out,node_tool_source_sha='a'*40,source_sha='b'*40,
                                   run_id=10,artifact_id=20,profile='stock',candidate_sha='c'*40,package_run=30,
                                   node_method_proof=out,node_method_proof_sha='d'*64)
        closure=Tests().closure()
        config={'setup':'#!/bin/sh\nset -eu\nmkdir -m 0777 -p "$VOL_DIR"'}
        backend_sha=hashlib.sha256(json.dumps(config,sort_keys=True).encode()).hexdigest()
        node={'metadata':{'name':'node','uid':'node-uid'},'status':{'conditions':[{'type':'Ready','status':'True'},{'type':'DiskPressure','status':'False'}],
                    'allocatable':{'devices.kubevirt.io/kvm':'1','devices.kubevirt.io/tun':'1','devices.kubevirt.io/vhost-net':'1'}}}
        fs={'capacityBytes':1000*stage.GIB,'availableBytes':900*stage.GIB}
        stats={'node':{'fs':fs,'runtime':{'imageFs':fs,'containerFs':fs}}}
        def run_metadata(sha,path,identifier):
            return {'id':identifier,'status':'completed','conclusion':'success','event':'workflow_dispatch','head_sha':sha,
                    'head_repository':{'full_name':stage.REPO},'repository':{'full_name':stage.REPO},'path':path}
        export_run=run_metadata(args.source_sha,'.github/workflows/performance-baseline.yml',10)
        package_run=run_metadata(args.candidate_sha,'.github/workflows/ci.yml',30)
        artifact={'id':20,'name':'marlin-benchmark-disk-stock','expired':False,'size_in_bytes':1024,
                  'digest':'sha256:'+'e'*64,'workflow_run':{'id':10,'head_sha':args.source_sha}}
        manifest={'schema':1,'export_succeeded':True,'observer_source_commit':args.source_sha,'gnome_profile':'stock',
                  'paired_desktops':['gnome','tuna'],'disk_sha256':'f'*64,'raw_sha256':'1'*64,
                  'storage':{'virtual_bytes':18*stage.GIB},'requested_pvc':'20Gi','firmware':'uefi',
                  'candidate_source_commit':args.candidate_sha,'package_selection':{'head_sha':args.candidate_sha,'run_id':30},'reference':{}}
        received={'manifest':manifest,'manifest_sha256':'2'*64,'disk_sha256':'f'*64}
        resources={};cleanup_calls=[];clock=[100.0];state={'converted':False,'readback':False}
        def process(argv,**kwargs):
            if argv[0]=='gh':
                path=argv[-1]
                value=package_run if path.endswith('/30') else export_run if 'runs/' in path else artifact
            elif 'create' in argv:
                value=json.loads(kwargs['input']);kind='pvc' if value['kind']=='PersistentVolumeClaim' else 'pod'
                value['metadata']['uid']=kind+'-'+str(len(resources))
                value['status']={'phase':'Bound'} if kind=='pvc' else {'phase':'Running','containerStatuses':[{'imageID':closure['image']}]}
                if kind=='pod':value['spec']['nodeName']='node'
                resources[(kind,value['metadata']['name'])]=copy.deepcopy(value)
                if failure=='ambiguous-create':raise stage.subprocess.TimeoutExpired(argv,1)
            elif 'exec' in argv:
                operation=argv[argv.index('--')+4]
                if operation=='usage':value={'/stage':0,'/target':0}
                elif operation=='convert':
                    state['converted']=True
                    value={'raw_sha256':'1'*64,'logical_bytes':18*stage.GIB}
                    if failure=='pvc-replaced':
                        key=next(k for k in resources if k[0]=='pvc' and k[1].endswith('-disk'))
                        resources[key]['metadata']['uid']='foreign-pvc'
                else:
                    state['readback']=True;value={'raw_sha256':'1'*64,'logical_bytes':18*stage.GIB}
                    if failure=='pod-replaced':
                        key=next(k for k in resources if k[0]=='pod' and k[1].endswith('-readback'))
                        resources[key]['metadata']['uid']='foreign-pod'
                    if failure=='deadline':clock[0]+=1801
            elif '--raw' in argv:value=stats
            else:
                index=argv.index('get');kind=argv[index+1];name=argv[index+2]
                value=node if kind=='node' else {'data':config} if kind=='configmap' else resources[(kind,name)]
            return types.SimpleNamespace(returncode=0,stdout=json.dumps(value).encode(),stderr=b'')
        class FakeProcess:
            def __init__(self,argv,**kwargs):
                self.returncode=0;self.pid=123;self.stdout=io.BytesIO()
                if argv[0]=='kubectl':kwargs['stdout'].write(json.dumps(received).encode());kwargs['stdout'].flush()
            def poll(self):return 0
        def cleanup(context,namespace,owned,timeout):
            cleanup_calls.append(copy.deepcopy(owned))
            if failure=='cleanup':raise RuntimeError('controlled cleanup rejection')
            for kind,name,uid in owned:
                current=resources.get((kind,name))
                if current and current['metadata']['uid']==uid:del resources[(kind,name)]
            return {'all_owned_resources_absent':True}
        def placement(context,namespace,claim,node,timeout):
            return {'metadata':{'uid':('foreign-pv' if failure=='pv-replaced' and state['readback'] else 'pv-'+claim['metadata']['uid'])}}
        with mock.patch.object(stage,'load_tool',return_value=closure), mock.patch.object(stage,'method_proof',return_value={'local_path_config_sha256':backend_sha}), \
             mock.patch.object(stage.subprocess,'run',side_effect=process),mock.patch.object(stage.subprocess,'Popen',side_effect=FakeProcess), \
             mock.patch.object(stage.time,'monotonic',side_effect=lambda:clock[0]), \
             mock.patch.object(corral_owned,'get',side_effect=lambda context,namespace,kind,name,timeout:resources.get((kind,name))), \
             mock.patch.object(corral_owned,'placement',side_effect=placement),mock.patch.object(corral_owned,'cleanup',side_effect=cleanup), \
             mock.patch.object(corral_ssh,'export_policy',return_value={}),mock.patch.object(gnome_reference,'validate_image'):
            error=None
            try:stage.execute(args)
            except Exception as e:error=e
        receipt=json.loads((out/'node-stage-receipt.json').read_text())
        if (out/'capacity-latest.json').exists():
            receipt['_test_capacity']=json.loads((out/'capacity-latest.json').read_text())
            receipt['_test_plan']=json.loads((out/'storage-plan.json').read_text())
        return error,receipt,resources,cleanup_calls

    def test_actual_execute_disk_only_success(self):
        error,receipt,resources,cleanup=self.exercise()
        self.assertIsNone(error);self.assertTrue(receipt['node_stage_completed']);self.assertFalse(receipt['vm_created'])
        self.assertEqual(receipt['_test_capacity']['estimated_peak_bytes'],receipt['_test_plan']['estimated_peak_bytes'])
        self.assertEqual(len(resources),1);self.assertTrue(all(kind=='pvc' for kind,name in resources))

    def test_actual_execute_replacement_refused(self):
        for failure in ('pvc-replaced','pod-replaced'):
            error,receipt,resources,cleanup=self.exercise(failure)
            self.assertIsInstance(error,ValueError);self.assertFalse(receipt['completed'])
            self.assertTrue(any(v['metadata']['uid'].startswith('foreign-') for v in resources.values()))
            self.assertFalse(any(uid.startswith('foreign-') for call in cleanup for kind,name,uid in call))

    def test_actual_execute_pv_replacement_refused(self):
        error,receipt,resources,cleanup=self.exercise('pv-replaced')
        self.assertIsInstance(error,ValueError);self.assertFalse(receipt['completed'])

    def test_actual_execute_absolute_deadline_refused(self):
        error,receipt,resources,cleanup=self.exercise('deadline')
        self.assertIsInstance(error,TimeoutError);self.assertFalse(receipt['completed']);self.assertFalse(resources)

    def test_actual_execute_ambiguous_create_recovers_uid(self):
        error,receipt,resources,cleanup=self.exercise('ambiguous-create')
        self.assertIsInstance(error,stage.subprocess.TimeoutExpired);self.assertFalse(receipt['completed'])
        self.assertFalse(resources);self.assertEqual(len(cleanup[-1]),1)

    def test_actual_execute_cleanup_failure_stays_incomplete(self):
        error,receipt,resources,cleanup=self.exercise('cleanup')
        self.assertIsInstance(error,RuntimeError);self.assertFalse(receipt['completed'])
        self.assertEqual(receipt['cleanup_error'],'RuntimeError');self.assertTrue(resources)

if __name__ == '__main__':unittest.main()
