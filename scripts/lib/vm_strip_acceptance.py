"""Source-owned installed strip observations. No desired-state or PID-join fabrication."""
import json
import socket
import threading
import time

ROLES = ('alpha', 'beta', 'gamma')
EDGE_ROLES = (*ROLES, 'delta')
COLORS = {'alpha': (44, 102, 164), 'beta': (171, 65, 81), 'gamma': (65, 145, 89), 'delta': (147, 96, 179)}


class NotReady(RuntimeError):
    pass


def windows(sample, original, *, edge=False, live_edge=False):
    """Validate original process/boot identity before treating geometry as ready."""
    if type(edge) is not bool or type(live_edge) is not bool or (live_edge and not edge):
        raise RuntimeError('invalid source-owned fixture observation mode')
    roles = EDGE_ROLES if edge else ROLES
    owner, fixture, inventory = sample['owner'], sample['fixture'], sample['inventory']
    mode = 'edge' if edge else 'baseline'
    if owner.get('fixture_mode') != mode or fixture.get('fixture_mode') != mode:
        raise RuntimeError('original fixture mode/argv contract differs')
    if owner != original or fixture['nonce'] != original['nonce']:
        raise RuntimeError('fixture ownership differs from original admission')
    identity = fixture['identity']
    if identity != original['client'] or identity['boot_id'] != original['boot_id']:
        raise RuntimeError('fixture PID/start/executable/boot drift')
    if (fixture.get('backend') != 'wayland' or type(fixture.get('schema')) is not int
            or fixture['schema'] != 1):
        raise RuntimeError('actual Wayland fixture schema missing')
    if (type(fixture.get('sequence')) is not int or fixture['sequence'] <= 0):
        raise RuntimeError('genuine positive fixture sequence missing')
    state = inventory['state']
    if type(state.get('locked')) is not bool or type(state.get('overview_open')) is not bool:
        raise RuntimeError('genuine lock/overview flags missing')
    if state['locked'] or state['overview_open']:
        raise NotReady('window interaction is owned by lock/overview')
    core = original['core']
    processes = inventory['processes']
    for name, expected in core.items():
        matches = [p for p in processes if p['executable'] == name]
        if len(matches) != 1 or any(matches[0].get(k) != expected[k] for k in ('pid', 'uid', 'start_ticks')):
            raise RuntimeError('native compositor/shell process identity drift')
    if set(fixture['windows']) != set(roles):
        raise NotReady('all actual fixture windows not ready')
    result = {}
    for role in roles:
        actual = fixture['windows'][role]
        want = f"org.tuna.VmStrip.n{original['nonce']}.{role}"
        if tuple(actual.get('color', ())) != COLORS[role]:
            raise RuntimeError('fixture color differs from source-owned pattern')
        if actual['app_id'] != want:
            raise RuntimeError('fixture role/app ID differs from nonce admission')
        matches = [w for w in state['windows'] if w['app_id'] == want]
        if len(matches) != 1:
            raise NotReady('missing/ambiguous actual native map')
        window = matches[0]
        rect = window['rect']
        if (type(window['id']) is not int or window['id'] <= 0
                or len(rect) != 4 or any(type(v) is not int for v in rect)
                or rect[2] < 120 or rect[3] < 80):
            raise RuntimeError('invalid actual window identity/geometry')
        if type(actual.get('mapped')) is not bool:
            raise RuntimeError('genuine GTK mapped flag missing')
        for field in ('paint_count', 'width', 'height', 'painted_width', 'painted_height'):
            if type(actual.get(field)) is not int or actual[field] <= 0:
                raise NotReady('genuine positive GTK paint/allocation observations missing')
        if (not actual['mapped']
                or [actual['width'], actual['height']] != rect[2:]
                or [actual['painted_width'], actual['painted_height']] != rect[2:]):
            raise NotReady('actual GTK allocation/paint differs from compositor')
        result[role] = window
    if len({w['id'] for w in result.values()}) != len(roles):
        raise RuntimeError('fixture windows reused the same compositor identity')
    if (state.get('session_mode') not in ('gnome', 'scroll')
            or type(state.get('rendered_frames')) is not int or state['rendered_frames'] <= 0):
        raise NotReady('native render/session observation absent')
    if type(state.get('strip_animating')) is not bool:
        raise RuntimeError('genuine strip animation flag missing')
    if not live_edge and state['strip_animating']:
        raise NotReady('actual strip view or column animation still running')
    return result


