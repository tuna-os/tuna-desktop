# Marlin VM lifecycle proof

The `marlin-vm` and `marlin-vm-orca51` CI jobs install the ordinary Marlin
preview image built from source, then a CI-only fixture layer
(`packaging/marlin/vm-lane/Containerfile`). The layer gives the image the
published `marlin:tuna` login path: stock GDM with tunaOS's tuna-flavor GDM
steps (`tunaos.toml` `[desktops.tuna]` `after_install`: greeter theme
overlay, `FallbackSession=tuna` with `Session=tuna` account templates, every
other session hidden), vendored byte-for-byte in
`packaging/marlin/vm-lane/tunaos/` from tunaOS 193cc0ef, on the package's
own `Tuna Desktop` session entry. The fixture account has no saved session.
No autologin or initial session exists: the lane drives GDM's user list and
types the fixture password exactly as the published-flavor nightly does
(`V-GREETER-LIST`, `V-GDM-FALLBACK`, `V-GDM-LOGIN`;
`docs/marlin-tuna-nightly.md`), from the same `scripts/tuna-vm-lane` code.
Logout must return to that GDM greeter. This changes neither the shipped
preview image nor the number of CI VM profiles. GDM's session worker opens a
journal stream for `gdm-wayland-session`, which the fixture forwards to the
serial port. The fixture's session environment comes from `environment.d`,
which gdm-wayland-session imports from the user manager; the per-login Orca
test secret comes from a user environment generator.

## Record-free fresh accounts (tunaOS#4092)

`tuna-test` above has a session-less AccountsService record, so it proves
GDM's `FallbackSession` path only. `scripts/tuna-vm-lane --fresh-accounts`
is a separate boot of the same disk (QEMU `-snapshot`) that passes the
`tuna.vm.fresh-accounts` systemd credential (SMBIOS type 11). Only then does
`tuna-vm-fresh-accounts.service` run `useradd -m` before GDM starts, as an
installer would: `tunastd` (standard), `tunaadmin` (`wheel`), and the
control `tunaseeded`, whose record is pre-seeded. `tunastd` and `tunaadmin`
get no AccountsService record, no Session preselection and no autologin.
For this credential-only boot the fixture also sets GDM's own
`org.gnome.login-screen disable-user-list=true` in its own
`tuna-vm-fresh-accounts` dconf database, explicitly inserted into the GDM
profile ahead of the preserved vendor defaults. The fixture requires a real
`gsettings get` readback of `true` before it finishes. Run 41234017707 showed
the vendor profile need not include `system-db:gdm`, so writing `gdm.d`
without installing a profile does not apply the setting. The greeter then
asks for the user name. In run 41230684089 the old Tab count to
"Not listed?" instead selected the listed `tunaseeded`, typed `tunastd` as
its failed password, then authenticated `tunaseeded` with the fixture
password. The setting does not create account records or select a session.

