"""Real Core-app operations on the existing QMP/QGA native VM lifecycle.

This is a DEV/component consumer of the existing immutable-image admission. It
cannot produce signed-release qualification, and it never replaces a missing
native operation with a launch or desired-state record.
"""
import base64
import importlib
import math
from pathlib import Path
import subprocess
import unicodedata
import time

from vm_native_desktop import NativeDesktop
PLAIN_KEYS = {' ': 'spc', '\n': 'ret', '\r': 'ret', '\t': 'tab', '-': 'minus', '=': 'equal',
              '[': 'bracket_left', ']': 'bracket_right', '\\': 'backslash', ';': 'semicolon',
              "'": 'apostrophe', ',': 'comma', '.': 'dot', '/': 'slash', '`': 'grave_accent'}
SHIFTED_KEYS = {'!': '1', '@': '2', '#': '3', '$': '4', '%': '5', '^': '6', '&': '7', '*': '8',
                '(': '9', ')': '0', '_': 'minus', '+': 'equal', '{': 'bracket_left',
                '}': 'bracket_right', '|': 'backslash', ':': 'semicolon', '"': 'apostrophe',
                '<': 'comma', '>': 'dot', '?': 'slash', '~': 'grave_accent'}
GUEST_MODULES = ('guest', 'native', 'basic', 'data', 'media', 'system', 'web', 'autotz', 'geoclue_fixture')


def _isolate_preedit_ink(crop):
    """Crop an IME field capture down to the composition's own ink.

    The entry crop carries decorations tesseract cannot survive: the
    search icon, the clear button, and Pango's composition underline,
    which merges with CJK glyph bottoms at this size (round-1004: the
    full-entry crop OCR'd as latin garbage while the field visibly
    showed the exact preedit). The underline spans the composition
    width, so it locates the text without any geometry constants: a
    bottom dark row holding one dominant run marks the composition,
    and the enclosing dark-column cluster is its ink. Returns None
    when no composition underline is found so the caller falls back
    to the whole crop; the exact-equality OCR gate is unchanged.
    Isolation is best-effort sensor preparation, never load-bearing:
    any inspection failure also falls back to the whole crop.
    """
    try:
        gray = crop.convert('L')
        width, height = gray.size
        ink = gray.load()
        dark = lambda x, y: ink[x, y] < 150  # noqa: E731
        rows = [y for y in range(height) if any(dark(x, y) for x in range(width))]
        if not rows:
            return None
        span = None
        for line in rows[-3:]:
            runs = []
            x = 0
            while x < width:
                if dark(x, line):
                    start = x
                    while x < width and dark(x, line):
                        x += 1
                    runs.append((start, x - 1))
                else:
                    x += 1
            if not runs:
                continue
            longest = max(stop - start + 1 for start, stop in runs)
            total = sum(stop - start + 1 for start, stop in runs)
            if longest >= 12 and longest >= 0.9 * total:
                span = max(runs, key=lambda row: row[1] - row[0])
                break
        if span is None:
            return None
        columns = {x for x in range(width) if any(dark(x, y) for y in rows)}
        left, right = span
        while left - 1 in columns:
            left -= 1
        while right + 1 in columns:
            right += 1
        return crop.crop((left, rows[0], right + 1, rows[-1] + 1))
    except Exception:
        return None


