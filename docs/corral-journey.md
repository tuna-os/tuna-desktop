# Published-image journey through Corral

`scripts/tuna-corral-journey` walks the daily-driver journey on a
published Tuna flavor in a real VM, the way a person would: at GDM, with
the keyboard and the pointer, through the VM's console. It complements
the [published-flavor nightly](marlin-tuna-nightly.md), which checks the
same image headlessly over QMP and SSH, by leaving a browsable set of
console frames for each step.

## What it does

1. `corral bootc create` installs the image, pinned by digest, into a
   KubeVirt VM: 4 vCPUs, 6 GiB, a 40 GiB disk, q35 with UEFI, a USB
   tablet for an absolute pointer, and KubeVirt's default display
   (bochs-drm at 1280x800, no GPU). The same command creates the
   `tuna-test` account (password `tuna`, the VM lane's public fixture
   account) in the installed system with `--user` and
   `--password-stdin`. The image itself is not changed. The run stops if
   the booted image differs from the requested digest.
2. `corral wait` holds the run until the boot is complete, GDM is
   active, its greeter session is on seat0, and the screen is painted.
   `corral info --json` gives the VM facts for the manifest.
3. `corral console` serves the VM's own console page on a free port and
   prints its URL. Playwright opens that page. The page shows the noVNC
   canvas at 1:1, so each frame is the canvas's own pixels. Three steps
   also capture the same frame with `corral screenshot` as an
   independent check.
4. The journey: GDM user list, password, Tuna session (logind
   `gdm-password` session, `tuna-compositor` and `tuna-shell-gtk`
   running, no `gnome-shell`), overview, app grid, search and launch
   Files and Settings, Alt+Tab, Settings > Appearance dark style and
   background, quick settings and Night Light, the volume and brightness
   sliders, `notify-send` and a D-Bus `Notify`, the date menu, lock with
   Super+L, a refused wrong password, unlock, and log out back to GDM.
   Native portal requesters also check frontend availability, actual Screenshot
   delivery, and a ScreenCast PipeWire frame with caller isolation and bounded
   withdrawal on the actual Super+L lock; no candidate desktop binaries are copied.
5. Every step writes its frame, its guest-side checks, and the `tuna-*`
   journal lines it caused to `manifest.json` with timestamps. A step's
   result is `pass`, `fail` or `not-exercised` (with the reason). Then
   `corral delete --wait` removes the VM, its disks and any build
   leftovers, and `corral list` must no longer show the VM. Give `--keep`
   to keep the VM.

The script checks what the guest can report: processes, logind,
GSettings, D-Bus replies. It does not judge pixels. Whatever only a frame
can show, such as a tile's state or a banner covering a menu, needs a
person to look at the frame. Record that review next to the run.

## Running it

You need `corral` with its bootc plugin (`corral doctor` must pass for
the KubeVirt context) and Playwright for Python. The script does not use
`kubectl` or `virtctl`:

```sh
python3 -m venv ~/.cache/tuna-journey-venv
~/.cache/tuna-journey-venv/bin/pip install playwright pillow
~/.cache/tuna-journey-venv/bin/python -m playwright install chromium
~/.cache/tuna-journey-venv/bin/python scripts/tuna-corral-journey \
  --image ghcr.io/tuna-os/marlin@sha256:<index digest> \
  --node <node> --out docs/marlin/<date>-corral-journey
```

Pass the image by digest; resolve the tag first with
`skopeo inspect docker://ghcr.io/tuna-os/marlin:tuna`. VM names must
start with `tuna-q-` (the default is `tuna-q-<date>`), so the script
cannot name somebody else's VM. `--reuse` restarts a VM that is already
built from the same digest. The exit status is nonzero when any step
fails.

For the all-variant install/GDM/session/portal/lock/unlock/logout
subset, use `--qualification-only`; it omits the wider app/Settings/notification
tour and records `scope: qualification-core` in the manifest. The default
remains the full daily-driver journey. See the
[digest-pinned variant matrix](v1-variant-qualification.md) for sequential
orchestration and the distinction between portal availability and functional
capture/consent qualification.
The native Screenshot requester remains connected until the actual portal
Response and retains a returned PNG, without app-ID or consent bypass. Its
request-time console frame requires visual review; a successful PNG response
does not by itself prove a consent prompt, ScreenCast stream or revocation.
The separate ScreenCast requester consumes the real remote PipeWire stream and
holds it until the actual Super+L step. Its existing two-second withdrawal
observer is armed before the key, never replaced with explicit Session.Close.
The guest-side coordinator batches launch, ready/exit status and the initial
ScreenSaver state into one SSH exchange. After the host retains the prompt,
one further exchange checks that the same requester is still pending,
activates only its expected control (Screenshot Allow or ScreenCast Share),
and returns the helper tree, native status and response. For ScreenCast that
same guest operation observes the native active marker, checks unlocked state
and arms withdrawal. The very next host input is actual noVNC Super+L; PNGs
and per-step journals are collected afterwards. There are no intervening
SSH probes for exit, tree, active state, hashes or the revoke marker.
The native 60-second request/30-second grant hold and two-second withdrawal
bounds are unchanged, including arming the observer before the actual key.
The coordinator is frozen with the native requester sources, and receipts
retain their exact argv, source hashes, output and exit status. A failed or
expired requester never authorizes clicking a stale picker from another kind
of request; failure cleanup targets only its pinned, owned process group.

Capture probes require `org.gnome.ScreenSaver.GetActive=false` before the
request and after its response; both replies are retained in `session_state`.
Real pointer motion between slow guest probes keeps an unlocked session
active without changing any idle/lock setting or unlocking a locked session.
The external-capture AT-SPI helper checks ScreenSaver again immediately before
activating Allow/Share: a widget can report `STATE_SHOWING` while the compositor
lock curtain covers it. A locked or unreadable state fails the precondition,
not the capture-provider gate. No portal request, response or withdrawal
deadline is increased.

The [installed Yellowfin boundary receipts](variants/2026-10-31/unlocked-portals/README.md)
show why this distinction matters: the unchanged published payload returns
a real Screenshot URI and PipeWire frame when unlocked, but denies capture
when locked. GNOME's backend translates the locked Shell Screenshot
`(false, "")` into portal Response 0 with an empty URI; that is not an image
delivery. AT-SPI activation behind the curtain is not visible user consent.

Provisioning retains complete bootc builder stdout/stderr and exit status
(including partial output on the unchanged outer timeout), rather than only a
truncated exception. Console-start failure retains the complete Corral stderr;
long-lived console output is backed by a file, not an undrained stderr pipe.

## Limits

- The pointer coordinates in `POINTS` match the 1280x800 output and
  the Tuna Desktop 1.0.0-beta.2 layout. Night Light is row 2, right.
  The beta.1 coordinate now hits Wired and disconnects the VM's network.
  The run fails at its first step when the console reports another size.
  A layout change means updating that table.
- The script needs a Corral that has `corral console`, `corral wait`,
  `corral delete --wait`, `corral info --json` and `bootc create --user`
  (tuna-os/corral main after
  [#627](https://github.com/tuna-os/corral/pull/627)). Install it with
  Corral's `scripts/install.sh`, then `corral plugin install bootc`.
- KubeVirt VMs have no sound card and no backlight, so the volume and
  brightness sliders are reported as `not-exercised`.
- There is no GPU. These frames come from bochs-drm with software
  rendering, so they say nothing about GPU drivers, multiple outputs,
  hotplug or suspend.