def endpoint(sample, original, *, mode, previous=None, edge=False):
    actual = windows(sample, original, edge=edge)
    state = sample['inventory']['state']
    if state['session_mode'] != mode:
        raise NotReady('requested mode has not reached the actual consumer')
    if previous is not None:
        if sample['fixture']['sequence'] < previous['fixture']['sequence']:
            raise RuntimeError('fixture observation sequence regressed')
        old = {role: w['id'] for role, w in windows(previous, original, edge=edge).items()}
        if {role: w['id'] for role, w in actual.items()} != old:
            raise RuntimeError('native windows remapped during interaction')
        if state['rendered_frames'] <= previous['inventory']['state']['rendered_frames']:
            raise NotReady('no newer actual compositor render')
    return actual


def await_stationary_edge(read, original, before, *, deadline):
    """Observe genuine ongoing demand, then a settled terminal; sends NO input."""
    import math
    initial = windows(before, original, edge=True)
    target = initial['delta']['rect']
    if target[0] < 1280:
        raise RuntimeError('stationary target was already visible before demand')
    def number(value):
        if type(value) not in (int, float) or not math.isfinite(value):
            raise RuntimeError('genuine finite native viewport coordinate missing')
        return value
    start_offset = number(before['inventory']['state']['strip_offset'])
    start_frame = before['inventory']['state']['rendered_frames']
    original_ids = {role: item['id'] for role, item in initial.items()}
    def counters(sample):
        values = []
        for role in EDGE_ROLES:
            for field in ('move_requests', 'resize_requests', 'delivered_buttons', 'key_count'):
                value = sample['fixture']['windows'][role].get(field)
                if type(value) is not int or value < 0:
                    raise RuntimeError('genuine GTK input counter missing')
                values.append(value)
        return tuple(values)
    counters(before)
    initial_moves = before['fixture']['windows']['alpha']['move_requests']
    pointer = frozen_input = None
    previous_offset = start_offset
    previous_sequence = before['fixture']['sequence']
    last_frame = last_live_frame = start_frame
    observed_frames = set()
    advances = 0
    saw_live_advance = False
    terminal_key = None
    stable_terminal = 0
    observations = []
    while time.monotonic() < deadline:
        sample = read(deadline)
        if time.monotonic() >= deadline:
            raise TimeoutError('stationary read exceeded absolute deadline')
        if len(observations) >= 128 or len(json.dumps(sample).encode()) > 65536:
            raise RuntimeError('stationary evidence count/byte bound exceeded')
        observations.append(sample)
        try:
            found = windows(sample, original, edge=True, live_edge=True)
            state = sample['inventory']['state']
            if state['session_mode'] != 'scroll':
                raise RuntimeError('stationary consumer left actual strip mode')
            if {role: item['id'] for role, item in found.items()} != original_ids:
                raise RuntimeError('stationary native windows remapped')
            sequence = sample['fixture']['sequence']
            if sequence < previous_sequence or state['rendered_frames'] < last_frame:
                raise RuntimeError('stationary observation sequence/frame regressed')
            previous_sequence = sequence
            current = counters(sample)
            preview = state['tile_preview'] or {}
            point = state.get('pointer_position')
            if not isinstance(point, list) or len(point) != 2:
                raise RuntimeError('actual native pointer observation missing')
            actual_pointer = tuple(number(value) for value in point)
            if pointer is None:
                if (preview.get('window') != found['alpha']['id'] or not (1270 <= actual_pointer[0] < 1280 and 0 <= actual_pointer[1] < 800)
                        or sample['fixture']['windows']['alpha']['move_requests'] <= initial_moves):
                    raise NotReady('genuine delivered GTK edge gesture not yet admitted')
                pointer, frozen_input = actual_pointer, current
            elif actual_pointer != pointer or current != frozen_input:
                raise RuntimeError('stationary phase received pointer or fixture input changes')
            if preview.get('window') != found['alpha']['id']:
                raise RuntimeError('stationary initiating drag owner disappeared')
            offset = number(state['strip_offset'])
            shown = number(state['strip_view'])
            if offset < previous_offset or abs(shown - offset) > 1e-6:
                raise RuntimeError('stationary direct viewport regressed or retained lag')
            if offset > previous_offset:
                advances += 1
                saw_live_advance |= state['strip_animating']
            previous_offset = offset
            frame = state['rendered_frames']
            if frame > last_frame:
                observed_frames.add(frame)
                last_frame = frame
            if state['strip_animating']:
                last_live_frame = frame
            rect = found['delta']['rect']
            terminal = (not state['strip_animating'] and offset > start_offset + 8
                        and 0 <= rect[0] and rect[0] + rect[2] <= 1280
                        and frame > last_live_frame and advances >= 2 and len(observed_frames) >= 3 and saw_live_advance)
            key = (offset, tuple(rect), tuple(preview.get('rect', ())))
            if terminal and frame in observed_frames:
                stable_terminal = stable_terminal + 1 if key == terminal_key else 1
                terminal_key = key
            else:
                stable_terminal, terminal_key = 0, None
            if time.monotonic() >= deadline:
                raise TimeoutError('stationary validation exceeded absolute deadline')
            if stable_terminal >= 3:
                return sample, observations
        except NotReady:
            stable_terminal, terminal_key = 0, None
        time.sleep(min(.5, max(0, deadline - time.monotonic())))
    raise TimeoutError('actual stationary edge never reached a settled terminal')


