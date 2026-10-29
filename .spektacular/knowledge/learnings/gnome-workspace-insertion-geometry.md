---
tags: [gnome-parity, overview, workspace-thumbnails]
---

# GNOME workspace insertion geometry

GNOME 51 keeps thumbnails before the insertion point fixed and shifts later thumbnails by 24 px: an 18 px placeholder plus 6 px spacing. At 1280×800, the native capture shows the placeholder at `[619, 102, 18, 26]`, with later thumbnail positions moving from 619/668 to 643/692.

The dimensions come from GNOME 51's [thumbnail styling](https://github.com/GNOME/gnome-shell/blob/51.0/data/theme/gnome-shell-sass/widgets/_workspace-thumbnails.scss), [base spacing](https://github.com/GNOME/gnome-shell/blob/51.0/data/theme/gnome-shell-sass/_common.scss), and [allocation code](https://github.com/GNOME/gnome-shell/blob/51.0/js/ui/workspaceThumbnail.js). The native capture harness is retained in [PR #240](https://github.com/tuna-os/tuna-desktop/pull/240).
