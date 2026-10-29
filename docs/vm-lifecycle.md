# Marlin VM lifecycle proof

The existing `marlin-vm` CI job installs the ordinary Marlin preview image,
then a CI-only fixture layer. The fixture starts `tuna-test` once through
greetd's initial session. Its default greeter is Cage with normal-window
gtkgreet, matching the measured TunaOS launcher; logout must reach that
actual greeter. This changes neither the shipped preview image nor the
number of CI VM profiles.
The CI session wrapper routes its output through `systemd-cat` so greetd's
VT output reaches the fixture's journal-to-serial capture.

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
| V-LIFECYCLE-OWNER | Root probe, non-root login session (greetd on the CI fixture, gdm-password on the published flavor), three application surfaces/processes |
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
| V-POWER-BUTTON | gsd-media-keys (its unit's main PID, the session user) holds logind's `block` inhibitor on `handle-power-key:handle-suspend-key:handle-hibernate-key` with Why "GNOME handling keypresses". On a fixture desktop chassis, after each gsd restart a new gsd-media-keys must own its name, hold that inhibitor, have `XF86PowerOff` granted by `GrabAccelerators` and the shell must log that it yielded the bindings. QMP `system_powerdown` (ACPI power button) with `power-button-action` `interactive`: `AcceleratorActivated` reaches gsd, gsd calls `org.gnome.SessionManager.Shutdown`, the compositor calls `EndSessionDialog.Open(1, …)` and the `tuna-shell-end-session` layer shows within 15 s; 5 s after the press QEMU is `running` with the same compositor process and logind session and no logind power request; Escape (always sent, far inside the 60 s countdown) removes the layer, the shell emits `Canceled` and logs its D-Bus Power Off dialog. `nothing`: gsd receives the key, and for at least 8 s no Shutdown, dialog call, layer or logind request follows. On the image's `vm` chassis with a fixture `shutdown` block inhibitor held by root: the key reaches gsd and gsd's own process calls logind `PowerOffWithFlags(0)`; gsd sends it without a reply callback (NO_REPLY_EXPECTED), so logind's refusal has no bus reply, and 5 s later logind has emitted no `PrepareForShutdown`/`PrepareForSleep`, `PreparingForShutdown` is still false with the fixture inhibitor held, no other logind request or non-error reply followed, no dialog opened and QEMU, compositor and session are unchanged. Setting, chassis and `/etc/machine-info` are restored (lifecycle/power-button.json, #353, #589) |
| V-LOGOUT | Terminate the real logind session; the display manager's non-root greeter (gtkgreet under active greetd on the CI fixture; GDM's gnome-shell greeter in its active gdm-launch-environment session on the published flavor) repaints and no original compositor survives |

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
