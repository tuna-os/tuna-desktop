#!/usr/bin/env python3
"""Canonical selectors are inventory values, never reconstructed names."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import unittest
from urllib.parse import unquote

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
from corral_identity import Identity


class FakeCorral:
    name = 'tuna-q-variant-fixture-123'
    image = 'ghcr.io/tuna-os/fixture@sha256:' + 'a' * 64
    target = 'opaque%2Factual//inventory/subject%2F123'
    builder_target = 'opaque%2Factual//inventory/builder%2F123'
    namespace = 'qualification-ns'
    node = 'fixture-node'
    context = ''
    kubernetes_context = 'fixture-cluster'

    def __init__(self):
        self.calls, self.rows, self.resources = [], [], {}
        self.info_changes = {}
        self.snapshots = []

    def prepare(self):
        return Identity.prepare(self.command, self.name, self.namespace, self.node, self.image,
                                lambda state: self.snapshots.append(copy.deepcopy(state)))

    def add_vm(self, builder=False, alternatives=True):
        name = self.name + ('-bootc-builder' if builder else '')
        target = self.builder_target if builder else self.target
        row = {'name': name, 'id': target, 'backend': 'kubevirt', 'context': self.context,
               'namespace': self.namespace, 'node': self.node, 'status': 'Stopped'}
        self.rows.append(row)
        self.add_resource('vm', name, labels={('corral-bootc-builder' if builder else 'corral'): name})
        if alternatives:
            self.rows.extend([dict(row, id='other-context', context='another-context'),
                              dict(row, id='other-backend', backend='qemu'),
                              dict(row, id='other-namespace', namespace='another-namespace'),
                              dict(row, id='peer-subject', peer='another-host'),
                              dict(row, id='prefix-collision', name=name + '-another-run')])
        return row

    def add_resource(self, kind, name, labels=None):
        self.resources[kind, name] = {
            'name': name, 'namespace': self.namespace, 'uid': kind + '-uid-' + name,
            'creationTimestamp': '2099-01-01T00:00:00Z',
            'labels': labels if labels is not None else {'corral.dev/vm': self.name},
            'annotations': {'corral.bootc/image': self.image}}

    def add_vmi(self, builder=False, owner_uid=None):
        name = self.name + ('-bootc-builder' if builder else '')
        self.add_resource('vmi', name)
        self.resources['vmi', name]['ownerReferences'] = [{
            'kind': 'VirtualMachine', 'name': name, 'controller': True,
            'uid': owner_uid or self.resources['vm', name]['uid']}]

    def command(self, argv, timeout=120, input=None):
        self.calls.append((list(argv), input))
        stdout = ''
        if argv == ['corral', 'context', 'get']:
            stdout = 'fixture-alias\tkubevirt\t' + self.context + '\n'
        elif argv == ['kubectl', 'config', 'current-context']:
            stdout = self.kubernetes_context + '\n'
        elif argv[:3] == ['corral', '--json', 'list']:
            stdout = json.dumps(self.rows)
        elif argv[:3] == ['corral', '--json', 'info']:
            row = next(row for row in self.rows if row['id'] == argv[3])
            stdout = json.dumps(dict(row, inputs=[{'type': 'tablet'}], **self.info_changes))
        elif argv[:3] == ['corral', '--json', 'delete']:
            raise AssertionError('broad Corral purge is forbidden')
        elif argv[0] == 'kubectl' and 'get' in argv:
            index = argv.index('get')
            kind, name = argv[index + 1:index + 3]
            resource = self.resources.get((kind, name))
            if resource:
                stdout = '\n'.join((resource['name'], resource['namespace'], resource['uid'],
                                    resource['creationTimestamp'], json.dumps(resource['labels']),
                                    resource['annotations']['corral.bootc/image'],
                                    json.dumps(resource.get('ownerReferences', []))))
        elif argv[0] == 'kubectl' and '--raw' in argv:
            path = argv[argv.index('--raw') + 1].split('/')
            kind = {'persistentvolumeclaims': 'pvc', 'secrets': 'secret', 'virtualmachines': 'vm'}[path[-2]]
            name = unquote(path[-1])
            body = json.loads(input)
            assert body['preconditions']['uid'] == self.resources[kind, name]['uid']
            assert body['propagationPolicy'] == 'Foreground'
            resource = self.resources.pop((kind, name))
            if kind == 'vm':
                self.rows = [row for row in self.rows if not (row['name'] == name
                    and row['backend'] == 'kubevirt' and row['namespace'] == self.namespace
                    and row.get('context', '') == self.context and not row.get('peer'))]
                self.resources = {key: value for key, value in self.resources.items()
                    if not (key[0] == 'vmi' and any(owner.get('uid') == resource['uid']
                            for owner in value.get('ownerReferences', [])))}
        elif argv[0] == 'kubectl' and 'wait' in argv:
            pass
        else:
            raise AssertionError('unexpected command: ' + repr(argv))
        return {'argv': argv, 'exit': 0, 'stdout': stdout, 'stderr': ''}


class IdentityTests(unittest.TestCase):
    def test_actual_opaque_id_disambiguates_every_inventory_dimension(self):
        fake = FakeCorral()
        identity = fake.prepare()
        fake.add_vm()
        identity.acquire(boundary='before-start')
        self.assertEqual(identity.target, fake.target)
        selected = identity.state['identities'][fake.name]
        self.assertEqual(selected['expectedUID'], fake.resources['vm', fake.name]['uid'])
        self.assertEqual(selected['corral_context_alias'], 'fixture-alias')
        self.assertEqual(selected['kubernetes_context'], fake.kubernetes_context)
        self.assertEqual(selected['namespace'], fake.namespace)
        self.assertEqual(selected['context'], '')
        self.assertEqual([argv[3] for argv, _ in fake.calls if argv[:3] == ['corral', '--json', 'info']], [fake.target])

    def test_uid_replacement_refuses_each_boundary_and_delete(self):
        for boundary in ('before-start', 'before-console-journey', 'before-cleanup'):
            with self.subTest(boundary=boundary):
                fake = FakeCorral()
                identity = fake.prepare()
                fake.add_vm()
                identity.acquire()
                fake.resources['vm', fake.name]['uid'] = 'replacement-uid'
                with self.assertRaisesRegex(RuntimeError, 'UID mismatch'):
                    identity.acquire(boundary=boundary)
                self.assertFalse(identity.cleanup()['deleted'])
                self.assertFalse(any('delete' in argv for argv, _ in fake.calls))

    def test_info_scope_and_node_mismatches_refuse_target(self):
        for change in ({'backend': 'qemu'}, {'context': 'foreign'}, {'namespace': 'foreign'},
                       {'name': 'foreign'}, {'node': 'foreign'}):
            with self.subTest(change=change):
                fake = FakeCorral()
                identity = fake.prepare()
                fake.add_vm()
                fake.info_changes = change
                with self.assertRaisesRegex(RuntimeError, 'mismatch'):
                    identity.acquire()
                with self.assertRaisesRegex(RuntimeError, 'bare target fallback forbidden'):
                    _ = identity.target

    def test_cleanup_only_deletes_owned_actual_ids_not_peers_or_collisions(self):
        fake = FakeCorral()
        identity = fake.prepare()
        fake.add_vm()
        fake.add_vm(builder=True)
        fake.add_vmi()
        fake.add_vmi(builder=True)
        identity.acquire()
        result = identity.cleanup()
        self.assertTrue(result['deleted'])
        self.assertTrue(result['vm_delete_atomic_uid_precondition'])
        self.assertEqual([operation['canonical_identity']['id'] for operation in result['result']],
                         [fake.builder_target, fake.target])
        self.assertEqual([operation['resource']['name'] for operation in result['result']],
                         [fake.name + '-bootc-builder', fake.name])
        self.assertEqual(identity.state['resources']['vmi/' + fake.name]['expectedUID'], 'vmi-uid-' + fake.name)
        self.assertNotIn(('vmi', fake.name), fake.resources)
        self.assertTrue(any(row['id'] == 'prefix-collision' for row in fake.rows))

    def test_failed_create_without_vm_id_only_deletes_created_owned_leftovers(self):
        fake = FakeCorral()
        identity = fake.prepare()
        for kind, name in identity.resource_names():
            if kind in ('pvc', 'secret'):
                fake.add_resource(kind, name)
        self.assertIsNone(identity.acquire(required=False))
        result = identity.cleanup()
        self.assertTrue(result['deleted'])
        self.assertEqual(identity.state['identities'], {})
        self.assertFalse(any(argv[:3] == ['corral', '--json', 'delete'] for argv, _ in fake.calls))
        raw = [(argv, body) for argv, body in fake.calls if '--raw' in argv]
        self.assertEqual(len(raw), 2)
        for argv, body in raw:
            self.assertEqual(argv[argv.index('--context') + 1], fake.kubernetes_context)
            self.assertIn('/namespaces/' + fake.namespace + '/', argv[argv.index('--raw') + 1])
            self.assertTrue(json.loads(body)['preconditions']['uid'])
        for argv, _ in fake.calls:
            if 'get' in argv:
                self.assertNotEqual(argv[-1], 'jsonpath={.metadata}')
                self.assertNotIn('last-applied', argv[-1])
                self.assertNotIn('.data', argv[-1])

    def test_raw_delete_projects_secret_and_status_before_any_retention_or_exception(self):
        for outcome in ('success', 'error', 'timeout', 'non-json', 'unknown-json', 'unknown-status'):
            with self.subTest(outcome=outcome):
                fake = FakeCorral()
                identity = fake.prepare()
                name = fake.name + '-bootc-builder-cloudinit'
                fake.add_resource('secret', name)
                secret_values = ('PRIVATE-DATA-MARKER', 'PRIVATE-STRINGDATA-MARKER',
                                 'PRIVATE-LAST-APPLIED-MARKER', 'PRIVATE-ERROR-MARKER')
                payload = {'apiVersion': 'v1', 'kind': 'Secret',
                    'metadata': {'name': name, 'namespace': fake.namespace,
                        'uid': fake.resources['secret', name]['uid'], 'deletionTimestamp': '2099-01-01T00:00:01Z',
                        'annotations': {'kubectl.kubernetes.io/last-applied-configuration': secret_values[2]}},
                    'data': {'token': secret_values[0]}, 'stringData': {'password': secret_values[1]}}
                status = {'kind': 'Status', 'status': 'Failure', 'reason': 'Conflict', 'code': 409,
                          'message': secret_values[3], 'details': {'causes': [{'message': secret_values[0]}]},
                          'metadata': {'annotations': {'last-applied': secret_values[2]}}}
                encoded = json.dumps(payload)
                attempts = []
                def response(argv, **kwargs):
                    if '--raw' not in argv:
                        return fake.command(argv, **kwargs)
                    attempts.append(argv)
                    if outcome == 'success':
                        fake.command(argv, **kwargs)
                        return {'exit': 0, 'stdout': encoded, 'stderr': encoded, 'unexpected': payload}
                    if outcome == 'timeout':
                        raise subprocess.TimeoutExpired(argv, kwargs['timeout'],
                            output=encoded.encode(), stderr=json.dumps(status).encode())
                    if outcome == 'non-json':
                        return {'exit': 1, 'stdout': secret_values[0], 'stderr': secret_values[3]}
                    if outcome == 'unknown-json':
                        return {'exit': 1, 'stdout': json.dumps({'arbitrary': payload}),
                                'stderr': json.dumps({'kind': secret_values[0], 'body': payload})}
                    if outcome == 'unknown-status':
                        return {'exit': 1, 'stdout': json.dumps(dict(status, reason=secret_values[0])),
                                'stderr': encoded}
                    return {'exit': 1, 'stdout': encoded, 'stderr': json.dumps(status), 'unexpected': payload}
                identity.command = response
                report = {'result': []}
                identity.state['cleanup'] = report
                error_text = ''
                if outcome == 'success':
                    identity.delete_resource('secret', name, report)
                    retained = report['result'][0]['response']
                    projection = json.loads(retained['stdout'])['projection']
                    self.assertEqual(projection['kind'], 'Secret')
                    self.assertEqual(set(projection['metadata']), {'name', 'namespace', 'uid', 'deletionTimestamp'})
                    self.assertTrue(identity.cleanup()['deleted'])
                else:
                    with self.assertRaises(RuntimeError) as caught:
                        identity.delete_resource('secret', name, report)
                    error_text = str(caught.exception)
                    self.assertIn(('secret', name), fake.resources)
                self.assertEqual(len(attempts), 1)
                serialized = json.dumps({'state': identity.state, 'cleanup': report,
                                         'persisted': fake.snapshots, 'exception': error_text})
                for value in secret_values:
                    self.assertNotIn(value, serialized)
                raw_receipts = [record for record in identity.state['commands']
                                if '--raw' in record['argv']]
                self.assertEqual(len(raw_receipts), 1)
                self.assertTrue(raw_receipts[0]['raw_delete_output_sanitized'])
                self.assertNotIn('unexpected', raw_receipts[0])
                for stream in ('stdout', 'stderr'):
                    safe = json.loads(raw_receipts[0][stream])
                    self.assertTrue(safe['present'])
                    self.assertTrue(safe['redacted'])
                if outcome == 'timeout':
                    self.assertIsNone(raw_receipts[0]['exit'])
                    self.assertEqual(raw_receipts[0]['timeout'], 120)

    def test_vmi_foreign_owner_refuses_all_deletion_and_exact_dv_is_not_guessed_owned(self):
        for kind in ('vmi', 'dv'):
            with self.subTest(kind=kind):
                fake = FakeCorral()
                identity = fake.prepare()
                fake.add_vm()
                if kind == 'vmi':
                    fake.add_vmi(owner_uid='another-vm-uid')
                else:
                    fake.add_resource('dv', fake.name + '-bootc-disk')
                result = identity.cleanup()
                self.assertFalse(result['deleted'])
                self.assertFalse(any('delete' in argv for argv, _ in fake.calls))

    def test_api_uid_precondition_failure_never_recaptures_replacement_or_retries(self):
        fake = FakeCorral()
        identity = fake.prepare()
        fake.add_vm()
        identity.acquire()
        expected = identity.state['identities'][fake.name]['expectedUID']
        attempted = []
        def race(argv, **kwargs):
            if '--raw' in argv:
                attempted.append((argv, json.loads(kwargs['input'])))
                fake.resources['vm', fake.name]['uid'] = 'replacement-at-api'
                return {'exit': 1, 'stdout': '', 'stderr': json.dumps({
                    'kind': 'Status', 'status': 'Failure', 'reason': 'Conflict', 'code': 409,
                    'message': 'PRIVATE-UID-CONFLICT-DETAIL', 'data': {'token': 'PRIVATE-UID-CONFLICT-TOKEN'}})}
            return fake.command(argv, **kwargs)
        identity.command = race
        result = identity.cleanup()
        self.assertFalse(result['deleted'])
        self.assertEqual(len(attempted), 1)
        self.assertEqual(attempted[0][1]['preconditions']['uid'], expected)
        self.assertEqual(identity.state['identities'][fake.name]['expectedUID'], expected)
        self.assertIn('Conflict', result['failures'][0])
        self.assertNotIn('PRIVATE-UID-CONFLICT', json.dumps({'state': identity.state,
                                                          'cleanup': result, 'persisted': fake.snapshots}))

    def test_unproven_or_replaced_leftover_prevents_all_cleanup_deletes(self):
        for failure in ('foreign-label', 'old-created', 'uid-replaced'):
            with self.subTest(failure=failure):
                fake = FakeCorral()
                identity = fake.prepare()
                fake.add_vm()
                name = fake.name + '-bootc-disk'
                fake.add_resource('pvc', name)
                resource = fake.resources['pvc', name]
                if failure == 'foreign-label':
                    resource['labels'] = {'corral.dev/vm': 'another-run'}
                elif failure == 'old-created':
                    resource['creationTimestamp'] = '2000-01-01T00:00:00Z'
                else:
                    identity.ownership('pvc', name, resource)
                    resource['uid'] = 'replacement'
                self.assertFalse(identity.cleanup()['deleted'])
                self.assertFalse(any('delete' in argv for argv, _ in fake.calls))

    def test_inventory_failure_retains_full_argv_and_raw_diagnostics(self):
        fake = FakeCorral()
        identity = fake.prepare()
        detail = 'FULL STDERR ' * 1000
        def failed(argv, **kwargs):
            if argv[:3] == ['corral', '--json', 'list']:
                return {'exit': 1, 'stdout': 'raw stdout', 'stderr': detail}
            return fake.command(argv, **kwargs)
        identity.command = failed
        with self.assertRaises(RuntimeError) as caught:
            identity.acquire()
        self.assertIn(detail, str(caught.exception))
        self.assertEqual(identity.state['commands'][-1]['stdout'], 'raw stdout')
        self.assertEqual(identity.state['commands'][-1]['argv'],
                         ['corral', '--json', 'list', '--prefix', fake.name])
        with self.assertRaises(RuntimeError):
            _ = identity.target

    def test_existing_exact_resource_refuses_create_and_changed_context_refuses_route(self):
        fake = FakeCorral()
        fake.add_resource('pvc', fake.name + '-bootc-disk')
        with self.assertRaisesRegex(RuntimeError, 'already exist'):
            fake.prepare()
        fake = FakeCorral()
        identity = fake.prepare()
        fake.add_vm()
        fake.kubernetes_context = 'replacement-cluster'
        with self.assertRaisesRegex(RuntimeError, 'context changed'):
            identity.acquire()


if __name__ == '__main__':
    unittest.main()
