#!/usr/bin/python3
"""Supervised shell-role consumer for tuna-color-capture-proof.

Uses production caller admission: this exact PID is launched with --shell-bin.
It never acquires a portal identity or changes compositor capture policy.
Mutter 51 meta-stream-source.c:add_format_param/update_color_state define the
SDR BT.709/gamma-2.2 target checked at the real PipeWire appsink boundary.
The SDR consumer requests raw BGRx without supplying colourimetry; negotiated
producer metadata and actual pixels remain mandatory. Failed GStreamer state
changes retain bounded bus error/debug and actual source caps before teardown.
"""
import hashlib
import json
import os
from pathlib import Path
import queue
import socket
import struct
import subprocess
import time
import threading

import gi
gi.require_version('Gst', '1.0')
gi.require_version('GstVideo', '1.0')
gi.require_version('GstApp', '1.0')
from gi.repository import Gio, GLib, Gst, GstVideo, GstApp
from PIL import Image

ART = Path(os.environ['TUNA_COLOR_CAPTURE_ARTIFACTS'])
BUS = Gio.bus_get_sync(Gio.BusType.SESSION, None)
Gst.init(None)
CONTROL = None
CONTROL_EVENTS = queue.Queue()
CONTROL_COMMAND_ID = 0


def publish(name, value):
    path = ART / name
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def call(destination, path, interface, method, signature=None, args=()):
    parameters = GLib.Variant(signature, args) if signature else None
    return BUS.call_sync(destination, path, interface, method, parameters, None,
                         Gio.DBusCallFlags.NO_AUTO_START, 10000, None).unpack()


