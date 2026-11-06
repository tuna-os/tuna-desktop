"""Actual original-process Calculator, Fonts, Papers, Loupe and Settings."""
import base64
from pathlib import Path
import re

from core_apps_web import (content, control, field, nodes, press, replace_field,
                           require, settings_panel, unique)


COLORS = ((32, 74, 135), (204, 0, 0), (78, 154, 6), (252, 233, 79))


def image_payload(frame_path, frame_bounds, colors=COLORS):
    """Identify the actual decoded 2x2 payload, never another app's pixels."""
    from PIL import Image, ImageChops
    left, top, width, height = frame_bounds
    with Image.open(frame_path) as source:
        require(source.size == (1280, 800), 'actual native output dimensions changed')
        region = source.convert('RGB').crop((left, top, left + width, top + height))
    boxes = []
    for color in colors:
        channels = region.split()
        masks = [channel.point([255 if value == component else 0 for value in range(256)])
                 for channel, component in zip(channels, color)]
        mask = ImageChops.multiply(ImageChops.multiply(masks[0], masks[1]), masks[2])
        box = mask.getbbox()
        require(box is not None, 'known rendered image color absent from original actor region')
        boxes.append(box)
    x0, y0 = min(box[0] for box in boxes), min(box[1] for box in boxes)
    x1, y1 = max(box[2] for box in boxes), max(box[3] for box in boxes)
    require(x1 > x0 and y1 > y0, 'actual decoded image rectangle invalid')
    actual = region.crop((x0, y0, x1, y1)).resize((480, 320), Image.Resampling.NEAREST)
    # The nonce is drawn near the upper edge; these interior samples exclude it
    # and every interpolation boundary, while identifying all four quadrants.
    samples = []
    for index, (qx, qy) in enumerate(((0, 0), (1, 0), (0, 1), (1, 1))):
        for dx in (60, 120, 180):
            for dy in (60, 100, 130):
                point = (qx * 240 + dx, qy * 160 + dy)
                value = actual.getpixel(point)
                require(value == tuple(colors[index]), 'actual image quadrant/order differs from known decoder payload')
                samples.append({'point': list(point), 'rgb': list(value)})
    return {'physical_payload_bounds': [left + x0, top + y0, x1 - x0, y1 - y0],
            'actual_samples': samples, 'frame_path': frame_path}


def _text_bytes(receipt):
    return base64.b64decode(receipt['base64'], validate=True)


def formatted_result_matches(text, value, separators):
    """Calculator display arithmetic oracle independent of the grouping glyph.

    The app groups with its session locale's separator (a space on the
    qualification image, a comma elsewhere); the oracle pins the digit
    sequence and whether grouping is present, never the locale's exact
    glyph. Numbers below 10000 carry no grouping either way.
    """
    digits = ''.join(character for character in (text or '') if character in '0123456789')
    if digits != str(value):
        return False
    if value < 10000:
        return True
    return (digits != text) == separators


def metadata_label(details, text):
    # The details view shows the face family twice: once as the header
    # title (nested panels) and once as the Name metadata value (inside
    # a list row). Only a list row ancestor identifies the metadata
    # value; panel ancestors match the title too and break uniqueness.
    return unique(details, lambda node: node['role'] == 'label' and node.get('text') == text
                  and any(parent['role'] in ('list item', 'row') for parent in node['ancestors']),
                  'actual font metadata name label')


