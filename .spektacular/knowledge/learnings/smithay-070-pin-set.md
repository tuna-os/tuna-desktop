---
tags: [smithay, wayland, pinning]
---

# Smithay 0.7.0 pin set for the nested slice

Pinned 2026-09-27: smithay 0.7.0, calloop 0.14.5, wayland-protocols
0.32.13, wayland-server 0.31.14, wayland-client 0.31.15,
wayland-backend 0.3.17, winit 0.30.13 (via smithay `backend_winit`,
req ^0.30.0). smithay MSRV is 1.80.1; local toolchain 1.94.1.

Verified by `cargo generate-lockfile` + `cargo check` on
`smithay { default-features = false, features = [backend_winit,
wayland_frontend, desktop] }` — compiles clean. Upstream reference
revision is git tag `v0.7.0` (anvil + tests).

Calloop was updated to 0.14.5 in tested and merged PR #132 (when the project was named Roost). The user approved updating this binding pin entry on 2026-10-04; all other pins above remain unchanged.

The smithay 0.7.0 pin is the version requirement only: the workspace `[patch.crates-io]` in `Cargo.toml` replaces it with the vendored `third-party/smithay` tree, which carries local API patches (`third-party/smithay/*-PATCH.diff`, `ROOST-PATCH.md`). Debug and change smithay behaviour there, not against the crates.io release.