class Desktop(NativeDesktop):
    def __init__(self, qmp, agent_factory, out, metadata, select_mouse, nonce=None,
                 auth_records=None, auth_refusal=None):
        root = Path(__file__).resolve().parents[2]
        source_members = {
            '/usr/libexec/tuna-core-apps/' + name + '.py':
                root / 'packaging/marlin/vm-lane/core-apps' / (name + '.py')
            for name in GUEST_MODULES}
        source_members.update({
            '/usr/libexec/' + name: root / 'packaging/marlin/vm-lane' / name
            for name in ('tuna-core-apps-install', 'tuna-vm-lifecycle')})
        source_members['/usr/libexec/tuna-geoclue-agent-policy'] = root / 'packaging/marlin/tuna-geoclue-agent-policy'
        source_members['/usr/libexec/tuna-native-session/native_session.py'] = (
            root / 'packaging/marlin/vm-lane/tuna-native-session/native_session.py')
        super().__init__(qmp, agent_factory, out, metadata, action='core-apps',
                         source_members=source_members, nonce=nonce)
        self._select_mouse = select_mouse
        if auth_records is None or auth_refusal is None:
            raise ValueError('existing real native PAM milestone observers required')
        self._auth_records, self._auth_refusal = auth_records, auth_refusal
        self._results = []
        self._cuts = {}
        self._ime = {}
        self._actors = {}




    def lifecycle(self, action):
        if action not in ('capture-input', 'lock'):
            raise ValueError('unsupported existing lifecycle action')
        return self._execute(action, (), 'lifecycle-' + action, {'action': action})



    def keys(self, *keys):
        self.state()
        self.qmp.keys(*keys)

    def type_ascii(self, text):
        self.state()
        self._type_ascii_keys(text)

    def _type_locked_ascii(self, text):
        if self.state(unlocked=False)['locked'] is not True:
            raise RuntimeError('lock negative/password input requires the actual locked desktop')
        self._type_ascii_keys(text)

    def _type_ascii_keys(self, text):
        if not isinstance(text, str) or not text.isascii() or '\x00' in text:
            raise ValueError('physical QMP typing accepts actual ASCII keys only; Unicode needs real IME')
        if any(not (character.isalnum() or character in PLAIN_KEYS or character in SHIFTED_KEYS)
               for character in text):
            raise ValueError('unsupported ASCII control cannot be typed as an application payload')
        for character in text:
            if 'A' <= character <= 'Z':
                self.qmp.keys('shift', character.lower())
            elif character.isalnum():
                self.qmp.keys(character)
            elif character in PLAIN_KEYS:
                self.qmp.keys(PLAIN_KEYS[character])
            else:
                self.qmp.keys('shift', SHIFTED_KEYS[character])

    def _window(self, actor, state):
        found = [row for row in state['windows'] if row['id'] == actor['window_id']]
        if len(found) != 1:
            raise RuntimeError('original native surface not uniquely mapped')
        return found[0]



    def start(self, app_id, search_name):
        if app_id not in self.inventory['apps']:
            raise ValueError('app outside actual installed native Core inventory')
        before = self.state()
        self.keys('meta_l')
        self.type_ascii(search_name)
        self.await_state('desktop-search-' + app_id,
                         self.state, lambda value: value['overview_open'])
        self.keys('ret')
        launched = self.await_state('normal-launch-' + app_id, self.state,
                                    lambda value: not value['overview_open'] and any(
                                        row['app_id'] == app_id for row in value['windows']))
        windows = [row for row in launched['windows'] if row['app_id'] == app_id]
        focused = [row for row in windows if row['id'] == launched['focused']]
        new = [row for row in windows if row['id'] not in {old['id'] for old in before['windows']}]
        selected = focused or new or windows
        if len(selected) != 1:
            raise RuntimeError('normal desktop activation did not identify one original app window')
        actor = self.await_state('native-owner-' + app_id,
                                 lambda: self.guest('desktop', 'discover', app_id=app_id,
                                                    window_id=selected[0]['id']),
                                 lambda value: value.get('pid') is not None)
        actor['search_name'] = search_name
        self._actors[app_id] = actor
        self.retain('desktop-launch-' + app_id, {'before': before, 'actor': actor,
                                                'normal_desktop_input': True})
        self.observe('launched-' + app_id, actor)
        return actor


    @staticmethod
    def _matches(tree, name, role=None, actionable=False):
        return [node for node in tree['nodes'] if node['showing'] and node['name'] == name
                and (role is None or node['role'] == role)
                and (not actionable or (node['sensitive'] and node['role'] != 'label'))]

    def _node(self, actor, name, role=None, actionable=False):
        tree = self.tree(actor)
        matches = self._matches(tree, name, role, actionable)
        if len(matches) != 1:
            self.retain('ambiguous-control', {'actor': actor, 'name': name, 'role': role,
                                              'matches': matches, 'tree': tree})
            raise RuntimeError('actual accessible control not unique')
        return tree, matches[0]

    def press(self, actor, name, role=None):
        tree, node = self._node(actor, name, role, actionable=True)
        result = self.guest('desktop', 'press', actor=actor, path=node['path'])
        self.retain('native-action', {'actor': actor, 'before': tree, 'node': node,
                                      'result': result, 'action_not_replayed': True})
        return result

    def click(self, actor, name, role=None):
        tree, node = self._node(actor, name, role, actionable=True)
        return self._point(actor, tree, node, 0.5, 0.5)

    def click_path(self, actor, path):
        return self.click_at(actor, path, 0.5, 0.5)

    def click_at(self, actor, path, x_fraction, y_fraction):
        if not (0 < x_fraction < 1 and 0 < y_fraction < 1):
            raise ValueError('actual point must lie strictly inside the observed widget')
        tree = self.tree(actor)
        found = [node for node in tree['nodes'] if node['path'] == list(path)]
        if len(found) != 1 or not found[0]['showing'] or not found[0]['sensitive']:
            raise RuntimeError('actual pointer target no longer showing/sensitive')
        return self._point(actor, tree, found[0], x_fraction, y_fraction)

    def _point(self, actor, tree, node, fx, fy):
        bounds = node['bounds']
        if bounds is None or bounds[2] <= 0 or bounds[3] <= 0:
            raise RuntimeError('actual accessible pointer rectangle unavailable')
        if actor.get('kind') == 'shell':
            origin = self.guest('desktop', 'shell-origin', actor=actor, path=node['path'])
            x, y = origin['x'] + bounds[0] + bounds[2] * fx, origin['y'] + bounds[1] + bounds[3] * fy
        else:
            window = self._window(actor, tree['state'])
            x = window['rect'][0] + bounds[0] + bounds[2] * fx
            y = window['rect'][1] + bounds[1] + bounds[3] * fy
        x, y = int(x * self.scale), int(y * self.scale)
        if not (0 <= x < self.width and 0 <= y < self.height):
            raise RuntimeError('actual native pointer point outside admitted output')
        selected = self._select_mouse(self.qmp, False)
        self.qmp.cmd('input-send-event', events=[
            {'type': 'abs', 'data': {'axis': 'x', 'value': round(x * 32767 / (self.width - 1))}},
            {'type': 'abs', 'data': {'axis': 'y', 'value': round(y * 32767 / (self.height - 1))}}])
        # Separate transactions avoid the observed PS/2/tablet old-position race.
        time.sleep(0.08)
        self.qmp.cmd('input-send-event', events=[{'type': 'btn', 'data': {'down': True, 'button': 'left'}}])
        time.sleep(0.08)
        self.qmp.cmd('input-send-event', events=[{'type': 'btn', 'data': {'down': False, 'button': 'left'}}])
        return {'actor': actor, 'node': node, 'point': [x, y], 'mouse': selected}

    def _field_pixels(self, actor, state, node):
        if node['bounds'] is None:
            raise RuntimeError('original actual editable field has no bounds')
        window = self._window(actor, state)
        x, y, width, height = node['bounds']
        left = math.floor((window['rect'][0] + x) * self.scale)
        top = math.floor((window['rect'][1] + y) * self.scale)
        right = math.ceil((window['rect'][0] + x + width) * self.scale)
        bottom = math.ceil((window['rect'][1] + y + height) * self.scale)
        if not (0 <= left < right <= self.width and 0 <= top < bottom <= self.height):
            raise RuntimeError('original actual editable field is clipped outside the output')
        return left, top, right - left, bottom - top

    def replace(self, actor, name, text, role='text'):
        self.click(actor, name, role)
        self.keys('ctrl', 'a')
        self.type_ascii(text)
        return self.await_state('focused-exact-edit', lambda: self.tree(actor),
                                lambda value: any(node['focused'] and node['text'] == text
                                                  for node in value['nodes']))

    def text(self, actor, name=None, role=None):
        tree = self.tree(actor)
        nodes = [node for node in tree['nodes'] if node['showing'] and node['text'] is not None
                 and (name is None or node['name'] == name) and (role is None or node['role'] == role)]
        if len(nodes) != 1:
            raise RuntimeError('actual accessible text not unique')
        return nodes[0]['text']

    def select(self, actor):
        state = self.state()
        window = self._window(actor, state)
        if state['focused'] != actor['window_id']:
            x, y, width, height = window['rect']
            if width <= 0 or height <= 0:
                raise RuntimeError('original app is not visibly selectable')
            # Actual Alt+Tab cycles, bounded by the current real mapped set.
            for _ in range(len(state['windows'])):
                self.keys('alt', 'tab')
                state = self.state()
                if state['focused'] == actor['window_id']:
                    break
            else:
                raise RuntimeError('actual switcher did not restore original app focus')
        self.tree(actor)
        return state

    def clipboard(self, mime='text/plain;charset=utf-8'):
        return self.guest('desktop', 'clipboard', mime=mime)

    def file_picker(self, actor):
        return self.await_state('original-native-file-picker',
                                lambda: self.guest('desktop', 'picker', actor=actor),
                                lambda value: value.get('pid') is not None)

    def _prepare_picker(self, actor, path, save=False):
        if not str(path).startswith(self.fixture + '/'):
            raise ValueError('normal picker must select owned actual fixture content')
        relative = str(Path(path).relative_to(self.fixture))
        info = self.guest('desktop', 'files', paths=[relative])['files'][0]
        directory = bool(info.get('directory'))
        if not save and not info['exists']:
            raise ValueError('real Open content must exist before the caller operation')
        target = Path(path) if directory else Path(path).parent
        picker = self.file_picker(actor)
        self.keys('ctrl', 'l')
        self.type_ascii(str(target) + '/')
        self.await_state('picker-exact-directory-location', lambda: self.tree(picker),
                         lambda value: any(node['focused'] and node['text'] == str(target) + '/'
                                           for node in value['nodes']))
        self.keys('ret')
        if save:
            self.replace(picker, 'File Name', Path(path).name)
        elif not directory:
            basename = Path(path).name
            tree = self.await_state('picker-actual-content-cell', lambda: self.tree(picker),
                                    lambda value: any(node['showing'] and node['role'] == 'table cell'
                                                      and node['name'] in (basename, basename + '. File')
                                                      for node in value['nodes']))
            cells = [node for node in tree['nodes'] if node['showing'] and node['role'] == 'table cell'
                     and node['name'] in (basename, basename + '. File')]
            if len(cells) != 1:
                raise RuntimeError('actual native chooser content cell not unique')
            self.click_path(picker, cells[0]['path'])
            self.await_state('picker-actual-selected-content', lambda: self.tree(picker),
                             lambda value: any(node['path'] == cells[0]['path'] and node['selected']
                                               for node in value['nodes']))
        return picker

    def _accept_picker(self, actor, picker, path, save):
        tree = self.await_state('picker-real-default-accept-control', lambda: self.tree(picker),
                                lambda value: len([node for node in value['nodes'] if node['showing']
                                    and node['sensitive'] and node['default']
                                    and node['role'] in ('button', 'push button')]) == 1)
        button = next(node for node in tree['nodes'] if node['showing'] and node['sensitive']
                      and node['default'] and node['role'] in ('button', 'push button'))
        self.guest('desktop', 'press', actor=picker, path=button['path'])
        self.await_state('picker-caller-focus-restored', self.state,
                         lambda value: value['focused'] == actor['window_id']
                         and not any(row['id'] == picker['window_id'] for row in value['windows']))
        return {'picker': picker, 'path': str(path), 'save': save, 'actual_accept_control': button,
                'caller': self.tree(actor), 'consent': 'actual-native-accept'}

    def _choose(self, actor, path, save=False):
        picker = self._prepare_picker(actor, path, save)
        return self._accept_picker(actor, picker, path, save)

    def choose_file(self, actor, path, cancel_first=False):
        if cancel_first:
            raise ValueError('already-open chooser cancel must be reopened by its actual domain control')
        return self._choose(actor, path)

    def open_file(self, actor, path, cancel_first=True):
        self.select(actor)
        before = self.observe('before-native-open', actor)
        cancelled = None
        if cancel_first:
            self.keys('ctrl', 'o')
            picker = self._prepare_picker(actor, path)
            cancelled = self.observe('native-open-selected-before-cancel', picker)
            self.press(picker, 'Close')
            self.await_state('open-cancel-original-caller', self.state,
                             lambda value: value['focused'] == actor['window_id'])
            self.observe('native-open-cancelled', actor)
        self.keys('ctrl', 'o')
        return {'before': before, 'cancel_tested': cancel_first, 'cancelled_selection': cancelled,
                'accepted': self._choose(actor, path)}

    def save_file(self, actor, path, cancel_first=True, ime=False):
        self.select(actor)
        relative = str(Path(path).relative_to(self.fixture))
        before = self.guest('desktop', 'files', paths=[relative])
        if before['files'][0]['exists']:
            raise ValueError('new Save destination required for no-output cancel control')
        portal_ime = None
        cancelled = None
        if cancel_first:
            self.keys('ctrl', 's')
            picker = self._prepare_picker(actor, path, save=True)
            if ime:
                _, field = self._node(picker, 'File Name', 'text')
                portal_ime = self.ime(picker, field['name'], field_path=field['path'])
            cancelled = self.observe('native-save-selected-before-cancel', picker)
            self.press(picker, 'Close')
            actual = self.guest('desktop', 'files', paths=[relative])
            if actual != before:
                raise RuntimeError('Save cancellation created/modified destination')
        self.keys('ctrl', 's')
        return {'cancel_tested': cancel_first, 'before': before, 'cancelled_selection': cancelled,
                'portal_ime': portal_ime, 'accepted': self._choose(actor, path, save=True)}

    def portal(self, actor, interface='Camera', response='deny', **options):
        if interface == 'FileChooser':
            if response != 'accept' or not options.get('save') or 'path' not in options:
                raise ValueError('explicit already-open native Save picker acceptance required')
            return self._choose(actor, options['path'], save=True)
        if interface != 'Camera' or response not in ('deny', 'grant'):
            raise ValueError('unknown actual portal UI control')
        dialog = self.guest('desktop', 'portal', actor=actor, interface=interface)
        before = self.observe('camera-consent-original-provider', dialog)
        self.press(dialog, 'Deny Access' if response == 'deny' else 'Allow Access')
        after = self.await_state('camera-native-consent-completed', self.state,
                                 lambda value: value['focused'] == actor['window_id'])
        actual_response = self.await_state('original-camera-request-response',
            lambda: self.guest('desktop', 'camera-response', actor=actor,
                              request_path=dialog['backend_request']['body'][0]),
            lambda value: value.get('ready') is True)
        if (actual_response['response_code'] == 0) != (response == 'grant'):
            raise RuntimeError('real Camera Request response differs from actual native consent choice')
        return {'requester': actor, 'provider': dialog, 'response': response,
                'before': before, 'after': after, 'actual_request_response': actual_response,
                'permission_store': self.guest('desktop', 'camera-permission', actor=actor)}

    def open_default(self, path):
        default = self.guest('desktop', 'default-handler', path=str(path))
        files = self.start('org.gnome.Nautilus', 'Files')
        self.keys('ctrl', 'l')
        self.type_ascii(str(Path(path).parent) + '/')
        self.keys('ret')
        basename = Path(path).name
        # Nautilus exposes file cells as "<name>. File" (directories as
        # "<name>. Folder"); the bare name alone never matches. Same
        # spellings as the data-profile Files cell lookup.
        spellings = (basename, basename + '. File', basename + '. Folder')
        tree = self.await_state('default-file-visible', lambda: self.tree(files),
                                lambda value: any(node['showing'] and node['name'] in spellings
                                                  for node in value['nodes']))
        cells = [node for node in tree['nodes'] if node['showing'] and node['name'] in spellings
                 and node['role'] in ('table cell', 'list item', 'icon')]
        if len(cells) != 1:
            raise RuntimeError('actual Files default-handler selected cell not unique')
        self.click_path(files, cells[0]['path'])
        self.await_state('default-file-selected', lambda: self.tree(files),
                         lambda value: any(node['path'] == cells[0]['path'] and node['selected']
                                           for node in value['nodes']))
        self.keys('ret')
        target_id = default['default_handler'].removesuffix('.desktop')
        target = self.await_state('actual-default-consumer',
                                 lambda: self.guest('desktop', 'discover', app_id=target_id),
                                 lambda value: value.get('pid') is not None)
        target['search_name'] = self._actors.get(target_id, {}).get('search_name')
        self._actors[target_id] = target
        return default | {'actor': target, 'navigation': self.observe('files-default-open', target)}

    def close(self, actor):
        self.select(actor)
        self.keys('alt', 'f4')
        return self.await_state('actual-window-closed', self.state,
                                lambda value: not any(row['id'] == actor['window_id']
                                                      for row in value['windows']))

    def retire_frontends(self):
        """Failure cleanup only; never an operation, close or persistence receipt."""
        return self.guest('desktop', 'retire-frontends', actors=list(self._actors.values()))

    def restart(self, actor, search_name):
        self.select(actor)
        if actor['id'] == 'org.gnome.Console':
            tabs = [node for node in self.tree(actor)['nodes']
                    if node['showing'] and node['role'] == 'page tab']
            if len(tabs) > 1:
                raise RuntimeError('Console restart requires the original sole owned terminal tab')
            self.keys('ctrl', 'shift', 'w')
        else:
            self.keys('ctrl', 'q')
        self.await_state('actual-frontend-departed',
                         lambda: self.guest('desktop', 'departed', actor=actor),
                         lambda value: value['original_process_departed'])
        replacement = self.start(actor['id'], search_name)
        if (replacement['pid'], replacement['start_ticks']) == (actor['pid'], actor['start_ticks']):
            raise RuntimeError('restart reused original frontend process')
        for key in ('exe', 'exe_sha256', 'package', 'version', 'desktop_sha256'):
            if replacement[key] != actor[key]:
                raise RuntimeError('normal frontend restart changed installed code identity')
        return replacement

    def ime(self, actor, field_name, expected='你好', *, field_path=None):
        if expected != '你好':
            raise ValueError('known genuine libpinyin consumer control required')
        self.select(actor)
        if field_path is None:
            _, field = self._node(actor, field_name)
        else:
            nodes = [node for node in self.tree(actor)['nodes'] if node['path'] == list(field_path)
                     and node['showing'] and node['sensitive']]
            if len(nodes) != 1 or nodes[0]['name'] != field_name:
                raise RuntimeError('original observed IME entry identity changed')
            field = nodes[0]
        original = field['text']
        if original is None or 'password' in field['role'] or not field['editable']:
            raise RuntimeError('real nonpassword editable native IME consumer required')
        engine = self.guest('desktop', 'ime-engine', select=True)
        monitor_started = False
        try:
            self.click_path(actor, field['path'])
            # Keys typed before focus lands never reach the engine: no
            # preedit rows, and the engine idles while the await burns the
            # budget (run 41865506509 attempts 1-2). Settle the
            # exact field first; selection and typing follow only then.
            self.await_state('real-ime-field-focused', lambda: self.tree(actor),
                             lambda value: any(node['path'] == field['path'] and node['focused']
                                               and node['editable'] for node in value['nodes']))
            self.keys('ctrl', 'a')
            self.guest('desktop', 'ime-monitor-start', actor=actor)
            monitor_started = True
            self.type_ascii('nihao')
            # Healthy ime-state polls round-trip in ~2s (run 41887740732:
            # six polls over 9s); the default 10s budget starves the sixth
            # poll with a misleading guest-exec TimeoutError instead of an
            # honest readiness expiry. 30s fits ~15 polls.
            composition = self.await_state('real-native-libpinyin-preedit',
                                            lambda: self.guest('desktop', 'ime-state', actor=actor),
                                            lambda value: value['preedit_visible']
                                            and expected in value['candidates'],
                                            timeout=30)
            rendered = self.observe('original-app-visible-preedit', actor)
            current = [node for node in rendered['nodes'] if node['path'] == field['path']
                       and node['focused'] and node['editable']]
            if len(current) != 1:
                raise RuntimeError('original app lost actual IME field focus during composition')
            if current[0]['text'] == composition['preedit']:
                raise RuntimeError('composition was already committed instead of observed as preedit')
            x, y, width, height = self._field_pixels(actor, rendered['state'], current[0])
            from PIL import Image
            crop = self.out / f'{self._sequence + 1:05d}-original-preedit-field.png'
            with Image.open(rendered['frame_path']) as frame:
                field_crop = frame.crop((x, y, x + width, y + height))
                field_crop.save(crop)
            isolated = _isolate_preedit_ink(field_crop)
            isolated_path = None
            if isolated is not None:
                isolated_path = self.out / f'{self._sequence + 1:05d}-original-preedit-ink.png'
                isolated.save(isolated_path)
            ocr = subprocess.run(['tesseract', str(isolated_path or crop), 'stdout', '-l', 'eng+chi_sim', '--psm', '7'],
                                 text=True, capture_output=True, check=True, timeout=10).stdout
            normalize = lambda value: ''.join(character.casefold() for character in
                unicodedata.normalize('NFKC', value) if character.isalnum())
            if normalize(ocr) != normalize(composition['preedit']):
                raise RuntimeError('original app rendered field did not contain exact actual engine preedit')
            self.keys('spc')
            committed = self.await_state('real-native-libpinyin-commit', lambda: self.tree(actor),
                                         lambda value: any(node['path'] == field['path'] and node['focused']
                                                           and node['text'] == expected
                                                           for node in value['nodes']))
            self.keys('ctrl', 'a')
            self.keys('ctrl', 'c')
            copied = self.clipboard()
            if base64.b64decode(copied['base64']).decode() != expected:
                raise RuntimeError('actual IME consumer clipboard did not equal committed Unicode')
            evidence = {'actor': actor, 'field': field, 'composition': composition,
                        'preedit_frame': rendered, 'preedit_field_frame': str(crop),
                        'preedit_ink_frame': str(isolated_path) if isolated_path is not None else None,
                        'preedit_ocr': ocr,
                        'committed': committed, 'clipboard': copied, 'engine': engine,
                        'actual_input': 'physical-nihao-space', 'portal_only': actor.get('kind') == 'picker'}
            if actor.get('kind') == 'app':
                self._ime[actor['id']] = evidence
            return evidence
        finally:
            if monitor_started:
                self.guest('desktop', 'ime-monitor-stop')
            self.guest('desktop', 'ime-engine', select=False, original=engine['original'])

    @staticmethod
    def _binding_identity(value):
        return value | {'resources': [{key: data for key, data in row.items()
                                       if key != 'emitted_key_events'} for row in value['resources']]}

    @classmethod
    def _emission_advanced(cls, before, after):
        if cls._binding_identity(before) != cls._binding_identity(after):
            raise RuntimeError('original live connection/keyboard bindings changed during input')
        pairs = list(zip(before['resources'], after['resources'], strict=True))
        if not pairs or any(new['emitted_key_events'] < old['emitted_key_events'] for old, new in pairs):
            raise RuntimeError('actual keyboard emission counters retired or moved backwards')
        return all(new['emitted_key_events'] > old['emitted_key_events'] for old, new in pairs)

    def crosscut(self, actor, probe, restart_probe=None, ime_field=None):
        self.select(actor)
        baseline = probe(actor, 'baseline')
        before = self.observe('crosscut-baseline-' + actor['id'], actor)
        if ime_field is not None and actor['id'] not in self._ime:
            proof = self.ime(actor, ime_field)
            previous = proof['field']['text']
            if not previous.isascii():
                raise RuntimeError('domain must restore its actual non-ASCII IME field through real input')
            self.click_path(actor, proof['field']['path'])
            self.keys('ctrl', 'a')
            if previous:
                self.type_ascii(previous)
            else:
                self.keys('backspace')
        scales = self.guest('desktop', 'display-scale', scale=1.5)
        self.scale = scales['scale']
        try:
            scaled = probe(actor, 'scale150')
            scaled_frame = self.observe('crosscut-scale150-' + actor['id'], actor)
        finally:
            restored_scale = self.guest('desktop', 'display-scale', scale=1.0)
            self.scale = restored_scale['scale']
        self.select(actor)
        emission_before = self.guest('desktop', 'app-input', actor=actor, phase='unlocked-focused')
        self.keys('tab')
        positive = self.await_state('actual-original-app-positive-emission',
            lambda: self.guest('desktop', 'app-input', actor=actor, phase='unlocked-focused'),
            lambda value: self._emission_advanced(emission_before, value))
        buffers = {node['object_path']: node['text'] for node in self.tree(actor)['nodes']
                   if node['editable'] and node['text'] is not None}
        self.keys('meta_l', 'l')
        self.await_state('actual-app-locked', lambda: self.state(unlocked=False),
                         lambda value: value['locked'])
        lock_before = self.guest('desktop', 'app-input', actor=actor, phase='locked')
        if self._binding_identity(positive) != self._binding_identity(lock_before):
            raise RuntimeError('original client/binding changed across actual lock transition')
        self._type_locked_ascii('probe')
        capture = self.lifecycle('capture-input')
        if not (capture['connect_exit'] == 0 and 0 < capture['capture_exit'] < 124
                and 0 < capture['input_exit'] < 124 and not capture['capture_file']):
            raise RuntimeError('reachable ordinary clients did not genuinely deny capture/virtual input under lock')
        self.qmp.keys('esc')
        self.qmp.keys('ret')
        self.await_state('real-lock-password-field-ready',
                         lambda: self.guest('desktop', 'lock-ui'), lambda value: value['password_field_ready'])
        auth_offset = len(self._auth_records())
        self.qmp.keys('ctrl', 'a')
        self._type_locked_ascii('not-the-fixture-password')
        self.qmp.keys('ret')
        def refusal_state():
            records = self._auth_records()[auth_offset:]
            return {'locked': self.state(unlocked=False)['locked'], 'fresh_auth_records': records,
                    'refused': self._auth_refusal(records)}
        refusal = self.await_state('real-wrong-password-refused', refusal_state,
                                   lambda value: value['locked'] and value['refused'])
        denied_input = self.guest('desktop', 'app-input', actor=actor, phase='locked')
        if denied_input != lock_before:
            raise RuntimeError('ordinary/private/virtual input was emitted to original app while locked')
        self.qmp.keys('ctrl', 'a')
        self._type_locked_ascii('tuna')
        self.qmp.keys('ret')
        self.await_state('real-password-unlocked', lambda: self.state(unlocked=False),
                         lambda value: not value['locked'])
        recovered_input = self.guest('desktop', 'app-input', actor=actor, phase='postunlock')
        if recovered_input != lock_before:
            raise RuntimeError('protected held input leaked to original app during actual unlock')
        self.select(actor)
        after_buffers = {node['object_path']: node['text'] for node in self.tree(actor)['nodes']
                         if node['editable'] and node['text'] is not None}
        if after_buffers != buffers:
            raise RuntimeError('original editable buffer changed/retired across locked input and unlock')
        resumed_before = self.guest('desktop', 'app-input', actor=actor, phase='unlocked-focused')
        self.keys('tab')
        resumed = self.await_state('actual-original-app-postunlock-fresh-emission',
            lambda: self.guest('desktop', 'app-input', actor=actor, phase='unlocked-focused'),
            lambda value: self._emission_advanced(resumed_before, value))
        recovered = probe(actor, 'after-unlock')
        recovered_frame = self.observe('crosscut-unlocked-' + actor['id'], actor)
        replacement = self.restart(actor, actor['search_name'])
        persisted = (restart_probe or probe)(replacement, 'after-restart')
        result = {'baseline': baseline, 'baseline_observation': before,
                  'scale150': scaled, 'scale150_observation': scaled_frame,
                  'display_scale_before_after': [scales, restored_scale],
                  'prelock_server_emission_before': emission_before,
                  'prelock_positive_server_emission': positive,
                  'locked_input_before': lock_before, 'locked_input_after': denied_input,
                  'postunlock_before_fresh_input': recovered_input,
                  'postunlock_fresh_server_emission_before': resumed_before,
                  'postunlock_fresh_server_emission_after': resumed,
                  'unchanged_original_editable_buffers': {'before': buffers, 'after': after_buffers},
                  'capture_virtual_input_denial': capture, 'real_wrong_password_refusal': refusal,
                  'after_unlock': recovered, 'after_unlock_observation': recovered_frame,
                  'restart_actor': replacement, 'after_restart': persisted}
        self._cuts[actor['id']] = result
        return result

    def result(self, actor, operation, evidence, **extras):
        app_id = actor['id']
        if app_id not in self._cuts:
            raise RuntimeError('whole app result lacks actual scale/lock/frontend-restart consumer proof')
        if 'installed_signed_release_qualified' in extras:
            if extras.pop('installed_signed_release_qualified') is not False:
                raise ValueError('DEV app receipt cannot override signed-release qualification')
        protected = {'app', 'actor', 'principal_operation', 'evidence', 'crosscut', 'ime', 'source'}
        if protected & extras.keys():
            raise ValueError('domain evidence cannot replace authoritative app-result fields')
        result = {'app': self.inventory['apps'][app_id], 'actor': actor,
                  'principal_operation': operation, 'evidence': evidence,
                  'crosscut': self._cuts[app_id], 'ime': self._ime.get(app_id),
                  'installed_signed_release_qualified': False, 'source': self.metadata,
                  **extras}
        if result['ime'] is None and not extras.get('ime_applicability'):
            raise RuntimeError('actual IME receipt or precise verified noneditable interface reason required')
        # Observe ordinary surface retirement; do not claim that background
        # application/service processes have exited.
        current = self._actors[app_id]
        for key in ('id', 'exe', 'exe_sha256', 'package', 'version', 'desktop_sha256', 'code_entrypoints'):
            if current[key] != actor[key]:
                raise RuntimeError('final frontend changed its original installed application identity')
        self.close(current)
        result['retired_frontend_actor'] = current
        result['completed_frontend_surface_retired'] = True
        self._results.append(result)
        self.retain('app-result-' + app_id, result)
        return result

    def run(self):
        failure = None
        cleanup_errors = []
        saved_timezone = False
        fixture_attempted = False
        prepared = False
        timezone = None
        try:
            prepared = True
            self.inventory = self.guest('desktop', 'prepare')
            self.fixture = self.inventory['fixture']
            if self.inventory['observer_sha256'] != self._observer_members:
                raise RuntimeError('actual installed Core observers differ from admitted image/source')
            installation = self.inventory['installation']
            if (installation['source_commit'] != self.metadata['commit']
                    or installation['observer_sha256'] != self._observer_members):
                raise RuntimeError('Core image installation receipt differs from admitted source commit/members')
            for role in ('compositor', 'shell'):
                process = self.inventory['identity'][role]
                if self.metadata['member_sha256'].get(process['exe']) != process['exe_sha256']:
                    raise RuntimeError('actual original native ELF differs from admitted package/image')
            installed = self.inventory['installation']['after']
            if any(installed.get(app['package']) != app['version']
                   for app in self.inventory['apps'].values()):
                raise RuntimeError('actual original Core package versions differ from image installation')
            self.retain('immutable-subject', {'source': self.metadata, 'inventory': self.inventory})
            for name in ('basic', 'data', 'media', 'system'):
                importlib.import_module('core_apps_' + name).run(self)
            original = self.guest('autotz', 'save')
            saved_timezone = True
            self.retain('shared-location-original-datetime-settings', original)
            self.guest('autotz', 'configure', master=False, automatic=False)
            fixture_attempted = True
            self.retain('shared-official-geoclue-static-source',
                        self.guest('autotz', 'fixture-prepare'))
            endpoint = self.inventory['content_controls']['web']
            importlib.import_module('core_apps_web').run(
                self, prepared=endpoint['prepared'], endpoint=endpoint['endpoint'])
            timezone = importlib.import_module('core_apps_autotz').run(self, manage_fixture=False)
            observed = {row['app']['app_id'] for row in self._results}
            if observed != set(self.inventory['apps']) or len(self._results) != len(observed):
                raise RuntimeError('required actual installed Core applications remain unqualified')
        except Exception as error:
            failure = error
            inventory = getattr(self, 'inventory', {}).get('apps', {})
            completed = {row['app']['app_id'] for row in self._results}
            self.retain('incomplete-core-apps-matrix', {
                'source': self.metadata, 'completed_actual_app_results': self._results,
                'not_qualified_apps': sorted(set(inventory) - completed),
                'failure_type': type(error).__name__, 'unconfirmed_guest_child': self._uncertain,
                'installed_signed_release_qualified': False})
        finally:
            def cleanup(domain, operation, **arguments):
                if self._uncertain is not None:
                    cleanup_errors.append({'domain': domain, 'operation': operation,
                                           'status': 'not-executed-original-VM-retirement-required'})
                    return
                try:
                    value = self.guest(domain, operation, **arguments)
                    self.retain('cleanup-' + domain + '-' + operation, value)
                    if 'restored' in value and value['restored'] is not True:
                        raise RuntimeError('owned fixture restoration not verified')
                except Exception as error:
                    cleanup_errors.append({'domain': domain, 'operation': operation,
                                           'status': 'failed', 'failure_type': type(error).__name__})
            if saved_timezone:
                cleanup('autotz', 'configure', master=False, automatic=False)
            if fixture_attempted:
                cleanup('autotz', 'fixture-restore')
            if saved_timezone:
                cleanup('autotz', 'restore')
            if prepared:
                if failure is not None:
                    cleanup('desktop', 'retire-frontends', actors=list(self._actors.values()))
                cleanup('basic', 'cleanup')
                cleanup('web', 'stop-endpoint')
                cleanup('desktop', 'finish')
            self.retain('core-apps-owned-fixture-retirement', {
                'cleanup_errors': cleanup_errors,
                'original_VM_retirement_required': self._uncertain is not None,
                'installed_signed_release_qualified': False})
        if failure is not None:
            raise RuntimeError('actual Core matrix incomplete; original failure evidence retained') from failure
        if cleanup_errors:
            raise RuntimeError('actual Core matrix owned fixture restoration failed')
        result = {'apps': self._results, 'automatic_timezone': timezone,
                  'source': self.metadata, 'inventory': self.inventory,
                  'installed_signed_release_qualified': False,
                  'scope': 'actual DEV native operations; not final signed shipped-image qualification'}
        self.retain('core-apps-component-results', result)
        return result
