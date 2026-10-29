---
tags: [gsettings, gtk, fonts, settings]
---

# GTK CSS overrides desktop font settings

GTK's Wayland backend receives live GNOME font settings, but an explicit CSS font family and size can override them. The shell's fixed family and 11 pt base (from when the project was named Roost) prevented `font-name` changes from appearing in the shell. Custom shell typography must account for desktop font settings; [PR #253](https://github.com/tuna-os/tuna-desktop/pull/253) adds that behavior and graphical checks.
