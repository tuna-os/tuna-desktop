# Window minimize qualification

The compositor implements native normal and dialog minimize/restore using GNOME Shell 51's 400 ms ease-out-expo curve, motion policy, slowdown, monitor-sized fade, leading-corner fallback and GTK-published dash icon bounds. Full restore stays opaque. Opacity follows Clutter's integer truncation.

An attached transient inherits logical hiding from minimized ancestors without changing its own flag. Explicit activation restores ancestors. Independently minimized children remain minimized when the parent is restored. Hidden child map/close does not restore its parent. Managed modal children continue to dim their parent even when independently minimized.

Minimize renders current native surface buffers and cancels earlier map/size-change effects. A surface destroyed during minimize is copied to an independent texture and finishes the original clock. Retained textures share the existing 64 MiB / 32-effect budget; overflow or copy failure is diagnosed and skips retention. Snapshot rendering explicitly samples the full texture when scaling its destination.

## Reproducible local checks

`scripts/tuna-minimize-proof --bin-dir <immutable-source-matched-bundle> --artifacts <directory> --display :95` runs under `dbus-run-session`. The bundle must contain compositor, GTK shell and native proof client binaries, source commit, lockfile and SHA-256 manifest. The harness refuses a dirty or different source revision, isolates its runtime and accessibility buses, and treats unverified cleanup as failure. `--rtl` exercises GTK's inverted text direction.

The harness checks actual pixels for fallback, dash, restore, reduced/Off, fullscreen, interrupted-map reset, live SHM commits, safe destroyed-surface retention, attached-parent hiding and independent child flags. These checks are prepared; they have not yet passed on this source revision. Cargo coverage includes `clutter_guint_opacity_truncates_quarter_expo_samples`, `live_minimize_frame_uses_latest_buffer_geometry_and_owns_no_snapshot`, `shrinking_snapshot_samples_full_texture_instead_of_destination_crop`, the transient graph tests and three native protocol family regressions.

Qualification remains partial: this is not a paired GNOME benchmark. Installed-session, native GPU, multi-output/fractional-scale, X11, shadows, buffer resize during minimize, and event ordering within one dispatch batch require runtime comparison. No GNOME performance or complete v1 parity claim is made.

Primary references: [GNOME Shell 51 window manager](https://raw.githubusercontent.com/GNOME/gnome-shell/51.0/js/ui/windowManager.js), [Mutter window actor](https://raw.githubusercontent.com/GNOME/mutter/51.0/src/compositor/meta-window-actor.c), [Mutter plugin manager](https://raw.githubusercontent.com/GNOME/mutter/51.0/src/compositor/meta-plugin-manager.c), [Clutter interval opacity](https://raw.githubusercontent.com/GNOME/mutter/51.0/clutter/clutter/clutter-interval.c).
