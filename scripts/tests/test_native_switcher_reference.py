"""Controlled JSON/files/QMP unit tests. NONE is real GNOME/native runtime proof."""
import base64
import copy
import io
import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest
from unittest.mock import patch
import zlib

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import vm_gnome_reference as proof


def process(name, pid, profile='gnome', uid=1000):
    package = 'tuna-desktop' if name.startswith('tuna-') else name
    if name in ('at-spi2-registryd', 'at-spi-bus-launcher'):
        package = 'at-spi2-core'
    return {'pid': pid, 'start_ticks': pid * 10, 'uid': uid, 'exe': '/usr/bin/' + name,
            'exe_sha256': proof.digest(name.encode()), 'boot_id': 'controlled-boot-' + profile,
            'start_clock_domain': 'Linux /proc starttime ticks since boot', 'clock_ticks_per_second': 100,
            'package': {'name': package, 'version': '51.1-1'}}


def payload_integrity():
    evidence = {'ready': True}
    for key, name, count in (('stock', 'stock-payload.sha256', 4), ('current_tuna', 'current-payload.sha256', 6)):
        evidence[key] = {'receipt_path': '/usr/share/tuna-vm/' + name,
                         'receipt_sha256': proof.digest(name.encode()), 'checked_files': count}
    original = {n: '51.1-1' for n in ('gdm', 'gnome-shell', 'nautilus', 'gnome-control-center')}
    fixture = {**original, 'tuna-desktop': '51.1-1'}
    for key, name, packages in (('reference_packages', 'reference-packages.txt', original),
                                ('fixture_packages', 'fixture-packages.txt', fixture),
                                ('current_package', 'current-package.txt', {'tuna-desktop': '51.1-1'})):
        evidence[key] = {'receipt_path': '/usr/share/tuna-vm/' + name,
                         'receipt_sha256': proof.digest(name.encode()), 'packages': packages}
    return evidence


def admission(profile):
    compositor = process('gnome-shell' if profile == 'gnome' else 'tuna-compositor', 200, profile)
    shell = compositor if profile == 'gnome' else process('tuna-shell-gtk', 201, profile)
    gdm = process('gdm', 100, profile, 0)
    return {'schema': 1, 'profile': profile, 'ready': True, 'missing': [],
            'payload_integrity': payload_integrity(),
            'gdm': {'ready': False, 'available': True, 'service': {'ActiveState': 'active', 'SubState': 'running', 'MainPID': '100'},
                    'process': gdm, 'greeters': []},
            'session': {'ready': True, 'started': True, 'active_vt': 'tty2', 'compositor': compositor, 'shell': shell,
                        'user_bus': {'address': 'unix:path=/run/user/1000/bus', 'peer': process('dbus-broker', 251, profile),
                                     'identity_source': 'kernel SO_PEERCRED'},
                        'wayland_socket': {'path': '/run/user/1000/wayland-0', 'peer': compositor,
                                           'identity_source': 'kernel SO_PEERCRED'},
                        'native_gpu': {'ready': True, 'compositor': compositor, 'drm_fds': [
                            {'fd': 7, 'path': '/dev/dri/card0', 'major': 226, 'minor': 0,
                             'sysfs_device': '/sys/devices/pci0000:00/0000:01:00.0/virtio0', 'driver': 'virtio_gpu',
                             'pci': {'address': '0000:01:00.0', 'sysfs': '/sys/devices/pci0000:00/0000:01:00.0',
                                     'vendor': '0x1af4', 'device': '0x1050', 'driver': 'virtio-pci'},
                             'connectors': [{'name': 'card0-Virtual-1', 'status': 'connected',
                                             'enabled': 'enabled', 'modes': ['1280x800']}]}]},
                        'logind': {'User': '1000', 'Name': 'tuna-test', 'Class': 'user', 'Service': 'gdm-password',
                                   'Type': 'wayland', 'Active': 'yes', 'Remote': 'no', 'Seat': 'seat0',
                                   'State': 'active', 'VTNr': '2'}}}


def node(owner, suffix, name, role='frame', states=None):
    return {'atspi': {'bus_name': ':1.' + str(owner['pid']), 'path': '/org/a11y/' + suffix, 'id_domain': 'AT-SPI'},
            'name': name, 'role_name': role, 'states': states if states is not None else [25, 30], 'process': owner}


def snapshot(profile='gnome', nonce='a' * 32, *, active='settings', held=False, stamp=1, expanded=False, window_mode=False):
    admitted = admission(profile)
    nautilus, settings = process('nautilus', 300, profile), process('gnome-control-center', 301, profile)
    shell = admitted['session']['shell']
    user_bus, a11y_bus, launcher = admitted['session']['user_bus']['peer'], process('dbus-broker', 252, profile), process('at-spi-bus-launcher', 253, profile)
    raw_peer = lambda p: {k: v for k, v in p.items() if k != 'package'}
    windows = []
    for role, name, owner in [('files-first', 'first', nautilus), ('files-second', 'second', nautilus),
                              ('settings', 'Settings', settings)]:
        window = node(owner, role, name)
        window['role'] = role
        if role == active:
            window['states'].append(1)
        if role == 'settings':
            network_label = node(owner, 'network-label', 'Network', 'label')
            network_navigation = node(owner, 'network-navigation', 'Network navigation', 'list item', [23, 25, 30])
            tree = {k: copy.deepcopy(window[k]) for k in ('atspi', 'name', 'role_name', 'states', 'process')}
            tree['children'] = [{**copy.deepcopy(network_navigation), 'children': [copy.deepcopy(network_label)]}]
            window['accessibility_tree'] = tree
            window['panel_evidence'] = [{'selected_object': network_navigation, 'label_objects': [network_label]}]
        windows.append(window)
    base = '/run/user/1000/controlled-' + nonce
    directories = [{'path': path, 'device': 1, 'inode': index + 1000, 'uid': 1000, 'mode': 0o700}
                   for index, path in enumerate((base, base + '/first', base + '/second'))]
    suffix = 'window-selection' if window_mode else 'selection'
    selected = node(shell, suffix, 'second' if window_mode else 'Files', 'push button', [23, 12, 25, 30] if held else [])
    tree = copy.deepcopy(selected)
    tree['children'] = [node(shell, suffix + '-other', 'Network' if window_mode else 'Settings', 'push button')] if held else []
    if expanded and held:
        tree['states'] = [25, 30]
        selected = node(shell, 'first-thumbnail', 'first', 'push button', [23, 12, 25, 30])
        tree['children'].extend([copy.deepcopy(selected), node(shell, 'second-thumbnail', 'second', 'push button')])
    return {'schema': 1, 'profile': profile, 'nonce': nonce, 'ready': True, 'missing': [],
            'observed_monotonic_ns': stamp, 'clock_domain': 'guest CLOCK_MONOTONIC', 'admission': admitted,
            'settings': {'bindings': {action: ['<Alt>Tab' if action == 'switch-applications' else '<Alt>F6']
                                      for action in proof.ACTIONS},
                         'app_switcher': {'current-workspace-only': False},
                         'window_switcher': {'current-workspace-only': False},
                         'interface': {'scaling-factor': 1, 'text-scaling-factor': 1.0, 'gtk-theme': 'Adwaita',
                                       'icon-theme': 'Adwaita', 'cursor-theme': 'Adwaita', 'cursor-size': 24,
                                       'color-scheme': 'default', 'font-name': 'Sans 11', 'document-font-name': 'Sans 11',
                                       'monospace-font-name': 'Monospace 11', 'enable-animations': True},
                         'input_sources': {'sources': [['xkb', 'us']], 'current': 0,
                                           'mru-sources': [['xkb', 'us']], 'xkb-options': []},
                         'display_config': {'serial': 1,
                             'monitors': [[['Virtual-1', 'controlled-vendor', 'controlled-model', 'controlled-serial'],
                                           [['1280x800', 1280, 800, 60.0, 1.0, [1.0], {'is-current': True}]], {}]],
                             'logical_monitors': [[0, 0, 1.0, 0, True,
                                                   [['Virtual-1', 'controlled-vendor', 'controlled-model', 'controlled-serial']], {}]],
                             'properties': {}}},
            'atspi': {'address': 'unix:controlled-unit-test', 'registry': process('at-spi2-registryd', 250, profile),
                      'peers': [nautilus, settings, shell, user_bus, a11y_bus, launcher],
                      'session_bus_peer': raw_peer(user_bus), 'bus_peer': raw_peer(a11y_bus), 'launcher': raw_peer(launcher)},
            'windows': windows,
            'selection': {'shell_process': shell, 'objects': [tree], 'selected': [selected] if held else [],
                          'focused': [selected] if held else [],
                          'active_windows': [w for w in windows if w['role'] == active]},
            'fixture': {'directories': [d['path'] for d in directories[1:]], 'directory_identities': directories,
                        'processes': [nautilus, settings], 'launchers': [process('nautilus', 302, profile)]},
            'files': ['claim.json', 'prepare.json', 'snapshot.json']}


