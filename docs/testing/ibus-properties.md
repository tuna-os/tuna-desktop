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

The GTK menu shows the active IBus engine and its visible properties. It supports actions, toggles, radio groups at each menu level, separators, and submenus. InputMode symbols with one or two grapheme clusters can replace the indicator text. GTK provides labels for controls, plus sensitivity and checked state. GTK's AT-SPI bridge exports no Action for check buttons, so each toggle and radio carries a widget-local `property.toggle` action that clicks it; GTK withholds the action while the control is insensitive. Submenus use GTK expanders; the appearance of GNOME submenus remains unmeasured.

The expected `ibus-daemon` is the first executable found on PATH. The consumer resolves symlinks and holds its file descriptor. It compares device/inode identity with the executable of the socket peer, checks the peer UID, and pins process start time. Actions and async responses recheck these identities.

A wrapper mismatch, missing credentials, or unsupported stream disables the new consumer with a diagnostic. There is no basename fallback or fixed distribution path. PATH is user environment. This proves provenance of the expected peer. It does not isolate every compromise by the same user.

Only the reserved direct sender can alter the property graph. Engine signals use the resolved IBus owner. When the engine, context, bus, panel owner, or lock state changes, the consumer retires tokens for old actions. The existing composition and candidate paths remain separate. Unsupported property transport does not suppress the existing FocusOut path that hides candidates.

The property wire does not include an engine or context generation. Local tokens and the daemon's route to the focused context enforce the available boundaries. This does not claim a new transaction protocol across later server focus changes.

## Required CI execution

Rust parser/state/peer tests run in the existing workspace test job. Run `python3 scripts/ibus-property-proof-tests.py` for source and helper controls. These tests do not execute GTK or the daemon. CI invokes these controls in its early check.

The existing GTK journey now needs G-IME-PROPERTIES after its composition and candidate gates. It uses the actual private daemon and shell, an owned two-engine fixture, PID/start/executable checks, and real accessibility actions. It checks toggle/radio/action traffic, hidden and disabled controls, forged peer denial, and an old accessible action after an engine change. A fresh action from the same GTK producer provides an ordered positive callback. The receipt records the old action's actual return and checks the exact daemon traffic and current checked state. The helper bounds each call with an external timeout.

No live result exists for this source leaf yet. A successful gate qualifies its stated nested scope only. The successor adds live controls for context reentry, lock recovery with the same engine, and panel-owner loss. Those controls have not run yet. Installed package/image identities and a matched GNOME capture remain required.

## Remaining #592 requirements

This menu does not yet list configured XKB/IBus input sources or select a particular configured source. The existing cyclic source shortcut remains. A configured source chooser needs an authoritative active-source mapping and consumer proof. This change does not cover all toolkit composition clients, Unicode offsets, moving caret/windows, or all focus transitions. Existing IME gates remain required. Native installed and hardware cases remain open.

## Recovery follow-up

The consumer retires tokens from prior actions on every engine or FocusIn edge, even if the identifier is the same. The consumer watches both daemon and panel owners. Async responses need the same pinned transport, owner epoch, request, and lock state. Engine and context queries target the resolved daemon owner.

On unlock, the consumer reads current daemon and panel owners before it releases its own panel name. It then requests the name with ALLOW_REPLACEMENT and DO_NOT_QUEUE. It never uses REPLACE_EXISTING for recovery. The daemon sends fresh FocusIn and cached engine properties when it creates the new panel proxy. The consumer does not replay an old UI list.

Another panel retains its name; the menu stays disabled with a diagnostic. If that name becomes empty later, a previous panel can reclaim it after fresh owner checks. Five seconds bounds the refresh operation. Each call has a timeout of at most two seconds.

The candidate popup uses a weak callback to clear and hide when trust or panel ownership is lost, or lock starts. The callback does not reset the property connection recursively. Test diagnostics contain fixed method names, counters, and a SHA256 of the daemon context path. They contain no raw context paths, property keys, labels, text, or key values. Locked diagnostics stay silent.

The original menu gate still has a 40-second bound. The separate G-IME-PROPERTIES-LIFECYCLE gate has an 80-second bound for context reentry and two cycles of lock and unlock. The helper sees no password. The parent uses its existing private PAM fixture and native lock UI. The first cycle checks refresh without another panel. The other checks refusal while a second peer owns the panel name, then recovery after that peer releases it.

The observer needs fresh consumer traces, exact daemon traffic, checked state, and the same engine instance. An old action from accessibility may return handled, rejected, or defunct; its effects must remain absent. CI must execute all these runtime controls.
