# Multitasking preferences

This candidate connects the installed GNOME preferences to the live Tuna session:

- `org.gnome.mutter`: `dynamic-workspaces`, `workspaces-only-on-primary`, `edge-tiling`.
- `org.gnome.desktop.wm.preferences`: `num-workspaces` (1 through 36).
- `org.gnome.shell.app-switcher` and `org.gnome.shell.window-switcher`: independent `current-workspace-only`.

Fixed workspaces retain empty slots. Shrinking the count moves windows into the last valid slot without replacing their identities, titles, or focus. Out-of-range moves and workspace switches fail without partial updates. A topology change forces a full control snapshot so peers receive removed as well as added slots. Fixed mode suppresses trailing empty overview slots and new-workspace drag placeholders.

Secondary-output affinity follows greatest logical overlap, with primary winning ties and offscreen windows falling back to primary. The primary-only setting makes secondary windows visible across workspaces without slide translation or crossfade. Rendering, pointer hit testing, focus reconciliation, overview placement, and the shell's switcher filters use this same all-workspaces state. Output and primary changes refresh affinity on reconciliation. Explicit sticky windows use the same visibility flag. Pointer edge tiling can be disabled independently of explicit keyboard tiling.

The shell updates both switcher policies on settings changes, including while the popup is open; filtering covers application representatives, window counts and thumbnails. A filtered-out selection reconciles to a remaining item; an empty popup closes.

## Qualification

Direct exact-source tests currently prove the standalone state model (20 cases), shell model (25 cases), and output affinity (4 cases). They do not prove the complete control wire, production renderer, installed-settings behavior, or multiple physical monitors. Additional full workspace builds, protocol and overview tests, and runtime proofs are required before this candidate can qualify P-WM-08. Dynamic workspace compaction and complete multiple-output overview behavior remain to be audited; this document makes no full GNOME parity or performance claim.

GNOME 51 source references:

- https://github.com/GNOME/gnome-shell/blob/51.0/data/00_org.gnome.shell.gschema.override
- https://github.com/GNOME/gnome-shell/blob/51.0/data/org.gnome.shell.gschema.xml.in
- https://github.com/GNOME/gsettings-desktop-schemas/blob/51.0/schemas/org.gnome.desktop.wm.preferences.gschema.xml.in
