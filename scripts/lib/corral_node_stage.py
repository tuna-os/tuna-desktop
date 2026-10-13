"""Explicit node-stage admission; never a renamed CDI RAW qualification."""
import hashlib
import json
import math
from pathlib import Path
import re
import subprocess
import time
import zipfile

from corral_artifact import gh_json, original_run, REPO
from corral_perf import GIB

TOOL_WORKFLOW = '.github/workflows/corral-node-tools.yml'
BASE_IMAGE = 'quay.io/kubevirt/virt-launcher@sha256:574fabe0ff9719fa1e3800dcca8530fa4e02fd71ef74109508e59800e13e8b5d'
INSTALLER_IMAGE = 'registry.access.redhat.com/ubi9/ubi@sha256:ee3871c273cc65a2bd24970b47c15b90fbf962841bc2e41e4f915b1c4c0208e2'
QEMU_SHA = '250b2b4ee552fdd96e1078525d12bb0b1f2557f33f153c282bfaea1ece5b3fad'


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def tool_contract(closure):
    if (closure.get('schema') != 1 or closure.get('base_image') != BASE_IMAGE
            or closure.get('installer_image') != INSTALLER_IMAGE
            or closure.get('python_version', [])[:2] != [3, 12]
            or closure.get('qemu_before_sha256') != QEMU_SHA
            or closure.get('qemu_after_sha256') != QEMU_SHA
            or closure.get('registry_published') is not True
            or closure.get('smoke_passed') is not True
            or closure.get('uid') != 107
            or not re.fullmatch(r'ghcr.io/tuna-os/benchmark-node-tools@sha256:[0-9a-f]{64}', closure.get('image', ''))):
        raise ValueError('actual published exact tool closure required')
    libraries = closure.get('qemu_libraries')
    if not isinstance(libraries, dict) or not libraries or any(not p.startswith('/') or not re.fullmatch('[0-9a-f]{64}', d) for p, d in libraries.items()):
        raise ValueError('actual qemu ELF library closure required')
    layers = closure.get('layers')
    if not isinstance(layers, list) or not 0 < len(layers) <= 128:
        raise ValueError('complete bounded OCI layer closure required')
    total = 0
    for layer in layers:
        for key in ('compressed_bytes', 'uncompressed_bytes'):
            if type(layer.get(key)) is not int or not 0 < layer[key] <= 4*GIB:
                raise ValueError('actual layer byte sizes required')
            total += layer[key]
        if not re.fullmatch('sha256:[0-9a-f]{64}', layer.get('digest', '')):
            raise ValueError('immutable layer digest required')
    worker = Path(__file__).with_name('corral_node_worker.py')
    if closure.get('worker_sha256') != sha(worker):
        raise ValueError('tool worker source differs from admitted observer')
    return total + GIB//2  # Config, extraction metadata and writable tool layer.


def load_tool(directory, source):
    """Bind a small original closure artifact to trusted manual source CI."""
    directory = Path(directory)
    receipt = json.loads((directory/'artifact-api.json').read_text())
    run = gh_json('actions/runs/'+str(receipt['workflow_run']['id']))
    original_run(run, source, TOOL_WORKFLOW)
    remote = gh_json('actions/artifacts/'+str(receipt['id']))
    if (remote != receipt or remote.get('expired') is not False
            or remote.get('name') != 'corral-node-tool-closure'
            or remote.get('workflow_run', {}).get('head_sha') != source
            or not 0 < remote.get('size_in_bytes', 0) <= 1024*1024):
        raise ValueError('original tool artifact identity differs')
    archive = directory/'original-artifact.zip'
    if archive.stat().st_size != remote['size_in_bytes'] or 'sha256:'+sha(archive) != remote['digest']:
        raise ValueError('original tool artifact bytes differ')
    with zipfile.ZipFile(archive) as z:
        entries = z.infolist()
        if len(entries) != 1 or entries[0].filename != 'tool-closure.json' or entries[0].file_size > 128*1024:
            raise ValueError('bounded single tool manifest required')
        closure = json.loads(z.read(entries[0]))
    if closure.get('source_commit') != source:
        raise ValueError('tool closure source differs')
    tool_contract(closure)
    return closure