def colored_frame(image, rect, color):
    """Strict actual viewport pixels; never accept a model rectangle alone."""
    width, height, pixels = image
    if len(pixels) != width * height * 3:
        raise RuntimeError('invalid framebuffer byte count')
    x, y, w, h = rect
    left, right = max(0, x + 12), min(width, x + w - 20)
    top, bottom = max(0, y + 48), min(height, y + h - 20)
    if right - left < 40 or bottom - top < 40:
        raise NotReady('fixture body not visibly large enough to prove pixels')
    for row in range(1, 6):
        for column in range(1, 6):
            px = left + (right - left) * column // 6
            py = top + (bottom - top) * row // 6
            value = pixels[(py * width + px) * 3:(py * width + px) * 3 + 3]
            if any(abs(a - b) > 2 for a, b in zip(value, color)):
                raise NotReady('actual framebuffer differs from owned fixture color')
    return True


class QmpDeadline:
    """One phase's owned absolute I/O watchdog over the actual QMP socket."""
    def __init__(self, qmp, deadline):
        self.qmp, self.deadline = qmp, deadline
        self.original = qmp.cmd
        self.failed = False

    def command(self, command, **arguments):
        if self.failed:
            raise RuntimeError('installed QMP transport previously failed')
        remaining = min(5.0, self.deadline - time.monotonic())
        if remaining <= 0:
            self.failed = True
            raise TimeoutError('installed QMP phase absolute deadline')
        sock = self.qmp.sock
        expired = threading.Event()
        def abort():
            expired.set()
            try:
                sock.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        timer = threading.Timer(remaining, abort)
        timer.name = 'installed-strip-qmp-deadline'
        previous = sock.gettimeout()
        sock.settimeout(remaining)
        timer.start()
        began = time.monotonic()
        try:
            value = self.original(command, **arguments)
            if expired.is_set() or time.monotonic() - began >= remaining:
                raise TimeoutError('installed QMP phase absolute deadline')
            return value
        except BaseException as error:
            timed_out = expired.is_set()
            self.failed = True
            abort()
            if timed_out:
                raise TimeoutError('installed QMP phase absolute deadline') from error
            raise
        finally:
            timer.cancel()
            timer.join()
            try:
                sock.settimeout(previous)
            except OSError:
                if not self.failed:
                    self.failed = True
                    raise


