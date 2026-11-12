#!/usr/bin/env python3
"""Bounded physical native switcher collection; context readiness is not visual parity.

All timestamps describe host command/capture bounds or explicitly guest observations.
No timestamp here claims guest input delivery, presentation, or a WM/kernel window ID.
"""
import argparse
import base64
import copy
import hashlib
import json
import math
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import struct
import time
import zlib

ACTIONS = ('switch-applications', 'switch-applications-backward', 'switch-windows',
           'switch-windows-backward', 'switch-group', 'switch-group-backward')
PHASES = ('before', 'applications-held', 'applications-down-held',
          'applications-released', 'windows-held', 'windows-released')
FILES = ('claim.json', 'prepare.json', 'snapshot.json', 'stop.json', 'failure.json')
HOST_CLOCK = 'host CLOCK_MONOTONIC'
MIN_HOLD_NS = 500_000_000
PINNED_INDEX = 'sha256:29097ba660d6f9f7c35c2063b83cf7bbe830202a497852b9b1869bb84dfd6aa5'
PINNED_PLATFORM = 'sha256:104870f1a05fa0a75649ee4ed9952027b29ab12e53af83931ae69e8636e44c3f'
BASE_REPOSITORY = 'ghcr.io/tuna-os/marlin'
CERTIFICATE_SUBJECT = 'https://github.com/tuna-os/tunaOS/.github/workflows/reusable-build-image.yml@refs/heads/main'
CERTIFICATE_ISSUER = 'https://token.actions.githubusercontent.com'
BASE_FILES = ('index.json', 'platform-cosign.json', 'platform-verification.json')
FILE_CHUNK_BYTES = 262144


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def save_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def profile_name(profile):
    require(profile in ('gnome', 'tuna'), 'unknown native reference profile')
    return profile


def guest_call(agent, op, profile, nonce=None, *, retention=None, **payload):
    require(op in ('admit', 'prepare', 'observe', 'file', 'stop', 'overview'), 'unknown fixed operation')
    profile_name(profile)
    # Prepare alone gets headroom (round-18: GNOME Settings stayed absent
    # from every poll walk for 107s; 240s budget / 250s transport tests
    # slow-registration against poll-suppression). All other ops stay at 25s/30s.
    budget, timeout = (240, 250) if op == 'prepare' else (25, 30)
    body = {'budget_s': budget, **payload}
    if op != 'file':
        body['profile'] = profile
    if nonce is not None:
        require(re.fullmatch('[0-9a-f]{32}', nonce) is not None, 'invalid claim nonce')
        body['nonce'] = nonce
    value = agent.run('gnome-reference', op, json.dumps(body, sort_keys=True), timeout=timeout)
    require(isinstance(value, dict), 'guest operation returned no object')
    require(('response_file' in value) == (op in ('prepare', 'observe', 'stop')),
            'fixed operation did not use its required immutable response-file envelope')
    if 'response_file' in value:
        require(op in ('prepare', 'observe', 'stop') and value.get('schema') == 1
                and value.get('profile') == profile and value.get('nonce') == nonce
                and type(value.get('ready')) is bool, 'invalid actual response-file envelope')
        entry = value['response_file']
        require(isinstance(entry, dict) and re.fullmatch(op + r'-[0-9]{6}\.json', entry.get('name', '')) is not None,
                'response envelope does not name its fixed immutable operation receipt')
        envelope = copy.deepcopy(value)
        wire = {'operation': op, 'envelope': envelope}
        if retention is not None:
            out, receipt = retention
            receipt.setdefault('wire_responses', []).append(wire)
            require(entry['name'] not in receipt['guest_files'] and not (out / 'guest' / entry['name']).exists(),
                    'immutable response filename was replayed or already retained')
        data = retrieve_guest_file(agent, profile, nonce, entry['name'])
        require(entry.get('size') == len(data) and entry.get('sha256') == digest(data),
                'immutable full response differs from its actual envelope')
        value = json.loads(data)
        require(isinstance(value, dict) and 'response_file' not in value
                and all(value.get(k) == envelope[k] for k in ('schema', 'profile', 'nonce', 'ready')),
                'immutable full response disagrees with actual envelope')
        if retention is not None:
            directory = out / 'guest'
            directory.mkdir(exist_ok=True)
            (directory / entry['name']).write_bytes(data)
            retained = {'path': 'guest/' + entry['name'], 'size': len(data), 'sha256': digest(data)}
            receipt['guest_files'][entry['name']] = retained
            wire['artifact'] = retained
    return value


def guest_filename(name):
    require(isinstance(name, str) and (name in FILES or re.fullmatch(r'(prepare|observe|stop)-[0-9]{6}\.json', name) is not None),
            'guest receipt filename is not fixed/immutable allowlisted')
    return name


def retrieve_guest_file(agent, profile, nonce, name):
    guest_filename(name)
    data, offset, total, full_hash = bytearray(), 0, None, None
    while True:
        chunk = guest_call(agent, 'file', profile, nonce, name=name, offset=offset, limit=FILE_CHUNK_BYTES)
        raw = base64.b64decode(chunk['base64'], validate=True)
        require(chunk.get('name') == name and type(chunk.get('offset')) is int and chunk['offset'] == offset
                and type(chunk.get('length')) is int and chunk['length'] == len(raw)
                and 0 <= len(raw) <= FILE_CHUNK_BYTES and type(chunk.get('size')) is int
                and chunk['size'] >= offset + len(raw)
                and re.fullmatch('[0-9a-f]{64}', chunk.get('sha256', '')) is not None
                and chunk.get('chunk_sha256') == digest(raw) and type(chunk.get('eof')) is bool,
                'actual file chunk failed offset/length/hash/type validation')
        if total is None:
            total, full_hash = chunk['size'], chunk['sha256']
        require(chunk['size'] == total and chunk['sha256'] == full_hash, 'retained guest file changed between chunks')
        data.extend(raw)
        offset += len(raw)
        require(chunk['eof'] == (offset == total), 'actual file chunk EOF disagrees with full size')
        if chunk['eof']:
            require(digest(data) == full_hash, 'assembled actual guest file failed full SHA256')
            return bytes(data)
        require(bool(raw), 'actual file chunk made no progress before EOF')


def session_status(agent, *, profile):
    """Return the actual public admission, including unready login observations."""
    value = guest_call(agent, 'admit', profile)
    require(value.get('schema') == 1 and value.get('profile') == profile, 'admission schema/profile mismatch')
    require(type(value.get('ready')) is bool, 'missing actual admission readiness')
    require(type(value.get('gdm', {}).get('ready')) is bool, 'missing actual GDM status')
    require(type(value.get('session', {}).get('started')) is bool, 'missing actual login-started status')
    return value


def identity(value):
    require(isinstance(value, dict), 'missing original process identity')
    for field in ('pid', 'start_ticks', 'uid'):
        require(type(value.get(field)) is int and value[field] >= (0 if field == 'uid' else 1),
                'invalid process ' + field)
    require(isinstance(value.get('exe'), str) and value['exe'].startswith('/'), 'missing original ELF path')
    require(re.fullmatch('[0-9a-f]{64}', value.get('exe_sha256', '')) is not None, 'missing original ELF hash')
    require(isinstance(value.get('boot_id'), str) and bool(value['boot_id']), 'missing process boot identity')
    require(value.get('start_clock_domain') == 'Linux /proc starttime ticks since boot'
            and type(value.get('clock_ticks_per_second')) is int and value['clock_ticks_per_second'] > 0,
            'missing labeled actual process start clock')
    package = value.get('package', {})
    require(isinstance(package.get('name'), str) and bool(package['name'])
            and isinstance(package.get('version'), str) and bool(package['version']), 'missing original package')
    return value


def app_identity(value, executable):
    identity(value)
    require(value['uid'] == 1000 and value['exe'] == '/usr/bin/' + executable
            and value['package']['name'] == executable, 'wrong original app principal/package')
    return value


def atspi_id(node):
    value = node.get('atspi', {})
    require(value.get('id_domain') == 'AT-SPI', 'accessibility identity is not labeled AT-SPI')
    require(isinstance(value.get('bus_name'), str) and value['bus_name'].startswith(':'), 'missing AT-SPI bus peer')
    require(isinstance(value.get('path'), str) and value['path'].startswith('/'), 'missing AT-SPI object path')
    require(isinstance(node.get('name'), str) and isinstance(node.get('role_name'), str)
            and bool(node['role_name']), 'missing observed accessibility role/name')
    require(isinstance(node.get('states'), list) and all(type(s) is int for s in node['states']),
            'missing observed accessibility states')
    identity(node.get('process'))
    return value['bus_name'], value['path']