def png(seed=0, width=1280, height=800):
    def chunk(kind, payload):
        return struct.pack('>I', len(payload)) + kind + payload + struct.pack('>I', zlib.crc32(kind + payload) & 0xffffffff)
    rows = (b'\0' + bytes([seed % 255, 40, 60]) * width) * height
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))


class Clock:
    def __init__(self):
        self.now = 1_000_000_000

    def ns(self):
        self.now += 1_000_000
        return self.now

    def sleep(self, seconds):
        self.now += int(seconds * 1e9)


class ControlledGuest:
    """A stateful deterministic transport harness, not a native evidence producer."""
    def __init__(self, profile='gnome', failure=None):
        self.profile, self.failure = profile, failure
        self.nonce, self.files, self.calls, self.keys = None, {}, [], set()
        self.budgets = []
        self.order = []
        self.overview_open = True
        self.active, self.action, self.stamp = 'settings', 0, 0
        self.frame_count, self.stopped, self.capture_changed = 0, False, False
        self.stop_value = None
        self.expanded = False
        self.sequence, self.file_requests, self.wire_sizes = 0, [], []

    def store(self, name, value):
        self.files[name] = json.dumps(value, sort_keys=True).encode()

    def envelope(self, operation, value):
        self.sequence += 1
        name = f'{operation}-{self.sequence:06d}.json'
        full = {**value, 'files': sorted(set(self.files) | {name})}
        self.store(name, full)
        data = self.files[name]
        self.files[{'prepare': 'prepare.json', 'observe': 'snapshot.json', 'stop': 'stop.json'}[operation]] = data
        wire = {'schema': 1, 'profile': self.profile, 'nonce': self.nonce, 'ready': full['ready'],
                'response_file': {'name': name, 'size': len(data), 'sha256': proof.digest(data)}}
        self.wire_sizes.append(len(json.dumps(wire).encode()))
        if operation == 'stop':
            self.stop_value = full
        if self.failure == 'direct-response':
            return full
        if self.failure == 'envelope-hash':
            wire['response_file']['sha256'] = '0' * 64
        return wire

    def observe(self):
        self.stamp += 1
        value = snapshot(self.profile, self.nonce, active=self.active, held='alt' in self.keys, stamp=self.stamp,
                         expanded=self.expanded and self.action == 1, window_mode=self.action >= 2)
        if self.failure == 'scale':
            value['settings']['interface']['text-scaling-factor'] = 1.25
        elif self.failure == 'app-package':
            for window in value['windows']:
                if window['role'].startswith('files-'):
                    window['process']['package']['version'] = '51.1-controlled-different'
            value['fixture']['processes'][0]['package']['version'] = '51.1-controlled-different'
        elif self.failure == 'large':
            owner = value['selection']['shell_process']
            value['selection']['objects'][0]['children'].extend(
                node(owner, 'controlled-label-' + str(i), 'controlled accessible label ' + str(i), 'label')
                for i in range(600))
        if self.capture_changed:
            value['selection']['objects'][0]['name'] = 'changed across capture'
        self.store('snapshot.json', value)
        return value

    def run(self, route, op, payload, *, timeout):
        budget_cap, timeout_cap = (240, 250) if op == 'prepare' else (25, 30)
        if route != 'gnome-reference' or timeout > timeout_cap:
            raise AssertionError('unbounded/nonfixed guest route')
        args = json.loads(payload)
        if args['budget_s'] > budget_cap:
            raise AssertionError('unbounded guest request')
        self.budgets.append((op, args['budget_s'], timeout))
        if op == 'file':
            if set(args) != {'nonce', 'name', 'offset', 'limit', 'budget_s'} or not 0 < args['limit'] <= 262144:
                raise AssertionError('file operation accepts no profile or arbitrary fields')
        elif args.get('profile') != self.profile:
            raise AssertionError('foreign guest request')
        self.calls.append(op)
        self.order.append(op)
        if op == 'admit':
            admitted = admission(self.profile)
            if self.failure == 'greeter-flap':
                admitted['gdm']['greeters'] = [{'uid': 60578, 'shells': 1}]
            return admitted
        if op == 'overview':
            return {'schema': 1, 'ready': True, 'operation': 'overview', 'profile': self.profile,
                    'nonce': args['nonce'], 'overview_open': self.overview_open, 'shell_pid': 200,
                    'observed_monotonic_ns': 1, 'clock_domain': 'guest CLOCK_MONOTONIC'}
        if op == 'prepare':
            if self.failure == 'prepare':
                raise RuntimeError('controlled prepare failure')
            self.nonce = args['nonce']
            value = self.observe()
            self.store('claim.json', {'schema': 1, 'profile': self.profile, 'nonce': self.nonce,
                                      'fixture': value['fixture'], 'admission': value['admission']})
            prepared = {'schema': 1, 'profile': self.profile, 'nonce': self.nonce, 'ready': True,
                        'admission': value['admission'], 'processes': value['fixture']['processes'],
                        'fixture': value['fixture'], 'snapshot': value, 'files': list(self.files)}
            self.store('prepare.json', prepared)
            return self.envelope('prepare', prepared)
        if op == 'observe':
            return self.envelope('observe', self.observe())
        if op == 'stop':
            self.stopped = True
            fixture = json.loads(self.files['claim.json'])['fixture']
            value = {'schema': 1, 'profile': self.profile, 'nonce': self.nonce, 'ready': True,
                     'cleaned': True, 'errors': [],
                     'processes': [{'original': p, 'retired': True, 'method': 'pidfd'}
                                   for p in fixture['processes'] + fixture['launchers']],
                     'directories': [{'original': d, 'removed': True} for d in fixture['directory_identities']]}
            if self.failure == 'cleanup':
                value.update(ready=False, cleaned=False, errors=['controlled pidfd failure'])
            self.store('stop.json', value)
            return self.envelope('stop', value)
        if op == 'file':
            data = self.files[args['name']]
            if self.failure == 'raw-snapshot' and args['name'] == 'snapshot.json':
                value = json.loads(data)
                value['nonce'] = 'b' * 32
                data = json.dumps(value).encode()
            offset, limit = args['offset'], args['limit']
            self.file_requests.append((args['name'], offset, limit))
            part = data[offset:offset + limit]
            return {'name': args['name'], 'base64': base64.b64encode(part).decode(),
                    'offset': offset, 'length': len(part), 'size': len(data),
                    'sha256': proof.digest(data), 'chunk_sha256': proof.digest(part),
                    'eof': offset + len(part) == len(data)}
        raise AssertionError('unknown operation')

    def cmd(self, command, **arguments):
        if command == 'query-kvm':
            return {'enabled': self.failure != 'kvm', 'present': True}
        if command == 'query-pci':
            return [{'bus': 0, 'devices': [
                {'bus': 0, 'slot': 1, 'function': 0, 'id': {'vendor': 0x1b36, 'device': 0x000c},
                 'class_info': {'class': 0x0604}, 'qdev_id': 'controlled-root-port',
                 'pci_bridge': {'bus': {'number': 0, 'secondary': 1, 'subordinate': 1}, 'devices': [
                     {'bus': 1, 'slot': 0, 'function': 0,
                      'id': {'vendor': 0x1af4, 'device': 0x1050, 'subsystem': 0x1100, 'subsystem-vendor': 0x1af4},
                      'class_info': {'class': 0x0300}, 'qdev_id': 'controlled-native-vga'}]}},
                {'bus': 0, 'slot': 2, 'function': 0, 'id': {'vendor': 0x1b36, 'device': 0x000c},
                 'class_info': {'class': 0x0604}, 'qdev_id': 'controlled-empty-port',
                 'pci_bridge': {'bus': {'number': 0, 'secondary': 2, 'subordinate': 2}}}]}]
        if command != 'input-send-event':
            raise AssertionError('not physical input')
        for event in arguments['events']:
            if event['type'] != 'key':
                continue
            key, down = event['data']['key']['data'], event['data']['down']
            self.order.append(('input', key, down))
            if key == 'esc' and down:
                self.overview_open = False
            if down:
                self.keys.add(key)
                if key in ('tab', 'f6'):
                    self.action += 1
                if key == 'down':
                    self.expanded = True
                if self.failure == 'keydown' and key == 'tab':
                    raise OSError('controlled lost acknowledgement after physical key-down')
            else:
                self.keys.remove(key)
                if key == 'alt':
                    self.active = 'files-first' if self.action == 1 else 'files-second'

    def frame(self, path):
        self.frame_count += 1
        if self.failure == 'frame' and self.frame_count == 2:
            raise OSError('controlled framebuffer error')
        Path(path).write_bytes(png(self.frame_count + (20 if self.profile == 'tuna' else 0)))
        if self.failure == 'unstable' and self.frame_count == 2:
            self.capture_changed = True