def await_endpoint(read, original, predicate, *, deadline, previous=None):
    """Three stable actual samples; invalid identity is terminal, partial readiness retries."""
    last = None
    stable = 0
    observations = []
    while time.monotonic() < deadline:
        sample = read(deadline)
        if time.monotonic() >= deadline:
            raise TimeoutError('installed strip observation exceeded absolute deadline')
        if len(observations) >= 256 or len(json.dumps(sample).encode()) > 65536:
            raise RuntimeError('installed observation count/byte bound exceeded')
        observations.append(sample)
        try:
            found = predicate(sample, original, previous)
            signature = json.dumps(found, sort_keys=True)
        except NotReady:
            stable, last = 0, None
        else:
            stable = stable + 1 if signature == last else 1
            last = signature
            if stable >= 3:
                if time.monotonic() >= deadline:
                    raise TimeoutError('installed strip validation exceeded absolute deadline')
                return sample, observations
        time.sleep(min(.05, max(0, deadline - time.monotonic())))
    raise TimeoutError('installed strip endpoint did not become ready before deadline')


def prove_installed_strip(qmp, agent, reconnect, out, image, read_ppm, record, *, edge=False):
    """Run actual installed input. CI image admission is separate from signed release admission."""
    import hashlib
    import uuid
    from pathlib import Path
    import re
    if type(edge) is not bool:
        raise RuntimeError('genuine explicit installed edge flag missing')
    required = ('commit', 'image_id', 'base_image', 'package_sha256', 'member_sha256')
    if image.get('schema') != 1 or any(key not in image for key in required):
        raise RuntimeError('actual selected image/package identity receipt missing')
    if not re.fullmatch('[0-9a-f]{40}', image['commit']) or not re.fullmatch('[0-9a-f]{64}', image['package_sha256']):
        raise RuntimeError('invalid exact image source/package identity')
    if not re.fullmatch('(sha256:)?[0-9a-f]{64}', image['image_id']) or '@sha256:' not in image['base_image']:
        raise RuntimeError('immutable image/base identity missing')
    root = Path(__file__).resolve().parents[2]
    for name in ('tuna-vm-input', 'tuna-vm-lifecycle'):
        local = root / 'packaging/marlin/vm-lane' / name
        if image['member_sha256'].get('/usr/libexec/' + name) != hashlib.sha256(local.read_bytes()).hexdigest():
            raise RuntimeError('installed fixture/observer differs from exact executing source')
    destination = Path(out) / 'strip-acceptance'
    destination.mkdir(mode=0o700, exist_ok=False)
    nonce = uuid.uuid4().hex
    deadline = time.monotonic() + 120
    transport = QmpDeadline(qmp, deadline)
    receipt = {'schema': 1, 'nonce': nonce, 'image': image, 'completed': False,
               'cleanup_confirmed': False, 'installed_signed_release_qualified': False,
               'native_window_kernel_pid_join_qualified': False, 'cases': [], 'stationary_edge_requested': edge}
    original = None
    started = False
    primary = None

    def guest(action, *args, budget=12):
        remaining = min(budget, deadline - time.monotonic())
        if remaining <= 0:
            raise TimeoutError('installed strip phase absolute deadline')
        return agent.run(action, *args, timeout=remaining)

    def read(sample_deadline):
        remaining = min(12, sample_deadline - time.monotonic(), deadline - time.monotonic())
        if remaining <= 0:
            raise TimeoutError('installed sample absolute deadline')
        return agent.run('strip-state', nonce, timeout=remaining)

    def ready(mode, previous=None, extra=lambda _sample, _found: True):
        def predicate(sample, owner, old):
            found = endpoint(sample, owner, mode=mode, previous=old, edge=edge)
            if not extra(sample, found):
                raise NotReady('specific actual input endpoint absent')
            return {role: {'id': item['id'], 'rect': item['rect']} for role, item in found.items()}
        return await_endpoint(read, original, predicate, deadline=min(deadline, time.monotonic() + 15), previous=previous)[0]

    def move(x, y):
        if not (0 <= x < 1280 and 0 <= y < 800):
            raise RuntimeError('actual input point is outside the admitted viewport')
        transport.command('input-send-event', events=[
            {'type': 'abs', 'data': {'axis': 'x', 'value': int(x * 32767 / 1280)}},
            {'type': 'abs', 'data': {'axis': 'y', 'value': int(y * 32767 / 800)}}])

    def button(down, name='left'):
        transport.command('input-send-event', events=[{'type': 'btn', 'data': {'button': name, 'down': down}}])

    def chord(*keys):
        transport.command('send-key', keys=[{'type': 'qcode', 'data': key} for key in keys], **{'hold-time': 80})

    def capture(label, sample, role='alpha'):
        found = windows(sample, original, edge=edge)
        png = destination / (label + '.png')
        ppm = destination / (label + '.ppm')
        if png.exists() or ppm.exists():
            raise RuntimeError('refuse reused framebuffer capture path')
        transport.command('screendump', filename=str(png), format='png')
        transport.command('screendump', filename=str(ppm), format='ppm')
        actual_image = read_ppm(str(ppm))
        if actual_image[:2] != (1280, 800):
            raise RuntimeError('actual framebuffer dimensions differ from admitted viewport')
        colored_frame(actual_image, found[role]['rect'], COLORS[role])
        # QMP PNG and PPM are separate acquisitions. Validate both actual
        # images; an earlier unvalidated PNG is not a qualified walkthrough.
        from PIL import Image
        with Image.open(png) as frame:
            if frame.format != 'PNG' or frame.size != (1280, 800):
                raise RuntimeError('actual PNG framebuffer format/dimensions differ')
            png_image = frame.convert('RGB')
            colored_frame((png_image.width, png_image.height, png_image.tobytes()),
                          found[role]['rect'], COLORS[role])
        after = read(deadline)
        if time.monotonic() >= deadline:
            raise TimeoutError('installed framebuffer validation exceeded absolute deadline')
        if windows(after, original, edge=edge) != found:
            raise RuntimeError('actual window geometry changed across framebuffer acquisition')
        entry = {'label': label, 'sample': sample, 'after': after,
                 'png_sha256': hashlib.sha256(png.read_bytes()).hexdigest(),
                 'ppm_sha256': hashlib.sha256(ppm.read_bytes()).hexdigest(),
                 'capture_completed_monotonic_ns': time.monotonic_ns(),
                 'png_rgb_verified': True, 'ppm_rgb_verified': True,
                 'same_acquisition_qualified': False, 'presented_latency_qualified': False}
        receipt['cases'].append(entry)
        return after

    try:
        # Launch uncertainty exists before QGA send; a lost reply is never
        # represented as an absent fixture or successful cleanup.
        started = True
        admitted = guest('strip-edge-start' if edge else 'strip-start', nonce)
        original = admitted['owner']
        for name, identity in original['core'].items():
            if identity['exe_sha256'] != image['member_sha256'].get('/usr/bin/' + name):
                raise RuntimeError('running native executable differs from selected installed image')
        first = ready('gnome')
        found = windows(first, original, edge=edge)
        alpha = found['alpha']['rect']
        move(alpha[0] + 12, alpha[1] + 20)
        button(True)
        button(False)
        first = ready('gnome', extra=lambda sample, found: sample['inventory']['state']['focused'] == found['alpha']['id'])
        initial_ids = {w['id']: w['workspace'] for w in first['inventory']['state']['windows']}
        initial_rects = {w['id']: w['rect'] for w in first['inventory']['state']['windows']}
        capture('01-floating', first)
        chord('meta_l', 'shift', 't')
        strip = ready('scroll', first)
        if {w['id']: w['workspace'] for w in strip['inventory']['state']['windows']} != initial_ids:
            raise RuntimeError('mode toggle changed actual window/workspace identities')
        capture('02-strip-enabled', strip)
        # Actual preset input must resize this same client's allocated/painted buffer.
        before_width = windows(strip, original, edge=edge)['alpha']['rect'][2]
        chord('meta_l', 'r')
        preset = ready('scroll', strip, extra=lambda sample, found: found['alpha']['rect'][2] != before_width)
        capture('03-preset-width', preset)
        # Navigation must reach another actual native window and come back.
        chord('meta_l', 'right')
        neighbor = ready('scroll', preset, extra=lambda sample, found: sample['inventory']['state']['focused'] == found['beta']['id'])
        capture('04-navigation-beta', neighbor, 'beta')
        chord('meta_l', 'left')
        owner = ready('scroll', neighbor, extra=lambda sample, found: sample['inventory']['state']['focused'] == found['alpha']['id'])
        # Real GTK bottom-right grip requests xdg resize. Keep target within
        # the viewport; choose a shrink if the current preset reaches the edge.
        rect = windows(owner, original, edge=edge)['alpha']['rect']
        x, y = rect[0] + rect[2] - 6, rect[1] + rect[3] - 6
        delta = 53 if x + 53 < 1270 else -53
        move(x, y)
        button(True)
        # Genuine initial movement also crosses GTK's gesture threshold.
        move(x + (12 if delta > 0 else -12), y)
        pressed = ready('scroll', extra=lambda sample, found: sample['fixture']['windows']['alpha']['resize_requests'] > owner['fixture']['windows']['alpha']['resize_requests'])
        move(x + delta, y)
        resized = ready('scroll', pressed, extra=lambda sample, found: found['alpha']['rect'][2] == rect[2] + delta)
        button(False)
        resized = ready('scroll', extra=lambda sample, found: found['alpha']['rect'][2] == rect[2] + delta)
        capture('05-pointer-width', resized)
        # Reorder via actual custom titlebar Gdk begin_move. Select the
        # current visible Beta center, using the real observed slot geometry.
        current = windows(resized, original, edge=edge)
        alpha, beta = current['alpha']['rect'], current['beta']['rect']
        origin_x, origin_y = alpha[0] + 24, alpha[1] + 20
        target_x = min(1260, beta[0] + beta[2] // 2 + 30)
        move(origin_x, origin_y)
        button(True)
        move(origin_x + 12, origin_y)
        dragging = ready('scroll', extra=lambda sample, found: (sample['fixture']['windows']['alpha']['move_requests'] > resized['fixture']['windows']['alpha']['move_requests'] and (sample['inventory']['state']['tile_preview'] or {}).get('window') == found['alpha']['id']))
        move(target_x, origin_y)
        # A real second button must not replace the initiating owner.
        button(True, 'right')
        button(False, 'right')
        still_owned = ready('scroll', extra=lambda sample, found: (sample['inventory']['state']['tile_preview'] or {}).get('window') == found['alpha']['id'])
        button(False)
        reordered = ready('scroll', still_owned, extra=lambda sample, found: (sample['inventory']['state']['tile_preview'] is None and found['alpha']['rect'][0] > found['beta']['rect'][0]))
        capture('06-pointer-reorder', reordered)
        # The actual libseat PauseSession callback must cancel, not commit,
        # a held titlebar move when switching VT and returning.
        before_cancel = windows(reordered, original, edge=edge)
        rect = before_cancel['alpha']['rect']
        beta_rect = before_cancel['beta']['rect']
        # Grab near the visible right side, so an actual in-viewport motion
        # can move the actor center left of Beta's center even for a wide A.
        grab_x = min(1250, rect[0] + rect[2] - 24)
        if not rect[0] < grab_x < rect[0] + rect[2]:
            raise RuntimeError('actual cancellation title target is not visible')
        grab_offset = grab_x - rect[0]
        target_x = max(20, min(1260, beta_rect[0] + beta_rect[2] // 2 - 48 - rect[2] // 2 + grab_offset))
        move(grab_x, rect[1] + 20)
        button(True)
        move(grab_x - 12, rect[1] + 20)
        ready('scroll', extra=lambda sample, found: (sample['inventory']['state']['tile_preview'] or {}).get('window') == found['alpha']['id'])
        move(target_x, rect[1] + 20)
        def before_beta(sample, found):
            preview = sample['inventory']['state']['tile_preview'] or {}
            bar = preview.get('rect', [])
            return (preview.get('window') == found['alpha']['id'] and len(bar) == 4
                    and bar[2] == 8 and bar[0] == found['beta']['rect'][0] - 4)
        held = ready('scroll', extra=before_beta)
        # This observed insertion would change [Beta, A, ...] on release.
        # Cancellation must instead preserve every original canonical rectangle.
        if before_cancel['alpha']['rect'][0] <= before_cancel['beta']['rect'][0]:
            raise RuntimeError('cancel control did not start after Beta')
        vt = int(held['inventory']['session']['VTNr'])
        away = 2 if vt != 2 else 3
        if guest('vt', away)['active_vt'] != f'tty{away}':
            raise RuntimeError('actual VT did not leave original session')
        if guest('vt', vt)['active_vt'] != f'tty{vt}':
            raise RuntimeError('actual VT did not return to original session')
        returned = ready('scroll', extra=lambda sample, found: sample['inventory']['state']['tile_preview'] is None)
        if {role: item['rect'] for role, item in windows(returned, original, edge=edge).items()} != {role: item['rect'] for role, item in before_cancel.items()}:
            raise RuntimeError('VT cancellation committed a column move')
        button(False)
        capture('07-vt-cancel', returned)
        if edge:
            before_edge = returned
            before_found = windows(before_edge, original, edge=True)
            rect = before_found['alpha']['rect']
            move(rect[0] + 24, rect[1] + 20)
            button(True)
            move(rect[0] + 36, rect[1] + 20)
            # ONE movement into the band; the following observer has no input
            # capability and cannot repeatedly drive the viewport itself.
            move(1279, rect[1] + 20)
            terminal, observations = await_stationary_edge(
                read, original, before_edge, deadline=min(deadline, time.monotonic() + 60))
            receipt['stationary_edge'] = {'observations': observations,
                'stationary_input_verified': True, 'sampled_not_every_frame': True,
                'presented_latency_qualified': False}
            capture('08-stationary-delta-revealed', terminal, 'delta')
            final = windows(terminal, original, edge=True)
            bar = terminal['inventory']['state']['tile_preview']['rect']
            delta_rect = final['delta']['rect']
            if bar[0] != delta_rect[0] + delta_rect[2] + 16 - 4:
                raise RuntimeError('actual stationary insertion is not after Delta')
            button(False)
            returned = ready('scroll', terminal, extra=lambda sample, found:
                sample['inventory']['state']['tile_preview'] is None and
                found['alpha']['rect'][0] > found['delta']['rect'][0])
            capture('09-stationary-matching-release', returned)
        chord('meta_l', 'shift', 't')
        restored = ready('gnome', returned)
        if {w['id']: w['workspace'] for w in restored['inventory']['state']['windows']} != initial_ids:
            raise RuntimeError('mode restore changed actual native identities')
        if {w['id']: w['rect'] for w in restored['inventory']['state']['windows']} != initial_rects:
            raise RuntimeError('mode restore lost independent floating rectangles')
        capture('10-floating-restored' if edge else '08-floating-restored', restored)
        if time.monotonic() >= deadline:
            raise TimeoutError('installed strip completion exceeded absolute deadline')
        receipt['completed'] = True
    except BaseException as error:
        primary = error
        receipt['error_type'] = type(error).__name__
    finally:
        if started:
            cleanup_agent = None
            try:
                # Always use an independently synchronized cleanup stream,
                # never reuse a timed-out stream's buffered replies.
                cleanup_agent = reconnect()
                cleanup = cleanup_agent.run('strip-stop', nonce, timeout=10)
                receipt['cleanup'] = cleanup
                receipt['cleanup_confirmed'] = cleanup.get('cleanup_confirmed') is True
                if not receipt['cleanup_confirmed']:
                    raise RuntimeError('owned fixture cleanup unconfirmed')
            except BaseException as error:
                receipt['completed'] = False
                receipt['cleanup_error_type'] = type(error).__name__
                if primary is None:
                    primary = error
            finally:
                if cleanup_agent is not None:
                    cleanup_agent.close()
        (destination / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    if primary is not None:
        record('V-STRIP-INSTALLED', False, 'actual installed strip input/identity/pixels/cleanup failed; receipt retained')
        raise primary
    record('V-STRIP-INSTALLED', True, f"{len(receipt['cases'])} actual installed strip frames, same native identities, mode restore and exact owned fixture cleanup")
    if edge:
        record('V-STRIP-EDGE-STATIONARY', True, 'one real edge motion, unchanged actual pointer/counters, live viewport/new frames, terminal Delta PNG/PPM and matching release')
    return receipt