def _fonts(desktop):
    actor = desktop.start('org.gnome.font-viewer', 'Fonts')
    evidence = {'before': desktop.guest('basic', 'font-state')}
    require(not evidence['before']['user_font']['exists'], 'owned font-install precondition already used')
    require('Noto Sans CJK JP' not in evidence['before']['match']['family'],
            'official font control is already installed; no duplicate-install operation claimed')
    filename = desktop.inventory['content_controls']['font']['filename']
    source = desktop.fixture + '/' + filename
    opened = desktop.open_default(source)
    require(opened['actor']['id'] == actor['id'], 'actual OTF default handler is not installed Fonts')
    actor = opened['actor']
    evidence['default_handler'] = opened
    content(desktop, actor, 'font-real-face-preview', 'Noto Sans CJK JP', 'Regular')
    desktop.keys('alt', 'left')
    cancelled = desktop.guest('basic', 'font-state')
    require(cancelled == evidence['before'], 'leaving font preview unexpectedly installed or changed the font')
    evidence['preview-without-install-negative'] = cancelled
    actor = desktop.open_default(source)['actor']
    press(desktop, actor, 'Install')
    wanted_sha = desktop.inventory['content_controls']['font']['sha256']
    installed = desktop.await_state('font-installed-real-consumer', lambda: desktop.guest('basic', 'font-state'),
        lambda value: value['user_font'].get('sha256') == wanted_sha
        and value['match']['sha256'] == wanted_sha
        and value['match']['family'] == 'Noto Sans CJK JP')
    evidence['actual-user-install'] = installed
    content(desktop, actor, 'font-installer-completed', 'Installed')
    press(desktop, actor, ('Info', 'Font Details'))
    details = content(desktop, actor, 'font-real-source-details', 'Name', 'Noto Sans CJK JP', 'Style', 'Regular')
    label = metadata_label(details, 'Noto Sans CJK JP')
    desktop.click_path(actor, label['path'])
    desktop.keys('ctrl', 'a')
    desktop.keys('ctrl', 'c')
    evidence['actual-details-clipboard'] = desktop.clipboard()
    require(_text_bytes(evidence['actual-details-clipboard']) == b'Noto Sans CJK JP',
            'font details clipboard is not the actual opened face family')
    desktop.keys('alt', 'left')
    desktop.keys('alt', 'left')
    desktop.keys('ctrl', 'f')
    search = field(desktop, actor)
    evidence['ime'] = desktop.ime(actor, search['name'], field_path=search['path'])
    replace_field(desktop, actor, 'Noto Sans CJK JP', search['name'])
    def probe(current, stage):
        tree = content(desktop, current, 'font-index-' + stage, 'Noto Sans CJK JP')
        actual = desktop.guest('basic', 'font-state')
        require(actual['match']['sha256'] == wanted_sha and actual['user_font']['sha256'] == wanted_sha,
                'installed face disappeared or changed during native consumer recovery')
        return {'actual_font_index': tree, 'fontconfig_consumer': actual}
    def restarted(current, stage):
        desktop.keys('ctrl', 'f')
        replace_field(desktop, current, 'Noto Sans CJK JP')
        return probe(current, stage)
    evidence['crosscut'] = desktop.crosscut(actor, probe, restart_probe=restarted)
    return desktop.result(actor, 'native-font-file-preview-metadata-copy-install-index-and-user-font-persistence', evidence,
        service_applicability={'backend': 'actual installed Fontconfig and FreeType; no remote font service',
                               'negative': 'preview exit left original user font inventory unchanged'},
        source_contracts=['https://github.com/GNOME/gnome-font-viewer/blob/main/src/font-view-window.c'])


