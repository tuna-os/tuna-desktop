---
tags: [proof, gsettings, testing]
---

# The proof host has no org.gnome.shell schema

CI runners (and many dev hosts) don't install gnome-shell, so
`org.gnome.shell` is missing. The GTK shell reads `favorite-apps` and
`app-picker-layout` from it, and `gio::Settings::strv` on a key the
installed schema lacks aborts the process. That makes partial schemas
dangerous as well as missing ones. The proof therefore compiles a minimal
schema holding exactly those keys into `$ARTIFACTS/schemas`, prepended to
`GSETTINGS_SCHEMA_DIR`, but only when the host has no `org.gnome.shell`.
Code reading GNOME schemas should look the key up with `has_key` before
reading it.