def provenance(profile):
    return {'base_image': proof.BASE_REPOSITORY + '@' + proof.PINNED_PLATFORM,
            'test_image_id': 'sha256:' + ('2' if profile == 'gnome' else '3') * 64,
            'commit': '4' * 40}


class RetainedFileTransport:
    """Read actual temporary receipt bytes; inject precise malformed chunk metadata."""
    def __init__(self, path, fault=None):
        self.path, self.fault, self.offsets = path, fault, []

    def run(self, route, operation, payload, *, timeout):
        args = json.loads(payload)
        if route != 'gnome-reference' or operation != 'file' or timeout > 30:
            raise AssertionError('not bounded fixed file transport')
        if set(args) != {'nonce', 'name', 'offset', 'limit', 'budget_s'} or args['budget_s'] > 25:
            raise AssertionError('invalid typed file request')
        data = self.path.read_bytes()
        offset, limit = args['offset'], args['limit']
        self.offsets.append(offset)
        if not 0 < limit <= 262144:
            raise AssertionError('unbounded chunk')
        part = data[offset:offset + limit]
        if self.fault == 'no-progress':
            part = b''
        value = {'name': args['name'], 'offset': offset, 'length': len(part), 'size': len(data),
                 'sha256': proof.digest(data), 'chunk_sha256': proof.digest(part),
                 'base64': base64.b64encode(part).decode(), 'eof': offset + len(part) == len(data)}
        if self.fault == 'offset':
            value['offset'] += 1
        elif self.fault == 'length':
            value['length'] += 1
        elif self.fault == 'chunk-hash':
            value['chunk_sha256'] = '0' * 64
        elif self.fault == 'full-hash':
            value['sha256'] = '0' * 64
        elif self.fault == 'changed-size' and offset:
            value['size'] += 1
            value['eof'] = False
        elif self.fault == 'early-eof':
            value['eof'] = True
        return value