def _calculator(desktop):
    actor = desktop.start('org.gnome.Calculator', 'Calculator')
    original = desktop.guest('basic', 'calculator-state')
    desired = not original['settings']['show-thousands']
    desktop.keys('ctrl', 'comma')
    row = control(desktop, actor, ('Thousands Separators', 'Thousands Separator', 'Group Digits'),
                  ('switch', 'toggle button', 'check box', 'row'))
    desktop.click_path(actor, row['path'])
    desktop.await_state('calculator-real-preference-write', lambda: desktop.guest('basic', 'calculator-state'),
                         lambda value: value['settings']['show-thousands'] is desired)
    desktop.keys('esc')
    equation = unique(desktop.tree(actor), lambda node: node['editable'] and node['text'] is not None,
                      'original editable arithmetic display')
    ime = desktop.ime(actor, equation['name'], field_path=equation['path'])
    def enter(current, expression, value, label):
        display = unique(desktop.tree(current), lambda node: node['editable'] and node['text'] is not None,
                         'original arithmetic display')
        desktop.click_path(current, display['path'])
        desktop.keys('ctrl', 'a')
        desktop.type_ascii(expression)
        desktop.keys('ret')
        observed = desktop.await_state(label, lambda: desktop.observe(label, current),
            lambda snapshot: any(node['object_path'] == display['object_path'] and node['focused']
                                 and formatted_result_matches(node['text'], value, desired)
                                 for node in snapshot['nodes']))
        desktop.keys('ctrl', 'a')
        desktop.keys('ctrl', 'c')
        copied = desktop.clipboard()
        require(formatted_result_matches(_text_bytes(copied).decode(), value, desired),
                'actual calculator result/clipboard differs from arithmetic oracle')
        return {'expression': expression, 'mathematical_result': value,
                'formatted': {'digits': str(value), 'grouped': bool(desired and value >= 10000)},
                'original_display': observed, 'clipboard': copied}
    evidence = {'original_settings': original, 'ime': ime,
                'negative_changed_operand': enter(actor, '7*8', 56, 'calculator-distinct-operands')}
    desktop.keys('ctrl', 'a')
    desktop.type_ascii('1/0')
    desktop.keys('ret')
    evidence['division_by_zero'] = desktop.await_state('calculator-real-undefined-operation',
        lambda: desktop.observe('calculator-real-undefined-operation', actor),
        lambda value: any('division by zero' in (node.get('text') or node['name']).lower()
                          for node in nodes(value)))
    def probe(current, stage):
        expression, value = {'baseline': ('123*456', 56088), 'scale125': ('19*37', 703),
                             'after-unlock': ('17*43', 731), 'after-restart': ('23*29', 667)}[stage]
        settings = desktop.guest('basic', 'calculator-state')
        require(settings['settings']['show-thousands'] is desired, 'actual calculator preference did not persist')
        return {'calculation': enter(current, expression, value, 'calculator-' + stage), 'settings': settings}
    evidence['crosscut'] = desktop.crosscut(actor, probe)
    return desktop.result(actor, 'fresh-native-arithmetic-error-clipboard-IME-and-persisted-formatting', evidence,
        file_link_portal_applicability={'actual_desktop': desktop.inventory['apps'][actor['id']],
            'reason': 'native arithmetic display has no document file or URI input/default handler; numerical and IME controls exercised'},
        service_applicability={'backend': 'installed in-process arithmetic library; actual undefined-operation error, no external calculator service'})


