#!/usr/bin/python3
"""Observe gnome-settings-daemon's gsd-media-keys against the GTK shell.

The GTK shell proof (G-GSD-MEDIA-KEYS) runs the distribution's real
gsd-media-keys on its private session bus. This helper is its bus-side
witness and the reader of the nested log's media diagnostics:

  monitor JSONL READY SECONDS
      Become a D-Bus monitor (org.freedesktop.DBus.Monitoring) and append
      one JSON row per org.gnome.Shell call/signal, MPRIS Player call and
      NameOwnerChanged, plus the replies/errors to recorded calls. Writes
      READY once monitoring; exits after SECONDS or on SIGTERM.
  grabs JSONL OWNER ACCEL... [--timeout S]
      Wait until OWNER (a unique name) holds a live, nonzero org.gnome.Shell
      grab for every ACCEL; print {accel: action} as JSON.
  osd JSONL OWNER SINCE_NS ICON... [--timeout S]
      Wait for a ShowOSD call from OWNER after SINCE_NS whose icon is one
      of ICON; print that icon.
  activations JSONL ACTION SINCE_NS
      Print how many AcceleratorActivated signals fired ACTION after SINCE_NS.
  forwarded LOG OFFSET ACTION KEYSYM [--timeout S]
      Wait for the shell's accelerators-send diagnostic (after byte OFFSET
      of the nested log) that hands grab ACTION of KEYSYM to the compositor.
  fired LOG OFFSET ACTION
      Print how many compositor media-accelerator-fired rows fired ACTION.
  native LOG OFFSET
      Print how many native shell media-action rows were logged.

Exit status 1 when a wait times out; the reason goes to stderr.
"""
import json
import sys
import threading
import time
from pathlib import Path

DIAGNOSTIC = 'tuna-media-diagnostic: '
SHELL_IFACE = 'org.gnome.Shell'
MPRIS_PLAYER = 'org.mpris.MediaPlayer2.Player'


def load_rows(path):
    """JSONL rows; a torn final line (still being written) is skipped."""
    try:
        text = Path(path).read_text(encoding='utf-8', errors='replace')
    except FileNotFoundError:
        return []
    rows = []
    for line in text.splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if isinstance(row, dict):
            rows.append(row)
    return rows


def _reply(rows, call):
    for row in rows:
        if (row.get('type') in ('return', 'error')
                and row.get('destination') == call.get('sender')
                and row.get('reply_serial') == call.get('serial')):
            return row
    return None


def live_grabs(rows, owner):
    """{accelerator: action} of OWNER's org.gnome.Shell grabs still held."""
    grabs = {}
    for row in rows:
        if (row.get('type') != 'call' or row.get('sender') != owner
                or row.get('interface') != SHELL_IFACE):
            continue
        member, body = row.get('member'), row.get('body') or []
        reply = _reply(rows, row)
        if member == 'GrabAccelerators':
            if not reply or reply.get('type') != 'return' or not body:
                continue
            ids = (reply.get('body') or [[]])[0]
            for (accel, *_), action in zip(body[0], ids):
                if action:
                    grabs[accel] = action
                else:
                    grabs.pop(accel, None)
        elif member == 'GrabAccelerator':
            if reply and reply.get('type') == 'return' and body:
                action = (reply.get('body') or [0])[0]
                if action:
                    grabs[body[0]] = action
        elif member in ('UngrabAccelerators', 'UngrabAccelerator'):
            released = body[0] if member == 'UngrabAccelerators' else body[:1]
            released = set(released or [])
            grabs = {a: i for a, i in grabs.items() if i not in released}
    return grabs


def osd_icons(rows, owner, since):
    """Icons of OWNER's ShowOSD calls at or after SINCE (monotonic ns)."""
    icons = []
    for row in rows:
        if (row.get('type') == 'call' and row.get('sender') == owner
                and row.get('interface') == SHELL_IFACE
                and row.get('member') == 'ShowOSD'
                and row.get('ns', -1) >= since):
            params = (row.get('body') or [{}])[0]
            if isinstance(params, dict):
                icons.append(params.get('icon'))
    return icons


def activations(rows, action, since):
    return sum(1 for row in rows
               if row.get('type') == 'signal'
               and row.get('interface') == SHELL_IFACE
               and row.get('member') == 'AcceleratorActivated'
               and row.get('ns', -1) >= since
               and (row.get('body') or [None])[0] == action)


def diagnostics(path, offset):
    """Media diagnostic rows of the nested log after byte OFFSET."""
    try:
        with open(path, 'rb') as log:
            log.seek(offset)
            text = log.read().decode('utf-8', errors='replace')
    except FileNotFoundError:
        return []
    rows = []
    for line in text.splitlines():
        start = line.find(DIAGNOSTIC)
        if start < 0:
            continue
        try:
            row = json.loads(line[start + len(DIAGNOSTIC):])
        except ValueError:
            continue
        if isinstance(row, dict):
            rows.append(row)
    return rows


def forwarded(rows, action, keysym):
    """The shell handed grab ACTION of KEYSYM to the compositor."""
    for row in rows:
        details = row.get('details') or {}
        if (row.get('actor') == 'shell' and row.get('stage') == 'accelerators-send'
                and details.get('write_ok') is True
                and any(isinstance(entry, list) and entry[:2] == [action, keysym]
                        for entry in details.get('media') or [])):
            return True
    return False


def fired(rows, action):
    return sum(1 for row in rows
               if row.get('actor') == 'compositor'
               and row.get('stage') == 'media-accelerator-fired'
               and (row.get('details') or {}).get('action') == action)


def native(rows):
    return sum(1 for row in rows
               if row.get('actor') == 'shell' and row.get('stage') == 'media-action')


