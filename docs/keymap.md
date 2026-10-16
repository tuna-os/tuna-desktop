# Tuna Desktop keymap

The single reference for every keyboard chord in the session:
shell chords drive the panel, dock, popups, and overview; the
compositor owns window chords. The two sets never overlap — shell
chords stay off Super+{tap,PgUp,PgDn,arrows,R,T} and Alt+{Tab,F4},
pinned by collision tests on both crates.

## Shell: focus movement

The shell keeps one keyboard cursor on a region: the panel strip
(clock, three tiles, hosted indicator cells), the dock slots, an
open popup's rows, or the overview's result list. The focused stop
paints an accent ring.

| Chord | Effect |
|---|---|
| Tab / Shift+Tab | Next / previous stop or row, wrapping |
| Left / Right | Previous / next stop or row |
| Up / Down | Panel to dock and back; rows inside a popup or the overview |
| F6 | Cycle panel and dock regions (skips an empty dock) |
| Super+D | Focus the dock |
| Alt+F1 | Focus the panel |

While the overview is open, movement keys walk the result list;
the region keys still jump the shell underneath.

## Shell: activation and Escape

| Chord | Effect |
|---|---|
| Enter, overview open | Activate the focused result row (top hit from any other region) |
| Enter, overview closed | Activate the focused stop: strip stops toggle their popups, dock slots take a left press (launch, switch, stack), popup rows fire |
| Typing, overview open | Search query (restarts the result cursor) |
| Backspace, overview open | Erase a query character |
| Escape, popup open | Close the popup, restore the parked shell focus |
| Escape, overview open, no popup | Dismiss the overview onto the previously focused window |
| Escape, all closed | No-op |

Opening a popup or the overview parks the live cursor on a return
stack; closing settles back through it, so focus never sticks on a
closed surface. No chord here is rebindable yet; there is no AT-SPI
bridge yet.

## Compositor: windows and session

| Chord | Effect |
|---|---|
| Super tap | Toggle the overview |
| Super+PgUp / Super+PgDn | Switch workspace (plus Shift: move the focused window and follow) |
| Alt+Tab / Shift+Alt+Tab | Step the window switcher; Alt release commits, Escape cancels |
| Super+Up / Super+Down | Maximize / restore the focused window |
| Super+Left / Super+Right | Tile the focused window left / right half |
| Alt+F4 | Ask the focused window to close |
| Super+Shift+T | Flip floating / strip session mode |
| Super+R / Super+Shift+R | Step the focused column through width presets (strip mode) |

## GTK shell: top bar, menus, overview

The GTK shell follows GNOME 51's chords, every one rebindable
through its settings key (`org.gnome.shell.keybindings`,
`org.gnome.desktop.wm.keybindings`, `org.gnome.mutter.keybindings`,
`org.gnome.settings-daemon.plugins.media-keys`):