def wait(predicate, label, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        while GLib.MainContext.default().pending():
            GLib.MainContext.default().iteration(False)
        value = predicate()
        if value:
            return value
        time.sleep(.01)
    raise RuntimeError('timed out: ' + label)


def match_pixel(got, expected):
    assert all(abs(a - b) <= 2 for a, b in zip(got, expected)), (got, expected)


def png_evidence(path, expected):
    # Decode actual production PNG data, and inspect actual chunks rather
    # than trusting a filename, source-code substring or reported colours.
    data = path.read_bytes()
    assert data[:8] == b'\x89PNG\r\n\x1a\n'
    chunks = {}
    offset = 8
    while offset < len(data):
        length = struct.unpack_from('>I', data, offset)[0]
        kind = data[offset + 4:offset + 8]
        chunks[kind] = data[offset + 8:offset + 8 + length]
        offset += length + 12
    assert struct.unpack('>I', chunks[b'gAMA']) == (45455,)
    assert struct.unpack('>8I', chunks[b'cHRM']) == (
        31270, 32900, 64000, 33000, 30000, 60000, 15000, 6000)
    assert not any(kind in chunks for kind in (b'sRGB', b'iCCP', b'cICP', b'mDCV', b'cLLI'))
    with Image.open(path) as image:
        assert image.mode == 'RGBA', image.mode
        got = list(image.getpixel((image.width // 2, image.height // 2))[:3])
        match_pixel(got, expected)
        size = list(image.size)
    return {'path': str(path), 'sha256': hashlib.sha256(data).hexdigest(),
            'size': size, 'centre_rgb': got, 'gamma_scaled': 45455,
            'chromaticities_scaled': list(struct.unpack('>8I', chunks[b'cHRM']))}


def capture_pipeline_failure(pipeline, prefix, target, node, phase):
    # Read the real bus before NULL flushes it. Bound both the wait and the
    # retained messages; a state-change return alone loses the plugin's cause.
    result, current, pending = pipeline.get_state(0)
    source = pipeline.get_by_name('producer')
    pad = source.get_static_pad('src')
    current_caps = pad.get_current_caps()
    queried_caps = pad.query_caps(None)
    evidence = {
        'node': node, 'target': target, 'phase': phase,
        'state_result': result.value_nick, 'current_state': current.value_nick,
        'pending_state': pending.value_nick,
        'source_path': source.get_property('path'),
        'source_fd': source.get_property('fd'),
        'source_current_caps': current_caps.to_string()[:4096] if current_caps else None,
        'source_query_caps': queried_caps.to_string()[:4096] if queried_caps else None,
        'messages': [],
    }
    bus = pipeline.get_bus()
    kinds = Gst.MessageType.ERROR | Gst.MessageType.WARNING | Gst.MessageType.EOS
    message = bus.timed_pop_filtered(Gst.SECOND, kinds)
    for _ in range(8):
        if message is None:
            break
        entry = {
            'type': Gst.MessageType.get_name(message.type),
            'source': message.src.get_path_string()[:1024] if message.src else None,
        }
        if message.type in (Gst.MessageType.ERROR, Gst.MessageType.WARNING):
            error, debug = (message.parse_error() if message.type == Gst.MessageType.ERROR
                            else message.parse_warning())
            entry.update(domain=error.domain, code=error.code,
                         message=error.message[:2048], debug=(debug or '')[:4096])
        evidence['messages'].append(entry)
        message = bus.pop_filtered(kinds)
    if not evidence['messages']:
        evidence['bus_diagnostic'] = 'no error, warning or EOS queued within one second'
    publish(f'{prefix}-cast-{target}-gstreamer-error.json', evidence)
    return RuntimeError('PipeWire capture failed: ' + json.dumps(evidence))


def capture_stream(prefix, target, connector, window, expected, hdr=False):
    destination = 'org.gnome.Mutter.ScreenCast'
    session_interface = destination + '.Session'
    session, = call(destination, '/org/gnome/Mutter/ScreenCast', destination,
                    'CreateSession', '(a{sv})', ({},))
    pipeline = None
    subscription = None
    try:
        if target == 'window':
            stream, = call(destination, session, session_interface, 'RecordWindow',
                           '(a{sv})', ({'window-id': GLib.Variant('t', window)},))
        elif target == 'area':
            stream, = call(destination, session, session_interface, 'RecordArea',
                           '(iiiia{sv})', (320, 200, 640, 400, {}))
        else:
            stream, = call(destination, session, session_interface, 'RecordMonitor',
                           '(sa{sv})', (connector, {}))
        nodes = []

        def added(_bus, _sender, _path, _interface, _signal, parameters):
            nodes.append(parameters.unpack()[0])

        subscription = BUS.signal_subscribe(
            None, destination + '.Stream', 'PipeWireStreamAdded', stream, None,
            Gio.DBusSignalFlags.NONE, added)
        call(destination, session, session_interface, 'Start')
        node = wait(lambda: nodes[0] if nodes else None, 'PipeWire node')
        if hdr:
            assert target == 'monitor', 'Mutter 51 HDR offer policy is monitor-only'
            raw_path = ART / f'{prefix}-cast-monitor.xb30'
            result = subprocess.run(
                [os.environ['TUNA_HDR_CAPTURE_CONSUMER'], str(node), str(raw_path)],
                capture_output=True, text=True, timeout=20)
            (ART / f'{prefix}-hdr-consumer.log').write_text(result.stdout + result.stderr)
            assert result.returncode == 0, result.stderr
            evidence = json.loads(result.stdout)
            assert evidence['color_range'] == 1 and evidence['color_matrix'] == 1, evidence
            assert evidence['color_primaries'] == 7 and evidence['transfer_function'] == 14, evidence
            assert evidence['format'] == 'xBGR_210LE', evidence
            raw = raw_path.read_bytes()
            centre = ((evidence['height'] // 2) * evidence['stride']
                      + (evidence['width'] // 2) * 4)
            word = struct.unpack_from('<I', raw, centre)[0]
            actual = [word & 1023, (word >> 10) & 1023, (word >> 20) & 1023]
            assert actual == evidence['centre_rgb10'], evidence
            assert all(abs(a - b) <= 4 for a, b in zip(actual, expected)), (actual, expected)
            return {**evidence, 'node': node, 'expected_rgb10': expected,
                    'raw_sha256': hashlib.sha256(raw).hexdigest(),
                    'consumer_stderr': result.stderr}
        # Direct Mutter ScreenCast supplies a node on the session remote,
        # not a portal OpenPipeWireRemote FD; pipewiresrc's default fd=-1
        # connects to that same inherited private PipeWire session.
        # Both bare pipewiresrc and appsink advertise ANY. PipeWire's
        # gst_caps_to_format_all(ANY) emits no EnumFormat, so the stream
        # gets media.class=Stream/Input/Unknown and WirePlumber's media-type
        # check rejects the Video target. Request the actual SDR format,
        # but never inject colourimetry or use a conversion/filter to fix it.
        pipeline = Gst.parse_launch(
            f'pipewiresrc name=producer path={node} num-buffers=1 ! '
            'video/x-raw,format=BGRx ! '
            'appsink name=capture sync=false max-buffers=1 drop=false')
        deadline = time.monotonic() + 15
        if pipeline.set_state(Gst.State.PLAYING) == Gst.StateChangeReturn.FAILURE:
            raise capture_pipeline_failure(pipeline, prefix, target, node, 'set-playing')
        result, current, _ = pipeline.get_state(
            max(0, int((deadline - time.monotonic()) * Gst.SECOND)))
        if result == Gst.StateChangeReturn.FAILURE or current != Gst.State.PLAYING:
            raise capture_pipeline_failure(pipeline, prefix, target, node, 'wait-playing')
        sink = pipeline.get_by_name('capture')
        sample = sink.emit('try-pull-sample',
                           max(0, int((deadline - time.monotonic()) * Gst.SECOND)))
        if sample is None:
            raise capture_pipeline_failure(pipeline, prefix, target, node, 'pull-sample')
        caps = sample.get_caps()
        info = GstVideo.VideoInfo.new_from_caps(caps)
        assert caps.get_structure(0).get_string('format') == 'BGRx', caps.to_string()
        # GST_VIDEO_COLOR_RANGE_0_255 is 1 (video-color.h); using its
        # value avoids Python GI's leading-digit enum-name differences.
        assert int(info.colorimetry.range) == 1, caps.to_string()
        assert info.colorimetry.matrix == GstVideo.VideoColorMatrix.RGB, caps.to_string()
        assert info.colorimetry.transfer == GstVideo.VideoTransferFunction.GAMMA22, caps.to_string()
        assert info.colorimetry.primaries == GstVideo.VideoColorPrimaries.BT709, caps.to_string()
        buffer = sample.get_buffer()
        meta = GstVideo.buffer_get_video_meta(buffer)
        stride = meta.stride[0] if meta else info.stride[0]
        offset = meta.offset[0] if meta else info.offset[0]
        mapped, mapping = buffer.map(Gst.MapFlags.READ)
        assert mapped, 'PipeWire buffer is not readable'
        try:
            raw = bytes(mapping.data)
            at = offset + (info.height // 2) * stride + (info.width // 2) * 4
            b, g, r = raw[at:at + 3]
            got = [r, g, b]
            match_pixel(got, expected)
            raw_path = ART / f'{prefix}-cast-{target}.bgrx'
            raw_path.write_bytes(raw)
            # This is only a viewing aid. Production colour metadata is
            # recorded from the negotiated caps, not invented for this PNG.
            preview = ART / f'{prefix}-cast-{target}-preview.png'
            Image.frombytes('RGB', (info.width, info.height), raw[offset:],
                            'raw', 'BGRX', stride).save(preview)
        finally:
            buffer.unmap(mapping)
        return {'node': node, 'target': target, 'caps': caps.to_string(),
                'centre_rgb': got, 'width': info.width, 'height': info.height,
                'stride': stride, 'offset': offset, 'raw_path': str(raw_path),
                'raw_sha256': hashlib.sha256(raw).hexdigest(),
                'preview_only': str(preview), 'full_range_rgb_bt709_gamma22': True}
    finally:
        if pipeline is not None:
            pipeline.set_state(Gst.State.NULL)
            pipeline.get_state(3 * Gst.SECOND)
        if subscription is not None:
            BUS.signal_unsubscribe(subscription)
        call(destination, session, session_interface, 'Stop')


def control_codec(*arguments, frame=None):
    return subprocess.check_output(
        [os.environ['TUNA_HDR_CAPTURE_CONSUMER'], *arguments],
        input=frame, timeout=5)


def control_event(predicate, label):
    def received():
        while True:
            try:
                event = CONTROL_EVENTS.get_nowait()
            except queue.Empty:
                return None
            assert event['kind'] != 'error', event
            if predicate(event):
                return event
    return wait(received, label)


def connect_control():
    global CONTROL
    CONTROL = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    CONTROL.connect(os.environ['TUNA_CONTROL_SOCKET'])
    CONTROL.sendall(control_codec('--shell-hello'))

    def exact(length):
        result = bytearray()
        while len(result) < length:
            block = CONTROL.recv(length - len(result))
            if not block:
                raise RuntimeError('supervised control socket closed')
            result.extend(block)
        return bytes(result)

    def reader():
        try:
            while True:
                prefix = exact(4)
                length = struct.unpack('<I', prefix)[0]
                # Transport bound from canonical MAX_FRAME_BYTES. All
                # message variants/tags are decoded by the Rust codec.
                assert length <= 1024 * 1024
                event = json.loads(control_codec('--inspect-shell-frame', frame=prefix + exact(length)))
                CONTROL_EVENTS.put(event)
        except Exception as error:
            CONTROL_EVENTS.put({'kind': 'error', 'error': repr(error)})

    threading.Thread(target=reader, daemon=True).start()
    hello = control_event(lambda event: event['kind'] == 'hello', 'canonical control Hello')
    snapshot = control_event(lambda event: event['kind'] == 'snapshot', 'canonical control Snapshot')
    assert not snapshot['locked'], snapshot
    return hello


def configure_hdr(request):
    global CONTROL_COMMAND_ID
    destination = 'org.gnome.Mutter.DisplayConfig'
    path = '/org/gnome/Mutter/DisplayConfig'
    serial, monitors, logical, properties = call(destination, path, destination, 'GetCurrentState')
    assert len(monitors) == 1 and len(logical) == 1, (monitors, logical)
    spec, modes, monitor_properties = monitors[0]
    if request['action'] == 'observe-mode':
        assert monitor_properties['color-mode'] == request['expected_mode'], monitor_properties
        return {'connector': spec[0], 'observed_monitor': monitor_properties,
                'observed_without_apply': True}
    supported = monitor_properties['supported-color-modes']
    expect_rejection = request['action'] == 'reject-hdr'
    desired_mode = 0 if request['action'] == 'disable-hdr' else 1
    assert (1 not in supported) if expect_rejection else (desired_mode in supported), supported
    current = next(mode for mode in modes if mode[6].get('is-current'))
    x, y, scale, transform, primary, _, _ = logical[0]
    configuration = [(x, y, scale, transform, primary,
                      [(spec[0], current[0], {'color-mode': GLib.Variant('u', desired_mode)})])]
    options = {}
    if 'layout-mode' in properties:
        options['layout-mode'] = GLib.Variant('u', properties['layout-mode'])
    try:
        call(destination, path, destination, 'ApplyMonitorsConfig',
             '(uua(iiduba(ssa{sv}))a{sv})',
             (serial, 2 if request.get('persistent') else 1, configuration, options))
    except GLib.Error as error:
        if not expect_rejection or 'not supported' not in error.message:
            raise
        return {'connector': spec[0], 'unsupported_hdr_rejected': True,
                'supported_color_modes': supported, 'error': error.message}
    assert not expect_rejection, 'unsupported HDR mode was accepted'
    confirmation = None
    if request.get('persistent'):
        assert CONTROL is not None, 'persistent changes require the real supervised shell connection'
        shown = control_event(lambda event: event['kind'] == 'confirm-display', 'actual display confirmation')
        CONTROL_COMMAND_ID += 1
        CONTROL.sendall(control_codec('--keep-display-change', str(CONTROL_COMMAND_ID)))
        result = control_event(
            lambda event: event['kind'] == 'command-result' and event['id'] == CONTROL_COMMAND_ID,
            'actual keep-display-change result')
        assert result['applied'], result
        confirmation = {'shown': shown, 'result': result, 'supervised_pid': os.getpid()}

    def active():
        _, current_monitors, _, _ = call(destination, path, destination, 'GetCurrentState')
        monitor = current_monitors[0]
        return monitor[2] if monitor[2]['color-mode'] == desired_mode else None

    return {'connector': spec[0], 'active_monitor': wait(active, 'active admitted colour mode'),
            'supported_color_modes': supported, 'confirmation': confirmation}


def capture(request):
    if request.get('action') == 'observe-outputs':
        serial, monitors, logical, properties = call(
            'org.gnome.Mutter.DisplayConfig', '/org/gnome/Mutter/DisplayConfig',
            'org.gnome.Mutter.DisplayConfig', 'GetCurrentState')
        return {'serial': serial, 'monitors': monitors, 'logical': logical,
                'properties': properties, 'observed_without_apply': True}
    if request.get('action') in ('enable-hdr', 'disable-hdr', 'reject-hdr', 'observe-mode'):
        return configure_hdr(request)
    prefix, expected = request['id'], request['expected']
    _, monitors, _, _ = call(
        'org.gnome.Mutter.DisplayConfig', '/org/gnome/Mutter/DisplayConfig',
        'org.gnome.Mutter.DisplayConfig', 'GetCurrentState')
    assert len(monitors) == 1, monitors
    connector = monitors[0][0][0]

    def selected_window():
        windows, = call('org.gnome.Shell.Introspect', '/org/gnome/Shell/Introspect',
                        'org.gnome.Shell.Introspect', 'GetWindows')
        selected = [key for key, value in windows.items()
                    if value.get('app-id', '').startswith('tuna-color-')
                    and value.get('has-focus')]
        return selected[0] if len(selected) == 1 else None

    window = wait(selected_window, 'focused colour proof window')
    evidence = {'id': prefix, 'expected_rgb': expected, 'window': window,
                'screenshots': {}, 'casts': {}}
    for kind, method, signature, flags in (
            ('monitor', 'Screenshot', '(bbs)', (False, False)),
            ('window', 'ScreenshotWindow', '(bbbs)', (False, False, False))):
        path = ART / f'{prefix}-screenshot-{kind}.png'
        ok, used = call('org.gnome.Shell.Screenshot', '/org/gnome/Shell/Screenshot',
                        'org.gnome.Shell.Screenshot', method, signature,
                        (*flags, str(path)))
        assert ok and used == str(path), (ok, used)
        evidence['screenshots'][kind] = png_evidence(path, expected)
    for target in ('monitor', 'window', 'area'):
        evidence['casts'][target] = capture_stream(
            prefix, target, connector, window, expected)
    if 'expected_hdr10' in request:
        evidence['casts']['hdr-monitor'] = capture_stream(
            prefix + '-hdr', 'monitor', connector, window, request['expected_hdr10'], hdr=True)
    return evidence


control_hello = connect_control() if os.environ.get('TUNA_HDR_CAPTURE_CONSUMER') else None
publish('consumer-ready.json', {'pid': os.getpid(), 'control_hello': control_hello})
seen = None
deadline = time.monotonic() + 300
while time.monotonic() < deadline:
    if (ART / 'stop-consumer').exists():
        break
    try:
        request = json.loads((ART / 'capture-request.json').read_text())
    except (OSError, ValueError):
        time.sleep(.01)
        continue
    if request['id'] == seen:
        time.sleep(.01)
        continue
    seen = request['id']
    try:
        publish('capture-response.json', {'id': seen, 'evidence': capture(request)})
    except Exception as error:
        publish('capture-response.json', {'id': seen, 'error': repr(error)})
        raise
    deadline = time.monotonic() + 300
if CONTROL is not None:
    CONTROL.close()
