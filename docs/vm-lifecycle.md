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

The published-flavor lane uses these same strict assertions. `--published`
only records acquisition: no old-package or absent-instrumentation branch
can pass by crediting a current-build lane. Every boot manifest explicitly
records `acquisition`, `qualification_mode: "strict"`, `waived_assertions: []`
and `skipped_assertions: []`; the nightly/repeat receipts preserve and
classify every actual boot ([receipt contract](marlin-tuna-nightly.md)).


## Pristine release-disk qualification is separate

The signed release-disk consumer and its native observer live in
`scripts/lib/try_tuna_desktop.py` and `scripts/lib/try_tuna_qualification.py`.
They do not turn the fixture journeys above into pristine firstboot proof.
The observer requires the normal-release signed manifest and its exact
declared bytes, with the observer checkout matching the signed producer
source. It boots a writable copy while retaining the shipping disk's hash;
there are no injected accounts, passwords, SSH keys or fixture services in
that shipping disk.

Each native Linux/macOS amd64/arm64 host must execute its matching signed
Corral binary, real local QEMU, browser and noVNC transport. Executable
headers reject foreign host binaries even where Rosetta/binfmt could run
them. The guest remains the declared qualified amd64/UEFI disk; ARM hosts
explicitly use TCG. Receipts retain actual QEMU target/accelerator, firmware
and binary hashes rather than claiming native guest acceleration.

Actual GNOME Initial Setup creates a passworded operator account in the
copy. Normal GDM must then enter Tuna, including visible application search,
with wheel membership and password-required sudo; logout/login and reboot
must preserve that account and user data. RFB connection alone means
browser/transport readiness, never successful guest boot or Tuna login.
Terminal cancellation (SIGINT, SIGTERM or SIGHUP), stop, failed browser launch and failed acquisition
must retire owned processes/listeners/locks without touching an unrelated
VM. Normal launch preserves writable user data; qualification explicitly
deletes only its own disposable copies after retirement.
The owned stop socket remains serviceable while browser readiness is pending;
an opener returning zero is not a connection. Native qualification must prove
early stop after that real no-RFB opener, and each cancellation signal must
follow an actual current RFB connection in native Chromium. Its retained
executable/profile identity and actual owned-child wait/exit receipt must agree;
a pre-browser cancellation or a stale readiness receipt cannot pass.

The consumer requires Python 3 and the native package manager (on macOS,
Homebrew and its normal command-line-tool prerequisites). It obtains the
checksum-pinned Sigstore verifier itself and acquires QEMU plus amd64 UEFI
through native package trust, including when QEMU exists but firmware is
missing. Linux without caller access to `/dev/kvm` explicitly uses TCG.
Interactive Linux hosts use their normal sudo authentication, with its password
input connected directly to the real terminal; the wrapper never reads or stores
that password. Without a terminal, sudo must permit noninteractive installation
or acquisition fails closed. Cancellation waits for an already-started native
package transaction (and its normal authentication prompt) to finish safely;
it does not start a VM afterward.
No Podman or guest disk build belongs in this consumer. An artifact directory
changes only where the same signed bytes are read; it does not replace
identities, trust roots or descriptors, and is not a promise of network-free
dependency bootstrap.

The existing public wrapper is not cut over until a real signed disk and
manifest pass all four native firstboot/browser/lifecycle qualifications.
Private helper code or fixture receipts alone do not establish that result.

## Record-free fresh accounts (tunaOS#4092)

`tuna-test` above has a session-less AccountsService record, so it proves
GDM's `FallbackSession` path only. `scripts/tuna-vm-lane --fresh-accounts`
is a separate boot of the same disk (QEMU `-snapshot`) that passes the
`tuna.vm.fresh-accounts` systemd credential (SMBIOS type 11). Only then does
`tuna-vm-fresh-accounts.service` run `useradd -m` before GDM starts, as an
installer would: `tunastd` (standard), `tunaadmin` (`wheel`), and the
control `tunaseeded`, whose record is pre-seeded. `tunastd` and `tunaadmin`
get no AccountsService record, no Session preselection and no autologin.

