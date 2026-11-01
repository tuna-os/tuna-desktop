"""Receipt-bound Corral selectors and narrowly owned qualification cleanup.

Inventory IDs are opaque. Deletion uses exact Kubernetes API paths and atomic
UID preconditions, never Corral's broad derived-name/label-selector purge.
Only explicitly projected safe metadata is requested, never Secret payloads.
"""
import datetime
import json
from pathlib import Path
import subprocess
from urllib.parse import quote


def timestamp():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec='seconds')


def save_identity(path, state):
    path = Path(path)
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(state, indent=2) + '\n')
    temporary.replace(path)


def safe_delete_output(value):
    """Project only Kubernetes identity/Status fields before retaining output."""
    receipt = {'present': bool(value), 'redacted': True}
    try:
        payload = json.loads(value) if value else None
    except (ValueError, TypeError, UnicodeError):
        payload = None
    if not isinstance(payload, dict):
        return json.dumps(receipt)
    receipt['json_object'] = True
    kind = payload.get('kind')
    if kind not in ('Status', 'Secret', 'VirtualMachine', 'PersistentVolumeClaim'):
        return json.dumps(receipt)
    projection = {'kind': kind}
    metadata = payload.get('metadata')
    if isinstance(metadata, dict):
        projection['metadata'] = {key: metadata[key] for key in
            ('name', 'namespace', 'uid', 'deletionTimestamp') if isinstance(metadata.get(key), str)}
    if kind == 'Status':
        if payload.get('status') in ('Success', 'Failure'):
            projection['status'] = payload['status']
        code = payload.get('code')
        if type(code) is int and 100 <= code <= 599:
            projection['code'] = code
        if payload.get('reason') in (
                'Unauthorized', 'Forbidden', 'NotFound', 'AlreadyExists', 'Conflict', 'Gone',
                'Invalid', 'ServerTimeout', 'Timeout', 'TooManyRequests', 'BadRequest',
                'MethodNotAllowed', 'NotAcceptable', 'RequestEntityTooLarge',
                'UnsupportedMediaType', 'InternalError', 'Expired', 'ServiceUnavailable', 'Unknown'):
            projection['reason'] = payload['reason']
    receipt['projection'] = projection
    return json.dumps(receipt)


