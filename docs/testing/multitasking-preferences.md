# Multitasking preferences

This candidate connects the installed GNOME preferences to the live Tuna session:

- `org.gnome.mutter`: `dynamic-workspaces`, `workspaces-only-on-primary`, `edge-tiling`, `center-new-windows` (protocol 0.41), `attach-modal-dialogs`.
- `org.gnome.desktop.interface`: `enable-hot-corners` (proofs G-SETTINGS-INPUT, G-HOT-CORNER).
- `org.gnome.desktop.wm.preferences`: `num-workspaces` (1 through 36).
- `org.gnome.shell.app-switcher` and `org.gnome.shell.window-switcher`: independent `current-workspace-only`.

Fixed workspaces retain empty slots. Shrinking the count moves windows into the last valid slot without replacing their identities, titles, or focus. Out-of-range moves and workspace switches fail without partial updates. A topology change forces a full control snapshot so peers receive removed as well as added slots. Fixed mode suppresses trailing empty overview slots and new-workspace drag placeholders.

A window leaving the all-workspaces state (dragged from a secondary output onto the primary, `workspaces-only-on-primary` turned off, or unstuck) lands on the active workspace, as mutter 51 `meta_window_on_all_workspaces_changed` does.

New windows follow mutter 51 `place.c`: with `center-new-windows` (default) they are centred in the work area and cascade from there; without it they take `find_first_fit` from the work area's corner (below, then beside existing windows) and otherwise cascade from that corner, mirrored right-to-left. X11 dialogs and splash screens stay centred whatever the key, as `window_place_centered`.

Secondary-output affinity follows greatest logical overlap, with primary winning ties and offscreen windows falling back to primary. The primary-only setting makes secondary windows visible across workspaces without slide translation or crossfade. Rendering, pointer hit testing, focus reconciliation, overview placement, and the shell's switcher filters use this same all-workspaces state. Output and primary changes refresh affinity on reconciliation. Explicit sticky windows use the same visibility flag. Pointer edge tiling can be disabled independently of explicit keyboard tiling.

The shell updates both switcher policies on settings changes, including while the popup is open; filtering covers application representatives, window counts and thumbnails. A filtered-out selection reconciles to a remaining item; an empty popup closes.

## Qualification

CI evidence (exact-head, see the PR):

- Nested GTK/compositor proof gates in `scripts/tuna-gtk-shell-proof`: G-EDGE-TILING-OFF (no preview, floating drop), G-SWITCHER-WORKSPACE (window and app switcher keys, live), G-WS-FIXED (fixed count of 4, shrink to 2 moving the window on a removed workspace to the new last one, dynamic again compacting), G-CENTER-NEW-WINDOWS (centred vs. mutter's first fit, exact pixels). Hosts without mutter's or gnome-shell's schemas get `tests/settings/gnome51-mutter.gschema.xml` and `gnome51-shell-switchers.gschema.xml` (GNOME session defaults), recorded in `multitasking-schema-fixtures.txt`.
- Cargo: `secondary_output_windows_are_sticky_and_unstick_onto_the_active_workspace` (two outputs), `edge_tiling_off_shows_no_preview_and_drops_floating`, `uncentred_placement_takes_mutters_first_fit_then_corner_cascade`, `center_new_windows_survives_positional_control_wire`, plus the state, tracker and shell model tests listed in the ledger.

Not qualified: physical multiple monitors (the nested proof has one output, so `workspaces-only-on-primary` is proven by the two-output cargo test only), an installed GNOME Settings panel, and a side-by-side GNOME 51 comparison.

GNOME 51 source references:

- https://github.com/GNOME/gnome-shell/blob/51.0/data/00_org.gnome.shell.gschema.override
- https://github.com/GNOME/gnome-shell/blob/51.0/data/org.gnome.shell.gschema.xml.in
- https://github.com/GNOME/gsettings-desktop-schemas/blob/51.0/schemas/org.gnome.desktop.wm.preferences.gschema.xml.in


## Dynamic workspace tracker candidate

The runtime now schedules topology reconciliation before publishing/rendering,
after client window requests, and pauses it during pointer or overview drags.
A real-time tracker holds a workspace for one second after a native dialog/modal
or X11 dialog/splash is removed, following
[GNOME Shell 51 WorkspaceTracker](https://github.com/GNOME/gnome-shell/blob/51.0/js/ui/windowManager.js).
Compaction carries protected workspace indices forward and suppresses a spurious
workspace-switch animation caused only by renumbering.

The isolated state module has 30 passing tests; the isolated lease tracker has
five passing tests. These checks do not qualify the production integration.
The clean `3004c2db04504e88045bb802e573a79ce736c069` build passed 54 schema unit tests, 29 schema integration tests, 376 compositor tests (one ignored), 332 host tests, and 125 GTK tests (two ignored), plus strict all-target Clippy. All seven frozen binary hashes were verified against its manifest. The first native harness found the instrumented state omitted the workspace list, so it could not establish the count. The initial diagnostic field then exceeded the JSON macro's recursion limit; a separate postconstruction assignment corrected that build failure.

Clean source `d484e374b1d611b20c7574b0db78e5c53e63b5d5` passed a fresh build with the same test counts, strict all-target Clippy, and the union native build. All seven binary hashes were independently checked against `/tmp/tuna-v1-workspace-runtime-binaries-d484e374/manifest.json`.
The actual GTK/compositor proof at `/tmp/tuna-v1-workspace-topology-proof-d484-1` completed successfully: empty fixed counts 4, 36, 1, and 6; a live change to dynamic mode compacted to workspace IDs 0 and 1; and the active workspace stayed valid. Source and frozen-binary revisions matched. The owned runtime was removed and both verified accessibility processes stopped, with no cleanup query errors. The receipt records explicit settings-schema fixtures where installed schemas were absent. This does not qualify an installed GNOME Settings panel, window movement, multiple outputs, startup sequences, or lease expiry.

Explicit keep-alive leases are produced when the user leaves a workspace (keyboard, shell command, move/follow) and the runtime's idle scheduler wakes at the earliest lease deadline (`workspace_lease_deadline`). Startup-sequence leases have no producer: in GNOME 51 only launches that name a workspace (an app dropped on a workspace thumbnail, `open_new_window(index)`) carry one, and activation-token sequences use workspace -1 and protect nothing. Tuna has no launch-onto-workspace path, so there is no event to feed; that drag target is an overview feature outside these preferences.
