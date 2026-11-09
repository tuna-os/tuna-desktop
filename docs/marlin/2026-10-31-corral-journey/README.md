# Published Marlin beta.2: Corral daily-driver journey, 2026-10-31

## Subject and provenance

```text
ghcr.io/tuna-os/marlin:tuna
  OCI index sha256:d0e65bce0ac55cb6c58bcf3739e30d6372fe4be978e6ea7c0c1310b3962bdfa5
  amd64 manifest sha256:9706311b7850d4dfcbf312b996a98793f9f0581d200fbe08c1c344924d49b691
  image created 2026-10-31T09:55:45.753372452Z
  guest package tuna-desktop 1.0.0beta.2-2
```

`skopeo inspect docker://ghcr.io/tuna-os/marlin:tuna` resolved that index.
The guest's bootc reference and manifest match it. The image came from
[tunaOS build 41188180474](https://github.com/tuna-os/tunaOS/actions/runs/41188180474),
which passed, after [tunaOS#4122](https://github.com/tuna-os/tunaOS/pull/4122)
merged as `dfa6c57a72be3d083d29ef1a94e2193a0cae2ded`.

The repos' beta.2 release-1 packages still contained `tuna-greeter`: they
predated tunaos-packages#950. [tunaos-packages#952](https://github.com/tuna-os/tunaos-packages/pull/952)
bumped the recipe and spec to release 2. All eleven Tuna variants now pin
that rebuild: Arch `1.0.0beta.2-2`; EL10 `1.0.0~beta.2-2.el10`; Fedora 44
`-2.fc44`; Rawhide `-2.fc46`; ELN `-2.eln159`; Tumbleweed `-2`; Ubuntu,
Debian sid and trixie `1.0.0~beta.2-2`. All served package file lists were
checked. The three amd64 deb payloads were also extracted. None contains
`tuna-greeter`.

After this journey, `corral ssh` ran `pacman -Q tuna-desktop`, checked
both file and symlink absence of `/usr/bin/tuna-greeter`, and listed the
package files. It reported `TUNA_GREETER_ABSENT`; the six installed
binaries are in [package-and-cleanup.json](package-and-cleanup.json).

## Full journey

[The complete run](daily-driver/manifest.json) ran from 12:31:41 to
12:45:45 UTC on `tuna-q-beta2-20261031c`, node `karnataka`, through Corral
`76b6dd52d35d`. The VM had 4 vCPUs, 6 GiB RAM, a 40 GiB disk, UEFI/q35,
a USB tablet and bochs-drm at 1280×800, without a GPU or backlight.
GDM listed the fixture account and the sole Tuna Desktop session. No
in-image session binaries were replaced. Only Corral commands touched
the VM; no direct kubectl or virtctl commands were used.

```sh
scripts/tuna-corral-journey \
  --image ghcr.io/tuna-os/marlin@sha256:d0e65bce0ac55cb6c58bcf3739e30d6372fe4be978e6ea7c0c1310b3962bdfa5 \
  --vm tuna-q-beta2-20261031c --node karnataka \
  --out docs/marlin/2026-10-31-corral-journey/daily-driver --keep
```

Raw result: **24 passed, 3 failed, 0 not exercised**. One failure is the
VM's absent backlight, not a product regression. The other two are real
previously reported failures. The raw receipt is unchanged.

| Frames | Step | Observed result |
|---|---|---|
| 01–03 | GDM user list, password, Tuna login | pass; `gdm-password` Wayland session, Tuna compositor/shell, no user GNOME Shell |
| 04–07 | Desktop, overview, app grid, Files search | pass |
| 08–09 | Launch Files and Settings | pass; both processes present |
| 10–12 | Alt+Tab held, commit, switch back | pass |
| 13–15 | Appearance, Dark, wallpaper | pass; `prefer-dark`, changed `picture-uri` |
| 16 | Quick settings | script pass; **visual bug #844 remains**: Dark Style tile is off while Settings and GSettings are dark |
| 17 | Night Light | **pass**; setting true and `org.gnome.SettingsDaemon.Color` owned by `tuna-compositor` |
| 18 | Volume/backlight availability | raw fail: PipeWire Dummy Output exists, but zero backlight devices; no hardware slider qualification |
| 19 | `notify-send` | **fail**, `Unknown method 'GetServerInformation'` (#843) |
| 20 | D-Bus `Notify` | pass, returns notification id 1 and shows banner |
| 21 | Date menu | script pass; **visual bug #845 remains**: banner draws over open menu |
| 22 | Lock | **fail**: ScreenSaver active true but logind `LockedHint=no` (#846) |
| 23–24 | Wrong password, correct password | pass; refused password stays locked, correct password unlocks |
| 25 | Power menu | script pass; **visual bug #847 remains**: entire popover translucent |
| 26–27 | Logout dialog and logout | pass; user session ends and GDM returns on seat0 |

The visual findings above are direct review of frames 16, 21 and 25,
not automated pixel judgments. Frames 01, 03 and 22 also have independent
Corral screenshot cross-checks in the manifest.

## Comparison with beta.1

Compared with [2026-10-30](../2026-10-30-corral-journey/), Night Light
#339 now passes its service and setting checks. **The other five expected
fixes do not pass on this published beta.2 payload**: #843 and #846 still
fail their semantic checks; #844, #845 and #847 still reproduce visually.
This is not a claim about current source main. The image runs the tagged
beta.2 source, with only a package release rebuild to remove the greeter.

No new product bug was established. The slider failure is a hardware
capability limitation; a Dummy Output is not proof of physical audio.
Software rendering cannot qualify hardware gamma, GPUs, multiple outputs,
hotplug, suspend, audio devices or brightness hardware.

## Two interrupted attempts

The root manifest/frames and `complete/` manifest/frames are **interrupted
attempts**, not the full qualification. Both passed steps 01–16, then the
old beta.1 Night Light pointer coordinate clicked beta.2's Wired tile and
disabled networking. Subsequent SSH checks failed with broken-pipe /
connection-closed errors. This was a journey coordinate bug, not evidence
of a Night Light or SSH product failure. The runner now clicks beta.2's
Night Light at `(1165, 237)`, row 2 right; the `daily-driver/` run above
completed all 27 steps with that correction.

The first VM's deletion is in its manifest. The two retained VMs were
removed with `corral delete --wait`, including their VMIs and PVCs.
`corral --json list --prefix tuna-q-beta2-20261031` returned `[]`.

## Published-image nightly

Dispatched [main run 41199130139](https://github.com/tuna-os/tuna-desktop/actions/runs/41199130139)
with the exact index digest above (`digest` input), at desktop source
`83bf62fc71870ef84162d411cdd8630a43e280cd`.
At handoff, image resolution and fixture installation passed. **Real shipped
GDM PAM login, semantic tour and lifecycle failed.** The bounded soak is
still in progress; per-check publication and artifact uploads are pending.
The nightly is therefore not a passing gate result. Its final soak result
and finer-grained failure diagnostics are not yet available.
