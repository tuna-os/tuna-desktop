"""Synthetic OCI custody controls and actual importer two-run execution."""
import copy
import gzip
import hashlib
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import time
import unittest
from unittest import mock
import zipfile
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'lib'))
import corral_node_publish as pub
import corral_node_stage as stage
import test_corral_node_stage as fixtures


def digest(data):return 'sha256:'+hashlib.sha256(data).hexdigest()


def run(source,identity):
    return {'id':identity,'status':'completed','conclusion':'success','event':'workflow_dispatch',
            'head_sha':source,'run_attempt':1,'path':pub.WORKFLOW,
            'repository':{'full_name':pub.REPO},'head_repository':{'full_name':pub.REPO}}


def artifact(name,data,identity,source,run_id):
    return {'id':identity,'name':name,'size_in_bytes':len(data),'digest':digest(data),'expired':False,
            'workflow_run':{'id':run_id,'head_sha':source}}


def published_context(mode='publish'):
    root=Path(pub.__file__).resolve().parents[2]
    blobs={}
    for name in pub.publisher_sources():
        body=(root/name).read_bytes();blobs[name]=hashlib.sha1(b'blob '+str(len(body)).encode()+b'\0'+body).hexdigest()
    return {'run_id':2,'run_attempt':1,'job_id':20,'job':'node-tools-publish' if mode=='publish' else 'node-tools-anonymous-verify',
            'focused_job_id':21,'checkout_tree':'c'*40,'executing_git_blobs':blobs}


def chain_kwargs(mode='publish'):
    return {'publisher_commit':{'sha':'b'*40,'tree':{'sha':'c'*40}},
            'publisher_jobs':{'total_count':2,'jobs':[{'id':20,'name':published_context(mode)['job'],
                'run_id':2,'run_attempt':1,'head_sha':'b'*40,'status':'completed','conclusion':'success'},
                {'id':21,'name':'focused','run_id':2,'run_attempt':1,'head_sha':'b'*40,'status':'completed','conclusion':'success'}]}}