def _loupe(desktop):
    actor = desktop.start('org.gnome.Loupe', 'Image Viewer')
    path = desktop.fixture + '/principal.png'
    corrupt = desktop.guest('basic', 'decoder-control', kind='png')
    rejected = desktop.open_file(actor, corrupt['path'], cancel_first=False)
    decoder_error = content(desktop, actor, 'loupe-real-corrupt-decoder-rejection',
                            'Could not Load Image', timeout=30)
    evidence = {'open': desktop.open_file(actor, path, cancel_first=True)}
    evidence['decoder_negative'] = {'control': corrupt, 'native_open': rejected,
                                    'actual_original_error': decoder_error}
    desktop.keys('ctrl', '1')
    baseline = desktop.observe('loupe-original-decoder-content', actor)
    evidence['decoded'] = image_payload(baseline['frame_path'], baseline['frame_bounds'])
    desktop.keys('ctrl', 'equal')
    enlarged = desktop.observe('loupe-real-keyboard-zoom', actor)
    evidence['zoom'] = image_payload(enlarged['frame_path'], enlarged['frame_bounds'])
    require(evidence['zoom']['physical_payload_bounds'][2] > evidence['decoded']['physical_payload_bounds'][2],
            'real image zoom did not enlarge decoded source pixels')
    desktop.keys('ctrl', '1')
    desktop.keys('ctrl', 'r')
    rotated = desktop.observe('loupe-real-clockwise-rotation', actor)
    evidence['rotation'] = image_payload(rotated['frame_path'], rotated['frame_bounds'],
                                        (COLORS[2], COLORS[0], COLORS[3], COLORS[1]))
    desktop.keys('ctrl', 'shift', 'r')
    desktop.keys('ctrl', 'c')
    copied = desktop.clipboard('text/uri-list')
    require(_text_bytes(copied).decode().rstrip('\r\n') == Path(path).as_uri(),
            'real image clipboard URI is not the opened controlled image')
    evidence['clipboard'] = copied
    before = desktop.guest('basic', 'viewer-state', relative='principal.png', app_id=actor['id'])
    require(type(before['preferences'].get('show-properties')) is bool, 'installed Image Viewer lacks actual documented properties preference')
    desired = not before['preferences']['show-properties']
    desktop.keys('f9')
    desktop.await_state('loupe-real-properties-preference',
        lambda: desktop.guest('basic', 'viewer-state', relative='principal.png', app_id=actor['id']),
        lambda value: value['preferences']['show-properties'] is desired)
    desktop.keys('tab')
    press(desktop, actor, 'Zoom to Specific Level')
    zoom_entry = unique(desktop.tree(actor),
        lambda node: node['showing'] and node['editable'] and node['name'] == 'Custom Zoom Level',
        'original Image Viewer custom zoom entry')
    original_zoom = zoom_entry['text']
    require(original_zoom is not None and original_zoom.isascii(),
            'original numeric zoom cannot be restored by its ordinary native input')
    try:
        evidence['ime'] = desktop.ime(actor, zoom_entry['name'], field_path=zoom_entry['path'])
    finally:
        desktop.click_path(actor, zoom_entry['path'])
        desktop.keys('ctrl', 'a')
        desktop.keys('backspace')
        desktop.type_ascii(original_zoom)
        desktop.keys('ret')
        desktop.keys('esc')
    desktop.keys('ctrl', '1')
    def probe(current, stage):
        desktop.keys('ctrl', '1')
        observed = desktop.observe('loupe-' + stage, current)
        actual = image_payload(observed['frame_path'], observed['frame_bounds'])
        persisted = desktop.guest('basic', 'viewer-state', relative='principal.png', app_id=current['id'])
        require(persisted['preferences']['show-properties'] is desired, 'Image Viewer actual preference did not persist')
        files = desktop.guest('desktop', 'files', paths=['principal.png', 'untouched.txt'])
        require(files['files'][0]['sha256'] == desktop.inventory['content_controls']['png']['sha256'],
                'view/zoom/in-memory rotation modified source PNG bytes')
        require(files['files'][1]['sha256'] == desktop.inventory['control']['sha256'], 'unselected control changed')
        return {'decoded_pixels': actual, 'state': persisted, 'files': files, 'original_interface': observed}
    def restarted(current, stage):
        opened = desktop.open_file(current, path, cancel_first=False)
        return {'real_reopen': opened, 'consumer': probe(current, stage)}
    evidence['crosscut'] = desktop.crosscut(actor, probe, restart_probe=restarted)
    return desktop.result(actor, 'native-PNG-decode-zoom-rotation-file-clipboard-and-properties-persistence', evidence,
        service_applicability={'backend': 'real installed image decoder; actual native picker cancel/grant, known payload orientation and unchanged unselected/source bytes'},
        source_contracts=['https://github.com/GNOME/loupe/blob/main/src/widgets/image_view.ui'])