class NativeSwitcherReferenceTests(unittest.TestCase):
    def setUp(self):
        # The controlled index digest is explicitly patched; these bytes are
        # not the published index, cosign verification, or native runtime proof.
        self.index_bytes = json.dumps({'schemaVersion': 2, 'manifests': [
            {'digest': proof.PINNED_PLATFORM, 'platform': {'os': 'linux', 'architecture': 'amd64'}}]}).encode()
        self.cosign = [{'critical': {'image': {'docker-manifest-digest': proof.PINNED_PLATFORM},
                                     'identity': {'docker-reference': proof.BASE_REPOSITORY + '@' + proof.PINNED_PLATFORM},
                                     'type': 'https://sigstore.dev/cosign/sign/v1'},
                        'optional': {}}]
        patched = patch.object(proof, 'PINNED_INDEX', 'sha256:' + proof.digest(self.index_bytes))
        patched.start()
        self.addCleanup(patched.stop)

    def verification(self, output=None, platform_digest=proof.PINNED_PLATFORM):
        output = json.dumps(self.cosign).encode() if output is None else output
        return json.dumps({
            'schema': 1, 'argv': proof.cosign_argv('/usr/bin/cosign', platform_digest), 'exit_code': 0,
            'begin_host_monotonic_ns': 100, 'end_host_monotonic_ns': 200,
            'tool': {'executable': '/usr/bin/cosign', 'sha256': 'e' * 64, 'version': 'GitVersion: v3.1.4\n'},
            'stdout_sha256': proof.digest(output), 'stdout_bytes': len(output), 'stderr': '',
        }).encode()

    def base_evidence(self, root):
        directory = Path(root) / 'controlled-base-input'
        directory.mkdir(parents=True, exist_ok=True)
        (directory / 'index.json').write_bytes(self.index_bytes)
        (directory / 'platform-cosign.json').write_bytes(json.dumps(self.cosign).encode())
        (directory / 'platform-verification.json').write_bytes(self.verification())
        return directory

    def collect(self, root, profile='gnome', failure=None):
        guest, clock, records = ControlledGuest(profile, failure), Clock(), []
        base = self.base_evidence(root)
        with patch.object(proof.time, 'monotonic_ns', clock.ns), patch.object(proof.time, 'sleep', clock.sleep):
            result = proof.run(guest, guest, root, lambda *r: records.append(r),
                               profile=profile, provenance=provenance(profile), base_evidence_dir=base)
        return result, guest, records

    def pair(self, root):
        for profile in ('gnome', 'tuna'):
            self.collect(root / profile, profile)
        return root / 'gnome/member.json', root / 'tuna/member.json'

    def test_prepare_failure_retains_screendump_and_error(self):
        with tempfile.TemporaryDirectory() as directory:
            guest, clock, records = ControlledGuest(failure='prepare'), Clock(), []
            base = self.base_evidence(directory)
            with patch.object(proof.time, 'monotonic_ns', clock.ns), patch.object(proof.time, 'sleep', clock.sleep):
                with self.assertRaises(RuntimeError):
                    proof.run(guest, guest, directory, lambda *r: records.append(r),
                              profile='gnome', provenance=provenance('gnome'), base_evidence_dir=base)
            receipt = json.loads((Path(directory) / 'member.json').read_bytes())
            self.assertFalse(receipt['ready'])
            self.assertIn('controlled prepare failure', receipt['error']['message'])
            self.assertTrue((Path(directory) / 'prepare-failed.png').exists())
            self.assertEqual(guest.frame_count, 1)

    def test_greeter_flap_between_admit_and_prepare_stays_ready(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt, _, _ = self.collect(Path(directory), failure='greeter-flap')
            self.assertTrue(receipt['ready'])
            self.assertEqual(len(receipt['admission']['gdm']['greeters']), 1)
            self.assertEqual(receipt['prepare']['admission']['gdm']['greeters'], [])

    def test_prepare_budget_exceeds_observe_budget(self):
        with tempfile.TemporaryDirectory() as directory:
            _, guest, _ = self.collect(Path(directory))
            budgets = dict((op, (budget, timeout)) for op, budget, timeout in guest.budgets)
            self.assertEqual(budgets['prepare'], (240, 250))
            self.assertEqual(budgets['observe'], (25, 30))
            self.assertEqual(budgets['file'], (25, 30))
            self.assertEqual(budgets['overview'], (25, 30))

    def test_gnome_dismisses_overview_before_prepare(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt, guest, _ = self.collect(Path(directory), 'gnome')
        self.assertTrue(receipt['ready'])
        self.assertIn('pre-prepare-overview-dismiss', receipt['settle_polls'])
        self.assertFalse(guest.overview_open)
        prepare_at = guest.order.index('prepare')
        esc_at = next(index for index, entry in enumerate(guest.order)
                      if entry == ('input', 'esc', True))
        self.assertLess(esc_at, prepare_at)

    def test_tuna_dismisses_overview_before_prepare(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt, guest, _ = self.collect(Path(directory), 'tuna')
        self.assertTrue(receipt['ready'])
        self.assertIn('pre-prepare-overview-dismiss', receipt['settle_polls'])
        self.assertFalse(guest.overview_open)
        prepare_at = guest.order.index('prepare')
        esc_at = next(index for index, entry in enumerate(guest.order)
                      if entry == ('input', 'esc', True))
        self.assertLess(esc_at, prepare_at)

    def test_controlled_collection_brackets_real_retained_png_bytes_and_input_bounds(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt, guest, records = self.collect(root)
            self.assertTrue(receipt['ready'])
            self.assertEqual([p['phase'] for p in receipt['phases']], list(proof.PHASES))
            self.assertEqual(guest.keys, set())
            self.assertTrue(guest.stopped)
            self.assertEqual(records[0][:2], ('P-WM01-CONTEXT', True))
            self.assertEqual(receipt['provenance'], provenance('gnome'))
            self.assertEqual(proof.validate_member(root / 'member.json', 'gnome'), receipt)
            for phase in receipt['phases']:
                self.assertEqual(proof.digest((root / phase['path']).read_bytes()), phase['sha256'])
                self.assertLess(phase['before']['observed_monotonic_ns'], phase['after']['observed_monotonic_ns'])
                self.assertEqual(phase['clock_domain'], 'host CLOCK_MONOTONIC')
                if phase['held_keys']:
                    self.assertGreaterEqual(phase['capture_begin_ns'] - phase['hold_anchor_ns'], 500_000_000)
            self.assertNotIn('failure.json', receipt['guest_files'])

    def test_frame_and_changed_selection_failures_release_keys_and_preserve_strict_cleanup(self):
        for failure in ('frame', 'unstable', 'keydown'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                guest, clock, records = ControlledGuest(failure=failure), Clock(), []
                base = self.base_evidence(directory)
                with patch.object(proof.time, 'monotonic_ns', clock.ns), patch.object(proof.time, 'sleep', clock.sleep):
                    with self.assertRaises((OSError, ValueError)):
                        proof.run(guest, guest, directory, lambda *r: records.append(r),
                                  profile='gnome', provenance=provenance('gnome'), base_evidence_dir=base)
                self.assertTrue(guest.stopped)
                self.assertEqual(guest.keys, set())
                receipt = json.loads((Path(directory) / 'member.json').read_bytes())
                self.assertFalse(receipt['ready'])
                self.assertEqual(receipt['cleanup'], guest.stop_value)
                self.assertTrue((Path(directory) / 'guest/stop.json').is_file())
                self.assertFalse(records[-1][1])

    def test_cleanup_only_failure_is_not_ready_and_retains_exact_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, 'cleanup/retention'):
                self.collect(Path(directory), failure='cleanup')
            receipt = json.loads((Path(directory) / 'member.json').read_bytes())
            raw = json.loads((Path(directory) / 'guest/stop.json').read_bytes())
            self.assertEqual(raw, receipt['cleanup'])
            self.assertEqual(raw['errors'], ['controlled pidfd failure'])
            self.assertFalse(receipt['ready'])

    def test_missing_or_forged_accessibility_identity_and_provenance_are_rejected(self):
        for mutate in (
            lambda s: s['windows'].pop(),
            lambda s: s['windows'][0]['process'].update(exe='/usr/bin/fake-nautilus'),
            lambda s: s['windows'][0]['atspi'].update(id_domain='WM'),
            lambda s: s['selection']['selected'][0]['atspi'].update(path='/not/in/actual/tree'),
            lambda s: s['selection']['selected'][0].update(process=s['windows'][0]['process']),
            lambda s: s['admission']['session']['logind'].update(Service='autologin'),
            lambda s: s['settings'].pop('display_config'),
            lambda s: s['windows'][0]['states'].remove(25),
        ):
            value = snapshot(held=True)
            mutate(value)
            with self.assertRaises((ValueError, KeyError)):
                proof.validate_snapshot(value, 'gnome', 'a' * 32)

    def test_original_shell_selected_object_remains_distinct_from_real_gtk_windows(self):
        value = snapshot(held=True)
        proof.validate_snapshot(value, 'gnome', 'a' * 32)
        selected = proof.atspi_id(value['selection']['selected'][0])
        self.assertNotIn(selected, [proof.atspi_id(w) for w in value['windows']])
        value['selection']['active_windows'] = value['selection']['selected']
        with self.assertRaises(ValueError):
            proof.validate_snapshot(value, 'gnome', 'a' * 32)

    def test_empty_active_windows_is_observable_but_two_is_ambiguous(self):
        # Round-23/24: switcher grab and commit gaps observe zero active;
        # only two-active is a contradiction.
        value = snapshot(held=True)
        proof.validate_snapshot(value, 'gnome', 'a' * 32)
        value['selection']['active_windows'] = []
        proof.validate_snapshot(value, 'gnome', 'a' * 32)
        value['selection']['active_windows'] = [value['windows'][0], value['windows'][1]]
        with self.assertRaisesRegex(ValueError, 'ambiguous active GTK app windows'):
            proof.validate_snapshot(value, 'gnome', 'a' * 32)

    def test_configured_bindings_are_read_not_rewritten_and_unsupported_action_fails(self):
        settings = snapshot()['settings']
        original = copy.deepcopy(settings)
        self.assertEqual(proof.choose_binding(settings, 'switch-applications')['key'], 'tab')
        self.assertEqual(settings, original)
        settings['bindings']['switch-applications'] = ['<Hyper>Tab']
        with self.assertRaisesRegex(ValueError, 'configured'):
            proof.choose_binding(settings, 'switch-applications')

    def test_png_crc_truncation_dimensions_and_hash_are_validated(self):
        data = png()
        self.assertEqual(proof.png_info(data)['width'], 1280)
        for bad in (data[:-1], data[:45] + bytes([data[45] ^ 1]) + data[46:], b'not a screenshot'):
            with self.assertRaises(ValueError):
                proof.png_info(bad)
        with self.assertRaisesRegex(ValueError, '1280x800'):
            proof.png_info(png(width=1279))
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'capture.png').write_bytes(data)
            with self.assertRaisesRegex(ValueError, 'hash/size'):
                proof.retained_file(root, {'path': 'capture.png', 'sha256': '0' * 64, 'size': len(data)})
            with self.assertRaisesRegex(ValueError, 'escapes'):
                proof.retained_file(root, {'path': '../capture.png'})

    def test_pair_is_only_context_ready_never_visual_or_v1_completion(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            left, right = self.pair(root)
            value = proof.compare(left, right, root / 'pair')
            self.assertTrue(value['ready'], value)
            self.assertFalse(value['p_wm01_complete'])
            self.assertEqual(value['visual_verdict'], 'not-classified')
            self.assertNotIn('visual_match', value)

    def test_pair_rejects_retained_capture_corruption(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            left, right = self.pair(root)
            (root / 'gnome/applications-held.png').write_bytes(png(99))
            result = proof.compare(left, right, root / 'pair')
            self.assertFalse(result['ready'])
            self.assertIn('hash/size', result['error']['message'])

    def test_member_rejects_timing_replay_held_sets_missing_phases_and_cleanup_ownership(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original, _, _ = self.collect(root)
            mutations = (
                lambda r: r['phases'][1].update(hold_anchor_ns=r['phases'][1]['capture_begin_ns'] - 499_999_999),
                lambda r: r['phases'][1]['after'].update(observed_monotonic_ns=r['phases'][1]['before']['observed_monotonic_ns']),
                lambda r: r['phases'][1].update(held_keys=['meta_l']),
                lambda r: r['phases'].pop(),
                lambda r: r['cleanup']['processes'][0]['original'].update(start_ticks=999999),
                lambda r: r['cleanup']['directories'][0].update(removed=False),
                lambda r: r['commands'][1].update(held_before=['alt']),
            )
            for mutate in mutations:
                changed = copy.deepcopy(original)
                mutate(changed)
                proof.save_json(root / 'member.json', changed)
                with self.assertRaises((ValueError, KeyError)):
                    proof.validate_member(root / 'member.json', 'gnome')

    def test_pair_rejects_actual_scale_and_original_package_context_differences(self):
        for difference in ('scale', 'app-package'):
            with self.subTest(difference=difference), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.collect(root / 'gnome', 'gnome')
                self.collect(root / 'tuna', 'tuna', failure=difference)
                result = proof.compare(root / 'gnome/member.json', root / 'tuna/member.json', root / 'pair')
                self.assertFalse(result['ready'])
                self.assertIn('differ' if difference == 'scale' else 'mismatch', result['error']['message'])

    def test_platform_digest_argument_is_strict_and_argv_keeps_all_policy_flags(self):
        platform = 'sha256:' + 'a' * 64
        self.assertEqual(proof.cosign_argv('/usr/bin/cosign', platform), [
            '/usr/bin/cosign', 'verify', 'ghcr.io/tuna-os/marlin@' + platform,
            '--certificate-identity', proof.CERTIFICATE_SUBJECT,
            '--certificate-oidc-issuer', proof.CERTIFICATE_ISSUER, '--output', 'json'])
        for invalid in (None, '', 'a' * 64, 'sha256:' + 'A' * 64, 'sha256:' + 'a' * 63,
                        platform + '\n', proof.BASE_REPOSITORY + '@' + platform):
            with self.subTest(platform=invalid), self.assertRaisesRegex(ValueError, 'strict sha256'):
                proof.cosign_argv('/usr/bin/cosign', invalid)
            with self.assertRaisesRegex(ValueError, 'strict sha256'):
                proof.validate_platform_verification(b'[]', b'{}', invalid)
            with patch.object(proof.subprocess, 'run') as run:
                with self.assertRaisesRegex(ValueError, 'strict sha256'):
                    proof.verify_platform_image('/unused-controlled-output', invalid)
                run.assert_not_called()

    def test_process_without_actual_positive_elapsed_time_is_not_verified_evidence(self):
        output = json.dumps(self.cosign).encode()
        witness = json.loads(self.verification(output))
        witness['end_host_monotonic_ns'] = witness['begin_host_monotonic_ns']
        with self.assertRaisesRegex(ValueError, 'process/output'):
            proof.validate_platform_verification(output, json.dumps(witness).encode(), proof.PINNED_PLATFORM)

    def test_shared_platform_parser_requires_modern_envelope_and_matching_witness(self):
        platform = 'sha256:' + 'a' * 64
        signatures = copy.deepcopy(self.cosign)
        signatures[0]['critical']['image']['docker-manifest-digest'] = platform
        signatures[0]['critical']['identity']['docker-reference'] = proof.BASE_REPOSITORY + '@' + platform
        output = json.dumps(signatures).encode()
        witness = self.verification(output, platform)
        proof.validate_platform_verification(output, witness, platform)
        with self.assertRaisesRegex(ValueError, 'actual platform'):
            proof.validate_platform_verification(output, witness, proof.PINNED_PLATFORM)
        with self.assertRaisesRegex(ValueError, 'subject/issuer'):
            proof.validate_platform_verification(output, self.verification(output), platform)
        for mutate in (
                lambda row: row['critical']['image'].update({'docker-manifest-digest': proof.PINNED_PLATFORM}),
                lambda row: row['critical']['identity'].update({'docker-reference': proof.BASE_REPOSITORY}),
                lambda row: row['critical']['identity'].update({'docker-reference': 'ghcr.io/untrusted/image@' + platform}),
                lambda row: row['critical'].update(type='cosign container image signature'),
                lambda row: row.pop('optional'),
                lambda row: row.update(optional=None)):
            bad = copy.deepcopy(signatures)
            mutate(bad[0])
            bad_output = json.dumps(bad).encode()
            with self.assertRaisesRegex(ValueError, 'actual platform'):
                proof.validate_platform_verification(bad_output, self.verification(bad_output, platform), platform)
        for missing in (b'', b'{}', b'null'):
            with self.subTest(witness=missing), self.assertRaises(ValueError):
                proof.validate_platform_verification(output, missing, platform)
        for missing_output in (b'', b'[]', b'{}'):
            with self.subTest(output=missing_output), self.assertRaises(ValueError):
                proof.validate_platform_verification(missing_output, self.verification(missing_output, platform), platform)

    def test_shared_platform_parser_rejects_failed_or_changed_process_evidence(self):
        output = json.dumps(self.cosign).encode()
        for mutate in (
                lambda row: row.update(exit_code=1),
                lambda row: row.update(exit_code=False),
                lambda row: row.update(exit_code=0.0),
                lambda row: row.pop('exit_code'),
                lambda row: row['argv'].append('--insecure-ignore-tlog'),
                lambda row: row['argv'].__setitem__(2, proof.BASE_REPOSITORY + '@sha256:' + 'a' * 64),
                lambda row: row['argv'].__setitem__(-1, 'text'),
                lambda row: row.update(stdout_sha256='f' * 64),
                lambda row: row.update(stdout_bytes=len(output) + 1),
                lambda row: row.pop('stdout_sha256'),
                lambda row: row.pop('stdout_bytes'),
                lambda row: row.pop('stderr'),
                lambda row: row.update(begin_host_monotonic_ns=0),
                lambda row: row.update(end_host_monotonic_ns=99),
                lambda row: row.update(end_host_monotonic_ns=True),
                lambda row: row['tool'].update(executable='cosign'),
                lambda row: row['tool'].update(sha256='invalid'),
                lambda row: row['tool'].update(version='GitVersion: v2.0.0\n'),
                lambda row: row.pop('tool')):
            bad = json.loads(self.verification())
            mutate(bad)
            with self.assertRaises(ValueError):
                proof.validate_platform_verification(output, json.dumps(bad).encode(), proof.PINNED_PLATFORM)
        for field in ('--certificate-identity', '--certificate-oidc-issuer'):
            bad = json.loads(self.verification())
            bad['argv'][bad['argv'].index(field) + 1] = 'untrusted-controlled-value'
            with self.assertRaisesRegex(ValueError, 'subject/issuer'):
                proof.validate_platform_verification(output, json.dumps(bad).encode(), proof.PINNED_PLATFORM)

    def test_platform_and_strict_base_clis_retain_one_controlled_verification_process(self):
        # Mocked process bytes exercise retention/dispatch only, never crypto or UI proof.
        for operation in ('verify-platform', 'verify-base'):
            with self.subTest(operation=operation), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                executable = root / 'controlled-cosign'
                executable.write_bytes(b'controlled executable bytes, not a cosign binary')
                out = root / 'output'
                platform = 'sha256:' + 'a' * 64 if operation == 'verify-platform' else proof.PINNED_PLATFORM
                signatures = copy.deepcopy(self.cosign)
                signatures[0]['critical']['image']['docker-manifest-digest'] = platform
                signatures[0]['critical']['identity']['docker-reference'] = proof.BASE_REPOSITORY + '@' + platform
                output = json.dumps(signatures).encode()
                argv = proof.cosign_argv(str(executable), platform)
                result = proof.subprocess.CompletedProcess(argv, 0, output, b'controlled stderr\n')
                version = 'GitVersion: v3.1.4\n'
                args = [operation, '--out', str(out)]
                if operation == 'verify-base':
                    out.mkdir()
                    (out / 'index.json').write_bytes(self.index_bytes)
                else:
                    args += ['--platform-digest', platform]
                with patch.object(proof.shutil, 'which', return_value=str(executable)), \
                        patch.object(proof.subprocess, 'check_output', return_value=version) as tool_version, \
                        patch.object(proof.subprocess, 'run', return_value=result) as run, \
                        patch.object(proof.time, 'monotonic_ns', side_effect=[100, 200]):
                    self.assertEqual(proof.main(args), 0)
                tool_version.assert_called_once_with([str(executable), 'version'], timeout=30, text=True)
                run.assert_called_once_with(argv, capture_output=True, timeout=180)
                self.assertEqual((out / 'platform-cosign.json').read_bytes(), output)
                witness_bytes = (out / 'platform-verification.json').read_bytes()
                witness = json.loads(witness_bytes)
                self.assertEqual(witness['tool'], {'executable': str(executable),
                                                  'sha256': proof.digest(executable.read_bytes()), 'version': version})
                self.assertEqual(witness['argv'], argv)
                self.assertEqual(witness['exit_code'], 0)
                self.assertEqual(witness['begin_host_monotonic_ns'], 100)
                self.assertEqual(witness['end_host_monotonic_ns'], 200)
                self.assertEqual(witness['stdout_sha256'], proof.digest(output))
                self.assertEqual(witness['stdout_bytes'], len(output))
                self.assertEqual(witness['stderr'], 'controlled stderr\n')
                proof.validate_platform_verification(output, witness_bytes, platform)
                if operation == 'verify-platform':
                    self.assertFalse((out / 'index.json').exists())

    def test_base_index_bytes_membership_and_actual_verified_envelope_are_required(self):
        original = json.dumps(self.cosign).encode()
        proof.validate_base_bytes(self.index_bytes, original, self.verification())
        with self.assertRaisesRegex(ValueError, 'index29097'):
            proof.validate_base_bytes(self.index_bytes + b' ', original, self.verification())
        with self.assertRaisesRegex(ValueError, 'subject/issuer'):
            proof.validate_base_bytes(self.index_bytes, original, b'{}')
        bad = copy.deepcopy(self.cosign)
        bad[0]['critical']['image']['docker-manifest-digest'] = 'sha256:' + '6' * 64
        with self.assertRaisesRegex(ValueError, 'actual platform'):
            proof.validate_base_bytes(self.index_bytes, json.dumps(bad).encode(), self.verification(json.dumps(bad).encode()))
        for manifests in (
                [{'digest': proof.PINNED_PLATFORM, 'platform': {'os': 'linux', 'architecture': 'arm64'}}],
                [{'digest': proof.PINNED_PLATFORM, 'platform': {'os': 'linux', 'architecture': 'amd64'}}] * 2,
                [{'digest': 'sha256:' + 'a' * 64, 'platform': {'os': 'linux', 'architecture': 'amd64'}}]):
            wrong = json.dumps({'schemaVersion': 2, 'manifests': manifests}).encode()
            with patch.object(proof, 'PINNED_INDEX', 'sha256:' + proof.digest(wrong)):
                with self.assertRaisesRegex(ValueError, 'linux/amd64'):
                    proof.validate_base_bytes(wrong, original, self.verification())

    def test_snapshot_version_and_unknown_profile_do_not_fall_back_to_tuna(self):
        value = snapshot()
        value['admission']['session']['compositor']['package']['version'] = '51.0-1'
        with self.assertRaisesRegex(ValueError, 'GNOME 51.1'):
            proof.validate_snapshot(value, 'gnome', 'a' * 32)
        with self.assertRaises(ValueError):
            proof.session_status(ControlledGuest(), profile='fallback')

    def test_compare_cli_reads_retained_members_and_returns_context_exit_status(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            left, right = self.pair(root)
            with patch('sys.stdout', new_callable=io.StringIO) as stdout:
                result = proof.main(['compare', '--gnome', str(left), '--tuna', str(right), '--out', str(root / 'pair')])
            self.assertEqual(result, 0)
            self.assertTrue(json.loads(stdout.getvalue())['ready'])
            with patch('sys.stdout', new_callable=io.StringIO):
                result = proof.main(['compare', '--gnome', str(right), '--tuna', str(left), '--out', str(root / 'wrong')])
            self.assertEqual(result, 1)

    def test_missing_actual_metadata_fails_before_any_guest_effect_without_fallback(self):
        with tempfile.TemporaryDirectory() as directory:
            root, guest = Path(directory), ControlledGuest()
            base = self.base_evidence(root)
            actual = provenance('gnome')
            actual.pop('commit')
            with self.assertRaisesRegex(ValueError, 'source/test-image'):
                proof.run(guest, guest, root, lambda *r: None, profile='gnome',
                          provenance=actual, base_evidence_dir=base)
            self.assertEqual(guest.calls, [])
            receipt = json.loads((root / 'member.json').read_bytes())
            self.assertFalse(receipt['ready'])
            self.assertEqual(receipt['provenance'], actual)

    def test_actual_drm_payload_and_wayland_peer_observations_are_mandatory(self):
        for mutate in (
            lambda a: a['payload_integrity']['stock'].update(checked_files=0),
            lambda a: a['payload_integrity']['current_tuna'].update(checked_files=7),
            lambda a: a['session']['native_gpu']['drm_fds'][0]['pci'].update(device='0x1111'),
            lambda a: a['session']['native_gpu']['drm_fds'][0]['connectors'][0].update(enabled='disabled'),
            lambda a: a['session']['wayland_socket'].update(peer=process('gnome-shell', 999)),
            lambda a: a['payload_integrity']['fixture_packages']['packages'].update(gdm='51.0-1'),
        ):
            value = admission('gnome')
            mutate(value)
            with self.assertRaises(ValueError):
                proof.validate_admission(value, 'gnome')

    def test_disabled_kvm_fails_on_actual_query_before_any_guest_effect(self):
        with tempfile.TemporaryDirectory() as directory:
            root, guest = Path(directory), ControlledGuest(failure='kvm')
            with self.assertRaisesRegex(ValueError, 'enabled KVM'):
                proof.run(guest, guest, root, lambda *r: None, profile='gnome',
                          provenance=provenance('gnome'), base_evidence_dir=self.base_evidence(root))
            self.assertEqual(guest.calls, [])
            receipt = json.loads((root / 'member.json').read_bytes())
            self.assertFalse(receipt['native_topology']['query-kvm']['result']['enabled'])
            self.assertFalse(receipt['ready'])

    def test_valid_hashed_but_foreign_raw_snapshot_cannot_qualify_member(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(ValueError, 'cleanup/retention'):
                self.collect(root, failure='raw-snapshot')
            receipt = json.loads((root / 'member.json').read_bytes())
            self.assertTrue(receipt['cleanup']['cleaned'])
            self.assertFalse(receipt['ready'])
            self.assertEqual(receipt['cleanup_errors'][-1]['stage'], 'retained-receipt-validation')
            self.assertEqual(json.loads((root / 'guest/snapshot.json').read_bytes())['nonce'], 'b' * 32)

    def test_large_full_trees_use_immutable_envelopes_and_lossless_bounded_chunks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt, guest, _ = self.collect(root, failure='large')
            self.assertTrue(receipt['ready'])
            self.assertTrue(all(size < 262144 for size in guest.wire_sizes))
            self.assertTrue(any(offset > 0 for _, offset, _ in guest.file_requests))
            self.assertTrue(all(limit <= 262144 for _, _, limit in guest.file_requests))
            self.assertEqual(len(receipt['wire_responses']), 20)  # +2 settle, +2 dismiss, +2 settle-stable
            self.assertEqual(receipt['settle_polls'],
                             {'pre-prepare-overview-dismiss': 1, 'overview-dismiss': 1,
                              'overview-settle': 2, 'applications-released': 1,
                              'windows-released': 1})
            first = receipt['wire_responses'][0]
            raw = (root / first['artifact']['path']).read_bytes()
            self.assertGreater(len(raw), 262144)
            self.assertEqual(proof.digest(raw), first['envelope']['response_file']['sha256'])
            body = json.loads(raw)
            labels = body['snapshot']['selection']['objects'][0]['children']
            self.assertEqual(len(labels), 600)
            self.assertEqual(labels[-1]['name'], 'controlled accessible label 599')
            proof.validate_member(root / 'member.json', 'gnome')

    def test_chunk_assembly_validates_actual_file_offsets_hashes_eof_and_progress(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'snapshot.json'
            value = snapshot(held=True)
            owner = value['selection']['shell_process']
            value['selection']['objects'][0]['children'].extend(
                node(owner, 'retained-row-' + str(i), 'retained accessible row ' + str(i), 'label')
                for i in range(900))
            data = json.dumps(value).encode()
            path.write_bytes(data)
            valid = RetainedFileTransport(path)
            assembled = proof.retrieve_guest_file(valid, 'gnome', 'a' * 32, 'snapshot.json')
            self.assertEqual(json.loads(assembled), value)
            self.assertGreater(len(valid.offsets), 1)
            self.assertEqual(valid.offsets, list(range(0, len(data), 262144)))
            for fault in ('offset', 'length', 'chunk-hash', 'full-hash', 'changed-size', 'early-eof', 'no-progress'):
                with self.subTest(fault=fault), self.assertRaises(ValueError):
                    proof.retrieve_guest_file(RetainedFileTransport(path, fault), 'gnome', 'a' * 32, 'snapshot.json')

    def test_direct_legacy_responses_and_foreign_envelope_hashes_have_no_fallback(self):
        for failure in ('direct-response', 'envelope-hash'):
            guest = ControlledGuest(failure=failure)
            with self.subTest(failure=failure), self.assertRaises(ValueError):
                proof.guest_call(guest, 'prepare', 'gnome', 'a' * 32)

    def test_causal_failure_is_retained_before_any_missing_prepare_or_snapshot_probe(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            receipt = {'error': {'message': 'prepare failed'}, 'cleanup_errors': [],
                       'retrieval_errors': [], 'guest_files': {}}
            original = json.dumps({'operation': 'prepare', 'error': 'original causal failure'}).encode()
            calls = []
            def retrieve(agent, profile, nonce, name):
                calls.append(name)
                if name == 'failure.json':
                    return original
                raise FileNotFoundError(name)
            with patch.object(proof, 'retrieve_guest_file', side_effect=retrieve):
                proof.retain_guest_files(object(), 'gnome', 'a' * 32, root, receipt)
            self.assertEqual(calls[0], 'failure.json')
            self.assertEqual((root / 'guest/failure.json').read_bytes(), original)
            self.assertEqual(receipt['guest_files']['failure.json']['sha256'], proof.digest(original))
            self.assertEqual({error['name'] for error in receipt['retrieval_errors']},
                             {'claim.json', 'prepare.json', 'snapshot.json', 'stop.json'})

    def test_network_panel_requires_actual_selected_gtk_ancestry_not_a_window_title(self):
        value = snapshot()
        settings = next(w for w in value['windows'] if w['role'] == 'settings')
        self.assertEqual(settings['name'], 'Settings')
        proof.validate_network_panel(settings)
        settings['panel_evidence'][0]['label_objects'][0]['atspi']['path'] = '/foreign/network-label'
        with self.assertRaisesRegex(ValueError, 'descendant'):
            proof.validate_network_panel(settings)

    def test_network_panel_accepts_button_role_navigation(self):
        value = snapshot()
        settings = next(w for w in value['windows'] if w['role'] == 'settings')
        navigation = settings['accessibility_tree']['children'][0]
        navigation['role_name'] = 'button'
        selected = settings['panel_evidence'][0]['selected_object']
        selected['role_name'] = 'button'
        proof.validate_network_panel(settings)


class SettlePollTests(unittest.TestCase):
    def test_first_poll_satisfied_never_sleeps(self):
        sleeps = []
        snapshot, polls = proof.poll_until_settled(
            lambda: 'fresh', lambda snap: snap == 'fresh', 15,
            clock=lambda: 0.0, sleep_fn=sleeps.append)
        self.assertEqual((snapshot, polls, sleeps), ('fresh', 1, []))

    def test_stale_cache_settles_on_third_poll(self):
        now, sleeps = [0.0], []
        script = iter(['stale', 'stale', 'fresh'])

        def observe():
            snap = next(script)
            now[0] += 1.9  # each observe is itself a multi-second AT-SPI dump
            return snap

        def sleep(secs):
            sleeps.append(secs)
            now[0] += secs

        snapshot, polls = proof.poll_until_settled(
            observe, lambda snap: snap == 'fresh', 15, clock=lambda: now[0], sleep_fn=sleep)
        self.assertEqual(snapshot, 'fresh')
        self.assertEqual(polls, 3)
        self.assertEqual(sleeps, [1.0, 1.0])

    def test_timeout_returns_last_snapshot_without_passing(self):
        now, sleeps = [0.0], []

        def observe():
            now[0] += 1.9
            return 'stale'

        def sleep(secs):
            sleeps.append(secs)
            now[0] += secs

        snapshot, polls = proof.poll_until_settled(
            observe, lambda snap: snap == 'fresh', 15, clock=lambda: now[0], sleep_fn=sleep)
        self.assertEqual(snapshot, 'stale')  # caller require() still fails on this
        self.assertGreaterEqual(polls, 5)  # ~15s / (1.9s observe + 1s gap)
        self.assertTrue(sleeps)

    def test_normalize_admission_tolerates_only_greeter_teardown(self):
        base = admission('gnome')
        flapped = copy.deepcopy(base)
        flapped['gdm']['greeters'] = [{'uid': 60578, 'shells': 1}]
        self.assertEqual(proof.normalize_admission(base), proof.normalize_admission(flapped))
        self.assertEqual(flapped['gdm']['greeters'], [{'uid': 60578, 'shells': 1}])
        substituted = copy.deepcopy(base)
        substituted['session']['shell'] = process('evil-shell', 999, 'gnome')
        self.assertNotEqual(proof.normalize_admission(base), proof.normalize_admission(substituted))

    def test_stable_context_ignores_live_content_but_not_identity(self):
        def snapshot(**overrides):
            window = {'role': 'settings', 'atspi': {'bus_name': 'b', 'path': '/w'},
                      'process': {'pid': 1}, 'name': 'Settings', 'states': [1, 25, 30],
                      'accessibility_tree': {'children': []}, 'panel_evidence': []}
            base = {'profile': 'tuna', 'nonce': 'n', 'admission': {}, 'settings': {},
                    'atspi': {}, 'fixture': {}, 'windows': [window],
                    'selection': {'active_windows': [window], 'selected': [], 'objects': []}}
            base.update(overrides)
            return base

        before = snapshot()
        churned = snapshot()
        churned['windows'][0] = dict(churned['windows'][0], states=[25, 30],
                                     accessibility_tree={'children': [{'x': 1}]})
        churned['selection'] = {'active_windows': churned['windows'], 'selected': [{'y': 2}],
                                'objects': []}
        self.assertEqual(proof.stable_context(before), proof.stable_context(churned))
        moved = snapshot()
        moved['selection'] = {'active_windows': [dict(moved['windows'][0], role='files-first')],
                              'selected': [], 'objects': []}
        self.assertNotEqual(proof.stable_context(before), proof.stable_context(moved))
        swapped = snapshot()
        swapped['windows'] = [dict(swapped['windows'][0], process={'pid': 2})]
        self.assertNotEqual(proof.stable_context(before), proof.stable_context(swapped))

    def test_overview_search_visible_matches_either_shell(self):
        tuna = {'selection': {'objects': [
            {'role_name': 'window', 'name': 'Search', 'states': [25, 30], 'children': [
                {'role_name': 'search box', 'name': 'Search', 'states': [25, 30], 'children': []}]}]}}
        self.assertTrue(proof.overview_search_visible(tuna))
        gnome = {'selection': {'objects': [
            {'role_name': 'entry', 'name': 'Type to search', 'states': [25, 30], 'children': []}]}}
        self.assertTrue(proof.overview_search_visible(gnome))

    def test_overview_search_hidden_or_absent_is_desktop(self):
        hidden = {'selection': {'objects': [
            {'role_name': 'search box', 'name': 'Search', 'states': [30], 'children': []}]}}
        self.assertFalse(proof.overview_search_visible(hidden))
        desktop = {'selection': {'objects': [
            {'role_name': 'push button', 'name': 'Activities', 'states': [25, 30], 'children': []}]}}
        self.assertFalse(proof.overview_search_visible(desktop))
        self.assertFalse(proof.overview_search_visible({'selection': {'objects': []}}))

    def test_stable_pair_returns_after_two_polls_without_sleep(self):
        sleeps = []
        snapshot, polls = proof.poll_until_stable(
            lambda: 'same', lambda snap: snap, 15,
            clock=lambda: 0.0, sleep_fn=sleeps.append)
        self.assertEqual((snapshot, polls, sleeps), ('same', 2, []))

    def test_changing_context_settles_on_third_poll(self):
        now, sleeps = [0.0], []
        script = iter(['a', 'b', 'b'])

        def observe():
            snap = next(script)
            now[0] += 1.9
            return snap

        def sleep(secs):
            sleeps.append(secs)
            now[0] += secs

        snapshot, polls = proof.poll_until_stable(
            observe, lambda snap: snap, 15, clock=lambda: now[0], sleep_fn=sleep)
        self.assertEqual(snapshot, 'b')
        self.assertEqual(polls, 3)
        self.assertEqual(sleeps, [1.0])

    def test_unstable_context_times_out_with_last_snapshot(self):
        now, seen = [0.0], []

        def observe():
            now[0] += 1.9
            seen.append(len(seen))
            return len(seen)

        def sleep(secs):
            now[0] += secs

        snapshot, polls = proof.poll_until_stable(
            observe, lambda snap: snap, 15, clock=lambda: now[0], sleep_fn=sleep)
        self.assertEqual(snapshot, len(seen))  # caller still gates on a fresh capture
        self.assertGreaterEqual(polls, 5)

    def test_predicate_sees_every_observation_in_order(self):
        seen = []
        snapshot, polls = proof.poll_until_settled(
            lambda: seen.append(len(seen)) or len(seen),
            lambda snap: seen.append(('check', snap)) or snap >= 2, 15,
            clock=lambda: 0.0, sleep_fn=lambda secs: None)
        self.assertEqual([entry for entry in seen if isinstance(entry, tuple)],
                         [('check', 1), ('check', 3)])
        self.assertEqual((snapshot, polls), (3, 2))


class PhaseContextTests(unittest.TestCase):
    # Round-25 GNOME shape: the top-bar clock stays selected from baseline,
    # while held, and after release; the St switcher item is an unnamed
    # visible 'button' with the names on sibling labels.
    def gnome_phases(self):
        base = snapshot(held=False)
        shell = base['selection']['shell_process']
        clock = node(shell, 'clock', '05', 'button', [8, 11, 23, 24, 30])
        base['selection']['selected'] = [copy.deepcopy(clock)]
        base['selection']['objects'] = [{**copy.deepcopy(clock), 'children': []}]

        def held(stamp, window_mode=False):
            snap = snapshot(held=True, stamp=stamp, window_mode=window_mode)
            item = node(shell, 'st-item-%d' % stamp, '', 'button', [8, 9, 10, 11, 23, 24, 25, 30])
            snap['selection']['selected'] = [copy.deepcopy(clock), item]
            tree = snap['selection']['objects'][0]
            tree['children'].append(copy.deepcopy(clock))
            tree['children'].append(copy.deepcopy(item))
            return snap, item

        def released_after():
            snap = snapshot(held=False)
            snap['selection']['selected'] = [copy.deepcopy(clock)]
            return snap

        app, app_item = held(1)
        down, _ = held(2)
        down['selection']['objects'][0]['children'].append(copy.deepcopy(app_item))
        win, _ = held(4, window_mode=True)
        idle = snapshot(held=False)
        return [{'phase': 'before', 'before': base, 'after': base},
                {'phase': 'applications-held', 'before': app, 'after': idle},
                {'phase': 'applications-down-held', 'before': down, 'after': idle},
                {'phase': 'applications-released', 'before': idle, 'after': released_after()},
                {'phase': 'windows-held', 'before': win, 'after': idle},
                {'phase': 'windows-released', 'before': idle, 'after': released_after()}]

    def test_persistent_baseline_selection_is_not_switcher_evidence(self):
        proof.validate_phase_context({'phases': self.gnome_phases()})

    def test_stale_popup_selection_still_fails_release(self):
        phases = self.gnome_phases()
        stuck = phases[2]['before']['selection']['selected'][1]
        phases[3]['after']['selection']['selected'].append(copy.deepcopy(stuck))
        with self.assertRaisesRegex(ValueError, 'release left the old Shell popup selected'):
            proof.validate_phase_context({'phases': phases})

    def test_held_without_new_selection_fails(self):
        phases = self.gnome_phases()
        phases[1]['before']['selection']['selected'] = phases[1]['before']['selection']['selected'][:1]
        with self.assertRaisesRegex(ValueError, 'no observed Shell switcher selection'):
            proof.validate_phase_context({'phases': phases})


class EmptyActiveTests(unittest.TestCase):
    # Round-23: no active window (switcher grab, commit gap) is an
    # observable state. Predicates must poll through it, never crash.
    def snap(self, role):
        active = [] if role == '' else [{'role': role}]
        return {'selection': {'active_windows': active}}

    def test_active_role_empty_is_gap_not_crash(self):
        self.assertEqual(proof.active_role(self.snap('')), '')
        self.assertEqual(proof.active_role(self.snap('settings')), 'settings')
        self.assertFalse(''.startswith('files-'))

    def test_released_to_different_excludes_gap(self):
        committed = self.snap('files-first')
        self.assertFalse(proof.released_to_different(self.snap(''), committed))
        self.assertFalse(proof.released_to_different(self.snap('files-first'), committed))
        self.assertTrue(proof.released_to_different(self.snap('files-second'), committed))
        self.assertTrue(proof.released_to_different(self.snap('settings'), committed))


if __name__ == '__main__':
    unittest.main()