def validate_admission(value, profile):
    require(value.get('schema') == 1 and value.get('profile') == profile and value.get('ready') is True,
            'native admission is missing or rejected')
    require(value.get('missing') == [] and value.get('gdm', {}).get('available') is True
            and value.get('gdm', {}).get('ready') is False
            and value.get('session', {}).get('ready') is True, 'actual GDM/logind admission incomplete')
    gdm = value['gdm']
    manager = identity(gdm['process'])
    require(manager['uid'] == 0 and manager['exe'] == '/usr/bin/gdm'
            and manager['package']['name'] == 'gdm', 'not actual original GDM')
    require(re.fullmatch(r'51\.1(?:[-+.]\S+)?', manager['package']['version'].split(':')[-1]) is not None,
            'not actual packaged GDM51.1')
    require(gdm.get('service', {}).get('ActiveState') == 'active'
            and gdm['service'].get('SubState') == 'running'
            and str(manager['pid']) == str(gdm['service'].get('MainPID'))
            and isinstance(gdm.get('greeters'), list), 'missing actual running GDM service evidence')
    session = value['session']
    logind = session.get('logind', {})
    expected = {'User': '1000', 'Name': 'tuna-test', 'Class': 'user', 'Service': 'gdm-password',
                'Type': 'wayland', 'Active': 'yes', 'Remote': 'no', 'Seat': 'seat0', 'State': 'active'}
    require(session.get('started') is True and all(logind.get(k) == v for k, v in expected.items())
            and str(logind.get('VTNr', '')).isdigit() and int(logind['VTNr']) > 0
            and session.get('active_vt') == 'tty' + logind['VTNr'], 'missing actual native logind/VT')
    compositor = identity(session['compositor'])
    shell = identity(session['shell'])
    require(compositor['uid'] == shell['uid'] == 1000, 'native session principals are not UID1000')
    if profile == 'gnome':
        require(compositor == shell and compositor['exe'] == '/usr/bin/gnome-shell'
                and shell['package']['name'] == 'gnome-shell', 'stock reference is not original GNOME Shell')
        require(re.fullmatch(r'51\.1(?:[-+.]\S+)?', shell['package']['version'].split(':')[-1]) is not None,
                'stock reference is not actual packaged GNOME 51.1')
    else:
        require(compositor['exe'] == '/usr/bin/tuna-compositor' and shell['exe'] == '/usr/bin/tuna-shell-gtk'
                and compositor['package']['name'] == shell['package']['name'] == 'tuna-desktop',
                'Tuna reference is not original native Tuna')
    validate_payload(value.get('payload_integrity', {}))
    validate_native_gpu(session.get('native_gpu', {}), compositor)
    wayland = session.get('wayland_socket', {})
    require(wayland.get('identity_source') == 'kernel SO_PEERCRED'
            and wayland.get('peer') == compositor
            and isinstance(wayland.get('path'), str) and wayland['path'].startswith('/run/user/1000/'),
            'missing actual original compositor Wayland socket peer')
    user_bus = session.get('user_bus', {})
    require(user_bus.get('address') == 'unix:path=/run/user/1000/bus'
            and user_bus.get('identity_source') == 'kernel SO_PEERCRED'
            and identity(user_bus.get('peer'))['uid'] == 1000, 'missing actual native user bus kernel peer')


def validate_payload(evidence):
    require(evidence.get('ready') is True, 'original payload integrity missing')
    for key, name in (('stock', 'stock-payload.sha256'), ('current_tuna', 'current-payload.sha256'),
                      ('reference_packages', 'reference-packages.txt'), ('fixture_packages', 'fixture-packages.txt'),
                      ('current_package', 'current-package.txt')):
        value = evidence.get(key, {})
        require(value.get('receipt_path') == '/usr/share/tuna-vm/' + name
                and re.fullmatch('[0-9a-f]{64}', value.get('receipt_sha256', '')) is not None,
                'missing actual fixed original payload receipt')
        if key in ('stock', 'current_tuna'):
            require(type(value.get('checked_files')) is int and value['checked_files'] >= (4 if key == 'stock' else 7),
                    'missing actual original ELF manifest checks')
            require(key != 'current_tuna' or value['checked_files'] == 7, 'current payload is not the exact seven original ELFs')
        else:
            require(isinstance(value.get('packages'), dict) and bool(value['packages'])
                    and all(isinstance(n, str) and isinstance(v, str) and n and v for n, v in value['packages'].items()),
                    'missing actual original package versions')
    original, fixture = evidence['reference_packages']['packages'], evidence['fixture_packages']['packages']
    require(all(fixture.get(n) == v for n, v in original.items()), 'fixture modified an original installed package')
    current = evidence['current_package']['packages']
    require(set(current) == {'tuna-desktop'} and fixture.get('tuna-desktop') == current['tuna-desktop'],
            'current source package receipt is not the actual installed package')


def validate_native_gpu(value, compositor):
    require(value.get('ready') is True and value.get('compositor') == compositor
            and bool(value.get('drm_fds')), 'missing actual original compositor DRM ownership')
    connected = False
    for device in value['drm_fds']:
        require(type(device.get('fd')) is int and device['fd'] >= 0
                and re.fullmatch(r'/dev/dri/(?:card[0-9]+|renderD[0-9]+)', device.get('path', '')) is not None
                and all(type(device.get(k)) is int and device[k] >= 0 for k in ('major', 'minor'))
                and isinstance(device.get('sysfs_device'), str) and device['sysfs_device'].startswith('/sys/')
                and bool(device.get('driver')), 'missing actual native DRM character-device identity')
        pci = device.get('pci', {})
        require(re.fullmatch('[0-9a-fA-F]{4}:[0-9a-fA-F]{2}:[0-9a-fA-F]{2}\\.[0-7]', pci.get('address', '')) is not None
                and pci.get('vendor') == '0x1af4' and pci.get('device') == '0x1050'
                and pci.get('driver') == 'virtio-pci' and device['driver'] == 'virtio_gpu'
                and isinstance(pci.get('sysfs'), str) and pci['sysfs'].startswith('/sys/'),
                'actual native PCI device is not preserved virtio-vga')
        require(isinstance(device.get('connectors'), list), 'missing actual DRM connectors')
        if '/card' in device['path']:
            connected |= any(c.get('status') == 'connected' and c.get('enabled') == 'enabled'
                             and isinstance(c.get('modes'), list) and bool(c['modes']) and bool(c.get('name'))
                             for c in device['connectors'])
    require(connected, 'no actual enabled connected native DRM card output')


def native_gpu_context(snapshot):
    # Keep runtime fd/process identities in the receipt, not in paired config.
    return sorted({json.dumps({k: d[k] for k in ('path', 'major', 'minor', 'sysfs_device', 'driver', 'pci', 'connectors')},
                              sort_keys=True) for d in snapshot['admission']['session']['native_gpu']['drm_fds']
                   if '/card' in d['path']})


def validate_network_panel(window):
    tree, owner = window.get('accessibility_tree'), window['process']
    require(isinstance(tree, dict) and atspi_id(tree) == atspi_id(window), 'missing original Settings public window tree')
    nodes = {}
    def visit(node):
        key = atspi_id(node)
        require(node['process'] == owner and key not in nodes, 'foreign/duplicate Settings subtree object')
        nodes[key] = node
        for child in node.get('children', []):
            visit(child)
    visit(tree)
    leaf = lambda node: {k: v for k, v in node.items() if k != 'children'}
    require(all(tree[k] == window[k] for k in ('atspi', 'name', 'role_name', 'states', 'process')),
            'original Settings tree root differs from actual window')
    evidence = window.get('panel_evidence')
    require(isinstance(evidence, list) and bool(evidence), 'missing observed selected Network panel evidence')
    for entry in evidence:
        selected = entry['selected_object']
        node = nodes.get(atspi_id(selected))
        require(node is not None and leaf(node) == selected
                and selected['role_name'] in ('list item', 'page tab', 'tree item', 'radio button', 'button')
                and {23, 25, 30} <= set(selected['states']),
                'Network panel selection is not actual original navigation evidence')
        descendants, pending = {}, [node]
        while pending:
            child = pending.pop()
            descendants[atspi_id(child)] = leaf(child)
            pending.extend(child.get('children', []))
        labels = entry.get('label_objects')
        require(isinstance(labels, list) and bool(labels), 'selected Network navigation has no observed labels')
        for label in labels:
            require(descendants.get(atspi_id(label)) == label and label['name'].strip().casefold() == 'network'
                    and {25, 30} <= set(label['states']), 'Network label is not an actual selected-node descendant')