| Gate | Actual operation and evidence |
| --- | --- |
| V-FRESH-RECORD-FREE-<user> | Before any login: account exists with a home, no `/var/lib/AccountsService/users/<user>`, no GDM automatic/timed login, `wheel` only for the admin |
| V-FRESH-CONTROL | The same probe classifies the pre-seeded `tunaseeded` as NOT record-free |
| V-FRESH-LOGIN-<user>-{first,second,reboot} | GDM "Not listed?" name and password typed through QMP; exactly one `gdm-password` VT session runs `tuna-compositor` and no `gnome-shell`/`gnome-session` |
| V-FRESH-RECORD-CREATED-<user> | After the first login AccountsService wrote the record; any saved `Session` is `tuna` |
| V-FRESH-SUDO-<user> | `sudo -S -k true` as the account with its password: admin succeeds, standard is refused by sudo policy |
| V-FRESH-LOGOUT-<user>-<round> | logind ends the session and the greeter is ready again |
| V-FRESH-REBOOT / V-FRESH-RECORD-KEPT-<user> | In-guest reboot (new `boot_id`), records survive, then both accounts log in again |

Receipts are in `fresh-accounts-receipts.json`; frames per attempt and
session. Only Arch/GDM 51 is covered here; pre-51 GDM is not exercised by
this lane.

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
| V-POINTER-LIVE | GNOME mouse speed/natural-scroll, acceleration profile (`flat`, then `adaptive`), middle-click emulation (on, then off) and scroll-wheel emulation (button 2 → `on-button-down` with BTN_MIDDLE and button lock, then off) reach live libinput device state and restore, asserted on the compositor's per-device readback. Each option needs at least one mouse offering it; wheel emulation is asserted on mice offering button scrolling and BTN_MIDDLE in `pointer_buttons` (libinput rejects a button the device lacks). Both source-built and published guests must report profile/button state and provide GNOME 51's wheel-emulation schema; absence fails rather than skipping proof. Snapshots: lifecycle/pointer-live.json (#343) |
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
| V-LOCKED-HINT | logind's `LockedHint` for the login session reads `no`; the session user's `loginctl lock-session` locks the compositor and the hint turns `yes`; the real password unlocks and it reads `no`; Super+L locks with `yes`; `loginctl unlock-session` unlocks with `no`, as GNOME 51's `screenShield.js` does (lifecycle/locked-hint.json, #846). Required on both acquisitions |
| V-POWER-BUTTON | gsd-media-keys (its unit's main PID, the session user) holds logind's `block` inhibitor on `handle-power-key:handle-suspend-key:handle-hibernate-key` with Why "GNOME handling keypresses". On a fixture desktop chassis, after each gsd restart a new gsd-media-keys must own its name, hold that inhibitor, have `XF86PowerOff` granted by `GrabAccelerators` and the shell must log that it yielded the bindings. QMP `system_powerdown` (ACPI power button) with `power-button-action` `interactive`: `AcceleratorActivated` reaches gsd, gsd calls `org.gnome.SessionManager.Shutdown`, the compositor calls `EndSessionDialog.Open(1, …)` and the `tuna-shell-end-session` layer shows within 15 s; 5 s after the press QEMU is `running` with the same compositor process and logind session and no logind power request; Escape (always sent, far inside the 60 s countdown) removes the layer, the shell emits `Canceled` and logs its D-Bus Power Off dialog. `nothing`: gsd receives the key, and for at least 8 s no Shutdown, dialog call, layer or logind request follows. On the image's `vm` chassis with a fixture `shutdown` block inhibitor held by root: the key reaches gsd and gsd's own process calls logind `PowerOffWithFlags(0)`; gsd sends it without a reply callback (NO_REPLY_EXPECTED), so logind's refusal has no bus reply, and 5 s later logind has emitted no `PrepareForShutdown`/`PrepareForSleep`, `PreparingForShutdown` is still false with the fixture inhibitor held, no other logind request or non-error reply followed, no dialog opened and QEMU, compositor and session are unchanged. Setting, chassis and `/etc/machine-info` are restored (lifecycle/power-button.json, #353, #589) |
| V-LOGOUT | The product's Log Out must use `org.gnome.SessionManager.Logout(1)` on the session bus and end the compositor (session leader) through orderly shutdown, as gnome-session's exit ends a GNOME session. Within 30 s GDM's non-root gnome-shell greeter in its active gdm-launch-environment session repaints the pre-login greeter frame and no original compositor survives. A logind `Session.Terminate` substitute does not pass: signaling GDM's worker with the compositor previously left GDM without a visible greeter on the session VT (published runs 40883859027 and 40940329310 attempt 1) |

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
