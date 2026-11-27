# GNOME Shell's on-screen keyboard layouts

These layouts (`*.json`, one per keyboard group plus the `digits`, `number`,
`phone`, `email`, `url` and `*-extended` terminal variants) and `emoji.json`
are copied unchanged from GNOME Shell 51.0 (`data/osk-layouts`,
https://gitlab.gnome.org/GNOME/gnome-shell, GPL-2.0-or-later). GNOME Shell
bundles them as `gnome-shell-osk-layouts.gresource`; Tuna Desktop bundles them
the same way, compiled into the shell as a GResource under
`/org/tuna/Shell/osk-layouts` (`build.rs`), so the on-screen keyboard shows the
keys GNOME shows.

`ge.json` is malformed JSON upstream at 51.0 (a missing `,`). GNOME fails to
load it and falls back to the `us` layout for the Georgian group; Tuna keeps
the file byte-identical and falls back the same way.