def validate_snapshot(value, profile, nonce):
    require(value.get('schema') == 1 and value.get('profile') == profile and value.get('nonce') == nonce,
            'snapshot claim/profile mismatch')
    require(value.get('ready') is True and value.get('missing') == [], 'snapshot has missing observations')
    require(value.get('clock_domain') == 'guest CLOCK_MONOTONIC'
            and type(value.get('observed_monotonic_ns')) is int and value['observed_monotonic_ns'] > 0,
            'missing labeled actual guest observation clock')
    validate_admission(value['admission'], profile)
    settings = value['settings']
    require(set(settings.get('bindings', {})) == set(ACTIONS), 'missing actual six bindings')
    for bindings in settings['bindings'].values():
        require(isinstance(bindings, list) and all(isinstance(b, str) for b in bindings), 'invalid actual bindings')
    for key in ('app_switcher', 'window_switcher'):
        require(type(settings.get(key, {}).get('current-workspace-only')) is bool, 'missing workspace filter')
    required_interface = {'text-scaling-factor', 'scaling-factor', 'gtk-theme', 'icon-theme', 'cursor-theme',
                          'cursor-size', 'color-scheme', 'font-name', 'document-font-name', 'monospace-font-name', 'enable-animations'}
    require(required_interface <= settings.get('interface', {}).keys(), 'missing actual interface settings')
    require({'sources', 'current', 'mru-sources', 'xkb-options'} <= settings.get('input_sources', {}).keys(),
            'missing actual input-source settings')
    interface = settings['interface']
    require(type(interface['scaling-factor']) is int and interface['scaling-factor'] >= 0
            and type(interface['text-scaling-factor']) in (int, float)
            and math.isfinite(interface['text-scaling-factor']) and interface['text-scaling-factor'] > 0,
            'missing actual interface scaling observations')
    display = settings.get('display_config', {})
    require(bool(display.get('monitors')) and bool(display.get('logical_monitors'))
            and 'properties' in display and 'serial' in display, 'missing public display topology/scales')
    for monitor in display['monitors']:
        require(isinstance(monitor, list) and len(monitor) == 3 and bool(monitor[0]) and bool(monitor[1])
                and isinstance(monitor[2], dict), 'partial actual public monitor state')
    for logical in display['logical_monitors']:
        require(isinstance(logical, list) and len(logical) == 7
                and all(type(logical[k]) is int for k in (0, 1, 3))
                and type(logical[2]) in (int, float) and math.isfinite(logical[2]) and logical[2] > 0
                and type(logical[4]) is bool and bool(logical[5]) and isinstance(logical[6], dict),
                'missing actual public logical-monitor topology/scale')
    accessibility = value['atspi']
    require(bool(accessibility.get('address')) and bool(accessibility.get('peers')), 'missing public AT-SPI peers')
    identity(accessibility['registry'])
    require(accessibility['registry']['package']['name'] == 'at-spi2-core', 'public registry is not original at-spi2-core')
    for peer in accessibility['peers']:
        identity(peer)
    def resolved_peer(field):
        raw = accessibility.get(field)
        peers = [p for p in accessibility['peers'] if {k: v for k, v in p.items() if k != 'package'} == raw]
        require(len(peers) == 1 and peers[0]['uid'] == 1000, 'missing unique actual public bus/launcher peer')
        return peers[0]
    require(resolved_peer('session_bus_peer') == value['admission']['session']['user_bus']['peer'],
            'AT-SPI observation used a foreign session bus')
    resolved_peer('bus_peer')
    require(resolved_peer('launcher')['package']['name'] == 'at-spi2-core', 'public AT-SPI launcher is not original')
    windows = value['windows']
    require(isinstance(windows, list) and len(windows) == 3, 'fixture is not exactly two Files plus Settings')
    require(sorted(w.get('role', '') for w in windows) == ['files-first', 'files-second', 'settings'],
            'missing actual fixture window roles')
    ids = []
    for window in windows:
        ids.append(atspi_id(window))
        require(window['role_name'].lower() in ('frame', 'window'), 'fixture object is not an actual window')
        require(25 in window['states'] and 30 in window['states'], 'fixture window is not showing and visible')
        app_identity(window['process'], 'gnome-control-center' if window['role'] == 'settings' else 'nautilus')
        if window['role'] == 'settings':
            validate_network_panel(window)
    require(len(set(ids)) == 3, 'duplicate accessibility window object')
    fixture = value['fixture']
    dirs = fixture.get('directories', [])
    require(len(dirs) == 2 and len(set(dirs)) == 2 and all(isinstance(d, str) and d.startswith('/') for d in dirs),
            'missing two distinct original fixture directories')
    require([Path(d).name for d in dirs] == ['first', 'second'] and Path(dirs[0]).parent == Path(dirs[1]).parent,
            'actual fixture directory context differs')
    directory_ids = fixture.get('directory_identities', [])
    require(len(directory_ids) == 3 and len({d['path'] for d in directory_ids}) == 3
            and set(dirs).issubset({d['path'] for d in directory_ids}),
            'missing owned base and two directory identities')
    for directory in directory_ids:
        require(directory.get('uid') == 1000 and directory.get('mode') == 0o700
                and all(type(directory.get(k)) is int for k in ('device', 'inode', 'mode')),
                'missing actual owned directory stat identity')
    processes = fixture.get('processes', [])
    require(bool(processes), 'missing owned original fixture principals')
    for proc in processes:
        require(proc in [w['process'] for w in windows], 'fixture owns a non-window application principal')
    require(all(w['process'] in processes for w in windows), 'unowned fixture window principal')
    for launcher in fixture.get('launchers', []):
        app_identity(launcher, 'nautilus')
    selection = value['selection']
    shell = identity(selection.get('shell_process'))
    require(shell == value['admission']['session']['shell'], 'selection is not owned by admitted shell')
    require(isinstance(selection.get('objects'), list) and bool(selection['objects']), 'missing public Shell tree')
    tree_nodes = {}
    def visit(node):
        atspi_id(node)
        require(node['process'] == shell, 'Shell tree has foreign owner')
        tree_nodes[atspi_id(node)] = {k: v for k, v in node.items() if k != 'children'}
        for child in node.get('children', []):
            visit(child)
    for node in selection['objects']:
        visit(node)
    for field in ('selected', 'focused'):
        require(isinstance(selection.get(field), list), 'missing actual Shell ' + field)
        for node in selection[field]:
            atspi_id(node)
            require(node['process'] == shell, 'selected accessibility object is not Shell-owned')
            require(atspi_id(node) not in ids, 'Shell selection falsely joined to GTK window identity')
            require(tree_nodes.get(atspi_id(node)) == {k: v for k, v in node.items() if k != 'children'},
                    'selected/focused object is not in actual retained Shell tree')
            require((23 if field == 'selected' else 12) in node['states'],
                    'selection/focus has no actual accessibility state')
    active = selection.get('active_windows')
    # Zero active is observable (round-23: switcher grab, commit gap);
    # only ambiguity (two active) is a contradiction.
    require(isinstance(active, list) and len(active) <= 1, 'ambiguous active GTK app windows')
    if active:
        require(1 in active[0]['states'], 'active app window lacks actual ACTIVE accessibility state')
        require(atspi_id(active[0]) in ids and active[0] in windows, 'active app window not independently observed')
    return value


def active_role(snapshot):
    # Empty active is an observable state (round-23: switcher grab while a
    # modifier is held, or a post-commit gap), never a crash: '' keeps the
    # applications-released startswith predicate polling and fails the
    # baseline/settings requires honestly. Callers testing "a different
    # window activated" must exclude '' explicitly (see windows-released).
    windows = snapshot['selection']['active_windows']
    return windows[0]['role'] if windows else ''


def released_to_different(snapshot, committed):
    """A different window is actually active after release.

    A commit gap (no active window) never qualifies: '' differs from every
    role string, so it must be excluded explicitly or a gap observation
    would satisfy "a different window activated".
    """
    return active_role(snapshot) not in ('', active_role(committed))


def normalize_admission(admission):
    """Admission minus transitional GDM greeter presence.

    Greeters tear down asynchronously after login; an admission taken seconds
    before prepare can legitimately list one more greeter than every later
    observation while all security-relevant principals (session, shell,
    compositor, buses, processes) stay identical. Comparisons use this so a
    benign teardown mid-run is not misread as fixture substitution.
    """
    normalized = copy.deepcopy(admission)
    if isinstance(normalized.get('gdm'), dict):
        normalized['gdm']['greeters'] = []
    return normalized


def stable_context(snapshot):
    """Fixture identity across observations: same session, windows, and focus.

    Compares identities, never live content. Window records carry full
    accessibility trees and panel evidence, and selection carries live shell
    UI state; Tracker churn, live panels, and fade animations legitimately
    change those between two observations of one unmoved fixture. What must
    not change is which session, which processes, which windows, and which of
    them is active. Replay protection stays separate (observed_monotonic_ns).
    """
    context = {key: snapshot[key] for key in ('profile', 'nonce', 'settings', 'atspi', 'fixture')}
    context['admission'] = normalize_admission(snapshot['admission'])
    context['windows'] = [(w['role'], w['atspi'], w['process']) for w in snapshot['windows']]
    context['active'] = [(w['role'], w['atspi'], w['process'])
                         for w in snapshot['selection']['active_windows']]
    return context


def shell_nodes(snapshot):
    pending = list(snapshot['selection']['objects'])
    nodes = {}
    while pending:
        node = pending.pop()
        nodes[atspi_id(node)] = node
        pending.extend(node.get('children', []))
    return nodes


def validate_phase_context(receipt):
    phases = receipt['phases']
    require([p['phase'] for p in phases] == list(PHASES), 'missing fixed phase context')
    baseline = shell_nodes(phases[0]['before'])
    visible = lambda n: 25 in n['states'] and 30 in n['states']
    # Both shells' switcher items are buttons (round-25: GNOME St exposes
    # role 'button' with the app/window names on sibling labels, while GTK
    # exposes named 'push button'/'list item' directly).
    item_role = lambda n: n['role_name'].lower() in ('push button', 'list item', 'button')
    label_role = lambda n: n['role_name'].lower() == 'label'
    baseline_visible = {key for key, node in baseline.items() if visible(node)}
    # The shell may carry a persistent selection (round-25: GNOME keeps the
    # top-bar clock selected at baseline); only newly-selected nodes are
    # switcher-popup evidence, and only those must clear on release.
    baseline_selected = {atspi_id(n) for n in phases[0]['before']['selection']['selected']}
    for index in (1, 2, 4):
        snapshot = phases[index]['before']
        selected = [n for n in snapshot['selection']['selected'] if atspi_id(n) not in baseline_selected]
        require(bool(selected), 'no observed Shell switcher selection')
        for node in selected:
            require(visible(node) and item_role(node) and atspi_id(node) not in baseline_visible,
                    'held selection is not a newly showing Shell switcher item')
        # Validate actual popup structure without assigning Shell object IDs to
        # application windows. GNOME St.Button and GTK ListItem remain distinct.
        items = [n for key, n in shell_nodes(snapshot).items()
                 if key not in baseline_visible and visible(n)
                 and (item_role(n) or (label_role(n) and n['name']))]
        require(len(items) >= 2, 'held Shell popup lacks actual switcher item context')
        require(any(n['name'] for n in items), 'held Shell popup shows no named switcher item')
    app = phases[1]['before']['selection']
    down = phases[2]['before']['selection']
    require(app != down, 'physical Down produced no observed switcher selection/tree change')
    require(all(atspi_id(n) in shell_nodes(phases[2]['before']) for n in app['selected']),
            'Down did not retain the same actual application popup objects')
    for held_index, released_index in ((2, 3), (4, 5)):
        old = ({atspi_id(n) for n in phases[held_index]['before']['selection']['selected']}
               - baseline_selected)
        released = phases[released_index]['after']['selection']['selected']
        require(not old.intersection(atspi_id(n) for n in released), 'release left the old Shell popup selected')


