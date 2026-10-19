# Packaging

| Target | Path | Built by |
|---|---|---|
| Debian / Ubuntu `.deb` (Ubuntu 26.04) | `scripts/tuna-release` | CI `journey` job |
| Arch Linux package (TunaOS Marlin) | `packaging/arch/PKGBUILD` | CI `arch-package` job |
| Marlin preview image | `packaging/marlin/Containerfile` | CI `marlin-image` job (main) |

All three stamp the version from the git tag, and every binary's
`--version` must match it; `crates/compositor/tests/release_contract.rs`
pins that for all three.

## TunaOS Marlin

Marlin (`ghcr.io/tuna-os/marlin`) is TunaOS's Arch Linux bootc variant
and Tuna Desktop's first platform. Its GNOME image ships GNOME 51, Tuna Desktop's
parity baseline. The preview image layers the Tuna Desktop package on
`marlin:gnome`, so baseline and candidate share one image family.

The preview image offers "Tuna Desktop (preview)" on the GDM login screen
beside GNOME: the DRM/KMS backend runs the hardware session (#52), the
lock screen unlocks through PAM (`/etc/pam.d/tuna-lock`, #62), and the
power menu logs out back to GDM. A "Tuna Desktop (nested preview)" launcher
also opens Tuna Desktop in a window inside GNOME. Upstreaming a `marlin:tuna`
flavor to `tuna-os/tunaOS` (#69) follows once hardware runs in the VM
lane (#68) are green.

The Arch package and `.deb` ship both shells. The compositor runs the
GTK4/libadwaita shell (`tuna-shell-gtk`, ADR 0006) when it is installed
beside it and falls back to the legacy `tuna-shell-host`; set
`TUNA_SHELL_BIN=tuna-shell-host` to choose the legacy shell. The
`.deb` targets Ubuntu 26.04; Debian 13 requires a separate packaged
backport and installed-session qualification (see `docs/install.md`).

The release builder uses `dpkg-deb --root-owner-group`. CI verifies original
archive ownership with `scripts/check-release-package`, then runs
`scripts/check-deb-clean-install` on a pristine `ubuntu:26.04` container
without recommendations. Installed GTK session and real greeter acceptance
are separate from this clean dependency and binary-version check.

Try the preview on a Marlin machine:

```sh
sudo bootc switch ghcr.io/tuna-os/tuna-desktop/marlin-tuna-preview:latest
```

## Upgrading from the former name

<!-- tuna-rename: keep-begin -->

The rename landed in #536; #505 tracks its compatibility transition. The
package is `tuna-desktop` (not `tuna`: Fedora ships an unrelated package),
with replacement/conflict/provision relations for `roost`. Those relations
do not migrate a saved login choice, user data or a running executable.

Fresh and upgraded packages temporarily retain exactly two regular legacy
paths: `/usr/share/wayland-sessions/roost.desktop` and
`/etc/pam.d/roost-lock`. The hidden session entry executes `tuna-session`;
it does **not** change GDM's remembered desktop-file name. Explicitly choose
**Tuna Desktop** at a later login and verify the next login remembers it.
The old PAM name remains necessary for an already-running pre-rename
compositor. Old binary symlinks and `ROOST_*` environment aliases stay removed.

The compositor startup guard adopts safe old-only settings with relative
links, including when the beta was installed but never launched. It stops
on conflicting or unsafe state instead of overwriting, merging or deleting
data. Debian conffiles, Arch backup files and Marlin deployments have
different preservation/recovery behavior; none is replaced with a cleanup
hook. Read the [upgrade and recovery guide](../docs/legacy-upgrade.md),
including its explicitly unqualified native routes and bridge-removal gates.
<!-- tuna-rename: keep-end -->

Other branches rename themselves with `scripts/rename-to-tuna`.
