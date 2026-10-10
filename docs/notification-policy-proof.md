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
For a proof-only correction, the receipt retains the actual compiled revision
and records the separate proof revision/hash. It explicitly verifies production
Rust, Cargo and crate contents are identical, and refuses every other changed
path except this proof script and this document. No new compiled revision is
inferred from a script correction. Authentication fixtures clear the printable
space that opens the curtain before typing the known fixture password; real
wrong-password failure and successful unlock are both required.
It checks hidden admission versus the queue cap, source registration, live
expansion geometry, critical and disabled-source controls, normal messages held
past banner timeout, real lock/unlock, lock detail redaction and positive
controls, short-output authentication layout, app installation during retained
history, and a supervised shell restart. It records screenshots, source/binary
identity, assertions, GTK version and private session/accessibility identities.
Accessibility queries use bulk children enumeration; query errors refuse proof.
Owned accessibility service pidfds are retained and exit/runtime removal are
verified during cleanup.

## CI qualification

The GTK CI job runs `scripts/ci-notification-proof` from one clean checkout.
It cleans every workspace package, builds the compositor, GTK shell and host,
and captures the exact GTK test executable from Cargo's JSON output. Copied
binaries receive a source revision and SHA256 manifest before the runtime proof
starts. Artifacts live outside the checkout so source cleanliness remains
meaningful. Build commands, Cargo environment overrides, logs and a CI receipt
are retained even when a build or proof fails; the workflow always uploads them.

CI requires `--real-audio`. The proof launches private PipeWire, Pulse and
WirePlumber services using its private runtime and disables physical device
discovery. This configuration currently requires WirePlumber 0.4; unsupported
versions fail explicitly. The actual libcanberra player feeds a private null
sink recorded at 48 kHz. Every capture must contain real frames. Cases cover
theme-name priority, local file and file URI fallback, absent or remote hints,
app and global suppression, LOW, hidden normal and critical controls, disabled
sources, history reenable, locked delivery, and event sounds over playing media.
Media recordings check the carrier and residual event energy before and after
playback. Audio service exits and accessibility daemon/registry exits are
verified before removing the runtime. Cancellation also enters owned cleanup.
These are digital output checks; they do not qualify physical speakers.

The exact source `32f75039` passed strict Clippy, 353 compositor, 338 host,
81 protocol and 126 GTK tests (with one compositor and three GTK tests ignored).
Its matching immutable binaries passed 57 canonical local runtime assertions,
including XRes positive/spoof-negative cases and the real ignored GTK viewport
regression. The new CI/audio cases require their own successful CI receipt;
the earlier result does not qualify those additions.

## Qualification limits

Notification sounds follow sender sound-name/file hints, per-app sound alerts
and global event sounds; no hint means silence. Actual shown banners trigger
playback once per arrival/update. Bounded notification players operate separately
from volume feedback and can overlay media. LOW notifications retain history and
lock summaries without banner or sound. Private authenticated native peer PIDs
are matched to the DBus sender before desktop-entry and installed app-name
fallbacks. X11 owners use the X server's XRes `LOCAL_CLIENT_PID`, queried on the
existing bounded property worker. This is server-reported ownership, not a
kernel-authenticated X11 window credential. The shared Xwayland Wayland peer and
client-provided `_NET_WM_PID` never establish the owner. Lookup stays unknown
until its asynchronous reply; replies are checked against the mapped XID and
session-monotonic window ID and never overwrite a known PID. Unknown ownership
uses installed hint/name fallback. The real X11 fixture sets another mapped
sender's PID property before mapping, then requires disabled-source rejection
and positive controls for both actual senders.

Tracked windows additionally supply private, length-bounded X11 instance
metadata. The host resolves exact StartupWMClass instance before class, then
desktop basenames in original case before ASCII lowercase/space-to-dash forms,
with GNOME's vendor-prefix order. Duplicate StartupWMClass entries use GNOME's
cache-scan exact/visible replacement rules. Desktop discovery occurs at intake
outside the notification-center lock and caches only owned strings/flags; the
live AppInfoMonitor invalidates that catalog without scanning on the GTK thread.
Notification desktop hints and app-name fallback remain exact installed IDs.
The canonical proof adds separate real X11 instance/class, exact-case and
canonical vendor controls plus exact installed app-name controls. These new
cases remain unqualified until the corresponding exact-source CI receipt passes.
Sandbox/GTK application properties, startup sequences and window-group or
transient associations remain further tracked-identity requirements.

The default local proof captures actual player dispatch arguments. `--real-audio`
also requires recorded digital output. Neither mode certifies
audible output, sound device behavior, physical outputs, host PAM, GTK variants
not actually run, or a paired GNOME performance/visual comparison. P-NT-03 must
remain partial until all actual baseline cases are qualified. No parity result
is inferred from unit tests or inventory labels.