def wait(probe, timeout):
    end = time.monotonic() + timeout
    while True:
        result = probe()
        if result or time.monotonic() >= end:
            return result
        time.sleep(0.2)


def _json_default(value):
    if isinstance(value, (bytes, bytearray)):
        return value.hex()
    return str(value)


def monitor(out, ready, seconds):
    import gi
    gi.require_version('Gio', '2.0')
    from gi.repository import Gio, GLib

    address = Gio.dbus_address_get_for_bus_sync(Gio.BusType.SESSION, None)
    conn = Gio.DBusConnection.new_for_address_sync(
        address,
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
        | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
        None, None)
    conn.set_exit_on_close(False)
    me = conn.get_unique_name()
    lock = threading.Lock()
    calls = set()
    sink = open(out, 'a', encoding='utf-8')
    kinds = {
        Gio.DBusMessageType.METHOD_CALL: 'call',
        Gio.DBusMessageType.METHOD_RETURN: 'return',
        Gio.DBusMessageType.ERROR: 'error',
        Gio.DBusMessageType.SIGNAL: 'signal',
    }

    def record(message):
        kind = kinds.get(message.get_message_type())
        if kind is None:
            return
        sender = message.get_sender()
        if kind == 'call':
            if message.get_interface() not in (SHELL_IFACE, MPRIS_PLAYER):
                return
            calls.add((sender, message.get_serial()))
        elif kind in ('return', 'error'):
            key = (message.get_destination(), message.get_reply_serial())
            if key not in calls:
                return
            calls.discard(key)
        elif not (message.get_interface() == SHELL_IFACE
                  or message.get_member() == 'NameOwnerChanged'):
            return
        body = message.get_body()
        row = {
            'ns': time.monotonic_ns(), 'type': kind,
            'serial': message.get_serial(), 'reply_serial': message.get_reply_serial(),
            'sender': sender, 'destination': message.get_destination(),
            'path': message.get_path(), 'interface': message.get_interface(),
            'member': message.get_member(),
            'error': message.get_error_name() if kind == 'error' else None,
            'body': list(body.unpack()) if body is not None else [],
        }
        sink.write(json.dumps(row, default=_json_default, separators=(',', ':')) + '\n')
        sink.flush()

    def on_message(_conn, message, incoming, *_):
        # Our own traffic (Hello, BecomeMonitor's reply, NameLost) goes on
        # to GDBus; every observed copy stops here, so a monitor connection
        # never answers a call addressed to someone else.
        if not incoming or message.get_destination() == me:
            return message
        with lock:
            try:
                record(message)
            except Exception as error:  # a bad body must not kill the witness
                print(f'gsd-media-keys-bus: unrecorded message: {error}', file=sys.stderr)
        return None

    conn.add_filter(on_message)
    rules = [
        f"type='method_call',interface='{SHELL_IFACE}'",
        f"type='signal',interface='{SHELL_IFACE}'",
        f"type='method_call',interface='{MPRIS_PLAYER}'",
        "type='method_return'",
        "type='error'",
        "type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged'",
    ]
    conn.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus',
                   'org.freedesktop.DBus.Monitoring', 'BecomeMonitor',
                   GLib.Variant('(asu)', (rules, 0)), None,
                   Gio.DBusCallFlags.NONE, 5000, None)
    Path(ready).write_text(f'monitoring {me}\n', encoding='utf-8')
    loop = GLib.MainLoop()
    GLib.timeout_add_seconds(seconds, loop.quit)
    # SIGTERM's default action is fine: every row is flushed as written.
    loop.run()
    with lock:
        sink.close()
    return 0


def _timeout(args):
    if '--timeout' in args:
        at = args.index('--timeout')
        value = float(args[at + 1])
        return args[:at] + args[at + 2:], value
    return args, 0.0


def main(argv):
    if not argv:
        print(__doc__, file=sys.stderr)
        return 2
    command, args = argv[0], argv[1:]
    args, timeout = _timeout(args)
    if command == 'monitor':
        return monitor(args[0], args[1], int(args[2]))
    if command == 'grabs':
        path, owner, wanted = args[0], args[1], args[2:]
        got = wait(lambda: (lambda g: g if all(a in g for a in wanted) else None)(
            live_grabs(load_rows(path), owner)), timeout)
        if not got:
            held = live_grabs(load_rows(path), owner)
            print(f'{owner} holds {json.dumps(held)}; missing '
                  f'{[a for a in wanted if a not in held]}', file=sys.stderr)
            return 1
        print(json.dumps({a: got[a] for a in wanted}, sort_keys=True))
        return 0
    if command == 'osd':
        path, owner, since, icons = args[0], args[1], int(args[2]), args[3:]
        hit = wait(lambda: next((i for i in osd_icons(load_rows(path), owner, since)
                                 if i in icons), None), timeout)
        if not hit:
            print(f'no ShowOSD {icons} from {owner}; saw '
                  f'{osd_icons(load_rows(path), owner, since)}', file=sys.stderr)
            return 1
        print(hit)
        return 0
    if command == 'activations':
        print(activations(load_rows(args[0]), int(args[1]), int(args[2])))
        return 0
    if command == 'forwarded':
        path, offset, action, keysym = args[0], int(args[1]), int(args[2]), int(args[3])
        if not wait(lambda: forwarded(diagnostics(path, offset), action, keysym), timeout):
            print(f'no accelerators-send of action {action} keysym {keysym:#x}', file=sys.stderr)
            return 1
        return 0
    if command == 'fired':
        print(fired(diagnostics(args[0], int(args[1])), int(args[2])))
        return 0
    if command == 'native':
        print(native(diagnostics(args[0], int(args[1]))))
        return 0
    print(f'unknown command {command}', file=sys.stderr)
    return 2


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
