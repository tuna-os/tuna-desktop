# Installing Tuna Desktop

> **Backends.** `tuna-compositor --backend auto` (the default) runs as a
> DRM/KMS hardware session when started without a host display, for example
> as a GDM session or from a TTY, and as a nested window otherwise. A hardware
> session needs a seat from logind or seatd. Hardware support is new: it is
> proven on the kernel's virtual KMS device in CI (`scripts/tuna-drm-smoke`)
> and not yet qualified on real GPUs; the cursor is drawn in software and
> client cursor images are not composited yet.

## Test-drive in a VM (one command)

To evaluate Tuna Desktop without installing packages or compiling from source:

```sh
curl -fsSL https://raw.githubusercontent.com/tuna-os/tuna-desktop/main/scripts/try-tuna-desktop | bash
```

This downloads Corral (checksum-verified), pulls and cosign-verifies the published TunaOS image, and launches Tuna Desktop in local QEMU with a local-only browser console over noVNC. Stop anytime with Ctrl-C. See `scripts/try-tuna-desktop` for options (`TRY_TUNA_MEM`, `TRY_TUNA_PORT`, etc.).

## From the Debian-format package

The package ships the preferred GTK shell beside the compositor. The first
installed-package CI target is Ubuntu 26.04 amd64. CI includes a clean
`ubuntu:26.04` container install with only declared dependencies and installed
binary version checks; real session and hardware qualification remain
separate acceptance requirements. Packages must be built against their target
distribution's libraries. The required gtk4-layer-shell version is at least
1.1; [Debian 13 stable provides 1.0.4](https://packages.debian.org/trixie/libgtk4-layer-shell0),
so that target still needs a genuine packaged backport and separate installed
session qualification. [Ubuntu 26.04 provides 1.3.0](https://packages.ubuntu.com/resolute/amd64/libgtk4-layer-shell0).
A package built on Ubuntu does not establish Debian 13 ABI compatibility.

```sh
sudo apt install ./tuna-desktop_X.Y.Z_amd64.deb
```

Dependencies resolve automatically on a target with the declared runtime
packages available. After installing, the Tuna Desktop session is
selectable from the login screen. Upgrading installs the newer package over
the older one; the [legacy-upgrade guide](legacy-upgrade.md) explains the
temporary two-file bridge, explicit session reselection, preserved settings
and conflict recovery. Package replacement alone does not qualify login or
unlock on an existing session. In particular, do not rely on it to repair an
administrator-removed PAM service. The guide lists the native tests still
required for each supported route. If you uninstall, removing the package
drops the Tuna Desktop-owned program files and session entries while
preserving user data. See `docs/release.md` for the release process.

## From source

Prerequisites mirror CI (`check` job plus the display libraries the nested
backend dlopens): a stable Rust toolchain, `libxkbcommon-dev`,
`libxkbcommon-x11-dev`, `libegl1`, `libgl1-mesa-dri`, `libgbm1`,
`pkg-config`. The GTK shell also needs `libgtk-4-dev`, `libadwaita-1-dev`
and gtk4-layer-shell (`scripts/ci-install-gtk4-layer-shell` builds it
where the distribution has no package).

```sh
cargo build --release \
  -p tuna-compositor -p tuna-shell-host -p tuna-shell-gtk
```

Binaries land in `target/release/`: `tuna-compositor`, `tuna-shell-gtk`,
`tuna-shell-host`, and `tuna-session` (the last is a second binary target of
the compositor crate).

## Installing

```sh
PREFIX=/usr/local
install -Dm0755 target/release/tuna-compositor "$DESTDIR$PREFIX/bin/tuna-compositor"
install -Dm0755 target/release/tuna-shell-gtk  "$DESTDIR$PREFIX/bin/tuna-shell-gtk"
install -Dm0755 target/release/tuna-shell-host "$DESTDIR$PREFIX/bin/tuna-shell-host"
install -Dm0755 target/release/tuna-session   "$DESTDIR$PREFIX/bin/tuna-session"
install -Dm0644 share/wayland-sessions/tuna.desktop \
  "$DESTDIR$PREFIX/share/wayland-sessions/tuna.desktop"
```

Keep the binaries side by side: `tuna-session` finds its
`tuna-compositor` sibling by directory. The compositor picks its shell
from `TUNA_SHELL_BIN` when set, then `tuna-shell-gtk` and then
`tuna-shell-host` beside it, then the same two on `PATH`. Set
`TUNA_SHELL_BIN=tuna-shell-host` to run the legacy shell.

## Logging in through GDM

GDM is the supported login manager. It lists the entries in
`/usr/share/wayland-sessions`: choose **Tuna Desktop** from the session menu
on the login screen. The entry's `Exec=` line is `tuna-session`, never the
compositor directly, so shell supervision stays in the loop, and
`X-GDM-SessionRegisters=true` tells GDM the session registers itself.

The lock screen authenticates through the `tuna-lock` PAM service. The
packages install it; a source install needs it too, or PAM falls back to the
`other` service (deny on most distributions) and a locked session cannot be
unlocked:

```sh
install -Dm0644 share/pam.d/tuna-lock /etc/pam.d/tuna-lock
```