def choose_binding(settings, action):
    """Interpret a configured accelerator, never install or rewrite one."""
    modifiers = {'alt': 'alt', 'mod1': 'alt', 'super': 'meta_l', 'meta': 'meta_l',
                 'control': 'ctrl', 'ctrl': 'ctrl', 'primary': 'ctrl', 'shift': 'shift'}
    keys = {'Tab': 'tab', 'ISO_Left_Tab': 'tab', 'Escape': 'esc', 'Above_Tab': 'grave_accent',
            'grave': 'grave_accent', 'F6': 'f6'}
    for binding in settings['bindings'][action]:
        match = re.fullmatch(r'((?:<[A-Za-z0-9]+>)+)([A-Za-z0-9_]+)', binding)
        if not match or match[2] not in keys:
            continue
        names = re.findall(r'<([^>]+)>', match[1])
        if not all(n.lower() in modifiers for n in names):
            continue
        mods = [modifiers[n.lower()] for n in names]
        if len(set(mods)) != len(mods):
            continue
        if match[2] == 'ISO_Left_Tab' and 'shift' not in mods:
            mods.append('shift')
        return {'action': action, 'binding': binding, 'modifiers': mods, 'key': keys[match[2]]}
    raise ValueError('no supported actual configured physical binding for ' + action)


def png_info(data):
    """Validate actual retained PNG structure/CRCs and decompressed scanline size."""
    require(data.startswith(b'\x89PNG\r\n\x1a\n'), 'capture is not PNG bytes')
    offset, header, compressed, ended = 8, None, bytearray(), False
    while offset < len(data):
        require(offset + 12 <= len(data), 'truncated PNG chunk')
        size = struct.unpack('>I', data[offset:offset + 4])[0]
        kind = data[offset + 4:offset + 8]
        end = offset + 12 + size
        require(end <= len(data), 'truncated PNG data')
        payload = data[offset + 8:offset + 8 + size]
        crc = struct.unpack('>I', data[offset + 8 + size:end])[0]
        require(zlib.crc32(kind + payload) & 0xffffffff == crc, 'PNG CRC mismatch')
        if header is None:
            require(kind == b'IHDR' and size == 13, 'missing PNG header')
            header = struct.unpack('>IIBBBBB', payload)
        elif kind == b'IDAT':
            compressed.extend(payload)
        elif kind == b'IEND':
            require(size == 0 and end == len(data), 'invalid PNG ending')
            ended = True
        offset = end
    require(header is not None and ended and bool(compressed), 'incomplete PNG capture')
    width, height, depth, color, compression, filtering, interlace = header
    require((width, height) == (1280, 800), 'capture differs from fixed 1280x800 topology')
    require(depth == 8 and color in (2, 6) and compression == filtering == interlace == 0,
            'unsupported actual PNG layout')
    stride = width * (3 if color == 2 else 4) + 1
    decoder = zlib.decompressobj()
    raw = decoder.decompress(bytes(compressed), stride * height + 1)
    require(decoder.eof and not decoder.unused_data and len(raw) == stride * height,
            'invalid actual PNG pixel stream')
    require(all(raw[row * stride] <= 4 for row in range(height)), 'invalid PNG scanline filter')
    return {'sha256': digest(data), 'size': len(data), 'width': width, 'height': height}


class PhysicalInput:
    def __init__(self, qmp, commands):
        self.qmp, self.commands, self.held = qmp, commands, set()

    def command(self, events, *, down=None, key=None):
        before = sorted(self.held)
        if down is True:
            require(key not in self.held, 'attempt to press already owned key')
            self.held.add(key)  # A failed command may have delivered; finally must release it.
        if down is False:
            require(key in self.held, 'attempt to release unowned key')
        item = {'command': 'input-send-event', 'events': events, 'clock_domain': HOST_CLOCK,
                'begin_ns': time.monotonic_ns(), 'held_before': before}
        self.commands.append(item)
        try:
            self.qmp.cmd('input-send-event', events=events)
            if down is False:
                self.held.remove(key)
            item['ok'] = True
        except BaseException as error:
            item['ok'] = False
            item['error_type'] = type(error).__name__
            raise
        finally:
            item['end_ns'] = time.monotonic_ns()
            item['held_after'] = sorted(self.held)

    def key(self, key, down):
        self.command([{'type': 'key', 'data': {'down': down, 'key': {'type': 'qcode', 'data': key}}}],
                     down=down, key=key)

    def tap(self, key):
        self.key(key, True)
        time.sleep(0.08)
        self.key(key, False)

    def pointer_outside(self):
        # Same physical point on each member, no click and no popup hover.
        self.command([{'type': 'abs', 'data': {'axis': 'x', 'value': 32767}},
                      {'type': 'abs', 'data': {'axis': 'y', 'value': 32767}}])

    def begin(self, binding):
        require(not self.held, 'independent action began with owned keys held')
        for modifier in binding['modifiers']:
            self.key(modifier, True)
        self.tap(binding['key'])
        # Wait a full 500ms after the action trigger has completed, not merely
        # after modifier-down (which precedes the trigger itself).
        anchor = self.commands[-1]['end_ns']
        remaining = (anchor + MIN_HOLD_NS - time.monotonic_ns()) / 1e9
        if remaining > 0:
            time.sleep(remaining)
        return anchor

    def release(self):
        errors = []
        for key in sorted(self.held):
            try:
                self.key(key, False)
            except BaseException as error:
                errors.append({'key': key, 'error_type': type(error).__name__})
        return errors


def retain_guest_files(agent, profile, nonce, out, receipt):
    directory = out / 'guest'
    directory.mkdir(exist_ok=True)
    names = set(FILES[:-1])
    if receipt.get('error') or receipt['cleanup_errors']:
        names.add('failure.json')
    responses = [receipt.get('prepare', {}), receipt.get('cleanup', {}), *receipt.get('observations', [])]
    responses.append(receipt.get('prepare', {}).get('snapshot', {}))
    for response in responses:
        manifest = response.get('files', [])
        if not isinstance(manifest, list):
            receipt['retrieval_errors'].append({'stage': 'guest-file-manifest', 'message': 'manifest is not a list'})
            continue
        for name in manifest:
            try:
                names.add(guest_filename(name))
            except Exception as error:
                receipt['retrieval_errors'].append({'stage': 'guest-file-manifest', 'message': str(error)})
    # Retain the causal operation failure before any absent receipt probes.
    for name in sorted(names, key=lambda name: (name != 'failure.json', name)):
        try:
            if name not in FILES and name in receipt['guest_files']:
                retained_file(out, receipt['guest_files'][name])
                continue
            data = retrieve_guest_file(agent, profile, nonce, name)
            (directory / name).write_bytes(data)
            receipt['guest_files'][name] = {'path': 'guest/' + name, 'sha256': digest(data), 'size': len(data)}
            if name == 'stop.json' and 'cleanup' not in receipt:
                receipt['cleanup_retained'] = json.loads(data)
        except Exception as error:
            receipt['retrieval_errors'].append({'name': name, 'error_type': type(error).__name__, 'message': str(error)})


def validate_cleanup(cleanup, profile, nonce, prepared=None):
    require(cleanup.get('schema') == 1 and cleanup.get('profile') == profile and cleanup.get('nonce') == nonce,
            'cleanup claim/profile mismatch')
    require(cleanup.get('ready') is True and cleanup.get('cleaned') is True and cleanup.get('errors') == [],
            'strict owned fixture cleanup failed')
    processes = cleanup.get('processes', [])
    require(bool(processes) and all(p.get('retired') is True and p.get('method') in ('pidfd', 'already-exited') for p in processes),
            'missing strict original-principal retirement')
    for entry in processes:
        identity(entry['original'])
    directories = cleanup.get('directories', [])
    require(len(directories) == 3 and all(d.get('removed') is True and isinstance(d.get('original'), dict)
                                        for d in directories), 'missing strict owned-directory cleanup')
    if prepared is not None:
        fixture = prepared['snapshot']['fixture']
        originals = fixture['processes'] + fixture.get('launchers', [])
        require(sorted((json.dumps(p, sort_keys=True) for p in originals)) ==
                sorted((json.dumps(p['original'], sort_keys=True) for p in processes)),
                'cleanup did not retire exactly prepared original principals')
        require(sorted((json.dumps(d, sort_keys=True) for d in prepared['snapshot']['fixture']['directory_identities'])) ==
                sorted((json.dumps(d['original'], sort_keys=True) for d in directories)),
                'cleanup did not remove exactly prepared original directories')


def query_topology(qmp, receipt):
    values = {}
    receipt['native_topology'] = values
    for command in ('query-kvm', 'query-pci'):
        observation = {'command': command, 'clock_domain': HOST_CLOCK, 'begin_ns': time.monotonic_ns()}
        values[command] = observation
        try:
            observation['result'] = qmp.cmd(command)
        finally:
            observation['end_ns'] = time.monotonic_ns()
    validate_topology(receipt)


def pci_topology(buses):
    require(isinstance(buses, list) and bool(buses), 'missing actual QMP PCI buses')
    def devices(items):
        require(isinstance(items, list), 'missing actual PCI device list')
        result = []
        for item in items:
            require(all(k in item for k in ('slot', 'function', 'id', 'class_info')), 'partial actual PCI identity')
            value = {k: item[k] for k in ('slot', 'function', 'id', 'class_info')}
            value['qdev_id'] = item.get('qdev_id')
            if 'pci_bridge' in item:
                bridge = item['pci_bridge']['bus']
                value['bridge'] = {k: bridge[k] for k in ('number', 'secondary', 'subordinate') if k in bridge}
                value['bridge']['devices'] = devices(item['pci_bridge'].get('devices', []))
            result.append(value)
        return result
    return [{'bus': bus['bus'], 'devices': devices(bus['devices'])} for bus in buses]