def _papers(desktop):
    actor = desktop.start('org.gnome.Papers', 'Document Viewer')
    path = desktop.fixture + '/principal.pdf'
    expected = desktop.inventory['content_controls']['pdf']['text']
    corrupt = desktop.guest('basic', 'decoder-control', kind='pdf')
    rejected = desktop.open_file(actor, corrupt['path'], cancel_first=False)
    decoder_error = content(desktop, actor, 'papers-real-corrupt-decoder-rejection',
                            'Unable to Open Document', timeout=30)
    evidence = {'open': desktop.open_file(actor, path, cancel_first=True)}
    evidence['decoder_negative'] = {'control': corrupt, 'native_open': rejected,
                                    'actual_original_error': decoder_error}
    def copy_document(current, stage):
        observed = desktop.await_state('papers-content-' + stage, lambda: desktop.observe('papers-content-' + stage, current),
            lambda value: any(node['showing'] and node['role'] == 'paragraph' and node['text'] == expected
                              for node in value['nodes']))
        paragraph = unique(observed, lambda node: node['role'] == 'paragraph' and node['text'] == expected,
                           'actual decoded PDF paragraph')
        desktop.click_path(current, paragraph['path'])
        desktop.keys('ctrl', 'a')
        desktop.keys('ctrl', 'c')
        clipboard = desktop.clipboard()
        require(_text_bytes(clipboard).decode() == expected, 'actual selected PDF copy differs from decoded source content')
        return {'actual_document': observed, 'clipboard': clipboard}
    evidence['document'] = copy_document(actor, 'initial')
    press(desktop, actor, 'Zoom In')
    def zoom(current):
        return unique(desktop.tree(current), lambda node: node.get('text') is not None
                      and re.fullmatch(r'[0-9]+(?:[.,][0-9]+)?%', node['text']) is not None,
                      'actual PDF zoom percentage control')['text']
    wanted_zoom = zoom(actor)
    evidence['zoom'] = {'actual_user_value': wanted_zoom,
                        'independent': desktop.guest('basic', 'viewer-state', relative='principal.pdf', app_id=actor['id'])}
    desktop.keys('ctrl', 'f')
    search = unique(desktop.tree(actor), lambda node: node['showing'] and node['focused'] and node['editable'],
                    'original document search entry')
    evidence['ime'] = desktop.ime(actor, search['name'], field_path=search['path'])
    evidence['search-negative-consumer'] = content(
        desktop, actor, 'papers-original-ime-search-no-match', 'No Results Found', timeout=30)
    desktop.keys('ctrl', 'a')
    desktop.keys('backspace')
    desktop.keys('esc')
    link = unique(desktop.tree(actor), lambda node: node['role'] == 'link', 'actual PDF URI annotation')
    desktop.press(actor, link['name'], role='link')
    browser = desktop.await_state('papers-real-link-consumer',
        lambda: desktop.guest('desktop', 'discover', app_id='org.gnome.Epiphany'),
        lambda value: value.get('pid') is not None)
    evidence['link'] = content(desktop, browser, 'papers-real-default-browser-content', 'Browser content TUNA-WEB-' + desktop.nonce, timeout=30)
    requests = desktop.guest('web', 'endpoint')['requests']
    require(any(row['path'] == '/' + desktop.nonce + '/' and 'AppleWebKit' in row['user_agent'] for row in requests),
            'real PDF URI did not reach the actual ordinary content endpoint through WebKit')
    desktop.close(browser)
    desktop.select(actor)
    def probe(current, stage):
        value = copy_document(current, stage)
        files = desktop.guest('desktop', 'files', paths=['principal.pdf', 'untouched.txt'])
        require(files['files'][0]['sha256'] == desktop.inventory['content_controls']['pdf']['sha256'], 'view/search/selection changed source PDF bytes')
        require(files['files'][1]['sha256'] == desktop.inventory['control']['sha256'], 'unselected content changed')
        return value | {'independent_metadata': desktop.guest('basic', 'viewer-state', relative='principal.pdf', app_id=current['id']), 'files': files}
    def restarted(current, stage):
        opened = desktop.open_file(current, path, cancel_first=False)
        require(zoom(current) == wanted_zoom, 'actual PDF document zoom did not persist across frontend restart')
        return {'real_reopen': opened, 'consumer': probe(current, stage)}
    evidence['crosscut'] = desktop.crosscut(actor, probe, restart_probe=restarted)
    return desktop.result(actor, 'native-PDF-content-selection-clipboard-search-IME-link-and-document-zoom-persistence', evidence,
        service_applicability={'backend': 'actual installed document decoder and GVfs per-document metadata; native chooser cancellation/grant and ordinary real HTTP URI consumer'})


