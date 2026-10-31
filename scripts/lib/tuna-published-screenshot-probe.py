#!/usr/bin/env python3
"""Persistent native portal client, run as the installed VM's fixture user.

This is only a test requester. It does not implement/replace a portal, spoof an
app ID, bypass consent, or import desktop candidate code into the guest.
"""
import hashlib
import json
import os
from pathlib import Path
import struct
import sys
import urllib.parse

from gi.repository import Gio, GLib

result_path = Path(sys.argv[1])
loop = GLib.MainLoop()
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
request_path = None
finished = False


def finish(result):
    global finished
    if finished:
        return
    finished = True
    result_path.write_text(json.dumps(result, indent=2) + '\n')
    loop.quit()


def response(connection, sender, path, interface, signal, parameters, *_user_data):
    if path != request_path:
        return
    code, results = parameters.unpack()
    result = {'response': code, 'results': results}
    if code == 0:
        try:
            uri = urllib.parse.urlparse(results['uri'])
            if uri.scheme != 'file' or uri.netloc not in ('', 'localhost'):
                raise ValueError('Screenshot did not return a local file URI')
            image = Path(urllib.parse.unquote(uri.path))
            if image.stat().st_size > 16 * 1024 * 1024:
                raise ValueError('Screenshot exceeds 16 MiB evidence bound')
            data = image.read_bytes()
            if len(data) < 24 or data[:8] != b'\x89PNG\r\n\x1a\n':
                raise ValueError('Screenshot URI is not a PNG')
            result.update(path=str(image), sha256=hashlib.sha256(data).hexdigest(),
                          dimensions=list(struct.unpack('>II', data[16:24])), bytes=len(data))
        except Exception as error:
            result['error'] = f'{type(error).__name__}: {error}'
    finish(result)


subscription = bus.signal_subscribe(
    None, 'org.freedesktop.portal.Request', 'Response', None, None,
    Gio.DBusSignalFlags.NONE, response)
try:
    options = {'interactive': GLib.Variant('b', False),
               'handle_token': GLib.Variant('s', f'tuna_variant_{os.getpid()}')}
    request_path = bus.call_sync(
        'org.freedesktop.portal.Desktop', '/org/freedesktop/portal/desktop',
        'org.freedesktop.portal.Screenshot', 'Screenshot',
        GLib.Variant('(sa{sv})', ('', options)), GLib.VariantType.new('(o)'),
        Gio.DBusCallFlags.NONE, 30000, None).unpack()[0]
    GLib.timeout_add_seconds(90, lambda: finish({'error': 'portal Response timed out', 'request': request_path}))
    loop.run()
except Exception as error:
    finish({'error': f'{type(error).__name__}: {error}'})
finally:
    bus.signal_unsubscribe(subscription)
