---
tags: [gsettings, gio, main-loop]
---

# GTK host settings need GLib dispatch

Polling GSettings from a host loop does not dispatch change notifications by itself. Tuna Desktop’s host must dispatch the owning GLib main context before reading live settings. Keep that dispatch bounded so a busy source cannot monopolize the compositor loop.