| Chord | Effect |
|---|---|
| Ctrl+Alt+Tab / Ctrl+Alt+Shift+Tab | Focus the top bar: first / last control (`switch-panels`) |
| Tab / Shift+Tab, bar focused | Walk every panel control: Activities, clock, tray icons, accessibility, system |
| Left / Right, bar focused, no menu | Walk the bar's controls (an open menu owns the arrows) |
| Enter / Space, bar focused | Activate the focused control; on Activities, toggle the overview |
| Escape, panel menu open | Close the menu; focus stays on its button |
| Escape, bar focused, no menu | Return focus to the client |
| Super+S / Super+V | Quick settings / date menu; Tab, arrows and Enter operate them, Escape closes |
| Super+A | App grid: arrows and PgUp/PgDn move, Enter launches, Escape leaves |
| Super+1..9 / Super+Ctrl+1..9 | Dash app: switch-or-launch / open a new window |
| Super+N | Focus the notification banner's buttons; Escape releases |
| Print / Shift+Print / Alt+Print | Screenshot UI / fullscreen shot / window shot (S/C/W pick, Enter captures, Escape closes) |
| Super+Shift+Left/Right/Up/Down | Move the focused floating window to the neighboring monitor (`move-to-monitor-*`, #347) |
| Super+drag / Super+middle-drag / Super+right-click | Move / resize from the nearest corner / window menu (`mouse-button-modifier`, `resize-with-right-button`, #347) |

While the bar holds keyboard focus, NORMAL/OVERVIEW chords pause
(the compositor's exclusive-popup mode); volume, brightness and
input-source keys keep working. Opening the overview hands the
keyboard over; closing it releases a hold the bar never won.

## P-A11Y-02 audit: every pointer action's keyboard route

Enumerated from the GTK shell source; GNOME 51 baseline
gnome-shell `2177bdf9` (`js/ui/ctrlAltTab.js`,
`js/ui/windowManager.js`) and gsettings-desktop-schemas `1db238b8`
(`switch-panels` defaults). WM rows are owned by #347/PR561.

| Pointer action | Keyboard route | State |
|---|---|---|
| Activities click | Super tap; Enter on Activities once the bar is focused | Proof G-PANEL-FOCUS added; exact-head CI pending |
| Clock click (date menu) | Super+V / Super+M, then Tab/arrows + Enter | Verified (proof G-KEYBINDINGS) |
| Date menu: notifications, Clear, Do Not Disturb, calendar days, month arrows, Open Calendar | Same menu via Super+V; every row is a focusable button | Verified in source; AT-SPI `showing` in proof |
| Screencast stop click | Ctrl+Alt+Tab, Tab to Stop Screencast, Enter | Implemented; nested proof open (no recorder in proof env) |
| Tray icon menu | Ctrl+Alt+Tab, Tab to the icon, Enter, Tab + Enter on a row | Implemented; nested proof open (no items in proof env) |
| Accessibility menu toggles | Ctrl+Alt+Tab, Tab to Accessibility, Enter, Tab + Space | Implemented; nested proof open (indicator hidden in proof env) |
| System click (quick settings) | Super+S, then Tab/arrows + Enter; sliders take arrows | Verified (proof G-KEYBINDINGS) |
| Quick-settings power rows (Suspend, Restart, Power Off, Log Out) | Same menu; each opens a confirm dialog with Cancel focused, Escape cancels | Verified (proof G-POWER-ESCAPE) |
| Overview search result click | Type, then Enter (top hit) or Tab to the row + Enter | Verified in source (entry `activate` + native focus) |
| Dash app click / Show Apps | Super+1..9; Super+A for the grid | Verified (proof G-KEYBINDINGS) |
| App-grid icon click | Arrows / PgUp/PgDn + Enter (native grid navigation) | Verified in source |
| Folder open / rename / remove | Enter opens the dialog (Rename focused); Escape unwinds without saving | Verified in source |
| Workspace thumbnail click | Super+PgUp/PgDn (compositor WM chord) | Implemented (WM-owned) |
| Window preview click (focus) | Alt+Tab switcher (compositor WM chord) | Implemented (WM-owned) |
| Drag preview to workspace card | Move-to-workspace chords (compositor WM chord) | Implemented (WM-owned) |
| Notification banner buttons | Super+N, then Tab + Enter; Escape releases | Implemented; proof open |
| Lock-screen unlock typing | Type the password, Enter submits; Escape returns to the curtain | Implemented (WM/lock-owned) |
| Screenshot/polkit/network/consent/end-session/folder dialogs | Exclusive keyboard, first control focused, Escape cancels | Verified (proof G-POWER-ESCAPE, G-POLKIT, G-SCREENSHOT-UI) |
| Window menu rows (Alt+Space) | Tab + Enter work; Up/Down do not move between rows | Handoff to #347 (WM surface/focus policy) |
| Ctrl+Alt+Tab group-switcher dialog | Tuna focuses its single bar directly; GNOME lists Windows/bar/tray groups | Open follow-up (shell-side) |
| Lock-screen panel/a11y access | None: the lock screen shows no panel (GNOME binds `switch-panels` there) | Open; overlaps #352 (Orca paths) |