def _settings(desktop):
    actor = desktop.start('org.gnome.Settings', 'Settings')
    original = desktop.guest('basic', 'style-save')
    target = 'default' if original['original']['color_scheme'] == 'prefer-dark' else 'prefer-dark'
    wanted_name = 'Dark' if target == 'prefer-dark' else 'Default'
    labels = ('Dark', 'Dark Style') if target == 'prefer-dark' else ('Default', 'Light', 'Light Style')
    evidence = {'original': original}
    try:
        actor = settings_panel(desktop, 'Appearance', actor=actor)
        chosen = control(desktop, actor, labels, ('toggle button', 'radio button', 'button', 'push button'))
        desktop.click_path(actor, chosen['path'])
        evidence['actual-style-write'] = desktop.await_state('settings-real-appearance-write',
            lambda: desktop.guest('basic', 'style-state'), lambda value: value['color_scheme'] == target)
        # Observe a real backend outage, not an injected settings-service response.
        outage = desktop.guest('basic', 'dconf-interrupt')
        try:
            opposite = ('Default', 'Light', 'Light Style') if target == 'prefer-dark' else ('Dark', 'Dark Style')
            button = control(desktop, actor, opposite, ('toggle button', 'radio button', 'button', 'push button'))
            desktop.click_path(actor, button['path'])
            blocked = desktop.guest('basic', 'style-state')
            require(blocked['color_scheme'] == target, 'settings writer suspension did not withhold the pending persistent write')
            evidence['service-negative'] = {'actual_suspended_owner': outage, 'independent_withheld_write': blocked,
                                           'actual_pending_UI': desktop.observe('settings-real-writer-outage', actor)}
        finally:
            evidence['service-restored'] = desktop.guest('basic', 'dconf-restore')
        # This is a new deliberate UI choice after the unavailable writer recovered,
        # not a replay that converts the recorded negative into a positive.
        desktop.await_state('settings-original-writer-pending-write-recovered',
            lambda: desktop.guest('basic', 'style-state'), lambda value: value['color_scheme'] != target)
        chosen = control(desktop, actor, labels, ('toggle button', 'radio button', 'button', 'push button'))
        desktop.click_path(actor, chosen['path'])
        desktop.await_state('settings-real-writer-positive-recovery',
            lambda: desktop.guest('basic', 'style-state'), lambda value: value['color_scheme'] == target)
        search_button = control(desktop, actor, ('Search', 'Search Settings'), ('button', 'push button', 'toggle button'))
        desktop.click_path(actor, search_button['path'])
        search = unique(desktop.tree(actor), lambda node: node['focused'] and node['editable'], 'original Settings search entry')
        evidence['ime'] = desktop.ime(actor, search['name'], field_path=search['path'])
        desktop.keys('esc')
        actor = settings_panel(desktop, 'Appearance', actor=actor)
        def probe(current, stage):
            actual = desktop.guest('basic', 'style-state')
            require(actual['color_scheme'] == target, 'actual Settings user preference did not persist')
            selected = control(desktop, current, labels, ('toggle button', 'radio button', 'button', 'push button'))
            require(selected['checked'] or selected['selected'], 'native Settings selected style differs from independent preference')
            return {'native_style': desktop.observe('settings-' + stage, current), 'selected': selected,
                    'actual_writer_and_preferences': actual, 'chosen_style': wanted_name}
        def restarted(current, stage):
            current = settings_panel(desktop, 'Appearance', actor=current)
            return probe(current, stage)
        evidence['crosscut'] = desktop.crosscut(actor, probe, restart_probe=restarted)
        return desktop.result(actor, 'native-settings-search-IME-appearance-write-real-dconf-outage-and-preference-persistence', evidence,
            file_link_portal_applicability={'actual_desktop': desktop.inventory['apps'][actor['id']],
                'reason': 'Appearance preference controls do not consume documents or ask sandbox file grants; original search/appearance/service controls exercised'})
    finally:
        desktop.retain('settings-exact-original-style-cleanup', desktop.guest('basic', 'cleanup'))


def run(desktop):
    # Fonts is a real user font installation before linguistic content probes;
    # no CJK font is silently added to the default shipping image.
    return [_fonts(desktop), _calculator(desktop), _loupe(desktop), _papers(desktop), _settings(desktop)]
