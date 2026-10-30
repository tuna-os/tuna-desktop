# Remote desktop consent and input

Tuna Desktop exposes GNOME's Mutter RemoteDesktop interface to the installed,
root-owned portal backend and to the installed gnome-remote-desktop daemon.
Creating a session requires the same authenticated portal/shell authority as
capture, or the grd identity below. Every subsequent operation requires its
original unique D-Bus owner. Creation, start, EIS connection, clipboard and
input are denied while locked. Sessions are single-use, with a maximum of 64
remote grants.

The GNOME portal owns the user consent picker. A linked ScreenCast session uses
`remote-desktop-session-id`, must have the same caller, and shares the remote
grant's permanent revocation flag. RemoteDesktop.Start starts its linked streams.
An unrelated caller cannot attach a stream or inject input.

Advertised devices are keyboard and pointer (bitmask 3). Legacy evdev keycodes,
keysyms, relative/stream-relative absolute motion, buttons and scroll are
supported. NotifyKeyboardKeysym follows mutter's `notify_keyval`: the first key
of the effective layout typing the keysym, with Shift or AltGr pressed before
and released after it for its level; a keysym the layout lacks types nothing.
ConnectToEIS supplies one Unix descriptor per session and, as in mutter, may be
called before Start (gnome-remote-desktop does); input sent before Start is
dropped. Its EIS sender
seat exposes only requested keyboard/pointer capabilities, a sealed current XKB
keymap and logical output regions. The per-grant queue is limited to 256 events;
legacy calls over that bound are rejected. EIS overflow or disconnect permanently
revokes that grant. Runtime delivery
checks the flag again, balances held key/button pairs and releases them on stop
or lock. Relative pointer motion stays inside an output.

The registry checks caller/backend loss and lock every 100 ms; the EIS worker
checks revocation on its 20 ms dispatch loop. Runtime input also checks lock and
revocation before delivery. These intervals are scheduling bounds, not real-time
guarantees. Capture nodes share the revocation flag and stop with their grant.

Touch is not implemented or advertised (NotifyTouch* return NotSupported).
EIS output regions and keymap are snapshots at connection time;
clients need a new consent session after output/keymap changes. Rebinding is
limited to 32 device generations per connection to bound unacknowledged object
and outgoing keymap-FD churn. The vendored reis 0.7.1 transport additionally fails
when undecoded incoming or unread outgoing bytes exceed 2 MiB or FD
storage exceeds 64; FD-clone failure closes the connection; its upstream
1 MiB message limit remains. See third-party/reis/ROOST-PATCH.md and regression tests. EIS receiver contexts are
rejected. No physical input device, GPU or VT acceptance follows from nested
software-rendered proof.

## gnome-remote-desktop (Settings > System > Remote Desktop)

gnome-remote-desktop 51 drives Mutter directly, with no portal: `grd-session.c`
calls RemoteDesktop.CreateSession, ConnectToEIS (empty options), then
ScreenCast.CreateSession with `remote-desktop-session-id` and
`disable-animations`, RecordMonitor with connector `""` (the primary monitor,
mirror mode) or RecordVirtual (extend mode and remote login), Start, and
Stream.Start. It needs each stream's `mapping-id` parameter (`grd-stream.c`)
and the EIS region with the same id for absolute pointer motion. Its RDP
clipboard uses EnableClipboard, DisableClipboard, SetSelection,
SelectionWrite, SelectionWriteDone and SelectionRead with the
SelectionOwnerChanged (`mime-types` as `(as)`, `session-is-owner`) and
SelectionTransfer signals. Keysyms are resolved by grd itself over EIS; it
never calls NotifyKeyboardKeysym.

Admission: RemoteDesktop.CreateSession, and only a ScreenCast session linked
to that caller's own grant, also accept a same-user caller whose
`/proc/<pid>/exe` is the root-owned, non-writable
`/usr/{libexec,lib}/gnome-remote-desktop-daemon` and that owns one of grd's
bus names (`org.gnome.RemoteDesktop.User`, `.Handover`, `.Headless`). A
process that merely takes those names, or a user-owned copy of the binary, is
refused; grd gets no Screenshot, Introspect or unlinked ScreenCast access.
Settings' configured credentials are the consent, as in GNOME. The grant is
revoked when that identity or name is lost, on Stop and on lock. GNOME keeps
an RDP session across the lock screen; Tuna Desktop ends it, as for every
remote grant (#61).

Linked monitor streams carry `mapping-id` `tuna-monitor-<connector>`, and the
EIS pointer regions carry the same ids (ei_device v2). Each keymap send is a
fresh read-only description at offset 0, because grd `read()`s it.

Clipboard follows `meta-clipboard-session.c`: a session's SetSelection becomes
the seat's compositor selection; a local paste emits SelectionTransfer and
SelectionWrite returns a pipe; SelectionRead returns a pipe fed by the local
owner. Errors keep mutter's texts ("Clipboard not enabled", "Already
enabled", "Tried to read own selection", "Tried to read in parallel",
unknown serials). Data never crosses the compositor loop: each transfer is
pumped on its own thread, at most 256 MiB, ended after 15 s without progress
(mutter's transfer-request timeout), at most 16 unanswered requests and 8
writes per session, and cut off on Stop, owner loss, lock or Disable. Mime
lists are limited to 64 names of 255 bytes. These bounds are Tuna Desktop's;
mutter has none. Signals go only to the session's owner.

Not covered: grd records its monitor after RemoteDesktop.Start and then calls
ScreenCast.Stream.Start; Tuna Desktop's ScreenCast sessions still accept
streams only before Start and serve no Stream.Start/Stop, so grd's RecordMonitor
fails with "capture session already started" and the RDP client disconnects
before the first frame (Corral VM, 2026-10-30). Also not covered: RecordVirtual (so grd's extend mode and remote login through
GDM's headless sessions), cursor metadata (`cursor-mode` is ignored, so the
RDP client draws its own pointer), `disable-animations`, touch, smartcard and
Kerberos redirection, and physical hardware. The published Marlin image does
not ship gnome-remote-desktop, so Settings shows no Remote Desktop row there.

`scripts/tuna-remote-security --candidate DIR --out DIR` tests a packaged
compositor and GTK shell with the installed Fedora GNOME 51 portal backend,
a real Wayland GTK input client and the independently installed libei client.
It requires actual Cancel/Allow, a linked PipeWire frame, foreign input denial,
legacy and EIS keyboard/button delivery, node removal after Close/client/backend
loss/EIS disconnect/lock, and new-session lock denial. Candidate commit, runtime
versions, consent screenshots, input logs and frame evidence accompany the run.
The exact `1f441bb` Remote head passed all sixteen checks in [CI 37214121497](https://github.com/tuna-os/tuna-desktop/actions/runs/37214121497). The combined Screenshot/Remote candidate `3249f362` passed the same complete GNOME 51 journey in [CI 37214394538](https://github.com/tuna-os/tuna-desktop/actions/runs/37214394538), including independent installed libei 1.6 keyboard and pointer delivery. See `remote-desktop-evidence.json` for candidate, runtime versions and retained artifacts. The proof requires fresh key/button events after each delivery baseline, then no new client input and an empty held seat after backend/lock revocation; a frontend void-method reply alone is not an admission oracle. New locked sessions are denied at CreateSession with response 2.

Issue #61's stated consent, stream, revoke and locked-denial acceptance is complete. The unsupported capabilities above remain broader GNOME parity limits, and P-SY-05 stays partial.