class Identity:
    def __init__(self, command, state, persist=lambda state: None):
        self.command, self.state, self.persist = command, state, persist

    def save(self):
        self.persist(self.state)

    def invoke(self, argv, timeout=120, input=None):
        try:
            kwargs = {'timeout': timeout}
            if input is not None:
                kwargs['input'] = input
            result = self.command(argv, **kwargs)
        except subprocess.TimeoutExpired as error:
            def text(value):
                return value.decode(errors='replace') if isinstance(value, bytes) else value
            result = {'argv': argv, 'timeout': timeout, 'exit': None,
                      'stdout': text(error.stdout), 'stderr': text(error.stderr)}
        except OSError as error:
            result = {'argv': argv, 'exit': None, 'stdout': '', 'stderr': str(error)}
        if argv[0] == 'kubectl' and 'delete' in argv and '--raw' in argv:
            # A foreground DELETE can return the full Secret, including data and
            # kubectl's last-applied annotation. Sanitize BOTH streams and timeout
            # partials before state persistence, returned receipts, or exceptions.
            safe = {'argv': argv, 'exit': result['exit'],
                    'stdout': safe_delete_output(result.get('stdout')),
                    'stderr': safe_delete_output(result.get('stderr')),
                    'raw_delete_output_sanitized': True}
            if 'timeout' in result:
                safe['timeout'] = timeout
            result = safe
        else:
            result = dict(result, argv=argv)
        self.state.setdefault('commands', []).append(result)
        self.save()
        if result['exit'] != 0:
            raise RuntimeError('identity command failed: ' + json.dumps(result))
        return result

    def json(self, argv):
        result = self.invoke(argv)
        try:
            return json.loads(result['stdout']) if result['stdout'].strip() else None
        except (ValueError, AttributeError) as error:
            raise RuntimeError('identity command returned invalid JSON: ' + json.dumps(result)) from error

    @classmethod
    def prepare(cls, command, name, namespace, node, image, persist, reuse=False):
        if not name.startswith('tuna-q-variant-'):
            raise ValueError('qualification identity requires a tuna-q-variant-* name')
        identity = cls(command, {'schema': 1, 'identities': {}, 'resources': {}, 'commands': []}, persist)
        selected = identity.invoke(['corral', 'context', 'get'])['stdout'].rstrip('\r\n').split('\t')
        if len(selected) != 3 or not selected[0] or selected[1] != 'kubevirt':
            raise RuntimeError('selected Corral destination is not an explicit KubeVirt context')
        alias, backend, context = selected
        kubernetes_context = context or identity.invoke(['kubectl', 'config', 'current-context'])['stdout'].strip()
        if not kubernetes_context:
            raise RuntimeError('Kubernetes context is empty')
        identity.state['scope'] = {'name': name, 'namespace': namespace, 'node': node,
                                   'image': image, 'backend': backend, 'context': context,
                                   'corral_context_alias': alias, 'kubernetes_context': kubernetes_context}
        identity.state['reuse'] = reuse
        identity.save()
        before = {}
        for kind, resource in identity.resource_names():
            before[kind + '/' + resource] = identity.metadata(kind, resource)
        identity.state['before'] = before
        identity.save()
        if not reuse and any(before.values()):
            raise RuntimeError('refusing create: exact scoped qualification resources already exist')
        # A successful absence check plus creation time and Corral ownership labels
        # is required to recover an interrupted create, even before any VM exists.
        identity.state['create_started'] = timestamp() if not reuse else None
        identity.save()
        return identity

    @property
    def scope(self):
        return self.state['scope']

    def resource_names(self):
        name = self.scope['name']
        return (('vm', name), ('vm', name + '-bootc-builder'),
                ('vmi', name), ('vmi', name + '-bootc-builder'),
                ('pvc', name + '-bootc-disk'), ('dv', name + '-bootc-disk'),
                ('secret', name + '-bootc-builder-cloudinit'))

    def metadata(self, kind, name):
        scope = self.scope
        # Project safe metadata explicitly: kubectl apply's last-applied annotation
        # can contain the entire Secret, so even `.metadata` is too broad.
        projection = ('{.metadata.name}{"\\n"}{.metadata.namespace}{"\\n"}{.metadata.uid}{"\\n"}'
                      '{.metadata.creationTimestamp}{"\\n"}{.metadata.labels}{"\\n"}'
                      '{.metadata.annotations.corral\\.bootc/image}{"\\n"}{.metadata.ownerReferences}')
        result = self.invoke(['kubectl', '--context', scope['kubernetes_context'], '-n', scope['namespace'],
                              'get', kind, name, '--ignore-not-found', '-o', 'jsonpath=' + projection])
        if not result['stdout']:
            return None
        fields = result['stdout'].split('\n')
        if len(fields) != 7:
            raise RuntimeError('invalid projected Kubernetes metadata: ' + json.dumps(result))
        actual_name, namespace, uid, created, labels, image, owners = fields
        metadata = {'name': actual_name, 'namespace': namespace, 'uid': uid,
                    'creationTimestamp': created, 'labels': json.loads(labels) if labels else {},
                    'annotations': {'corral.bootc/image': image},
                    'ownerReferences': json.loads(owners) if owners else []}
        if actual_name != name or namespace != scope['namespace'] or not uid:
            raise RuntimeError('Kubernetes resource identity mismatch: ' + json.dumps(metadata))
        return metadata

    def inventory(self):
        rows = self.json(['corral', '--json', 'list', '--prefix', self.scope['name']])
        if not isinstance(rows, list):
            raise RuntimeError('Corral inventory is not an array')
        return rows

    def matches(self, row, name):
        return (row.get('name') == name and row.get('backend') == self.scope['backend']
                and row.get('context', '') == self.scope['context']
                and row.get('namespace') == self.scope['namespace'] and not row.get('peer'))

    def check_routing(self):
        if not self.scope['context']:
            active = self.invoke(['kubectl', 'config', 'current-context'])['stdout'].strip()
            if active != self.scope['kubernetes_context']:
                raise RuntimeError('active Kubernetes context changed for the empty Corral context')

    def ownership(self, kind, name, metadata):
        previous = self.state['resources'].get(kind + '/' + name)
        if previous and previous['expectedUID'] != metadata['uid']:
            raise RuntimeError('resource UID mismatch: ' + kind + '/' + name)
        labels = metadata.get('labels', {})
        if kind == 'vm':
            key = 'corral' if name == self.scope['name'] else 'corral-bootc-builder'
            owned = labels.get(key) == name and metadata.get('annotations', {}).get('corral.bootc/image') == self.scope['image']
        elif kind == 'vmi':
            vm = self.state['identities'].get(name)
            owned = bool(vm) and any(owner.get('kind') == 'VirtualMachine'
                and owner.get('name') == name and owner.get('uid') == vm['expectedUID']
                and owner.get('controller') is True for owner in metadata.get('ownerReferences', []))
        else:
            owned = labels.get('corral.dev/vm') == self.scope['name']
        if not owned:
            raise RuntimeError('Corral resource ownership unproven: ' + kind + '/' + name)
        started = self.state.get('create_started')
        created = metadata.get('creationTimestamp')
        created_this_run = False
        if started and created and self.state.get('before', {}).get(kind + '/' + name, 'unknown') is None:
            created_this_run = (datetime.datetime.fromisoformat(created.replace('Z', '+00:00'))
                                >= datetime.datetime.fromisoformat(started))
        if not created_this_run and not (kind == 'vm' and self.state.get('reuse')):
            raise RuntimeError('resource creation this run unproven: ' + kind + '/' + name)
        resource = {'kind': kind, 'name': name, 'namespace': self.scope['namespace'],
                    'kubernetes_context': self.scope['kubernetes_context'], 'expectedUID': metadata['uid'],
                    'creationTimestamp': created, 'created_this_run': created_this_run,
                    'ownership_labels': labels, 'ownerReferences': metadata.get('ownerReferences', [])}
        self.state['resources'][kind + '/' + name] = resource
        self.save()
        return resource

    def acquire(self, name=None, required=True, boundary='resolve', rows=None):
        name = name or self.scope['name']
        if name not in (self.scope['name'], self.scope['name'] + '-bootc-builder'):
            raise RuntimeError('instance is outside exact owned names')
        previous = self.state['identities'].get(name)
        if previous:
            previous['validated'] = False
            self.save()
        self.check_routing()
        rows = self.inventory() if rows is None else rows
        selected = [row for row in rows if self.matches(row, name)]
        if len(selected) > 1 or (selected and not isinstance(selected[0].get('id'), str)):
            raise RuntimeError('canonical inventory selection is not unique: ' + json.dumps(rows))
        metadata = self.metadata('vm', name)
        if not selected and metadata is None and not required:
            return None
        if len(selected) != 1 or not selected[0].get('id') or metadata is None:
            raise RuntimeError('canonical inventory/VM resolution failed: ' + json.dumps(rows))
        row = selected[0]
        if previous and (previous['id'] != row['id'] or previous['expectedUID'] != metadata['uid']):
            raise RuntimeError('VM canonical identity or UID mismatch: ' + name)
        resource = self.ownership('vm', name, metadata)
        identity = dict(row, expectedUID=resource['expectedUID'],
                        corral_context_alias=self.scope['corral_context_alias'],
                        kubernetes_context=self.scope['kubernetes_context'],
                        validated=False, boundaries=list(previous.get('boundaries', [])) if previous else [])
        self.state['identities'][name] = identity
        self.save()  # Retain actual ID/UID even if normalized info/placement fails.
        info = self.json(['corral', '--json', 'info', identity['id']])
        if not isinstance(info, dict) or not self.matches(info, name):
            raise RuntimeError('Corral info identity mismatch: ' + json.dumps(info))
        node = self.scope['node']
        if node and (info.get('node') != node or row.get('node', node) != node):
            raise RuntimeError('VM node mismatch: ' + json.dumps({'inventory': row, 'info': info}))
        identity['node'] = info.get('node')
        identity.setdefault('boundaries', []).append({'boundary': boundary, 'at': timestamp(), 'expectedUID': metadata['uid']})
        identity['validated'] = True
        self.save()
        return info

    @property
    def target(self):
        identity = self.state['identities'].get(self.scope['name'])
        if not identity or not identity.get('validated'):
            raise RuntimeError('no validated canonical instance identity; bare target fallback forbidden')
        return identity['id']

    def validate_endpoint(self, endpoint):
        if (endpoint.get('vm') != self.scope['name'] or endpoint.get('namespace') != self.scope['namespace']
                or endpoint.get('context', '') != self.scope['context']):
            raise RuntimeError('console endpoint identity mismatch: ' + json.dumps(endpoint))

    def delete_resource(self, kind, name, report):
        metadata = self.metadata(kind, name)
        if metadata is None:
            return
        resource = self.ownership(kind, name, metadata)
        if not resource['created_this_run']:
            raise RuntimeError('cleanup refuses a resource not created this run: ' + kind + '/' + name)
        canonical = None
        if kind == 'vm':
            self.acquire(name, boundary='immediately-before-delete')
            canonical = self.state['identities'][name]
        scope = self.scope
        paths = {'vm': ('/apis/kubevirt.io/v1', 'virtualmachines'),
                 'pvc': ('/api/v1', 'persistentvolumeclaims'), 'secret': ('/api/v1', 'secrets')}
        prefix, plural = paths[kind]
        url = prefix + '/namespaces/' + quote(scope['namespace'], safe='') + '/' + plural + '/' + quote(name, safe='')
        body = {'apiVersion': 'v1', 'kind': 'DeleteOptions',
                'preconditions': {'uid': resource['expectedUID']}, 'propagationPolicy': 'Foreground'}
        argv = ['kubectl', '--context', scope['kubernetes_context'], 'delete', '--raw', url, '-f', '-']
        operation = {'resource': dict(resource), 'canonical_identity': dict(canonical) if canonical else None,
                     'api_path': url, 'body': body, 'argv': argv}
        report['result'].append(operation)
        self.save()  # Retain delete intent and exact UID even if interrupted in the API call.
        operation['response'] = self.invoke(argv, input=json.dumps(body))
        self.invoke(['kubectl', '--context', scope['kubernetes_context'], '-n', scope['namespace'],
                     'wait', '--for=delete', kind + '/' + name, '--timeout=120s'], timeout=130)

    def cleanup(self):
        report = {'deleted': False, 'result': [], 'failures': [],
                  'vm_delete_atomic_uid_precondition': True}
        self.state['cleanup'] = report
        self.save()
        try:
            self.check_routing()
            rows = self.inventory()
            # Resolve every VM and validate every dependent before any deletion.
            # VMI ownership comes from the retained VM UID, never a name/label.
            for kind, name in self.resource_names():
                if kind == 'vm':
                    self.acquire(name, required=False, boundary='before-cleanup', rows=rows)
                else:
                    metadata = self.metadata(kind, name)
                    if metadata is not None:
                        if kind == 'dv':
                            raise RuntimeError('unexpected exact DataVolume; bootc created only a PVC')
                        self.ownership(kind, name, metadata)
            for resource in self.state['resources'].values():
                if not resource['created_this_run']:
                    raise RuntimeError('cleanup ownership this run unproven: ' + resource['name'])
            for name in (self.scope['name'] + '-bootc-builder', self.scope['name']):
                self.delete_resource('vm', name, report)
            # Foreground GC uses owner UID references, not Corral purge selectors.
            # Independently prove each observed VMI retired, including when its
            # owning VM had already disappeared before emergency cleanup.
            for kind, name in self.resource_names():
                if kind == 'vmi' and kind + '/' + name in self.state['resources']:
                    self.invoke(['kubectl', '--context', self.scope['kubernetes_context'], '-n', self.scope['namespace'],
                                 'wait', '--for=delete', kind + '/' + name, '--timeout=120s'], timeout=130)
            # A failed create can leave only these exact PVC/Secret identities.
            for kind, name in self.resource_names():
                if kind in ('pvc', 'secret'):
                    self.delete_resource(kind, name, report)
            left = [row for row in self.inventory() if any(self.matches(row, name) for name in
                    (self.scope['name'], self.scope['name'] + '-bootc-builder'))]
            resources_left = [kind + '/' + name for kind, name in self.resource_names() if self.metadata(kind, name) is not None]
            report.update(still_listed=left, resources_left=resources_left, deleted=not left and not resources_left)
        except (RuntimeError, ValueError, KeyError, TypeError) as error:
            report['failures'].append(f'{type(error).__name__}: {error}')
        self.save()
        return report