The process/logind greeter predicate is not an input-readiness predicate.
In [run 41248530233](https://github.com/tuna-os/tuna-desktop/actions/runs/41248530233),
artifact `marlin-vm-orca51` (ID `13061267386`, SHA-256
`1de554a272722c7640b767cde1f75cfa7603910bd7c0c3cda4b4895f38d79af2`)
retains a black `fresh-accounts/fresh-greeter.png`; its
`tunastd-first-password.png` still shows **Username**, not Password, when
the old driver sends the password. The serial journal records
`pam_faillock(gdm-password:auth): User unknown` at 19.270579 seconds and no
user session. The standard counterpart reaches a visible Password prompt
and authenticates both accounts; its later admin sudo failure is separate.
The run's PR source is `df05bde92cd4392871db9d36d3d3a21ce43b846b`;
workflow checkout/manifest metadata is the synthetic merge
`b4d42581e7299323fd6ead9f76dcd94862990f1c`. Both use published Marlin
base `sha256:6c1e25b1fa9ddc07d7e71f767456839676333d3ebdc332b642a467f56c8ff71b`;
the Orca test-layer image ID is
`sha256:0e3b49a6df3ee644fb9ec4f1dc5b0971da0e6d67b4aedbbc0bedcb5ff116b2e8`.

The fresh-account driver now waits for the actual greeter's focused,
visible, showing, editable, sensitive and enabled AT-SPI entry before
typing each credential. The probe runs as the active non-root greeter UID,
binds the accessibility bus PID/UID to the original `gnome-shell` process
and start time, and distinguishes text from password-text roles without
reading names or entry contents or invoking accessibility actions.
Prompt receipts and pre-input frames are retained. Initial readiness uses
the existing greeter deadline; name, password and session readiness share
one existing login deadline. There is one credential pair per login, no
fixed-sleep increase and no retry. This fixes wrong-phase input; the old
receipts alone do not identify which input was lost during startup.

The first native observer run found 1267 distinct GNOME greeter actors,
exceeding the unchanged 512-node observation budget when hidden actors were
enumerated. This was not a cycle (maximum depth 15, no duplicate objects).
The actual focused username entry had all required flags, and every
non-root ancestor was SHOWING/VISIBLE. Pruning non-SHOWING branches visits
131 nodes instead, including hidden branch roots, without changing the
node/depth/time limits. The application metadata root is exempt from this
prune because it is not a visible component. This follows the existing
shell-focus observer's SHOWING traversal rule.


| Gate | Actual operation and evidence |
| --- | --- |
| V-FRESH-RECORD-FREE-<user> | Before any login: account exists with a home, no `/var/lib/AccountsService/users/<user>`, no GDM automatic/timed login, `wheel` only for the admin |
| V-FRESH-CONTROL | The same probe classifies the pre-seeded `tunaseeded` as NOT record-free |
| V-FRESH-LOGIN-<user>-{first,second,reboot} | Owner-bound focused editable GDM text prompt, then password-text prompt, observed before each credential is typed through QMP; exactly one `gdm-password` VT session of that account runs `tuna-compositor` and no `gnome-shell`/`gnome-session` |
| V-FRESH-SESSION-<user>-{first,second,reboot} | Read AccountsService's effective `Session` over the system bus: it must be `tuna`; if a cache record exists, any saved `Session` must also be `tuna` |
| V-FRESH-SUDO-<user> | `sudo -S -k true` as the account with its password: admin succeeds, standard is refused by sudo policy |
| V-FRESH-LOGOUT-<user>-<round> | logind ends the session and the greeter is ready again |
| V-FRESH-REBOOT / V-FRESH-SESSION-RELOADED-<user> | In-guest reboot (new `boot_id`), effective AccountsService `Session=tuna` reloads before login, then both accounts authenticate into Tuna again |

Receipts are in `fresh-accounts-receipts.json`; frames before each input,
on prompt timeout and at each session. Only Arch/GDM 51 is covered here;
pre-51 GDM is not exercised by this lane.

### Why a first login need not create a cache file

PR #931 incorrectly required a physical cache file after the first login.
The direct [GNOME reference receipt](../scripts/lib/fixtures/fresh-account-gnome51-reference.json)
boots the **same pinned published Marlin GNOME base** as failed diagnostic
run [41223633281](https://github.com/tuna-os/tuna-desktop/actions/runs/41223633281):
`ghcr.io/tuna-os/marlin:gnome@sha256:8d71660f78120a141e9ae8e620bd66645c6bfb52f1ab4cd663f9672aaa0b0b94`.
It ran on an owned Corral VM on `karnataka`, without a Tuna package or the
VM-lane layer. `useradd -m` created separate standard and wheel accounts;
neither had a cache record, `.dmrc`, preselected session or autologin.
Both first successful GDM password logins started stock GNOME Shell 51.1
with gnome-session 51.0, GDM 51.1.1 and AccountsService 26.27.3, in active
Wayland `gdm-password` sessions on VT2. **Both cache files remained absent**;
AccountsService exported empty `Session` and `Saved=false`. The monitored
system bus contained lookups but no `SetSession` or `CacheUser` calls before
the explicitly labelled post-observation intervention. The receipt retains
actual package/template/session identities, navigation distinctions and
successful VM/disk deletion plus exact-name inventory absence.

This matches upstream, rather than a missing Tuna session-start method:

- [AccountsService 26.27.3 `user_update_from_template`](https://gitlab.freedesktop.org/accountsservice/accountsservice/-/blob/26.27.3/src/user.c#L330-414)
  applies the template to in-memory/exported properties without saving a
  file. Its `user_change_session_authorized_cb` writes extra data only when
  the normalized new session differs from the current property.
- [GDM 51.1.1 session settings](https://github.com/GNOME/gdm/blob/d643e68963ab74564955025b554ec23cd794770d/daemon/gdm-session-settings.c#L300-L320)
  explicitly loads properties even without saved state. Its
  [`gdm_session_settings_save`](https://github.com/GNOME/gdm/blob/d643e68963ab74564955025b554ec23cd794770d/daemon/gdm-session-settings.c#L373-L415)
  only sends non-NULL session/type/language values, and caches only non-local
  users. Loading a local account is not `CacheUser`. Tuna's existing
  templates supply `Session=tuna`; setting that same value is not a reason
  for AccountsService to materialize a cache file.
- [GNOME Shell 51.1](https://github.com/GNOME/gnome-shell/blob/33da06854e229c256afd5872fedd83c28cbfa176/js/gdm/loginDialog.js#L535-L554)
  can request `SelectSession` when the session menu's active item changes;
  this is not a guarantee that every default login changes an account
  property. Its user/session UI reads AccountsService users; the observed
  GNOME login does not cache them. [gnome-session 51.0](https://github.com/GNOME/gnome-session/tree/aed00cf18229a98231c23e14f5798ddb918c4956)
  has no AccountsService mutation API.
- GNOME Initial Setup is a **different account-creation/property-writing
  path**, not installed in this Marlin reference. Its [51.0 account page](https://github.com/GNOME/gnome-initial-setup/blob/51.0/gnome-initial-setup/pages/account/gis-account-page-local.c#L447-L488)
  calls `CreateUser` and sets an icon; its [language page](https://github.com/GNOME/gnome-initial-setup/blob/51.0/gnome-initial-setup/pages/language/gis-language-page.c#L100-L153)
  writes the language. AccountsService's `CreateUser` path calls
  `cache_user`, unlike an installer using `useradd`. A record produced there
  must not be misattributed to the first ordinary GDM login.

The corrected proof does not write a record, call `SetSession`, preseed a
choice or add product-side cache writes. It reads the effective
AccountsService session with `FindUserByName`/`Get(Session)` and requires
`tuna` after **every** login and after reboot, while still rejecting a saved
GNOME choice. The record-free preflight, seeded negative control, real
authentication, standard/admin sudo policy, logout and reboot remain gates.

### Original #931 merge audit

[#931](https://github.com/tuna-os/tuna-desktop/pull/931) did **not** have green
exact-head CI. Head `d62ec870c0f402c22fbae6719afd227cd0353b81` was merged by
`hanthor` at `2026-10-31T15:42:38Z` as
`cba3cdbd5cd629d42c2512124f3fe7e35c117b55`, while its
[exact-head run 41215443626](https://github.com/tuna-os/tuna-desktop/actions/runs/41215443626)
was unfinished. `check` failed at 16:00:23Z, `marlin-vm` at 16:27:14Z,
`marlin-vm-orca51` at 16:30:35Z and `ci-gate` at 18:14:39Z.
An earlier PR comment already documented the missing-record failure as a
merge blocker. At investigation time the branch protection endpoint returned
“Branch not protected”; the effective organization ruleset required only
deletion/non-fast-forward protection, not successful status checks. These
observations explain why an ordinary merge was possible; they do not prove
an administrator bypass was used.

The Debian package journey must retain the original beta package's
`share/pam.d/tuna-lock` conffile bytes, including its comment. Run
41151873648's `journey` failed on dpkg's conffile prompt before the
upgrade could qualify. PR #930 restores those bytes;
the fresh-account change must include that fix, not force a conffile
answer or change PAM policy to bypass the upgrade failure.
The Python lane.boot API keeps its original four-value return and original
VM device profile by default. Lifecycle callers explicitly request
`guest_agent=True` to add the QGA transport and receive its socket as a fifth
value; existing performance callers keep working without a guest agent.

QEMU's guest agent executes a fixed probe with a root UID, while the
compositor, shell and application clients must belong to the non-root
session owner. Artifacts record window IDs, app IDs, rectangles, process
PIDs/start ticks and redacted session metadata. They contain no process
arguments, passwords or application titles.

The last of the existing five pristine boots runs these checks after the
feature tour:

| Gate | Actual operation and evidence |
| --- | --- |
| V-LIFECYCLE-OWNER | Root probe, non-root `gdm-password` login session, three application surfaces/processes |
| V-NORMAL-INPUT / V-NORMAL-INPUT-FAIL-CLOSED | A real GTK client receives ordinary QMP keys while unlocked, then receives none while locked; artifacts contain only counts and PID, never key values |
| V-MOUSE-HANDEDNESS | Nine physical QMP clicks pass through native libinput to the same non-root GTK client while the real GNOME mouse setting changes false→true→false; left/right swap, middle stays unchanged, exact before/after counters retained in lifecycle/mouse-handedness.json |
| V-POINTER-LIVE | GNOME mouse speed/natural-scroll, acceleration profile (`flat`, then `adaptive`), middle-click emulation (on, then off) and scroll-wheel emulation (button 2 → `on-button-down` with BTN_MIDDLE and button lock, then off) reach live libinput device state and restore, asserted on the compositor's per-device libinput readback with per-option availability (each option needs at least one mouse offering it; scroll-wheel emulation is asserted on mice that offer button scrolling and report BTN_MIDDLE in `pointer_buttons`, since libinput rejects a scroll button the device lacks, e.g. the guest's VMMouse); a published guest that predates profile or middle-click/scroll-button reporting skips only those checks, a current build that omits them fails, and a guest mouse schema without GNOME 51's scroll-wheel emulation keys skips only that check; snapshots retained in lifecycle/pointer-live.json (#343) |
| V-POINTER-HOTPLUG | A QMP-hotplugged USB mouse arrives with the current settings already applied (DRM DeviceAdded path); unplug drops it while survivors keep theirs; bus discovery, arrival and survivor snapshots retained in lifecycle/pointer-hotplug.json (#343) |
| V-VT | VT away/back, DRM pause/activate events, unchanged process identities and app rectangles, matching body plus a fresh visible count update from the real GTK client |
| V-SUSPEND | Real logind suspend reaches QEMU `suspended`; the first guest snapshot after `system_wakeup` must already be locked, with all application identities intact; the real varied lock mask must repaint within five seconds, matching the pre-suspend mask; QEMU inactive-output placeholders fail |
| V-SLEEP-AUTH-FAIL-CLOSED / V-SLEEP-FRESH-AUTH | The single real suspend interrupts a correct password attempt delayed by actual pam_exec; the real PAM success from the old generation is refused, then fresh correct authentication succeeds |
| V-SLEEP-CLIENT-REPAINT | The same GTK client receives a fresh key and visibly repaints after wake/unlock |
| V-VT-FAIL-CLOSED | VT away/back while locked keeps the mask and original application processes |
| V-AUTH-FAIL-CLOSED | Real PAM service temporarily uses `pam_deny`; fresh submitted/refused milestones prove the correct test password was attempted and denied; original service restored afterward |
| V-CAPTURE-INPUT-FAIL-CLOSED | Reachable real untrusted grim/wtype clients are denied capture/virtual-keyboard interfaces while locked; a successful Wayland connection is required and timeouts do not count as refusal; this does not assert that these interfaces are available while unlocked |
| V-SHELL-FAIL-CLOSED | Stop the portal and kill the lock UI; original app/compositor identities survive and the supervised replacement stays locked |
| V-GSD-PLUGINS | Each of the thirteen gnome-settings-daemon plugins the session target wants, MediaKeys included, is the packaged `gsd-*` binary, running as the session user as its unit's main PID and owning its bus name; `org.gnome.SettingsDaemon.Color` belongs to the compositor; A11ySettings has no owner; the session targets are active and the compositor holds `gnome-session-leader-fifo` (lifecycle/gsd-plugins.json, #355, #589) |
| V-GSD-POWER-IDLE | With `idle-delay` set to 20 s and no input, dbus-monitor records gsd-power's own `SetDimming(true)` before its `PowerSaveMode=3`; the compositor snapshot reports locked and `power_save_mode` 3; Return wakes it to 0 and gsd-power undims; the original delay is restored (lifecycle/gsd-power-idle.json, #355) |
| V-LOCKED-HINT | logind's `LockedHint` for the login session reads `no`; the session user's `loginctl lock-session` locks the compositor and the hint turns `yes`; the real password unlocks and it reads `no`; Super+L locks with `yes`; `loginctl unlock-session` unlocks with `no`, as GNOME 51's `screenShield.js` does (lifecycle/locked-hint.json, #846). The published flavor predates the integration and defers to the current-build lane |
| V-POWER-BUTTON | gsd-media-keys (its unit's main PID, the session user) holds logind's `block` inhibitor on `handle-power-key:handle-suspend-key:handle-hibernate-key` with Why "GNOME handling keypresses". On a fixture desktop chassis, after each gsd restart a new gsd-media-keys must own its name, hold that inhibitor, have `XF86PowerOff` granted by `GrabAccelerators` and the shell must log that it yielded the bindings. QMP `system_powerdown` (ACPI power button) with `power-button-action` `interactive`: `AcceleratorActivated` reaches gsd, gsd calls `org.gnome.SessionManager.Shutdown`, the compositor calls `EndSessionDialog.Open(1, …)` and the `tuna-shell-end-session` layer shows within 15 s; 5 s after the press QEMU is `running` with the same compositor process and logind session and no logind power request; Escape (always sent, far inside the 60 s countdown) removes the layer, the shell emits `Canceled` and logs its D-Bus Power Off dialog. `nothing`: gsd receives the key, and for at least 8 s no Shutdown, dialog call, layer or logind request follows. On the image's `vm` chassis with a fixture `shutdown` block inhibitor held by root: the key reaches gsd and gsd's own process calls logind `PowerOffWithFlags(0)`; gsd sends it without a reply callback (NO_REPLY_EXPECTED), so logind's refusal has no bus reply, and 5 s later logind has emitted no `PrepareForShutdown`/`PrepareForSleep`, `PreparingForShutdown` is still false with the fixture inhibitor held, no other logind request or non-error reply followed, no dialog opened and QEMU, compositor and session are unchanged. Setting, chassis and `/etc/machine-info` are restored (lifecycle/power-button.json, #353, #589) |
| V-LOGOUT | The product's Log Out: `org.gnome.SessionManager.Logout(1)` on the session bus, which ends the compositor (the session leader) through its orderly shutdown, as gnome-session's exit ends a GNOME session; a published guest that predates that manager logs out as its shell did, through logind `Session.Terminate`. Within 30 s GDM's non-root gnome-shell greeter in its active gdm-launch-environment session repaints the greeter frame captured before login and no original compositor survives. logind's Terminate signals GDM's session worker together with the compositor; the worker's switch to the login VT could then be cancelled by logind restoring the session VT, leaving GDM without a greeter on the session VT (published nightly runs 40883859027 and 40940329310 attempt 1) |

The original tour's `V-LOCK` still requires exactly unlocked → locked →
unlocked. Its observations end before lifecycle operations add their own
lock transitions. A QEMU pause is never a substitute for guest suspend.
The repaint comparison excludes the panel clock and tolerates up to 20%
changed body pixels for applications such as System Monitor; it also
requires exact window and process identity preservation.

V-POWER-BUTTON runs on the unlocked desktop after V-SHELL-FAIL-CLOSED and
before V-LOGOUT. gnome-settings-daemon 51 reads hostnamed's `Chassis` once
at startup and, on `vm`, powers off for every `power-button-action` but
`nothing`. QEMU guests report `vm`, so the fixture (root, through QGA)
records `/etc/machine-info` and `Chassis`, calls hostnamed `SetChassis`
with `desktop` and restarts `org.gnome.SettingsDaemon.MediaKeys.target`
as the session user. The service refuses manual start and stop, but a
restart of its `PartOf` target propagates; if refused, the fixture kills the
main PID and `Restart=on-failure` brings it back. Afterwards the fixture
restores the recorded chassis and file bytes and restarts gsd again, so the
rest of the lane sees the image default. The `vm` case stays safe because
gsd 51 sends `PowerOffWithFlags` without `SD_LOGIND_SKIP_INHIBITORS`, and
since systemd 257 logind refuses that request outright under a `block`
inhibitor. The fixture refuses to press the button on an older logind.
`power-button-action` `suspend` is not exercised. It would add a second
real S3 cycle, and the user-authorized single-S3 contract below forbids
that. `hibernate`, the lock-screen mode, and an unrestrained VM power-off
are not exercised either.

The compositor also listens to the trusted logind system-bus sleep signal:
preparation invalidates pending authentication generations and engages its own lock; wake restores KMS connector/plane
state and retires lost flip buffers without marking them presented. After
the blocking device reset, bounded nonblocking reads drain obsolete kernel
completions before any new frame is submitted. A valid modeset completion
can carry the preceding vblank's timestamp, so timestamps alone do not
identify abandoned frames. A failed reset or drain keeps scanout blocked
until recovery succeeds. Wake
recovery does not depend on a VT change; an off-seat wake waits for seat
activation. This does not establish a delay inhibitor or physical masked
frame acknowledgement before sleep.

These new gates require their own actual CI results before acceptance.
Output hotplug and loss of every compositor-adjacent service remain
unproven, as do physical hardware and 24-hour soak. This document does not
claim those parts of issue #62 are complete.

Protocol references: [QEMU guest agent](https://www.qemu.org/docs/master/interop/qemu-ga-ref.html)
and [QEMU wake-up](https://www.qemu.org/docs/master/interop/qemu-qmp-ref.html#command-system-wakeup).

The shared GPU profile `virtio-vga-pcie-pm-preserved-v1` keeps the same
virtio-vga, resolution, CPU, RAM and workloads. It places that GPU behind
one PCIe root port and advertises PM no-soft-reset. All native GNOME
capture and Tuna Desktop/performance boot callers use the same GPU arguments;
manifest/report fields record them. Fresh GNOME comparison evidence must
use this profile; old-profile latency or cadence does not qualify it.
The actual guest PCI capability chain must include PCIe, PM and the
no-soft-reset bit, and each real resume must produce fresh host resource
creation, backing attachment and nonzero scanout commands as well as the
painted lock mask. A configured flag alone never passes the gate. The
shipping AWS Bochs GPU is a separate, unmeasured suspend profile.

The diagnosed fixture runs kernel 7.2.8-arch1-2 and QEMU 10.2.1. In
[Linux 7.2.8 virtio GPU](https://raw.githubusercontent.com/gregkh/linux/v7.2.8/drivers/gpu/drm/virtio/virtgpu_drv.c)
the driver has no freeze/restore callbacks. The
[PCI PM path](https://raw.githubusercontent.com/gregkh/linux/v7.2.8/drivers/virtio/virtio_pci_common.c)
skips reset when the device advertises no-soft-reset; otherwise
[virtio restore](https://raw.githubusercontent.com/gregkh/linux/v7.2.8/drivers/virtio/virtio.c)
resets the device. [QEMU 10.2 virtio PCI](https://raw.githubusercontent.com/qemu/qemu/v10.2.0/hw/virtio/virtio-pci.c)
provides that capability only behind a PCIe port and preserves the device
in D3hot. A [GPU reset](https://raw.githubusercontent.com/qemu/qemu/v10.2.0/hw/display/virtio-gpu.c)
destroys its resources and clears the display. The prior actual trial
37211055978/111464650400 reached real S3, stayed locked with apps intact
and processed guest pageflips, but showed inactive host output and no
fresh resource uploads. It remains a failed qualification. The supported
PM profile requires its own strict actual run; no kernel/package override,
extra VM matrix cell or physical hardware claim is introduced. QEMU 10.2
is required, so the existing performance runner uses ubuntu-26.04.

The performance lane keeps the user-authorized single-S3 contract: the session
is prelocked and a real PAM attempt is pending before sleep. Its fail-closed
wake and stale-success/fresh-authentication checks share that cycle. Varied
mask repaint is retained as an observation; fresh host GPU uploads, client
input/repaint and unchanged application identity remain mandatory. The prior
native lifecycle qualification (#265) separately demonstrated locking from
an unlocked session and two real suspend cycles on the preserved-resource VM
profile; that evidence does not certify a later candidate automatically.

### Date & Time restoration and published capability failures

Published-flavor run 41151873652 observed the new Auckland panel time, then
restored timedated to UTC without observing the panel's UTC repaint.
`G-DATETIME-CLOCK-SET` immediately read the Auckland label after the manual
clock jump (39,610 seconds from UTC), so that assertion did not have a
restored-panel baseline.

The timezone round trip now retains `panel_revert` dumps and
`panel_revert_verified` in `lifecycle/datetime-evidence.json`, waiting within
the existing ten-second repaint deadline. The clock timezone gate requires
both outbound and return repaint. A failed restoration or dump remains
visible in the probe error, fails the clock timezone gate, and prevents the
manual-time probe from changing the clock against an unverified baseline.
The polkit timezone probe runs after the manual-clock probe, so its own
timezone round trip cannot invalidate that baseline.

Both published panel-change assertions stay strict. A released image that
cannot perform them is reported as unqualified/unsupported with its actual
failure evidence, never converted to a pass or credited with another
lane's result. No additional wait extends the capability deadline.

## Locked password speech

Run [41151873648, job 124396764753](https://github.com/tuna-os/tuna-desktop/actions/runs/41151873648/job/124396764753)
passed eight speech gates, then failed `G-ORCA-SPEECH-LOCK`: the retained
[`orca-speech` artifact 13025680053](https://github.com/tuna-os/tuna-desktop/actions/runs/41151873648/artifacts/13025680053)
contains `Lock Screen`, `Password`, `password text.` followed by an empty
`lock-type` speech section. Its sanitized state records an active password
conversation, `locked: true`, and an owned, watched KeyboardMonitor. The
AT-SPI registry was already active. This is not evidence of bus startup
failure.

The missing route was committed masked input to speech, not raw-key
delivery. The compositor intentionally suppresses raw KeyboardMonitor
events while locked. In the installed [Orca 50.3 insertion classifier](https://raw.githubusercontent.com/GNOME/orca/50.3/src/orca/ax_utilities_event.py),
`_get_insertion_text_key_reason` relies on a last printable key/space to
classify password insertion as typing echo. Without that cue,
`_get_insertion_fallback_reason` can return `UNKNOWN` or an unpresentable
auto-insertion reason; [the default insertion handler](https://raw.githubusercontent.com/GNOME/orca/50.3/src/orca/scripts/default.py)
does not speak those insertions. The retained artifact does not contain
Orca's internal event-reason trace, so no particular fallback branch is
claimed as an observed runtime result.

The native Orca consumer needs a package-level correction for the supported
50.3 and 51 versions so committed masked password insertions are presented
without raw locked-session keys. That correction and its package/runtime
qualification are separate and pending; this change does not claim the
masked-input speech failure is fixed. No shell-generated mask announcement
or replacement for Orca's native text-event handling is included.

`scripts/tuna-orca-speech-proof --artifacts orca-speech-artifacts` remains
the end-to-end native nested proof: it physically types into the lock
prompt, requires exactly one spoken mask per typed character, rejects
plaintext speech, verifies that the locked raw-key event count does not
advance, then unlocks through PAM. No injected Orca customization or
relaxation of the bullet assertion is used. These changes still require a
new native proof result; the failed run is not retroactively qualified.

The native proof isolates `XDG_STATE_HOME` and `XDG_CACHE_HOME` under its
artifact root as well as HOME/config/data. A local native attempt stopped
before the speech gates because inherited state selected
`~/.local/state/roost-shell` for upgrade, where the ownership/ancestor <!-- tuna-rename: keep -->
guard refused migration. `user_upgrade::pairs_from` and the shell's
`xdg::state_home_from` prefer an absolute `XDG_STATE_HOME` over HOME;
changing HOME alone does not isolate that route. The proof now exports
private state/cache roots explicitly. It does not touch the real user
paths or bypass the migration guard.

The local full speech proof is blocked by host Orca 46.1, which does not
satisfy the required KeyboardMonitor contract. A new Ubuntu 26.04 run with
distribution Orca 50.3 must establish all speech gates against the native
package correction. The original failed run's eight passing gates do not
establish a nine-gate pass for this change, and a GTK auth-prompt regression
does not qualify end-to-end Orca speech.
