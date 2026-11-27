# ADR 0001: Nested backend and pinned Smithay/protocol revisions

- Status: Decided (nested preview proven 001 T1–T5; production review stays a 004 gate);
  amended 2026-11-13: keep and extend the vendored 0.7.0 fork (#1047)
- Date: 2026-09-27
- Owner: project (unassigned)
- Dependent speks: 001 implementation (blocks compositor implementation
  per roadmap gate 1); inputs to 000 baseline, 002, 003.

## Context

Roadmap gate 1 requires a recorded provisional nested backend,
Smithay/protocol versions, and event-loop approach before 001
implementation starts. The 001 research questions asked for the exact
Smithay release, nested backend API, feature flags, rendering paths,
calloop integration, and protocol revisions.

## Decision

Build the nested developer preview on **smithay 0.7.0** with features
`backend_winit`, `wayland_frontend`, `desktop`, on **calloop 0.14.4**,
against **wayland-protocols 0.32.13**, **wayland-server 0.31.14**,
**wayland-client 0.31.15** (transitively wayland-backend 0.3.17,
winit 0.30.13). Protocol matrix for the pin: xdg-shell v7, viewporter
v1, presentation v2, linux-dmabuf v6, xdg-activation v1 (plus
fractional-scale/tearing/cursor-shape/single-pixel-buffer/xdg-dialog
v1–v2, ext-session-lock v1, explicit-sync v2, drm-syncobj v1 for later
slices). Upstream reference revision is git tag `v0.7.0`.

## Alternatives considered

- smithay 0.5.x line: older, lacks
  `XdgActivationState::create_external_token` and the calloop-native
  `WinitEventLoop`; rejected.
- Waiting for a future release: no dated need; 0.7.0 is the latest
  unyanked release and resolves cleanly. The 2026-11-13 amendment
  below replaces the original revisit condition (0.7.x yanked, or a
  later release changing the winit/calloop API).

## Consequences

- Nested rendering is GLES-over-EGL only (no software fallback in the
  winit backend); dev/CI environments must provide EGL (llvmpipe OK).
- This is a prototype pin, not a production decision: same-UID
  limitations are development-only and the threat-model review stays a
  004 gate.
- Follow-ups: EGL runtime probe on dev hardware; anvil/upstream-test
  review at tag `v0.7.0`.

## Amendment 2026-11-13: keep and extend the vendored 0.7.0 fork (#1047)

Colour management and HDR (#341) need protocols Smithay 0.7.0 does not
have, so #1047 asked whether to stay on the fork or track upstream. The
2026-11-10 audit of the fork against upstream master
([#1047 audit](https://github.com/tuna-os/tuna-desktop/issues/1047#issuecomment-6780395901))
found:

- No Smithay release newer than 0.7.0 exists (GitHub tags and
  crates.io); tracking upstream means a git pin with no semver
  guarantees.
- Of the protocols `docs/protocols.md` lists as blocked by Smithay,
  master unblocks only `xdg_wm_base` 7, `ext_background_effect` and
  `wl_fixes`. Colour management and representation have only stale
  upstream drafts. DRM lease, DRM syncobj and dmabuf feedback v4+ are
  already in 0.7.0; the gap is Tuna wiring.
- Master carries breaking rewrites (`Dispatch2`, DnD, virtual keyboard,
  input locking) of exactly the code the Tuna patches touch.

Decision ([#1047 decision](https://github.com/tuna-os/tuna-desktop/issues/1047#issuecomment-6843130799),
hanthor, 2026-11-13): keep the vendored Smithay 0.7.0 fork and keep
extending it; do not track upstream git master now.

- Build colour management and HDR (#341) as new isolated patches in
  `third-party/smithay`, one `*-PATCH.diff` per feature, each described
  in `third-party/smithay/ROOST-PATCH.md`.
- Take the wins that need no Smithay move: wire the DRM lease, DRM
  syncobj and dmabuf feedback v4 globals 0.7.0 already ships, and
  backport `wl_fixes` (about 75 lines) if the locked wayland-server
  0.31.14 has `destroy_object`.
- Offering a generic Tuna patch upstream (for example
  `PER-FRAME-READ-FENCE`, `SLEEP-RESET-API`) is welcome but blocks
  nothing.

Re-check trigger: re-evaluate when Smithay publishes a release newer
than 0.7.0 on crates.io, or when upstream merges colour management. At
that point rebase onto the release and drop the patches it absorbs; the
2026-11-10 audit lists which Tuna patches have an upstream equivalent.

Consequences: the cost of the fork grows with each patch, and the
eventual rebase meets the `Dispatch2`, DnD, virtual-keyboard and input
rewrites in one step. Moving now would cost that rebase plus a
semver-less git pin and still not unblock #341.

## Evidence

- crates.io index/API release dates; `cargo generate-lockfile` +
  `cargo check` on rustc 1.94.1, 2026-09-27 (lockfile excerpt:
  `.spektacular/work/gate1-research/pin-lockfile-excerpt.txt`).
- smithay-0.7.0 crate source (`backend/winit`, `wayland/socket.rs`,
  `examples/minimal.rs`); wayland-protocols-0.32.13 vendored XML.
- Knowledge: `learnings/smithay-070-pin-set.md`,
  `gotchas/winit-backend-requires-egl.md`,
  `decisions/smithay-070-for-nested-slice.md`.
- 001 implementation (T1–T5): the winit/calloop loop runs nested
  sessions under Xvfb with EGL, serving compositor, shm, xdg-shell,
  layer-shell, seat, and output globals; 130 workspace tests green
  with `clippy -D warnings`.