def validate_topology(receipt):
    previous = receipt['begin_ns']
    for name in ('query-kvm', 'query-pci'):
        value = receipt['native_topology'][name]
        require(value.get('command') == name and value.get('clock_domain') == HOST_CLOCK
                and type(value.get('begin_ns')) is int and type(value.get('end_ns')) is int
                and previous <= value['begin_ns'] <= value['end_ns'], 'missing actual QMP topology command bounds')
        if 'end_ns' in receipt:
            require(value['end_ns'] <= receipt['end_ns'], 'topology query outside actual member bounds')
        previous = value['end_ns']
    kvm = receipt['native_topology']['query-kvm']['result']
    require(kvm.get('enabled') is True and kvm.get('present') is True, 'QMP does not observe actual enabled KVM')
    pci_topology(receipt['native_topology']['query-pci']['result'])
    addresses = []
    def visit(devices):
        for device in devices:
            if (device.get('id', {}).get('vendor') == 0x1af4 and device['id'].get('device') == 0x1050
                    and device.get('class_info', {}).get('class') == 0x0300):
                addresses.append(f"0000:{device['bus']:02x}:{device['slot']:02x}.{device['function']}")
            if 'pci_bridge' in device:
                visit(device['pci_bridge'].get('devices', []))
    for bus in receipt['native_topology']['query-pci']['result']:
        visit(bus['devices'])
    require(len(addresses) == 1, 'QMP does not observe exactly the preserved native virtio-vga PCI device')
    if 'admission' in receipt:
        observed = {d['pci']['address'] for d in receipt['admission']['session']['native_gpu']['drm_fds']}
        require(observed == set(addresses), 'actual guest DRM PCI identity differs from actual QMP PCI device')


def validate_provenance(value):
    require(value.get('base_image') == BASE_REPOSITORY + '@' + PINNED_PLATFORM,
            'not the required actual Main104870amd64 platform base')
    require(re.fullmatch('[0-9a-f]{40}', value.get('commit', '')) is not None
            and isinstance(value.get('test_image_id'), str) and bool(value['test_image_id']),
            'missing actual source/test-image identity')


def cosign_argv(executable, platform_digest):
    require(isinstance(platform_digest, str) and re.fullmatch(r'sha256:[0-9a-f]{64}', platform_digest) is not None,
            'platform digest must be strict sha256 lowercase hex')
    return [executable, 'verify', BASE_REPOSITORY + '@' + platform_digest,
            '--certificate-identity', CERTIFICATE_SUBJECT,
            '--certificate-oidc-issuer', CERTIFICATE_ISSUER, '--output', 'json']


def verify_platform_image(directory, platform_digest):
    """Retain one actual platform verification, not invented certificate claims."""
    cosign_argv('cosign', platform_digest)
    directory = Path(directory)
    directory.mkdir(parents=True, exist_ok=True)
    executable = shutil.which('cosign')
    require(executable is not None, 'pinned native cosign executable unavailable')
    executable = str(Path(executable).resolve(strict=True))
    version = subprocess.check_output([executable, 'version'], timeout=30, text=True)
    argv = cosign_argv(executable, platform_digest)
    begin = time.monotonic_ns()
    result = subprocess.run(argv, capture_output=True, timeout=180)
    end = time.monotonic_ns()
    output = directory / 'platform-cosign.json'
    output.write_bytes(result.stdout)
    witness = {
        'schema': 1, 'argv': argv, 'exit_code': result.returncode,
        'begin_host_monotonic_ns': begin, 'end_host_monotonic_ns': end,
        'tool': {'executable': executable, 'sha256': digest(Path(executable).read_bytes()), 'version': version},
        'stdout_sha256': digest(result.stdout), 'stdout_bytes': len(result.stdout),
        'stderr': result.stderr.decode('utf-8', errors='strict'),
        'scope': 'actual successful verifier enforces certificate constraints; not an extracted certificate identity',
    }
    witness_bytes = (json.dumps(witness, sort_keys=True) + '\n').encode()
    (directory / 'platform-verification.json').write_bytes(witness_bytes)
    validate_platform_verification(result.stdout, witness_bytes, platform_digest)


def verify_base_image(directory):
    """Verify only the pinned stock reference and its exact index membership."""
    directory = Path(directory)
    verify_platform_image(directory, PINNED_PLATFORM)
    validate_base_bytes(*((directory / name).read_bytes() for name in BASE_FILES))


def validate_base_bytes(index_bytes, cosign_bytes, verification_bytes):
    require('sha256:' + digest(index_bytes) == PINNED_INDEX, 'raw base index does not hash to required index29097')
    index = json.loads(index_bytes)
    require(index.get('schemaVersion') == 2 and isinstance(index.get('manifests'), list), 'not an actual OCI image index')
    members = [m for m in index['manifests'] if m.get('platform', {}).get('os') == 'linux'
               and m.get('platform', {}).get('architecture') == 'amd64']
    require(len(members) == 1 and members[0].get('digest') == PINNED_PLATFORM,
            'required platform104870 is not the unique linux/amd64 index member')
    validate_platform_verification(cosign_bytes, verification_bytes, PINNED_PLATFORM)


def validate_platform_verification(cosign_bytes, verification_bytes, platform_digest):
    cosign_argv('cosign', platform_digest)
    signatures = json.loads(cosign_bytes)
    require(isinstance(signatures, list) and bool(signatures), 'no retained real cosign verify output')
    for signature in signatures:
        require(isinstance(signature, dict), 'cosign output is not a signature object')
        critical, optional = signature.get('critical', {}), signature.get('optional')
        require(critical.get('image', {}).get('docker-manifest-digest') == platform_digest
                and critical.get('identity', {}).get('docker-reference') == BASE_REPOSITORY + '@' + platform_digest
                and critical.get('type') == 'https://sigstore.dev/cosign/sign/v1'
                and isinstance(optional, dict),
                'cosign output does not name required actual platform image')
    witness = json.loads(verification_bytes)
    require(isinstance(witness, dict), 'actual cosign verification process witness is missing')
    tool = witness.get('tool', {})
    require(isinstance(tool, dict), 'actual cosign verification tool witness is missing')
    executable = tool.get('executable', '')
    require(witness.get('schema') == 1 and isinstance(executable, str) and executable.startswith('/')
            and witness.get('argv') == cosign_argv(executable, platform_digest)
            and type(witness.get('exit_code')) is int and witness['exit_code'] == 0,
            'actual successful cosign invocation with exact certificate subject/issuer is missing')
    require(isinstance(tool.get('sha256'), str) and re.fullmatch('[0-9a-f]{64}', tool['sha256']) is not None
            and isinstance(tool.get('version'), str)
            and re.search(r'(?m)^GitVersion:\s+v3\.1\.4\s*$', tool['version']) is not None,
            'verification does not retain the actual pinned native cosign tool')
    require(type(witness.get('begin_host_monotonic_ns')) is int
            and type(witness.get('end_host_monotonic_ns')) is int
            and 0 < witness['begin_host_monotonic_ns'] < witness['end_host_monotonic_ns']
            and witness.get('stdout_sha256') == digest(cosign_bytes)
            and type(witness.get('stdout_bytes')) is int and witness['stdout_bytes'] == len(cosign_bytes)
            and isinstance(witness.get('stderr'), str),
            'verification process/output identity differs from retained actual cosign bytes')


def retain_base_evidence(source, out):
    source = Path(source)
    target = out / 'base-verification'
    target.mkdir(exist_ok=True)
    entries, contents = {}, {}
    for name in BASE_FILES:
        data = (source / name).read_bytes()
        (target / name).write_bytes(data)
        contents[name] = data
        entries[name] = {'path': 'base-verification/' + name, 'sha256': digest(data), 'size': len(data)}
    validate_base_bytes(*(contents[name] for name in BASE_FILES))
    return {'files': entries,
            'scope': 'retained actual normal-CI preflight output; JSON validation is not cryptographic signature verification'}


def overview_search_visible(snapshot):
    """Whether the shell trees show a visible overview search entry.

    Both shells expose the overview search as a visible AT-SPI entry (`search box`
    on Tuna, `entry` under GNOME Shell's St.Entry); neither shows a search entry
    on the plain desktop. A switcher commit under an open overview parks instead
    of applying, freezing AT-SPI window-active forever, so the baseline must be
    overview-free on both legs.
    """
    found = []

    def walk(nodes):
        for node in nodes:
            states = set(node.get('states', []))
            if (node.get('role_name') in ('search box', 'entry') and 25 in states and 30 in states):
                found.append(node)
            walk(node.get('children', []))

    walk(snapshot.get('selection', {}).get('objects', []))
    return bool(found)


def poll_until_stable(observe_fn, key_fn, timeout_s, clock, sleep_fn):
    """Poll `observe_fn()` until two consecutive `key_fn` values agree.

    Dismissing the overview starts a close animation: the search entry vanishes
    before window states and selection settle, so a single overview-free
    observation is not a stable baseline. Returns the last snapshot plus the
    poll count; on timeout the caller still gates on a fresh capture.
    """
    polls, start = 0, clock()
    previous = key_fn(observe_fn())
    polls += 1
    while True:
        snapshot = observe_fn()
        polls += 1
        if key_fn(snapshot) == previous:
            break
        previous = key_fn(snapshot)
        if clock() - start >= timeout_s:
            break
        sleep_fn(1.0)
    return snapshot, polls


