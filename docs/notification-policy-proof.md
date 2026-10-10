# Notification policy and lock privacy

`notification_policy.rs` reads GNOME 51's global and relocatable application
settings. Admission happens before a message occupies a banner slot. A
replacement keeps its existing source identity. The GTK consumer watches live
settings and desktop-file changes, removes disabled sources from visible
history, and retires unlocked title/body/action widgets while locked.

The lock UI receives a separate projection containing only enabled, unseen
messages allowed by both master and source policy. With details disabled that
projection contains source/count information and no summary/body. Critical
urgency bypasses banner suppression for an enabled source, never lock privacy.
Lock rows are read-only, and the store rejects app action invocation while
locked or source-disabled. Banner timers do not acknowledge unseen locked
messages. Acknowledgment survives queue reload; old queue entries without this
field are conservatively treated as acknowledged.

## Local runtime proof

Build and freeze all workspace packages from clean committed source under the
shared build-and-freeze guard, recording `source_revision`, `source_dirty` and
`binary_sha256` in the frozen directory's `manifest.json`. Then run:

```
dbus-run-session -- /usr/bin/python3 scripts/tuna-notification-policy-proof \
  --bin-dir /absolute/immutable/binaries \
  --artifacts /absolute/new/artifacts --display :96
```

The proof refuses dirty source, mismatched binary/source revisions or hashes,
existing proof runtimes, and occupied displays. It runs the actual compositor,
GTK shell, notification service, private settings backend and PAM fixture.
The immutable bundle also contains `tuna-shell-gtk-tests`; the proof verifies its
hash and runs the real GTK viewport visibility regression on the private display.
This covers an initially zero-height lock window, a short output, policy removal,
and restoration without treating a hidden parent as absent notification content.
It checks hidden admission versus the queue cap, source registration, live
expansion geometry, critical and disabled-source controls, normal messages held
past banner timeout, real lock/unlock, lock detail redaction and positive
controls, short-output authentication layout, app installation during retained
history, and a supervised shell restart. It records screenshots, source/binary
identity, assertions, GTK version and private session/accessibility identities.
Accessibility queries use bulk children enumeration; query errors refuse proof.
Owned accessibility service pidfds are retained and exit/runtime removal are
verified during cleanup.

## Qualification limits

Notification sounds follow sender sound-name/file hints, per-app sound alerts
and global event sounds; no hint means silence. Actual shown banners trigger
playback once per arrival/update. Bounded notification players operate separately
from volume feedback and can overlay media. LOW notifications retain history and
lock summaries without banner or sound. Private authenticated native peer PIDs
are matched to the DBus sender before desktop-entry and installed app-name
fallbacks. X11's shared Xwayland peer cannot authenticate the application PID, so
those notifications use the installed hint/name fallback.

The local proof captures actual player dispatch arguments; it does not certify
audible output, sound device behavior, physical outputs, host PAM, GTK variants
not actually run, or a paired GNOME performance/visual comparison. P-NT-03 must
remain partial until all actual baseline cases are qualified. No parity result
is inferred from unit tests or inventory labels.
