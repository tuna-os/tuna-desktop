# P-ST-01 clock, font and text-scale qualification on Marlin (Corral VM)

- Image: `ghcr.io/tuna-os/marlin@sha256:ae6363c185015849701328fc38694f5ddcc0b4c804034a1d8d05577f1c70cf7d` (marlin:tuna, version cbb3569, built 2026-10-29), resolved with `skopeo inspect` on 2026-10-30. `bootc status` in the guest reported the same digest.
- VM: `tuna-q-setinv`, `corral bootc create --cpu 4 --mem 6G --disk 40Gi --user tuna-test --password-stdin`, KubeVirt, bochs-drm 1280x800, no GPU. Deleted with `corral delete --wait -f`; `corral list` no longer shows it.
- Session: GDM user list → password → `tuna-compositor --shell-bin /usr/bin/tuna-shell-gtk` and `tuna-shell-gtk` running for logind session 29 (seat0, tty2). The only gnome-shell process belonging to the user is `gnome-shell-calendar-server`.
- Guest software: `gnome-control-center 51.1-1`, `ptyxis 50.1-1`; no gnome-text-editor and no GNOME Tweaks.
- Input: every Settings change was made with `corral pointer --click` and `corral key`/`type` on the VM console. Shell code under test is main as shipped in the image; nothing was sideloaded.

| Step | Input | Observed | Result |
|---|---|---|---|
| Time Format 24-hour | Settings › Date & Time, clicked "24-hour" (03→04) | panel clock `2:35 PM` → `14:35` at once | pass |
| Week Day | same page, Week Day switch (04) | panel clock gains `Fri` | pass |
| Seconds | same page, maximized with Super+Up, Seconds switch (07) | panel clock `14:36:42` | pass |
| Text size | Settings › Accessibility › Seeing › Text Size, slider to max, Apply (11→12) | Settings and panel text grow; `text-scaling-factor=1.51`; accessibility indicator appears in the panel | pass |
| Interface font | `gsettings set … font-name 'DejaVu Serif 11'` over SSH (GNOME Settings 51 has no font UI) (14→15) | panel clock, Settings and Ptyxis chrome repaint in DejaVu Serif | pass (not via Settings UI) |
| Monospace font | `gsettings set … monospace-font-name 'Liberation Mono 15'` | open Ptyxis tab unchanged; a new tab (Ctrl+Shift+T) uses Liberation Mono 15 (16). Ptyxis owns this behaviour; nothing in the shell or compositor reads the key | app-owned, observed |
| Document font | `gsettings set … document-font-name 'Nimbus Roman 14'` | no installed app reads it; value stored and persisted only | not observable on this image |
| Persistence | system menu › Power Off › Log Out… › Log Out (60 s dialog), GDM, password (20→21) | all seven values unchanged after re-login; clock `Fri Oct 30 14:45:47`, serif large text (21, 22-relogin-and-restore.txt) | pass |
| Restore | `gsettings reset` then `clock-format 12h` (the starting value) | `Oct 30 2:47 PM`, default fonts and size (23) | pass |

GNOME Settings 51 exposes only Text Size among these font keys: `panels/universal-access/cc-ua-seeing-page.c:115-123` writes `text-scaling-factor`; no gnome-control-center 51.0 panel references `font-name`, `document-font-name` or `monospace-font-name`. The clock rows are `panels/system/datetime/cc-datetime-page.c`.
