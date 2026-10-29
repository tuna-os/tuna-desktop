---
tags: [portal, gnome51, session-environment]
---

# GNOME portal activation needs the Wayland session type

The measured GNOME 51 portal activation path requires `XDG_SESSION_TYPE=wayland`. Export it into the user service activation environment before starting or restarting the portal backends; setting it only in the compositor process does not update already-running user services.
