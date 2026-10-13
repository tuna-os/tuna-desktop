# IBus properties: bounded implementation and proof scope

This change consumes properties from the IBus panel. It does not close issue #592.

## Source references

- [GNOME Shell 51 property menu](https://github.com/GNOME/gnome-shell/blob/51.0/js/ui/status/keyboard.js): `_buildPropSection` and `_updateProperty`.
- [GNOME Shell 51 engine binding](https://github.com/GNOME/gnome-shell/blob/51.0/js/misc/ibusManager.js): engine changes and property registration.
- [IBus 1.5.32 property serialization and update](https://github.com/ibus/ibus/blob/1.5.32/src/ibusproperty.c): typed fields; updates preserve type and child list.
- [IBus 1.5.32 list serialization](https://github.com/ibus/ibus/blob/1.5.32/src/ibusproplist.c).
- [IBus sender for the panel and messages from peers](https://github.com/ibus/ibus/blob/1.5.32/bus/dbusimpl.c): the filter for output sets an absent direct sender to `org.freedesktop.DBus`. The filter for input replaces a peer sender with its unique name. The daemon reserves both names.
- [IBus global engine and fake context](https://github.com/ibus/ibus/blob/1.5.32/bus/ibusimpl.c): focus loss can transfer the global engine to its fake context. Panel acquisition sends the focused context and cached engine properties.
- [IBus engine removal from a context](https://github.com/ibus/ibus/blob/1.5.32/bus/inputcontext.c): the context disconnects handlers when it removes an engine. Focus starts registration of properties again.
- [GLib peer credentials](https://docs.gtk.org/gio/method.DBusConnection.get_peer_credentials.html): this method returns no credentials for bus clients. This implementation uses the actual Unix socket's credentials instead.

## Behavior and trust

The host parser limits each value to 64 KiB, 128 properties, eight levels, and 512 bytes per consumed text. It rejects unknown types, invalid states, duplicate nonempty keys, malformed text, and children on a leaf. The parser supports the current shape and the explicit legacy shape.

The GTK menu shows the active IBus engine and its visible properties. It supports actions, toggles, radio groups at each menu level, separators, and submenus. InputMode symbols with one or two grapheme clusters can replace the indicator text. GTK provides labels for controls, plus sensitivity and checked state. Submenus use GTK expanders; the appearance of GNOME submenus remains unmeasured.

The expected `ibus-daemon` is the first executable found on PATH. The consumer resolves symlinks and holds its file descriptor. It compares device/inode identity with the executable of the socket peer, checks the peer UID, and pins process start time. Actions and async responses recheck these identities.

A wrapper mismatch, missing credentials, or unsupported stream disables the new consumer with a diagnostic. There is no basename fallback or fixed distribution path. PATH is user environment. This proves provenance of the expected peer. It does not isolate every compromise by the same user.

Only the reserved direct sender can alter the property graph. Engine signals use the resolved IBus owner. When the engine, context, bus, panel owner, or lock state changes, the consumer retires tokens for old actions. The existing composition and candidate paths remain separate. Unsupported property transport does not suppress the existing FocusOut path that hides candidates.

The property wire does not include an engine or context generation. Local tokens and the daemon's route to the focused context enforce the available boundaries. This does not claim a new transaction protocol across later server focus changes.

## Required CI execution

Rust parser/state/peer tests run in the existing workspace test job. Run `python3 scripts/ibus-property-proof-tests.py` for source and helper controls. These tests do not execute GTK or the daemon.

The existing GTK journey now needs G-IME-PROPERTIES after its composition and candidate gates. It uses the actual private daemon and shell, an owned two-engine fixture, PID/start/executable checks, and real accessibility actions. It checks toggle/radio/action traffic, hidden and disabled controls, forged peer denial, and an old accessible action after an engine change. A fresh action from the same GTK producer provides an ordered positive callback. The receipt records the old action's actual return and checks the exact daemon traffic and current checked state. The helper bounds each call with an external timeout.

No live result exists for this source leaf yet. A successful gate qualifies its stated nested scope only. Owner-loss and context-change tokens have source tests; their complete live negative matrix still needs qualification. Installed package/image identities and a matched GNOME capture remain required.

## Remaining #592 requirements

This menu does not yet list configured XKB/IBus input sources or select a particular configured source. The existing cyclic source shortcut remains. A configured source chooser needs an authoritative active-source mapping and consumer proof. This change does not cover all toolkit composition clients, Unicode offsets, moving caret/windows, or all focus transitions. Existing IME gates remain required. Native installed and hardware cases remain open.