class Tests(unittest.TestCase):
    def fixture(self,root):
        # A real compressed layer whose decompressed bytes differ from its blob.
        raw=b'synthetic immutable layer\n'*10
        layer=gzip.compress(raw,mtime=0)
        config=json.dumps({'architecture':'amd64','os':'linux','rootfs':{'diff_ids':[digest(raw)]}}).encode()
        manifest=json.dumps({'schemaVersion':2,'config':{'digest':digest(config),'size':len(config)},
                             'layers':[{'digest':digest(layer),'size':len(layer),'mediaType':'application/vnd.oci.image.layer.v1.tar+gzip'}]}).encode()
        index=json.dumps({'manifests':[{'digest':digest(manifest),'size':len(manifest)}]}).encode()
        path=root/'original.oci.tar'
        with tarfile.open(path,'w') as archive:
            for name,data in [('index.json',index),('blobs/sha256/'+digest(manifest)[7:],manifest),
                              ('blobs/sha256/'+digest(config)[7:],config),('blobs/sha256/'+digest(layer)[7:],layer)]:
                item=tarfile.TarInfo(name);item.size=len(data);archive.addfile(item,io.BytesIO(data))
        closure=fixtures.Tests().closure()
        closure.update(source_commit='a'*40,registry_published=False,source_files_sha256={n:'e'*64 for n in pub.SOURCES},
                       image=pub.TARGET+'@'+digest(manifest),oci_sha256=pub.sha(path),
                       original_manifest=json.loads(manifest),config=json.loads(config),
                       layers=[{'digest':digest(layer),'diff_id':digest(raw),'compressed_bytes':len(layer),'uncompressed_bytes':len(raw)}])
        anonymous=root/'anonymous';anonymous.mkdir()
        (anonymous/'manifest.json').write_bytes(manifest)
        (anonymous/digest(config)[7:]).write_bytes(config)
        (anonymous/digest(layer)[7:]).write_bytes(layer)
        return path,closure,manifest,config,anonymous

    def envelope(self,closure,anonymous):
        return {'schema':1,'completed':True,'mode':'publish','authenticated_copy_succeeded':True,
                'current_publisher_admission':published_context(),
                'built_source_commit':'a'*40,'publisher_source_commit':'b'*40,'build_run_id':1,'publisher_run_id':2,
                'registry_published':True,'private_scratch_removed':True,'image':closure['image'],'original_oci_sha256':closure['oci_sha256'],
                'built_source_sha256':closure['source_files_sha256'],'publisher_source_sha256':pub.publisher_sources(),
                'registry_manifest_digest':closure['image'].split('@')[1],'anonymous_pull':anonymous}

    def test_actual_oci_and_anonymous_all_blobs(self):
        with tempfile.TemporaryDirectory() as d:
            p,c,m,k,a=self.fixture(Path(d))
            pub.immutable_build(c,'a'*40)
            self.assertEqual(pub.verify_oci(p,c),(m,k))
            actual=pub.anonymous_copy(a,m,k,c)
            self.assertTrue(actual['all_blobs_verified'])
            pub.publication_chain(self.envelope(c,actual),run('a'*40,1),run('b'*40,2),'a'*40,'b'*40,c,**chain_kwargs())

    def test_oci_hash_diffid_metadata_and_deadline_negatives(self):
        with tempfile.TemporaryDirectory() as d:
            p,c,*_=self.fixture(Path(d))
            cases=[]
            for key,value in [('oci_sha256','0'*64),('image',pub.TARGET+'@sha256:'+'0'*64)]:
                altered=copy.deepcopy(c);altered[key]=value;cases.append(altered)
            altered=copy.deepcopy(c);altered['layers'][0]['diff_id']='sha256:'+'0'*64;cases.append(altered)
            altered=copy.deepcopy(c);altered['config']['rootfs']['diff_ids']=[];cases.append(altered)
            for altered in cases:
                with self.assertRaises(ValueError):pub.verify_oci(p,altered)
            with self.assertRaises(TimeoutError):pub.verify_oci(p,c,deadline=time.monotonic()-1)

    def test_actual_manifest_and_config_declared_sizes_are_required(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);path,c,*_=self.fixture(root)
            with tarfile.open(path) as archive:
                entries={m.name:archive.extractfile(m).read() for m in archive if m.isfile()}
            original=copy.deepcopy(entries)
            index=json.loads(entries['index.json']);index['manifests'][0]['size']+=1
            entries['index.json']=json.dumps(index).encode()
            for changed in (entries,original):
                if changed is original:
                    manifest=copy.deepcopy(c['original_manifest']);manifest['config']['size']+=1
                    raw=json.dumps(manifest).encode();changed['blobs/sha256/'+digest(raw)[7:]]=raw
                    index=json.loads(changed['index.json']);index['manifests'][0]={'digest':digest(raw),'size':len(raw)}
                    changed['index.json']=json.dumps(index).encode()
                    c.update(image=pub.TARGET+'@'+digest(raw),original_manifest=manifest)
                with tarfile.open(path,'w') as archive:
                    for name,raw in changed.items():
                        entry=tarfile.TarInfo(name);entry.size=len(raw);archive.addfile(entry,io.BytesIO(raw))
                c['oci_sha256']=pub.sha(path)
                with self.assertRaisesRegex(ValueError,'descriptor size'):pub.verify_oci(path,c)

    def test_anonymous_missing_corrupt_and_symlink_blobs(self):
        with tempfile.TemporaryDirectory() as d:
            p,c,m,k,a=self.fixture(Path(d));layer=a/c['layers'][0]['digest'][7:]
            original=layer.read_bytes();layer.write_bytes(b'bad')
            with self.assertRaises(ValueError):pub.anonymous_copy(a,m,k,c)
            layer.unlink()
            with self.assertRaises(FileNotFoundError):pub.anonymous_copy(a,m,k,c)
            other=Path(d)/'other';other.write_bytes(original);layer.symlink_to(other)
            with self.assertRaises(ValueError):pub.anonymous_copy(a,m,k,c)

    def test_chain_requires_both_successful_original_sources_and_all_blobs(self):
        with tempfile.TemporaryDirectory() as d:
            p,c,m,k,a=self.fixture(Path(d));e=self.envelope(c,pub.anonymous_copy(a,m,k,c))
            for key,value in [('completed',False),('publisher_source_commit','c'*40),('original_oci_sha256','0'*64),
                              ('registry_manifest_digest','sha256:'+'0'*64),('authenticated_copy_succeeded',False),
                              ('built_source_sha256',{}),('publisher_source_sha256',{}),('private_scratch_removed',False),('current_publisher_admission',{})]:
                wrong=copy.deepcopy(e);wrong[key]=value
                with self.assertRaises(ValueError):pub.publication_chain(wrong,run('a'*40,1),run('b'*40,2),'a'*40,'b'*40,c,**chain_kwargs())
            wrong=copy.deepcopy(e);wrong['anonymous_pull']['blob_sha256']={}
            with self.assertRaises(ValueError):pub.publication_chain(wrong,run('a'*40,1),run('b'*40,2),'a'*40,'b'*40,c,**chain_kwargs())
            for identity in (1,2):
                runs=[run('a'*40,1),run('b'*40,2)];runs[identity-1]['path']='unregistered.yml'
                with self.assertRaises(ValueError):pub.publication_chain(e,*runs,'a'*40,'b'*40,c,**chain_kwargs())
            e.update(mode='verify-only',authenticated_copy_succeeded=False,current_publisher_admission=published_context('verify-only'))
            pub.publication_chain(e,run('a'*40,1),run('b'*40,2),'a'*40,'b'*40,c,**chain_kwargs('verify-only'))

    def test_actual_loader_preserves_build_flag_and_validates_nested_zip(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);p,c,m,k,a=self.fixture(root)
            envelope=self.envelope(c,pub.anonymous_copy(a,m,k,c))
            original=io.BytesIO()
            with zipfile.ZipFile(original,'w') as z:z.writestr('tool-closure.json',json.dumps(c))
            built=original.getvalue()
            api=artifact('corral-node-tool-closure',built,10,'a'*40,1)
            original_api=artifact('corral-node-tool-originals',b'original zip',11,'a'*40,1)
            def wrap(data):
                path=root/'original-artifact.zip'
                with zipfile.ZipFile(path,'w') as z:
                    for name,body in [('publication.json',json.dumps(envelope)),('build-artifact-api.json',json.dumps(api)),
                                      ('originals-artifact-api.json',json.dumps(original_api)),('build-closure.zip',data)]:z.writestr(name,body)
                receipt=artifact('corral-node-tool-publication',path.read_bytes(),12,'b'*40,2)
                (root/'artifact-api.json').write_text(json.dumps(receipt))
                return receipt
            receipt=wrap(built)
            def gh(path):return {'actions/runs/1':run('a'*40,1),'actions/runs/2':run('b'*40,2),
                                 'actions/artifacts/10':api,'actions/artifacts/11':original_api,
                                 'actions/artifacts/12':receipt,'git/commits/'+'b'*40:chain_kwargs()['publisher_commit'],
                                 'actions/runs/2/attempts/1/jobs?per_page=100':chain_kwargs()['publisher_jobs']}[path]
            with mock.patch.object(stage,'gh_json',side_effect=gh):
                actual=stage.load_tool(root,'a'*40,'b'*40)
                self.assertIs(actual['registry_published'],False)
                self.assertTrue(actual['publication_evidence']['registry_published'])
                with self.assertRaises(ValueError):stage.load_tool(root,'a'*40,'c'*40)
                receipt=wrap(b'changed nested original')
                with self.assertRaises(ValueError):stage.load_tool(root,'a'*40,'b'*40)

    def test_safe_single_closure_and_publication_members(self):
        for name in ('../tool-closure.json','tool-closure.json'):
            buf=io.BytesIO()
            with zipfile.ZipFile(buf,'w') as z:
                z.writestr(name,'{}');z.writestr('unexpected','bad')
            buf.seek(0)
            with self.assertRaises(ValueError):pub.read_build_closure(buf)

    def test_real_stalled_dribbling_and_oversized_command_retirement(self):
        import importlib.machinery
        import importlib.util
        import os
        loader=importlib.machinery.SourceFileLoader('publisher_fixture',str(Path(__file__).resolve().parents[1]/'tuna-corral-node-tools-publish'))
        spec=importlib.util.spec_from_loader(loader.name,loader)
        module=importlib.util.module_from_spec(spec);loader.exec_module(module)
        scripts=["import os,time;print(os.getpid(),flush=True);time.sleep(20)",
                 "import os,time;print(os.getpid(),flush=True);exec('while True: print(1,flush=True);time.sleep(.01)')"]
        for script in scripts:
            with tempfile.TemporaryDirectory() as d:
                output=Path(d)/'output';begin=time.monotonic()
                with self.assertRaises(TimeoutError):
                    module.bounded_command([sys.executable,'-c',script],output,begin+.2)
                self.assertLess(time.monotonic()-begin,2)
                pid=int(output.read_text().splitlines()[0])
                with self.assertRaises(ProcessLookupError):os.kill(pid,0)
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaises((RuntimeError,TimeoutError)):
                module.bounded_command([sys.executable,'-c','print("x"*100)'],Path(d)/'output',time.monotonic()+2,limit=1)
            with self.assertRaises(ValueError):
                module.bounded_command([sys.executable,'-c','import time;time.sleep(20)'],Path(d)/'output',time.monotonic()+2,data=b'x'*4097)

    def test_current_context_and_remote_blob_admission_negatives(self):
        import base64
        source='b'*40
        environment={'GITHUB_RUN_ID':'2','GITHUB_RUN_ATTEMPT':'1','GITHUB_REPOSITORY':pub.REPO,
                     'GITHUB_EVENT_NAME':'workflow_dispatch','GITHUB_SHA':source,'GITHUB_JOB':'node-tools-publish',
                     'GITHUB_REF':'refs/heads/fixture','GITHUB_WORKFLOW_REF':pub.REPO+'/'+pub.WORKFLOW+'@refs/heads/fixture'}
        current=run(source,2);current.update(status='in_progress',conclusion=None,run_attempt=1,head_branch='fixture')
        jobs={'total_count':2,'jobs':[{'id':20,'name':'node-tools-publish','run_id':2,'run_attempt':1,'head_sha':source,'status':'in_progress','conclusion':None},
                                    {'id':21,'name':'focused','run_id':2,'run_attempt':1,'head_sha':source,'status':'completed','conclusion':'success'}]}
        checkout={'head':source,'tree':'c'*40,'clean':True}
        commit={'sha':source,'tree':{'sha':'c'*40}}
        blobs={}
        for name in pub.publisher_sources():
            raw=(Path(pub.__file__).resolve().parents[2]/name).read_bytes()
            blobs[name]={'path':name,'type':'file','size':len(raw),'encoding':'base64',
                         'sha':hashlib.sha1(b'blob '+str(len(raw)).encode()+b'\0'+raw).hexdigest(),'content':base64.b64encode(raw).decode()}
        args=[current,jobs,environment,source,'publish',checkout,blobs,commit]
        self.assertEqual(pub.current_admission(*args)['job_id'],20)
        for index,key,value in [(0,'status','completed'),(0,'id',99),(0,'run_attempt',2),
                                (2,'GITHUB_REPOSITORY','other/repo'),(2,'GITHUB_RUN_ID','99'),
                                (2,'GITHUB_RUN_ATTEMPT','2'),(2,'GITHUB_JOB','node-tools-anonymous-verify'),
                                (2,'GITHUB_SHA','a'*40),(5,'clean',False),(5,'head','a'*40),(5,'tree','d'*40)]:
            wrong=copy.deepcopy(args);wrong[index][key]=value
            with self.subTest(index=index,key=key),self.assertRaises(ValueError):pub.current_admission(*wrong)
        for index,key,value in [(1,'jobs',[]),(6,next(iter(blobs)),{**next(iter(blobs.values())),'sha':'0'*40}),
                                (7,'tree',{'sha':'0'*40})]:
            wrong=copy.deepcopy(args);wrong[index][key]=value
            with self.assertRaises(ValueError):pub.current_admission(*wrong)
        wrong=copy.deepcopy(args);wrong[1]['jobs'][1]['conclusion']='failure'
        with self.assertRaises(ValueError):pub.current_admission(*wrong)

    def test_exited_leader_with_stdout_stderr_or_escaped_child_is_refused_and_reaped(self):
        import importlib.machinery
        import importlib.util
        import os
        loader=importlib.machinery.SourceFileLoader('publisher_children_fixture',str(Path(__file__).resolve().parents[1]/'tuna-corral-node-tools-publish'))
        module=importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name,loader));loader.exec_module(module)
        for stderr,escape in ((False,False),(True,False),(True,True)):
            with self.subTest(stderr=stderr,escape=escape),tempfile.TemporaryDirectory() as d:
                root=Path(d);identity=root/'child'
                code="import os,time;pid=os.fork();\nif pid: time.sleep(.08);os._exit(0)\n"+"if "+str(escape)+":os.setsid()\n"+"open("+repr(str(identity))+",'w').write(str(os.getpid()))\n"+"exec('while True: os.write("+('2' if stderr else '1')+",b\"writer\\n\");time.sleep(.01)')"
                begin=time.monotonic()
                with self.assertRaises(RuntimeError):module.bounded_command([sys.executable,'-c',code],root/'output',begin+2)
                self.assertLess(time.monotonic()-begin,2)
                pid=int(identity.read_text())
                with self.assertRaises(ProcessLookupError):os.kill(pid,0)
                self.assertEqual(Path('/proc/self/task/'+str(os.getpid())+'/children').read_text().strip(),'')

    def test_actual_publisher_cli_copy_and_anonymous_failure_receipts(self):
        import os
        import subprocess
        for verify_only,fail in ((False,False),(True,False),(False,True),(False,'dirty')):
            with self.subTest(verify_only=verify_only,fail=fail), tempfile.TemporaryDirectory() as d:
                root=Path(d);oci,c,m,k,anonymous=self.fixture(root)
                source=b'fixed original source'
                c['source_files_sha256']={name:hashlib.sha256(source).hexdigest() for name in pub.SOURCES}
                build_zip=root/'build.zip'
                with zipfile.ZipFile(build_zip,'w') as z:z.writestr('tool-closure.json',json.dumps(c))
                originals=root/'originals.zip'
                with zipfile.ZipFile(originals,'w') as z:
                    z.write(oci,'original-tools.oci.tar')
                    z.writestr('smoke.json',json.dumps({'uid':107,'smoke_passed':True}))
                    for name in ('rpm-inventory.txt','source-sha256.txt'):z.writestr(name,'fixture')
                api=artifact('corral-node-tool-closure',build_zip.read_bytes(),10,'a'*40,1)
                original_api=artifact('corral-node-tool-originals',originals.read_bytes(),11,'a'*40,1)
                data={'actions/runs/1':run('a'*40,1),'actions/runs/2':run('b'*40,2),
                      'actions/artifacts/10':api,'actions/artifacts/11':original_api}
                current=data['actions/runs/2'];current.update(status='in_progress',conclusion=None,run_attempt=1,head_branch='fixture')
                job='node-tools-anonymous-verify' if verify_only else 'node-tools-publish'
                data['actions/runs/2/attempts/1/jobs?per_page=100']={'total_count':2,'jobs':[
                    {'id':20,'name':job,'run_id':2,'run_attempt':1,'head_sha':'b'*40,'status':'in_progress','conclusion':None},
                    {'id':21,'name':'focused','run_id':2,'run_attempt':1,'head_sha':'b'*40,'status':'completed','conclusion':'success'}]}
                data['git/commits/'+'b'*40]={'sha':'b'*40,'tree':{'sha':'c'*40}}
                import base64
                checkout=Path(pub.__file__).resolve().parents[2]
                for name in pub.publisher_sources():
                    body=(checkout/name).read_bytes()
                    data['contents/'+name+'?ref='+'b'*40]={'path':name,'type':'file','size':len(body),'encoding':'base64',
                         'sha':hashlib.sha1(b'blob '+str(len(body)).encode()+b'\0'+body).hexdigest(),'content':base64.b64encode(body).decode()}
                (root/'api.json').write_text(json.dumps(data))
                (root/'digest').write_text(c['image'].split('@')[1])
                bin_path=root/'bin';bin_path.mkdir()
                prefix='#!'+sys.executable+'\n'
                (bin_path/'gh').write_text(prefix+"""
import json,os,sys
from pathlib import Path
r=Path(os.environ['FAKE_ROOT']);path=sys.argv[-1].split('repos/tuna-os/tuna-desktop/')[1]
if path=='actions/artifacts/10/zip':sys.stdout.buffer.write((r/'build.zip').read_bytes())
elif path=='actions/artifacts/11/zip':sys.stdout.buffer.write((r/'originals.zip').read_bytes())
elif path.startswith('contents/') and path.endswith('a'*40):sys.stdout.buffer.write(b'fixed original source')
else:print(json.dumps(json.loads((r/'api.json').read_text())[path]))
""")
                (bin_path/'skopeo').write_text(prefix+"""
import os,sys,json,shutil
from pathlib import Path
r=Path(os.environ['FAKE_ROOT']);a=sys.argv[1:]
assert 'NODE_TOOL_TOKEN' not in os.environ and 'GH_TOKEN' not in os.environ
with (r/'calls.jsonl').open('a') as f:f.write(json.dumps(a)+'\\n')
if a[0]=='--version':print('skopeo synthetic test')
elif a[0]=='login':
 auth=Path(a[a.index('--authfile')+1]);assert auth.exists() and auth.stat().st_mode&0o777==0o600
 secret=sys.stdin.buffer.read();assert secret==b'private-test-token'
 Path(a[a.index('--authfile')+1]).write_text('{"auths":{"fake":"private-test-token"}}')
elif a[0]=='inspect':
 if a[-1].startswith('docker:') and os.environ.get('FAKE_FAIL'):sys.exit(2)
 sys.stdout.buffer.write((r/'anonymous'/'manifest.json').read_bytes())
elif a[0]=='copy':
 if '--digestfile' in a:Path(a[a.index('--digestfile')+1]).write_text((r/'digest').read_text())
 else:
  assert '--src-no-creds' in a
  assert json.loads(Path(a[a.index('--src-authfile')+1]).read_text())=={'auths':{}}
  shutil.copytree(r/'anonymous',Path(a[-1].removeprefix('dir:')))
else:sys.exit(3)
""")
                (bin_path/'git').write_text(prefix+"import os,sys\narg=sys.argv[-1]\nif arg=='HEAD':print('b'*40)\nelif arg=='HEAD^{tree}':print('c'*40)\nelif os.environ.get('FAKE_DIRTY'):print('?? untracked')\n")
                for item in bin_path.iterdir():item.chmod(0o700)
                env={**os.environ,'PATH':str(bin_path)+os.pathsep+os.environ['PATH'],'FAKE_ROOT':str(root),
                     'NODE_TOOL_TOKEN':'private-test-token','GH_TOKEN':'private-gh-token','GITHUB_ACTOR':'fixture',
                     'GITHUB_RUN_ID':'2','GITHUB_RUN_ATTEMPT':'1','GITHUB_REPOSITORY':pub.REPO,
                     'GITHUB_EVENT_NAME':'workflow_dispatch','GITHUB_SHA':'b'*40,'GITHUB_JOB':job,
                     'GITHUB_REF':'refs/heads/fixture','GITHUB_WORKFLOW_REF':pub.REPO+'/'+pub.WORKFLOW+'@refs/heads/fixture',
                     'GITHUB_WORKSPACE':str(checkout)}
                if fail is True:env['FAKE_FAIL']='1'
                if fail=='dirty':env['FAKE_DIRTY']='1'
                output=root/'receipt'
                argv=[sys.executable,str(Path(__file__).resolve().parents[1]/'tuna-corral-node-tools-publish'),
                      '--build-run','1','--closure-artifact','10','--originals-artifact','11','--built-source','a'*40,
                      '--publisher-source','b'*40,'--publisher-run','2','--out',str(output)]
                if verify_only:argv.append('--verify-only')
                result=subprocess.run(argv,env=env,capture_output=True,timeout=15)
                self.assertEqual(result.returncode==0,not bool(fail),result.stderr.decode())
                receipt=json.loads((output/'publication.json').read_text())
                self.assertEqual(receipt['completed'],not bool(fail))
                self.assertEqual(receipt['authenticated_copy_succeeded'],not verify_only and fail!='dirty')
                self.assertFalse((root/'receipt-private').exists())
                self.assertNotIn(b'private-test-token',(output/'publication.json').read_bytes())
                if not fail:
                    pub.publication_chain(receipt,run('a'*40,1),run('b'*40,2),'a'*40,'b'*40,c,**chain_kwargs(receipt['mode']))
                if fail=='dirty':
                    self.assertFalse((root/'calls.jsonl').exists())
                    continue
                calls=(root/'calls.jsonl').read_text()
                self.assertIn('--preserve-digests',calls)
                self.assertEqual('login' in calls,not verify_only)

    def test_registered_jobs_scope_permissions_and_opt_in(self):
        source=(Path(__file__).resolve().parents[2]/'.github/workflows/corral-visual-tools.yml').read_text()
        publish=source.split('  node-tools-publish:\n')[1].split('  node-tools-anonymous-verify:\n')[0]
        verify=source.split('  node-tools-anonymous-verify:\n')[1]
        self.assertEqual(source.count('packages: write'),1)
        self.assertIn('needs: focused',publish)
        self.assertIn('      actions: read',publish)
        self.assertIn('      actions: read',verify)
        self.assertIn("github.event_name == 'workflow_dispatch'",publish)
        self.assertIn("github.repository == 'tuna-os/tuna-desktop'",publish)
        self.assertIn('default: disabled',source)
        self.assertIn('--verify-only',verify)
        self.assertNotIn('NODE_TOOL_TOKEN',verify)
        self.assertNotIn('packages: write',verify)


if __name__=='__main__':unittest.main()
