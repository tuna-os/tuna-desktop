# Published-image journey through Corral

`scripts/tuna-corral-journey` walks the daily-driver journey on a
published Tuna flavor in a real VM, the way a person would: at GDM, with
the keyboard and the pointer, through the VM's console. It complements
the [published-flavor nightly](marlin-tuna-nightly.md), which checks the
same image headlessly over QMP and SSH, by leaving a browsable set of
console frames for each step.

## What it does

1. `corral bootc create` installs the image, pinned by digest, into a
   KubeVirt VM: 4 vCPUs, 6 GiB, a 40 GiB disk, q35 with UEFI and
   KubeVirt's default display (bochs-drm at 1280x800, no GPU). The run
   stops if the booted image differs from the requested digest.
2. A `tuna-test` account (password `tuna`, the VM lane's public fixture
   account) is added at runtime over `corral ssh`. It gets an
   AccountsService record that lists it in GDM's user list and names no
   session, so the shipped image alone decides which session starts. The
   image itself is not changed.
3. The script starts `corral web` on a free port. Playwright opens the
   VM's noVNC console in that UI at 1:1 scale. Each frame is the canvas's
   own pixels. Three steps also capture the same frame through KubeVirt's
   VNC screenshot (`virtctl vnc screenshot`) as an independent check.
4. The journey: GDM user list, password, Tuna session (logind
   `gdm-password` session, `tuna-compositor` and `tuna-shell-gtk`
   running, no `gnome-shell`), overview, app grid, search and launch
   Files and Settings, Alt+Tab, Settings > Appearance dark style and
   background, quick settings and Night Light, the volume and brightness
   sliders, `notify-send` and a D-Bus `Notify`, the date menu, lock with
   Super+L, a refused wrong password, unlock, and log out back to GDM.
5. Every step writes its frame, its guest-side checks, and the `tuna-*`
   journal lines it caused to `manifest.json` with timestamps. A step's
   result is `pass`, `fail` or `not-exercised` (with the reason). The run
   then deletes the VM and its disk unless `--keep` is given.

The script checks what the guest can report: processes, logind,
GSettings, D-Bus replies. It does not judge pixels. Whatever only a frame
can show, such as a tile's state or a banner covering a menu, needs a
person to look at the frame. Record that review next to the run.

## Running it

You need `corral` with its bootc plugin, a KubeVirt context
(`corral doctor`), `kubectl`, `virtctl`, and Playwright for Python:

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
built from the same digest. `--web URL` uses a `corral web` that is
already running. The exit status is nonzero when any step fails.

## Limits

- The pointer coordinates in `POINTS` match the 1280x800 output and
  the Tuna Desktop 1.0.0 beta layout. The run fails at its first step
  when the console reports another size. A layout change means updating
  that table.
- `corral bootc create` up to Corral 0dfd85a gives KubeVirt guests only
  a relative PS/2 mouse, so noVNC clicks drift. The script adds a USB
  tablet to the VM and records `tablet_added_by_run` in the manifest.
  [tuna-os/corral#659](https://github.com/tuna-os/corral/pull/659) adds
  the tablet in Corral.
- KubeVirt VMs have no sound card and no backlight, so the volume and
  brightness sliders are reported as `not-exercised`.
- There is no GPU. These frames come from bochs-drm with software
  rendering, so they say nothing about GPU drivers, multiple outputs,
  hotplug or suspend.
