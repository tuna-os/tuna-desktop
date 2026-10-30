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

## Limits

- The pointer coordinates in `POINTS` match the 1280x800 output and
  the Tuna Desktop 1.0.0 beta layout. The run fails at its first step
  when the console reports another size. A layout change means updating
  that table.
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
