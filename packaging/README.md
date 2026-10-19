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

Tuna Desktop was called Roost until #505. The package is now
`tuna-desktop` (not `tuna`: Fedora and others ship an unrelated `tuna`),
and both packages declare `replaces`/`conflicts`/`provides=roost`, so an
upgrade swaps it in place. The beta's one-release compatibility layer
(the `/usr/bin/roost-*` symlinks, the `roost.desktop` session entry, the
`roost-lock` PAM service, the `ROOST_*` environment names, and the old
per-user directory adoption) was removed after that release: fresh
installs carry no `roost` names, and only the package relations above
still mention it. GDM users who upgraded through the beta keep their
remembered session because the beta's entry carried them over to `tuna`.
<!-- tuna-rename: keep-end -->

Other branches rename themselves with `scripts/rename-to-tuna`.
