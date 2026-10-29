---
tags: [proof, gtk-shell, testing]
---

# Start the GTK shell proof from a fresh artifacts directory

`scripts/tuna-gtk-shell-proof` keeps its XDG state under
`gtk-shell-artifacts/state`. That directory survives between local runs,
and the shell's persisted notification history comes back with it. With
old notifications in the list, the date menu never shows "No
Notifications", so G-CAL-OPEN fails even though the popover opened.
Delete `gtk-shell-artifacts/` before each local run. CI starts from a
fresh checkout, so it never sees this.