def archive_contract(run, artifact, source, profile):
    original_run(run, source, '.github/workflows/performance-baseline.yml')
    if (artifact.get('expired') is not False or artifact.get('name') != 'marlin-benchmark-disk-'+profile
            or artifact.get('workflow_run', {}).get('id') != run.get('id')
            or artifact.get('workflow_run', {}).get('head_sha') != source
            or type(artifact.get('size_in_bytes')) is not int
            or not 0 < artifact['size_in_bytes'] <= 3*GIB
            or not re.fullmatch('sha256:[0-9a-f]{64}', artifact.get('digest', ''))):
        raise ValueError('original bounded disk archive identity differs')


def storage_plan(archive, closure):
    """Full target and rounded staging reservation, not sparse guessing."""
    size = archive['size_in_bytes']
    if type(size) is not int or not 0 < size <= 3*GIB:
        raise ValueError('exact original archive size required')
    # Until the authenticated ZIP central directory is available, reserve the
    # full existing safe-members expansion ceiling. Do not guess compressibility.
    stage = math.ceil((size + 3*GIB + GIB//2)/GIB)*GIB
    layers = tool_contract(closure)
    return {'method': 'node-stage', 'target_pvc_bytes': 20*GIB,
            'staging_pvc_bytes': stage, 'original_archive_bytes': size,
            'safe_expansion_ceiling_bytes': 3*GIB, 'tool_closure_allowance_bytes': layers,
            'guest_write_allowance_bytes': GIB//2,
            'estimated_peak_bytes': 20*GIB + stage + layers + GIB//2,
            'full_image_import_qualified': False, 'performance_qualified': False}


def pod_spec(name, namespace, node, stage, target, image, worker_sha):
    return {'apiVersion': 'v1', 'kind': 'Pod', 'metadata': {'name': name, 'namespace': namespace,
                                                'annotations': {'tuna-tools-worker-sha256': worker_sha}},
            'spec': {'restartPolicy': 'Never', 'automountServiceAccountToken': False,
                     'nodeSelector': {'kubernetes.io/hostname': node},
                     'securityContext': {'runAsUser': 107, 'runAsGroup': 107, 'fsGroup': 107,
                                         'runAsNonRoot': True, 'seccompProfile': {'type': 'RuntimeDefault'}},
                     'containers': [{'name': 'stage', 'image': image, 'imagePullPolicy': 'IfNotPresent',
                                     'command': ['/usr/bin/python3.12', '-c', 'import time; time.sleep(3600)'],
                                     'securityContext': {'allowPrivilegeEscalation': False,
                                                         'readOnlyRootFilesystem': True,
                                                         'capabilities': {'drop': ['ALL']}},
                                     'resources': {'requests': {'cpu': '100m', 'memory': '256Mi'},
                                                   'limits': {'cpu': '1', 'memory': '512Mi'}},
                                     'volumeMounts': [{'name': 'stage', 'mountPath': '/stage'},
                                                      {'name': 'target', 'mountPath': '/target'}]}],
                     'volumes': [{'name': 'stage', 'persistentVolumeClaim': {'claimName': stage}},
                                 {'name': 'target', 'persistentVolumeClaim': {'claimName': target}}]}}


def method_proof(directory, expected, closure, node_uid, context, namespace):
    """A separate exact-method tiny receipt is mandatory, never the CDI receipt."""
    directory = Path(directory)
    receipt_bytes = (directory/'receipt.json').read_bytes()
    receipt = json.loads(receipt_bytes)
    sources = receipt.get('source_sha256', {})
    digests = {'receipt.json': hashlib.sha256(receipt_bytes).hexdigest()}
    for name in ('corral_node_stage.py', 'corral_node_worker.py'):
        value = sha(directory/'source'/name)
        if value != sources.get(name) or value != sha(Path(__file__).with_name(name)):
            raise ValueError('actual tiny method source differs')
        digests['source/'+name] = value
    if hashlib.sha256(json.dumps(digests, sort_keys=True).encode()).hexdigest() != expected:
        raise ValueError('explicit original node-stage method evidence SHA differs')
    if (receipt.get('method') != 'node-stage' or receipt.get('completed') is not True
            or receipt.get('context') != context or receipt.get('namespace') != namespace
            or receipt.get('node_uid') != node_uid or receipt.get('image') != closure['image']
            or receipt.get('cleanup', {}).get('all_owned_resources_absent') is not True
            or receipt.get('independent_readback_matched') is not True
            or receipt.get('full_image_import_qualified') is not False):
        raise ValueError('separate actual tiny node-stage qualification required')
    return receipt


def execute(args):
    """Future admitted method execution. Source preparation does not admit a VM."""
    import os
    import signal
    import uuid
    import corral_owned
    import corral_ssh
    from corral_perf import capacity_receipt, kubectl
    from corral_visual import candidate_intent
    from gnome_reference import validate_image
    out = args.out
    if args.start:
        raise ValueError('node-stage disk preparation does not create or start a VM')
    deadline = time.monotonic()+1800
    receipt = {'schema': 1, 'method': 'node-stage', 'completed': False,
               'performance_qualified': False, 'context': args.context,
               'namespace': args.namespace, 'owned_resources_retained': False}
    owned = []
    processes = []
    nonce = uuid.uuid4().hex
    prefix = 'tuna-node-stage-'+nonce[:12]
    worker = Path(__file__).with_name('corral_node_worker.py')
    worker_source = worker.read_text()

    def remaining():
        value = deadline-time.monotonic()
        if value <= 0:
            raise TimeoutError('absolute node-stage deadline')
        return value

    def record(name, value):
        (out/name).write_text(json.dumps(value, indent=2)+'\n')

    def command(*argv, input=None, limit=45):
        result = subprocess.run(argv, input=input, capture_output=True, timeout=min(limit, remaining()))
        if result.returncode or len(result.stdout) > 4*1024*1024:
            raise RuntimeError('bounded node-stage command failed: '+argv[0])
        return result.stdout

    def gh(path):
        return json.loads(command('gh', 'api', 'repos/'+REPO+'/'+path))

    def api(*argv, input=None):
        return json.loads(command('kubectl', '--context', args.context, *argv, input=input))

    def register(kind, name, uid):
        if (kind, name, uid) not in owned:
            owned.append((kind, name, uid))
        record('owned-objects.json', owned)

    def create(kind, obj):
        obj['metadata'].setdefault('labels', {})['tuna-perf-nonce'] = nonce
        name = obj['metadata']['name']
        try:
            actual = api('create', '-f', '-', '-o', 'json', input=json.dumps(obj).encode())
        except BaseException:
            recovered = corral_owned.get(args.context, args.namespace, kind, name,
                                         timeout=min(30, max(.01, deadline-time.monotonic())))
            if recovered is not None:
                if recovered['metadata'].get('labels', {}).get('tuna-perf-nonce') != nonce:
                    raise ValueError('ambiguous node-stage create is foreign')
                register(kind, name, recovered['metadata']['uid'])
            raise
        if actual['metadata'].get('labels', {}).get('tuna-perf-nonce') != nonce:
            raise ValueError('created node-stage nonce differs')
        register(kind, name, actual['metadata']['uid'])
        return actual

    def observe(pod, closure):
        actual = api('get', 'pod', pod['metadata']['name'], '-n', args.namespace, '-o', 'json')
        if (actual['metadata']['uid'] != pod['metadata']['uid']
                or actual['spec'].get('nodeName') != args.node
                or actual['spec'].get('automountServiceAccountToken') is not False):
            raise ValueError('tool pod identity, placement or token policy differs')
        status = actual['status'].get('containerStatuses', [])
        if len(status) != 1 or status[0].get('imageID') != closure['image']:
            raise ValueError('actual immutable tool image differs')
        return actual

    def reobserve_claims(claims, phase):
        observations = []
        for expected in claims:
            claim = api('get', 'pvc', expected['metadata']['name'], '-n', args.namespace, '-o', 'json')
            if claim['metadata']['uid'] != expected['metadata']['uid'] or claim['status'].get('phase') != 'Bound':
                raise ValueError('owned PVC identity changed during '+phase)
            pv = corral_owned.placement(args.context, args.namespace, claim, args.node, timeout=min(30, remaining()))
            original = expected.setdefault('_observed_pv_uid', pv['metadata']['uid'])
            if original != pv['metadata']['uid']:
                raise ValueError('owned PV replaced during '+phase)
            observations.append({'pvc_name': claim['metadata']['name'], 'pvc_uid': claim['metadata']['uid'],
                                 'pv_uid': pv['metadata']['uid'], 'node': args.node})
        record('placement-'+phase+'.json', observations)

    def wait_pod(pod, closure):
        while True:
            actual = api('get', 'pod', pod['metadata']['name'], '-n', args.namespace, '-o', 'json')
            if actual['metadata']['uid'] != pod['metadata']['uid']:
                raise ValueError('tool pod replaced while starting')
            if actual['status'].get('phase') == 'Running':
                return observe(pod, closure)
            if actual['status'].get('phase') in ('Failed', 'Succeeded'):
                raise ValueError('tool pod terminated before operation')
            time.sleep(min(.5, remaining()))

    def fresh_capacity(plan, used=None):
        node = api('get', 'node', args.node, '-o', 'json')
        stats = api('get', '--raw', '/api/v1/nodes/'+args.node+'/proxy/stats/summary')
        if node['metadata']['uid'] != receipt['node_uid']:
            raise ValueError('admitted node identity changed')
        current_config = api('get', 'configmap', 'local-path-config', '-n', 'local-path-storage', '-o', 'json')['data']
        if hashlib.sha256(json.dumps(current_config, sort_keys=True).encode()).hexdigest() != receipt['local_path_config_sha256']:
            raise ValueError('storage backend configuration changed')
        budget = plan['estimated_peak_bytes']
        if used is not None:
            if set(used) != {'/stage', '/target'} or any(type(v) is not int or v < 0 for v in used.values()):
                raise ValueError('measured owned allocation required')
            if used['/stage'] > plan['staging_pvc_bytes'] or used['/target'] > plan['target_pvc_bytes']:
                raise ValueError('actual owned allocation exceeds full admitted PVC reservation')
            # OCI tar sizes are not attributable node filesystem allocation.
            # Keep the ENTIRE tool allowance even after image readiness. Credit
            # only actual owned PVC blocks; never mapped or layer estimates.
            budget -= sum(used.values())
        admission = capacity_receipt(node, stats, estimated_peak_bytes=budget)
        record('capacity-latest.json', admission)
        if not admission['ready_for_disk_admission']:
            raise ValueError('full node-stage storage reservation refused')

    def remote(pod, operation, *values):
        observe(pod, closure)
        return ['kubectl', '--context', args.context, 'exec', '-n', args.namespace,
                pod['metadata']['name'], '-c', 'stage', '--', '/usr/bin/python3.12', '-c', worker_source,
                operation, *map(str, values)]

    try:
        closure = load_tool(args.node_tool_closure, args.node_tool_source_sha)
        run = gh('actions/runs/'+str(args.run_id))
        artifact = gh('actions/artifacts/'+str(args.artifact_id))
        archive_contract(run, artifact, args.source_sha, args.profile)
        node = api('get', 'node', args.node, '-o', 'json')
        config = api('get', 'configmap', 'local-path-config', '-n', 'local-path-storage', '-o', 'json')['data']
        if config.get('setup') != '#!/bin/sh\nset -eu\nmkdir -m 0777 -p "$VOL_DIR"':
            raise ValueError('qualified directory-only storage backend required')
        receipt.update(node_uid=node['metadata']['uid'],
                       local_path_config_sha256=hashlib.sha256(json.dumps(config, sort_keys=True).encode()).hexdigest(),
                       original_artifact=artifact, tool_closure=closure,
                       observer_sha256=sha(Path(__file__)), worker_sha256=sha(worker))
        proof = method_proof(args.node_method_proof, args.node_method_proof_sha, closure, receipt['node_uid'], args.context, args.namespace)
        if proof.get('local_path_config_sha256') != receipt['local_path_config_sha256']:
            raise ValueError('tiny method backend configuration differs')
        plan = storage_plan(artifact, closure)
        record('storage-plan.json', plan)
        fresh_capacity(plan)
        stage_name, target_name = prefix+'-stage', prefix+'-disk'
        claims = []
        for name, size in ((stage_name, plan['staging_pvc_bytes']), (target_name, 20*GIB)):
            claim = create('pvc', {'apiVersion': 'v1', 'kind': 'PersistentVolumeClaim',
                                  'metadata': {'name': name, 'namespace': args.namespace},
                                  'spec': {'storageClassName': 'local-path', 'volumeMode': 'Filesystem',
                                           'accessModes': ['ReadWriteOnce'],
                                           'resources': {'requests': {'storage': str(size)}}}})
            claims.append(claim)
        pod = create('pod', pod_spec(prefix+'-worker', args.namespace, args.node, stage_name,
                                     target_name, closure['image'], sha(worker)))
        wait_pod(pod, closure)
        reobserve_claims(claims, 'before-receive')
        # No GitHub token is passed to Kubernetes. Only the authenticated bytes
        # travel over host-owned gh stdout and authenticated exec stdin.
        fresh_capacity(plan, json.loads(command(*remote(pod, 'usage'))))
        exec_argv = remote(pod, 'receive', artifact['size_in_bytes'], artifact['digest'][7:])
        exec_argv.insert(exec_argv.index('--'), '-i')
        producer = subprocess.Popen(['gh', 'api', 'repos/'+REPO+'/actions/artifacts/'+str(args.artifact_id)+'/zip'],
                                    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, start_new_session=True)
        processes.append(producer)
        result_path = out/'receive-result.json'
        with result_path.open('xb') as result:
            consumer = subprocess.Popen(exec_argv, stdin=producer.stdout, stdout=result,
                                        stderr=subprocess.DEVNULL, start_new_session=True)
            processes.append(consumer)
            producer.stdout.close()
            while producer.poll() is None or consumer.poll() is None:
                remaining()
                if result_path.stat().st_size > 1024*1024:
                    raise ValueError('node archive receipt exceeds bound')
                used = json.loads(command(*remote(pod, 'usage')))
                fresh_capacity(plan, used)
                time.sleep(min(1, remaining()))
        if producer.returncode or consumer.returncode:
            raise ValueError('exact authenticated archive stream failed')
        observe(pod, closure)  # exec is name-based: reject surrounding UID drift.
        reobserve_claims(claims, 'after-receive')
        received = json.loads(result_path.read_text())
        manifest = received['manifest']
        if (manifest.get('export_succeeded') is not True or manifest.get('observer_source_commit') != args.source_sha
                or manifest.get('gnome_profile') != args.profile or manifest.get('paired_desktops') != ['gnome', 'tuna']
                or manifest.get('disk_sha256') != received['disk_sha256']
                or manifest.get('storage', {}).get('virtual_bytes') != 18*GIB
                or manifest.get('requested_pvc') != '20Gi' or manifest.get('firmware') != 'uefi'):
            raise ValueError('received original export contract differs')
        candidate_intent(manifest, args.candidate_sha, args.package_run)
        validate_image(manifest['reference'], args.profile)
        package = manifest['package_selection']
        original_run(gh('actions/runs/'+str(package['run_id'])), package['head_sha'], '.github/workflows/ci.yml')
        receipt['export_ssh'] = corral_ssh.export_policy(manifest)
        fresh_capacity(plan, json.loads(command(*remote(pod, 'usage'))))
        converted = json.loads(command(*remote(pod, 'convert', min(900, remaining())), limit=remaining()))
        if converted.get('raw_sha256') != manifest['raw_sha256'] or converted.get('logical_bytes') != 18*GIB:
            raise ValueError('actual node conversion receipt differs')
        observe(pod, closure)
        reobserve_claims(claims, 'after-convert')
        fresh_capacity(plan, json.loads(command(*remote(pod, 'usage'))))
        record('node-conversion.json', converted)
        observe(pod, closure)
        corral_owned.cleanup(args.context, args.namespace, [('pod', pod['metadata']['name'], pod['metadata']['uid'])],
                             timeout=min(180, remaining()))
        owned.remove(('pod', pod['metadata']['name'], pod['metadata']['uid']))
        readback_spec = pod_spec(prefix+'-readback', args.namespace, args.node, stage_name,
                                 target_name, closure['image'], sha(worker))
        for mount in readback_spec['spec']['containers'][0]['volumeMounts']:
            mount['readOnly'] = True
        for volume in readback_spec['spec']['volumes']:
            volume['persistentVolumeClaim']['readOnly'] = True
        readback = create('pod', readback_spec)
        wait_pod(readback, closure)
        verified = json.loads(command(*remote(readback, 'readback'), limit=remaining()))
        if verified != {'raw_sha256': manifest['raw_sha256'], 'logical_bytes': 18*GIB}:
            raise ValueError('independent final PVC readback differs')
        observe(readback, closure)
        reobserve_claims(claims, 'after-readback')
        final_used = json.loads(command(*remote(readback, 'usage')))
        fresh_capacity(plan, final_used)
        receipt['independent_readback'] = {'pod_uid': readback['metadata']['uid'],
                                           'pvc_uid': claims[1]['metadata']['uid'], **verified}
        temporary = [q for q in owned if q[0] == 'pod' or q[1] == stage_name]
        receipt['temporary_cleanup'] = corral_owned.cleanup(args.context, args.namespace, temporary,
                                                           timeout=min(180, remaining()))
        owned = [q for q in owned if q not in temporary]
        reobserve_claims([claims[1]], 'before-complete')
        fresh_capacity(plan, {'/stage': 0, '/target': final_used['/target']})
        # Actual VM admission is intentionally a separate boundary. The final
        # disk receipt is retained, but no source-only method may fabricate boot.
        receipt.update(completed=True, upload_completed=False, node_stage_completed=True,
                       pvc_name=target_name, pvc_uid=claims[1]['metadata']['uid'],
                       raw_sha256=manifest['raw_sha256'], disk_sha256=manifest['disk_sha256'],
                       manifest_sha256=received['manifest_sha256'], owned_resources_retained=True,
                       vm_created=False, limitation='Verified disk only; explicit VM attachment/boot still required.')
        return receipt
    except BaseException as error:
        receipt['error_type'] = type(error).__name__
        receipt['error'] = str(error)[:1024]
        raise
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
        if not receipt['completed'] and owned:
            try:
                receipt['cleanup'] = corral_owned.cleanup(args.context, args.namespace, owned, timeout=180)
            except Exception as error:
                receipt['cleanup_error'] = type(error).__name__
        record('node-stage-receipt.json', receipt)