def poll_until_settled(observe_fn, predicate, timeout_s, clock, sleep_fn):
    """Poll `observe_fn()` until `predicate` holds or `timeout_s` elapses.

    Returns the last snapshot plus the poll count. Each `observe()` is itself a
    multi-second AT-SPI dump under bus saturation, so the loop sleeps 1s between
    attempts to let focus events propagate instead of dump-starving the bus.
    Timeout returns the last snapshot (the caller still gates on it); it never
    passes a check by itself.
    """
    polls, start = 0, clock()
    while True:
        snapshot = observe_fn()
        polls += 1
        if predicate(snapshot):
            break
        if clock() - start >= timeout_s:
            break
        sleep_fn(1.0)
    return snapshot, polls


def run(qmp, agent, out, record, *, profile, provenance, base_evidence_dir):
    """Collect a real member. Any missing observation or cleanup is a hard failure."""
    profile_name(profile)
    require(isinstance(provenance, dict) and bool(provenance), 'caller supplied no actual provenance')
    out = Path(out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    require(not (out / 'member.json').exists() and not any((out / (p + '.png')).exists() for p in PHASES),
            'refusing to reuse a prior member collection directory')
    nonce = secrets.token_hex(16)
    receipt = {'schema': 1, 'kind': 'native-switcher-reference-member', 'profile': profile, 'nonce': nonce,
               'provenance': copy.deepcopy(provenance), 'clock_domain': HOST_CLOCK, 'begin_ns': time.monotonic_ns(),
               'ready': False, 'visual_verdict': 'not-classified', 'phases': [], 'commands': [],
               'guest_files': {}, 'retrieval_errors': [], 'cleanup_errors': []}
    physical = PhysicalInput(qmp, receipt['commands'])
    attempted, prepared, primary = False, None, None
    initial = None

    def observe():
        value = guest_call(agent, 'observe', profile, nonce, retention=(out, receipt))
        receipt.setdefault('observations', []).append(value)
        validate_snapshot(value, profile, nonce)
        if initial is not None:
            require(normalize_admission(value['admission']) == normalize_admission(initial['admission']),
                    'original member admission changed')
            for field in ('settings', 'fixture'):
                require(value[field] == initial[field], 'original member ' + field + ' changed')
            require([(w['role'], w['atspi'], w['process']) for w in value['windows']] ==
                    [(w['role'], w['atspi'], w['process']) for w in initial['windows']],
                    'original real windows changed')
        return value

    def capture(phase, held=False, anchor=None):
        before_begin = time.monotonic_ns()
        before = observe()
        before_end = time.monotonic_ns()
        require(not held or bool(before['selection']['selected']), 'held switcher has no actual Shell selection')
        item = {'phase': phase, 'before': before, 'before_host_bounds_ns': [before_begin, before_end],
                'held_keys': sorted(physical.held), 'clock_domain': HOST_CLOCK,
                'requested_minimum_hold_ns': MIN_HOLD_NS if held else 0, 'hold_anchor_ns': anchor,
                'capture_begin_ns': time.monotonic_ns(), 'path': phase + '.png'}
        receipt['phases'].append(item)
        qmp.frame(str(out / item['path']))
        item['capture_end_ns'] = time.monotonic_ns()
        item.update(png_info((out / item['path']).read_bytes()))
        item['after_host_bounds_ns'] = [time.monotonic_ns()]
        after = observe()
        item['after_host_bounds_ns'].append(time.monotonic_ns())
        item['after'] = after
        require(stable_context(before) == stable_context(after), 'selection/window context changed across framebuffer capture')
        require(after['observed_monotonic_ns'] > before['observed_monotonic_ns'], 'replayed guest snapshot around capture')
        require(not held or (anchor is not None and item['capture_begin_ns'] - anchor >= MIN_HOLD_NS),
                'physical modifier was not held at least 500ms before capture')
        require(sorted(physical.held) == item['held_keys'], 'owned held keys changed during capture')
        return after

    def capture_settled(phase, predicate, timeout_s, *, _observe=None, _capture=None,
                        _clock=time.monotonic, _sleep=time.sleep):
        """Poll validated observations until `predicate` holds, then take the phase capture.

        Each `observe()` takes ~2s under AT-SPI bus saturation (Tracker miners plus the
        dump traffic itself), so a fixed sub-second settle after modifier release samples a
        stale cache: the commit's focus event cannot propagate within 0.5s. Polling waits
        for the actual event (same `require` gates below still apply) instead of sampling
        faster than the harness can measure. On timeout the last observation is returned
        and the caller's `require` fails with its unchanged message. Poll attempts are
        recorded in `receipt['settle_polls']` as durable settling evidence.
        """
        observe_fn = _observe or observe
        capture_fn = _capture or capture
        _, polls = poll_until_settled(observe_fn, predicate, timeout_s, _clock, _sleep)
        receipt.setdefault('settle_polls', {})[phase] = polls
        return capture_fn(phase)

    try:
        validate_provenance(provenance)
        receipt['base_verification'] = retain_base_evidence(base_evidence_dir, out)
        query_topology(qmp, receipt)
        receipt['admission'] = session_status(agent, profile=profile)
        validate_admission(receipt['admission'], profile)
        validate_topology(receipt)
        # Both shells greet a login with the overview, and the stock one
        # clears window ACTIVE states while prepare's readiness requires
        # active Settings: without a dismiss the fixture maps but can never
        # become ready (round-20: 21 attempts all saw three inactive
        # windows). Dismiss to the desktop on both legs before the fixture
        # launches; the pair contract requires identical physical input,
        # and the tuna leg's active-clean prepare is unaffected (its
        # overview keeps ACTIVE, so closed-or-open both confirm). The
        # baseline dismiss below stays as the switcher-phase guard.
        physical.tap('esc')
        _, pre_polls = poll_until_settled(
            lambda: guest_call(agent, 'overview', profile, nonce),
            lambda result: result.get('overview_open') is False, 15,
            time.monotonic, time.sleep)
        receipt.setdefault('settle_polls', {})['pre-prepare-overview-dismiss'] = pre_polls
        dismissed = guest_call(agent, 'overview', profile, nonce)
        require(dismissed.get('overview_open') is False,
                'overview did not dismiss before prepare; Settings could never activate')
        attempted = True  # prepare may fail after claiming ownership.
        try:
            prepared = guest_call(agent, 'prepare', profile, nonce, retention=(out, receipt))
        except BaseException:
            try:
                qmp.frame(str(out / 'prepare-failed.png'))
            except BaseException:
                pass
            raise
        receipt['prepare'] = prepared
        require(prepared.get('ready') is True and prepared.get('nonce') == nonce, 'fixture prepare rejected')
        initial = validate_snapshot(prepared['snapshot'], profile, nonce)
        require(normalize_admission(initial['admission']) == normalize_admission(receipt['admission'])
                == normalize_admission(prepared['admission'])
                and initial['fixture'] == prepared['fixture']
                and initial['fixture']['processes'] == prepared['processes'],
                'prepared fixture/admission does not match actual original observation')
        receipt['actions'] = [choose_binding(initial['settings'], action)
                              for action in ('switch-applications', 'switch-windows')]
        physical.pointer_outside()
        # Both shells greet a login with the overview; a switcher commit under it
        # parks instead of applying. Dismiss to the desktop before baselining.
        physical.tap('esc')
        _, dismiss_polls = poll_until_settled(
            observe, lambda snap: not overview_search_visible(snap), 15,
            time.monotonic, time.sleep)
        receipt.setdefault('settle_polls', {})['overview-dismiss'] = dismiss_polls
        dismissed = observe()
        require(not overview_search_visible(dismissed),
                'overview did not dismiss before baseline; switcher commits would park')
        _, settle_polls = poll_until_stable(observe, stable_context, 15, time.monotonic, time.sleep)
        receipt.setdefault('settle_polls', {})['overview-settle'] = settle_polls
        baseline = capture('before')
        require(active_role(baseline) == 'settings', 'baseline is not actual active Settings')
        app, window = receipt['actions']
        anchor = physical.begin(app)
        capture('applications-held', True, anchor)
        physical.tap('down')
        capture('applications-down-held', True, anchor)
        require(not physical.release(), 'application modifier release failed')
        committed = capture_settled('applications-released',
                                    lambda snap: active_role(snap).startswith('files-'), 15)
        require(active_role(committed).startswith('files-'), 'app switch did not activate an actual original Files window')
        anchor = physical.begin(window)
        capture('windows-held', True, anchor)
        require(not physical.release(), 'window modifier release failed')
        final = capture_settled('windows-released',
                                lambda snap: released_to_different(snap, committed), 15)
        require(released_to_different(final, committed),
                'window switch did not activate a different actual original window')
        validate_phase_context(receipt)
    except BaseException as error:
        primary = error
        receipt['error'] = {'type': type(error).__name__, 'message': str(error)}
    finally:
        receipt['cleanup_errors'].extend(physical.release())
        if attempted:
            try:
                cleanup = guest_call(agent, 'stop', profile, nonce, retention=(out, receipt))
                receipt['cleanup'] = cleanup
                validate_cleanup(cleanup, profile, nonce, prepared)
            except Exception as error:
                receipt['cleanup_errors'].append({'stage': 'strict-stop', 'type': type(error).__name__, 'message': str(error)})
            retain_guest_files(agent, profile, nonce, out, receipt)
            required = ('claim.json', 'prepare.json', 'snapshot.json', 'stop.json')
            for name in required:
                if name not in receipt['guest_files']:
                    receipt['cleanup_errors'].append({'stage': 'missing-retained-guest-file', 'name': name})
        if primary is None and not receipt['cleanup_errors']:
            try:
                require(not receipt['retrieval_errors'], 'actual guest receipt retrieval failed')
                validate_raw_receipts(receipt, out)
            except Exception as error:
                receipt['cleanup_errors'].append({'stage': 'retained-receipt-validation',
                                                  'type': type(error).__name__, 'message': str(error)})
        receipt['end_ns'] = time.monotonic_ns()
        receipt['ready'] = primary is None and not receipt['cleanup_errors']
        save_json(out / 'member.json', receipt)
        record('P-WM01-CONTEXT', receipt['ready'],
               'actual original native switcher member context only; visual P-WM01 classification remains Main-owned')
    if primary is not None:
        raise primary
    require(receipt['ready'], 'native reference cleanup/retention failed; see member.json')
    return receipt


def retained_file(root, entry):
    path = Path(entry['path'])
    require(not path.is_absolute() and '..' not in path.parts, 'retained artifact escapes member directory')
    actual = (root / path).resolve()
    require(actual.is_relative_to(root.resolve()), 'retained artifact symlink escapes member directory')
    data = actual.read_bytes()
    require(entry.get('sha256') == digest(data) and entry.get('size') == len(data), 'retained artifact hash/size mismatch')
    return data


def validate_raw_receipts(receipt, root):
    initial = receipt['prepare']['snapshot']
    for name in ('claim.json', 'prepare.json', 'snapshot.json', 'stop.json'):
        raw = json.loads(retained_file(root, receipt['guest_files'][name]))
        require(raw.get('nonce') == receipt['nonce'], 'raw guest receipt has foreign claim')
        require(raw.get('profile') == receipt['profile'], 'raw guest receipt has foreign profile')
        if name == 'claim.json':
            require(raw.get('fixture') == initial['fixture'] and raw.get('admission') == initial['admission'],
                    'raw ownership claim differs from original admitted fixture')
        elif name == 'prepare.json':
            require(raw == receipt['prepare'], 'raw prepare differs from observed prepare')
        elif name == 'stop.json':
            require(raw == receipt['cleanup'], 'raw strict cleanup differs')
        else:
            require(raw == receipt['phases'][-1]['after'], 'last retained guest snapshot differs')
    require('failure.json' not in receipt['guest_files'], 'ready member retained an actual guest operation failure')
    observed = {'prepare': [receipt['prepare']], 'observe': receipt.get('observations', []), 'stop': [receipt['cleanup']]}
    for name, entry in receipt['guest_files'].items():
        guest_filename(name)
        raw_bytes = retained_file(root, entry)
        match = re.fullmatch(r'(prepare|observe|stop)-[0-9]{6}\.json', name)
        if match:
            require(json.loads(raw_bytes) in observed[match[1]], 'immutable operation receipt is not an actual observed response')
    expected_wires = [('prepare', receipt['prepare'])]
    expected_wires.extend(('observe', value) for value in receipt.get('observations', []))
    expected_wires.append(('stop', receipt['cleanup']))
    wires = receipt.get('wire_responses', [])
    require(len(wires) == len(expected_wires), 'missing immutable envelope for an actual operation response')
    for wire, (operation, observed_value) in zip(wires, expected_wires):
        require(wire.get('operation') == operation, 'immutable response operation sequence differs')
        envelope, artifact = wire['envelope'], wire['artifact']
        name = guest_filename(envelope['response_file']['name'])
        require(receipt['guest_files'].get(name) == artifact, 'actual response envelope artifact is not retained')
        data = retained_file(root, artifact)
        require(envelope['response_file'].get('size') == len(data)
                and envelope['response_file'].get('sha256') == digest(data)
                and json.loads(data) == observed_value, 'actual response envelope/full bytes disagree')


def validate_member(path, expected):
    path = Path(path)
    receipt = json.loads(path.read_bytes())
    require(receipt.get('schema') == 1 and receipt.get('kind') == 'native-switcher-reference-member'
            and receipt.get('profile') == expected and receipt.get('ready') is True,
            'member is not a genuine ready expected profile')
    require(receipt.get('visual_verdict') == 'not-classified', 'member improperly claims visual qualification')
    nonce = receipt.get('nonce', '')
    require(re.fullmatch('[0-9a-f]{32}', nonce) is not None, 'missing actual member nonce')
    require(receipt.get('clock_domain') == HOST_CLOCK and type(receipt.get('begin_ns')) is int
            and type(receipt.get('end_ns')) is int and receipt['begin_ns'] < receipt['end_ns'], 'invalid member clock bounds')
    require(receipt.get('cleanup_errors') == [], 'member has cleanup errors')
    require(receipt.get('retrieval_errors') == [], 'member has retained-byte retrieval errors')
    validate_admission(receipt['admission'], expected)
    validate_provenance(receipt['provenance'])
    base = receipt['base_verification']['files']
    validate_base_bytes(*(retained_file(path.parent, base[name]) for name in BASE_FILES))
    prepared = receipt['prepare']
    initial = validate_snapshot(prepared['snapshot'], expected, nonce)
    require(prepared.get('ready') is True and prepared.get('profile') == expected and prepared.get('nonce') == nonce
            and normalize_admission(prepared['admission']) == normalize_admission(initial['admission'])
            == normalize_admission(receipt['admission'])
            and prepared['fixture'] == initial['fixture'] and prepared['processes'] == initial['fixture']['processes'],
            'retained preparation is not the actual same admitted fixture')
    validate_cleanup(receipt['cleanup'], expected, nonce, receipt['prepare'])
    phases = receipt.get('phases', [])
    require([p.get('phase') for p in phases] == list(PHASES), 'missing or reordered actual capture phases')
    baseline = phases[0]['before']
    require(initial['observed_monotonic_ns'] < baseline['observed_monotonic_ns'], 'initial preparation snapshot was replayed')
    # Admission compares greeter-tolerant like the member flow: GDM greeter
    # teardown straddles login (round-26: both R25 members carried a
    # lingering session-1 greeter in the receipt admission only).
    require(normalize_admission(initial['admission']) == normalize_admission(baseline['admission']),
            'prepared fixture/config differs from baseline')
    for field in ('settings', 'fixture'):
        require(initial[field] == baseline[field], 'prepared fixture/config differs from baseline')
    prior_guest = 0
    prior_host = receipt['begin_ns']
    for index, phase in enumerate(phases):
        for side in ('before', 'after'):
            snap = validate_snapshot(phase[side], expected, nonce)
            require(snap['observed_monotonic_ns'] > prior_guest, 'replayed/nonmonotonic guest observations')
            prior_guest = snap['observed_monotonic_ns']
            require(normalize_admission(snap['admission']) == normalize_admission(baseline['admission']),
                    'original fixed fixture/config changed')
            for field in ('settings', 'fixture'):
                require(snap[field] == baseline[field], 'original fixed fixture/config changed')
            require([(w['role'], w['atspi'], w['process']) for w in snap['windows']] ==
                    [(w['role'], w['atspi'], w['process']) for w in initial['windows']],
                    'actual original accessibility windows changed')
        require(stable_context(phase['before']) == stable_context(phase['after']), 'unstable capture context')
        before, after = phase['before_host_bounds_ns'], phase['after_host_bounds_ns']
        bounds = [prior_host, *before, phase['capture_begin_ns'], phase['capture_end_ns'], *after, receipt['end_ns']]
        require(phase.get('clock_domain') == HOST_CLOCK and all(type(n) is int for n in bounds)
                and bounds == sorted(bounds), 'capture host bounds are invalid')
        prior_host = after[-1]
        info = png_info(retained_file(path.parent, phase))
        require(all(phase.get(k) == v for k, v in info.items()), 'retained capture metadata mismatch')
        held = index in (1, 2, 4)
        require(phase.get('requested_minimum_hold_ns') == (MIN_HOLD_NS if held else 0), 'wrong fixed held phase')
        require(bool(phase.get('held_keys')) == held, 'wrong actual held set')
        if held:
            require(type(phase.get('hold_anchor_ns')) is int
                    and phase['capture_begin_ns'] - phase['hold_anchor_ns'] >= MIN_HOLD_NS,
                    'capture occurred before physical 500ms hold')
            require(bool(phase['before']['selection']['selected']), 'missing actual held Shell selection')
    require(active_role(phases[0]['before']) == 'settings', 'member lacks selected Settings baseline')
    require(active_role(phases[3]['after']).startswith('files-')
            and active_role(phases[3]['after']) != active_role(phases[5]['after']), 'actual app/window activation absent')
    validate_phase_context(receipt)
    validate_topology(receipt)
    expected_actions = [choose_binding(baseline['settings'], a) for a in ('switch-applications', 'switch-windows')]
    require(receipt.get('actions') == expected_actions, 'actions are not the actual configured bindings')
    validate_commands(receipt)
    validate_raw_receipts(receipt, path.parent)
    return receipt


def validate_commands(receipt):
    held, previous = set(), receipt['begin_ns']
    commands = receipt.get('commands', [])
    require(bool(commands), 'missing actual physical QMP commands')
    for command in commands:
        require(command.get('command') == 'input-send-event' and command.get('clock_domain') == HOST_CLOCK
                and command.get('ok') is True, 'failed or nonphysical input command')
        require(type(command.get('begin_ns')) is int and type(command.get('end_ns')) is int
                and previous <= command['begin_ns'] <= command['end_ns'] <= receipt['end_ns'], 'invalid command bounds')
        require(command.get('held_before') == sorted(held), 'fabricated command held-before set')
        for event in command['events']:
            if event['type'] == 'key':
                key, down = event['data']['key']['data'], event['data']['down']
                require(event['data']['key']['type'] == 'qcode' and type(down) is bool, 'not physical qcode')
                require((key not in held) if down else (key in held), 'physical held transition mismatch')
                if down:
                    held.add(key)
                else:
                    held.remove(key)
            else:
                require(event['type'] == 'abs', 'unexpected pointer/button command')
        require(command.get('held_after') == sorted(held), 'fabricated command held-after set')
        previous = command['end_ns']
    require(not held, 'member leaves owned physical keys pressed')
    # Fixed physical protocol: a pre-prepare Escape dismisses the
    # login-greeting overview so the fixture can activate, then the pointer
    # moves outside popups, then the baseline Escape re-verifies the desktop
    # before the switcher actions.
    require(len(commands) >= 5, 'member physical protocol is incomplete')
    pointer = commands[2]['events']
    require(pointer == [{'type': 'abs', 'data': {'axis': 'x', 'value': 32767}},
                        {'type': 'abs', 'data': {'axis': 'y', 'value': 32767}}], 'pointer was not identically moved outside popup')
    # Reconstruct the exact fixed action sequence rather than accepting arbitrary QMP history.
    # The leading Escape pair dismisses the login-greeting overview before prepare so the
    # fixture maps active; the second pair re-verifies the desktop at baseline, without
    # which switcher commits park and AT-SPI window-active freezes.
    expected = [('esc', True), ('esc', False), ('esc', True), ('esc', False)]
    for index, action in enumerate(receipt['actions']):
        expected.extend((key, True) for key in action['modifiers'])
        expected.extend(((action['key'], True), (action['key'], False)))
        if index == 0:
            expected.extend((('down', True), ('down', False)))
        expected.extend((key, False) for key in sorted(action['modifiers']))
    actual = [(event['data']['key']['data'], event['data']['down'])
              for c in commands for event in c['events'] if event['type'] == 'key']
    require(actual == expected, 'physical action sequence differs from fixed configured protocol')
    keyed = [command for index, command in enumerate(commands) if index != 2]
    require(len(commands) == len(expected) + 1
            and all(len(c['events']) == 1 and c['events'][0]['type'] == 'key' for c in keyed),
            'member includes extra pointer commands or combined/unrecorded input')
    app_count = len(receipt['actions'][0]['modifiers'])
    window_count = len(receipt['actions'][1]['modifiers'])
    # Two Escape taps (pre-prepare, baseline) plus one pointer command precede the baseline.
    app_trigger_count = 5 + app_count + 2
    app_release_count = app_trigger_count + 2 + app_count
    window_trigger_count = app_release_count + window_count + 2
    prefixes = [5, app_trigger_count, app_trigger_count + 2,
                app_release_count, window_trigger_count, window_trigger_count + window_count]
    for index, phase in enumerate(receipt['phases']):
        held_at_frame = set()
        down_at_frame = {}
        completed = 0
        for command in commands:
            require(not (command['begin_ns'] < phase['after_host_bounds_ns'][-1]
                         and command['end_ns'] > phase['before_host_bounds_ns'][0]),
                    'input command overlaps observation/capture bracket')
            if command['end_ns'] > phase['before_host_bounds_ns'][0]:
                break
            completed += 1
            for event in command['events']:
                if event['type'] == 'key':
                    key = event['data']['key']['data']
                    if event['data']['down']:
                        held_at_frame.add(key)
                        down_at_frame[key] = command['end_ns']
                    else:
                        held_at_frame.remove(key)
        require(completed == prefixes[index], 'capture does not bracket the fixed physical action phase')
        require(sorted(held_at_frame) == phase['held_keys'], 'capture held set differs from actual commands')
        if held_at_frame:
            trigger_count = app_trigger_count if index in (1, 2) else window_trigger_count
            require(phase['hold_anchor_ns'] == commands[trigger_count - 1]['end_ns']
                    and max(down_at_frame[k] for k in held_at_frame) <= phase['hold_anchor_ns'],
                    'hold anchor is not actual action-trigger completion with modifier held')


def paired_settings(snapshot):
    value = copy.deepcopy(snapshot['settings'])
    # DisplayConfig serial is an observation generation, not a scale/topology setting.
    value['display_config'].pop('serial')
    # Monitor vendor/model/serial are compositor EDID identity (round-26:
    # stock mutter passes QEMU EDID through, tuna-compositor synthesizes
    # its own); the pair compares collection conditions (connector, modes,
    # scales, layout), never implementation identity.
    for monitor in value['display_config']['monitors']:
        monitor[0] = monitor[0][:1]
        # Per-monitor identity/advertisements, not conditions: display-name
        # is EDID text, min-refresh-rate/rgb-range are mutter-only
        # advertisements, while the active color-mode (still compared)
        # describes actual rendering. Unknown future properties fail loudly.
        for key in ('display-name', 'min-refresh-rate', 'rgb-range', 'supported-color-modes'):
            monitor[2].pop(key, None)
    for logical in value['display_config']['logical_monitors']:
        logical[5] = [entry[:1] for entry in logical[5]]
    # Mutter-only capability flags are implementation facts, not collection
    # conditions; unknown future properties still fail loudly below.
    value['display_config']['properties'].pop('supports-changing-layout-mode', None)
    return value


def original_apps(snapshot):
    return {w['role']: {k: w['process'][k] for k in ('exe', 'exe_sha256', 'package')}
            for w in snapshot['windows']}


def switch_sequence(receipt):
    # What the pair must share: the same physical input per phase, the same
    # activation outcomes on settled phases, and the same selection
    # appeared/cleared pattern. Shell-UI presentation is never portable
    # (round-26: GNOME St keeps the clock selected with unnamed switcher
    # buttons and deactivates under a switcher grab, while tuna-shell names
    # its selections and keeps ACTIVE), so names and held-phase active
    # roles stay per-leg evidence. Accessibility names/roles/states are
    # context evidence, never GTK/WM identity joins.
    baseline_selected = {atspi_id(n) for n in receipt['phases'][0]['before']['selection']['selected']}
    sequence = []
    for p in receipt['phases']:
        selected = [n for n in p['before']['selection']['selected'] if atspi_id(n) not in baseline_selected]
        sequence.append({'phase': p['phase'],
                         'active_role': active_role(p['before']) if not p['held_keys'] else None,
                         'has_new_selection': bool(selected),
                         'held_keys': p['held_keys'],
                         'requested_minimum_hold_ns': p['requested_minimum_hold_ns']})
    return sequence


def compare(gnome_receipt_path, tuna_receipt_path, out):
    """Validate retained paired CONTEXT only. Main owns actual visual P-WM01."""
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    result = {'schema': 1, 'kind': 'native-switcher-reference-pair-context', 'ready': False,
              'visual_verdict': 'not-classified', 'p_wm01_complete': False,
              'members': {'gnome': str(gnome_receipt_path), 'tuna': str(tuna_receipt_path)}}
    try:
        gnome = validate_member(gnome_receipt_path, 'gnome')
        tuna = validate_member(tuna_receipt_path, 'tuna')
        require(gnome['nonce'] != tuna['nonce'], 'paired members reuse claim identity')
        left, right = gnome['phases'][0]['before'], tuna['phases'][0]['before']
        require(left['admission']['session']['compositor']['boot_id'] != right['admission']['session']['compositor']['boot_id'],
                'paired native runtime boot identities were collapsed')
        require({p['boot_id'] for p in left['fixture']['processes']}.isdisjoint(
                    {p['boot_id'] for p in right['fixture']['processes']}),
                'paired original app runtime boot identities were collapsed')
        require(original_apps(left) == original_apps(right), 'original packaged Files/Settings ELF/package mismatch')
        require(paired_settings(left) == paired_settings(right), 'actual bindings/filters/scales/config differ')
        require(gnome['actions'] == tuna['actions'], 'actual configured actions differ')
        require(switch_sequence(gnome) == switch_sequence(tuna), 'actual switch sequence/held phases differ')
        lp, rp = gnome['provenance'], tuna['provenance']
        require(lp['base_image'] == rp['base_image'] and lp['commit'] == rp['commit'],
                'paired members differ in actual pinned base/source context')
        require(gnome['native_topology']['query-kvm']['result'] == tuna['native_topology']['query-kvm']['result']
                and pci_topology(gnome['native_topology']['query-pci']['result']) ==
                    pci_topology(tuna['native_topology']['query-pci']['result'])
                and native_gpu_context(left) == native_gpu_context(right), 'actual native KVM/PCI/DRM topology differs')
        require(left['admission']['payload_integrity'] == right['admission']['payload_integrity'],
                'actual original/current payload or installed package receipts differ')
        result.update(ready=True, base_image=lp['base_image'],
                      context='paired actual collection conditions ready for Main visual classification; no visual pass')
    except Exception as error:
        result['error'] = {'type': type(error).__name__, 'message': str(error)}
    save_json(out / 'pair-context.json', result)
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    operations = parser.add_subparsers(dest='operation', required=True)
    paired = operations.add_parser('compare')
    paired.add_argument('--gnome', required=True, type=Path)
    paired.add_argument('--tuna', required=True, type=Path)
    paired.add_argument('--out', required=True, type=Path)
    verification = operations.add_parser('verify-base')
    verification.add_argument('--out', required=True, type=Path)
    platform = operations.add_parser('verify-platform')
    platform.add_argument('--platform-digest', required=True)
    platform.add_argument('--out', required=True, type=Path)
    args = parser.parse_args(argv)
    if args.operation == 'verify-base':
        verify_base_image(args.out)
        return 0
    if args.operation == 'verify-platform':
        verify_platform_image(args.out, args.platform_digest)
        return 0
    value = compare(args.gnome, args.tuna, args.out)
    print(json.dumps(value, sort_keys=True))
    return 0 if value['ready'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
