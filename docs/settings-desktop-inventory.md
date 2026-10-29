# GNOME 51 desktop schema inventory

All 428 keys from `gsettings-desktop-schemas-51.0-1.fc45.x86_64` in the GNOME 51 reference container. Source re-audit: main `44edb6aca704ef0ada32b8c6111383ea5c3c6afe` (2026-10-06). See [the control-center audit](gnome-settings-audit.md) for open implementation and qualification gaps. “Via GTK” means the GTK Wayland backend translates the key through its settings portal or direct GSettings fallback; it does not imply support by every application or the compositor. “Ignored” records a current limitation, not permission to disregard binding knowledge.

Handedness follow-up: main `c822ab91abbb8c07fdd89ddde5c81a71f7843ff3`, after qualified #379. Mouse and touchpad dispositions below reflect the merged libinput consumers; tablet handedness remains unsupported.

Switcher follow-up: main `c387326d067a53b7b583aafb9e0206fba0fab59c` (2026-10-10). The six `switch-windows`, `cycle-windows`, and `cycle-group` forward/backward dispositions below already match the live `SetSwitcherKeys` registration in `crates/shell-gtk/src/keybindings.rs`. This review covers those six keys, not a fresh qualification of all 429 keys. The count checker checks completeness and duplicates only; an “Honored” disposition identifies a consumer and does not establish GNOME 51 behavioral parity. Legacy and app-owned contracts still require explicit enforcing-owner decisions and application/panel evidence under [#365](https://github.com/tuna-os/tuna-desktop/issues/365).

Orientation follow-up: w2-345 (#345). `orientation-lock` never lived under `org.gnome.desktop.peripherals.touchscreen`: mutter 51.0 reads it from `org.gnome.settings-daemon.peripherals.touchscreen` (`src/backends/meta-orientation-manager.c` `CONF_SCHEMA`), and the desktop touchscreen schema holds only `output`. The row and its pinned key are removed (429→428 keys); the settings-daemon key is tracked in [settings-map.md](settings-map.md).

Privacy follow-up: w3-356-privacy (#356). The three `org.gnome.desktop.privacy` device kill-switches move to Honored with the enforcing services identified per key; retention, diagnostics, and reporting rows stay Ignored as external-service responsibilities. Mechanism, GNOME 51 comparison, and proof matrix are in [privacy-enforcement.md](privacy-enforcement.md).

Focus-policy follow-up: feat/focus-policy (#347, ledger P-WM-09). `focus-mode`, `auto-raise`, `auto-raise-delay`, `raise-on-click`, `mouse-button-modifier` and `resize-with-right-button` move to Honored: the compositor reads them live on an owned GIO thread (`crates/compositor/src/wm_preferences.rs`) with Mutter 51.0's semantics (`crates/compositor/src/focus_policy.rs`). The three `action-*-click-titlebar` keys stay Via GTK: titlebars are client-side and GTK runs those actions itself.

Wellbeing renderer evidence: candidate `529f71648f3286092b55b93f59983a2882de3ec5` (2026-10-12). The GTK entry point starts [the wellbeing runtime](../crates/shell-gtk/src/wellbeing.rs), which watches the schemas, maintains `$XDG_DATA_HOME/gnome-shell/session-active-history.json`, registers notice actions and sends effects through the control client with correlated `Applied` responses. [Exact-source native renderer evidence](https://github.com/tuna-os/tuna-desktop/actions/runs/38320982358) was independently rechecked for 38 pixel comparisons from 44 original PNGs: entry/midpoint/endpoint, combined effects, immediate disable, reduced-motion/animations-off controls and cursor exclusion under nested winit/Xvfb/llvmpipe. This uses a supervised synthetic control peer; it does not qualify production GTK wellbeing policy, installed DRM/GPU sessions or the complete feature. The countdown stop hook now cancels only its owned OSD display and timer, preserving a newer volume/brightness display; the added actual GTK regression awaits exact-source CI execution. [#354](https://github.com/tuna-os/tuna-desktop/issues/354) remains open.

Source: [GTK Wayland settings translations](https://github.com/GNOME/gtk/blob/4.14.5/gdk/wayland/gdkdisplay-wayland.c). Related shell, Mutter and settings-daemon keys are in [settings-map.md](settings-map.md).

## org.gnome.desktop.a11y

| Key | Status | Reason |
|---|---|---|
| always-show-text-caret | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| always-show-universal-access-status | Honored (GTK shell) | Live accessibility indicator visibility follows GNOME 51: requested or any enabled menu feature; menu bindings do not establish compositor support for every feature. Focused source/runtime qualification is documented in accessibility-shell.md |
## org.gnome.desktop.a11y.applications

| Key | Status | Reason |
|---|---|---|
| screen-keyboard-enabled | Ignored | Tuna Desktop has no equivalent compositor accessibility feature; toolkit support is listed separately |
| screen-magnifier-enabled | Honored | GTK shell mirrors the switch plus every org.gnome.desktop.a11y.magnifier key into the session snapshot; the compositor draws GNOME 51's ZoomRegion on every output frame (zoom, tracking, cross-hairs, color effects, magnified pointer sprite), and captures/casts read back the same passes; Super+Alt+8 toggles it (M-KEYS). Accepted differences: per-monitor regions, touch mapped through the zoom, window-centre fallback for non-AT-SPI apps, shell popover focus untracked (#349) |
| screen-reader-enabled | Honored (hardware session only) | Hardware GTK shell publishes the preference to the compositor, which owns a genuine nonreplacement Orca child bound to its lifetime; nested runtimes refuse activation and never manage host Orca. Genuine Orca activation/logout VM probes are required; Settings UI activation and complete spoken lock/login/shell navigation remain unqualified (#352) |
## org.gnome.desktop.a11y.interface

| Key | Status | Reason |
|---|---|---|
| high-contrast | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| keyboard-focus-visible-timeout | Honored (GTK shell) | Live a11y.interface seconds preference controls shell focus visibility: zero forever, negative toolkit default; GTK property when present and a weak-window fallback on older GTK. External application/installed image qualification remains separate; see accessibility-shell.md |
| reduced-motion | Honored | GNOME 51 reduce/no-preference enum combines with enable-animations into the shared full/fade-only/off motion policy: reduce keeps fades (idle shield) and snaps shell GTK transitions and compositor motion; live proof G-ANIMATIONS-OFF; Introspect AnimationsEnabled stays true under reduce, as in GNOME 51 (G-INTROSPECT-MOTION, qualification pending) |
| show-status-shapes | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
## org.gnome.desktop.a11y.keyboard

| Key | Status | Reason |
|---|---|---|
| bouncekeys-beep-reject | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Rejected Bounce Keys events reach the shared bell policy; audible and installed-session acceptance remain pending (#350/#365) |
| bouncekeys-delay | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Live delay reaches compositor Bounce Keys rejection timing; exact combined runtime acceptance remains pending (#350/#365) |
| bouncekeys-enable | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Live preference enables compositor Bounce Keys before client delivery; exact combined runtime acceptance remains pending (#350/#365) |
| disable-timeout | Ignored | No Tuna inactivity-timeout consumer. Mutter51 native loads this legacy delay but never applies it; compatibility disposition remains open under #365. See [native keyboard audit](keyboard-native-parity.md) |
| enable | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Controls the five-Shift/eight-second-Shift accessibility shortcuts, independently of aid enable keys, as in Mutter51; combined runtime acceptance remains pending (#350/#365) |
| feature-state-change-beep | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Aid shortcut changes reach the shared bell policy; audible and installed-session acceptance remain pending (#350/#365) |
| mousekeys-accel-time | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Live preference reaches compositor Mouse Keys acceleration; actual motion and installed-session acceptance remain pending (#350/#365) |
| mousekeys-enable | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Live preference enables compositor keypad pointer/button synthesis; actual motion and installed-session acceptance remain pending (#350/#365) |
| mousekeys-init-delay | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Live preference reaches compositor Mouse Keys initial movement delay; actual motion and installed-session acceptance remain pending (#350/#365) |
| mousekeys-max-speed | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Live preference reaches compositor Mouse Keys speed limit; actual motion and installed-session acceptance remain pending (#350/#365) |
| slowkeys-beep-accept | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Accepted Slow Keys events reach the shared bell policy; audible and installed-session acceptance remain pending (#350/#365) |
| slowkeys-beep-press | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Slow Keys press events reach the shared bell policy; audible and installed-session acceptance remain pending (#350/#365) |
| slowkeys-beep-reject | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Rejected Slow Keys events reach the shared bell policy; audible and installed-session acceptance remain pending (#350/#365) |
| slowkeys-delay | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Live preference reaches compositor deferred key acceptance timing; exact combined runtime acceptance remains pending (#350/#365) |
| slowkeys-enable | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Live preference enables compositor Slow Keys before client delivery; exact combined runtime acceptance remains pending (#350/#365) |
| stickykeys-enable | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Live preference enables compositor modifier latch/lock delivery; active keymap resets are implemented, but AltGr/Level3 parity and combined runtime acceptance remain pending (#350/#365) |
| stickykeys-modifier-beep | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Sticky modifier transitions reach the shared bell policy; audible, Level3 and installed-session acceptance remain pending (#350/#365) |
| stickykeys-two-key-off | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Live preference controls compositor two-key Sticky Keys cancellation and GTK confirmation; exact combined runtime acceptance remains pending (#350/#365) |
| timeout-enable | Ignored | No Tuna inactivity-timeout consumer. Mutter51 native loads the flag but never tests it in the native aid engine; compatibility disposition remains open under #365. See [native keyboard audit](keyboard-native-parity.md) |
| togglekeys-enable | Honored (partial) | GTK kbd_a11y → combined protocol0.34 → compositor InputAids. Live preference reaches compositor Toggle Keys; actual XKB LED-change semantics and audible acceptance remain pending; see native keyboard audit (#350/#365) |
## org.gnome.desktop.a11y.magnifier

| Key | Status | Reason |
|---|---|---|
| brightness-blue | Honored | Shell.BrightnessContrast blue brightness (-1..1) on the live zoom region, captures and casts; see screen-magnifier-enabled |
| brightness-green | Honored | Green brightness on the live zoom region, captures and casts; see screen-magnifier-enabled |
| brightness-red | Honored | Red brightness on the live zoom region, captures and casts; see screen-magnifier-enabled |
| caret-tracking | Honored | AT-SPI caret events (and the text-input caret rectangle) retarget the live zoom region after GNOME's 1 s pointer rest (none/centered/proportional/push, default centered); see screen-magnifier-enabled |
| color-saturation | Honored | Clutter.DesaturateEffect (0 gray..1 unchanged) on the live zoom region; see screen-magnifier-enabled |
| contrast-blue | Honored | Blue contrast (-1..1) on the live zoom region, captures and casts; see screen-magnifier-enabled |
| contrast-green | Honored | Green contrast on the live zoom region, captures and casts; see screen-magnifier-enabled |
| contrast-red | Honored | Red contrast on the live zoom region, captures and casts; see screen-magnifier-enabled |
| cross-hairs-clip | Honored | Leaves GNOME's 100x100 hole around the magnified pointer; see screen-magnifier-enabled |
| cross-hairs-color | Honored | Hair color, parsed like Cogl.Color.from_string (M-CROSSHAIRS); see screen-magnifier-enabled |
| cross-hairs-length | Honored | Hair length 20..4096 device pixels; see screen-magnifier-enabled |
| cross-hairs-opacity | Honored | Hair opacity 0..1 (M-CROSSHAIRS); see screen-magnifier-enabled |
| cross-hairs-thickness | Honored | Hair thickness in logical pixels (M-CROSSHAIRS); see screen-magnifier-enabled |
| focus-tracking | Honored | AT-SPI focus events center on the focused widget (M-ATSPI-FOCUS); apps without AT-SPI fall back to the focused window centre (M-FOCUS) (none/centered/proportional/push, default proportional); see screen-magnifier-enabled |
| invert-lightness | Honored | Shell.InvertLightnessEffect on the live zoom region (M-INVERT); see screen-magnifier-enabled |
| lens-mode | Honored | A non-full-screen viewport centers on the tracked point, as in GNOME; full-screen lens stays put; see screen-magnifier-enabled |
| mag-factor | Honored | Zoom factor 0.1..=32.0 (two decimals, default 2.0) on the live output (M-LIVE, M-CURSOR), animated over 100 ms; Super+Alt+=/- step it (M-KEYS); see screen-magnifier-enabled |
| mouse-tracking | Honored | Pointer motion retargets the live zoom region (none/centered/proportional/push, default proportional; M-TRACK, M-LIVE); see screen-magnifier-enabled |
| screen-position | Honored | Full-screen or half-screen viewport on the live output; the other half shows the unmagnified desktop (M-HALF); see screen-magnifier-enabled |
| scroll-at-edges | Honored | Off (default) clamps the region of interest on screen; on lets it scroll past edges over the system background; see screen-magnifier-enabled |
| show-cross-hairs | Honored | Cross-hairs through the magnified pointer on the live output (M-CROSSHAIRS); see screen-magnifier-enabled |
## org.gnome.desktop.a11y.mouse

| Key | Status | Reason |
|---|---|---|
| click-type-window-visible | Ignored | GNOME Shell 51 does not read it either: the chooser follows dwell-click-enabled and dwell-mode |
| dwell-click-enabled | Honored | Hover click in the compositor's input pipeline, with GNOME's pie timer and panel chooser (G-A11Y-DWELL) |
| dwell-gesture-double | Honored | Gesture direction for a double click |
| dwell-gesture-drag | Honored | Gesture direction for a drag |
| dwell-gesture-secondary | Honored | Gesture direction for a secondary click (G-A11Y-DWELL) |
| dwell-gesture-single | Honored | Gesture direction for a single click |
| dwell-mode | Honored | `window` shows the panel's click-type chooser; `gesture` picks the click by direction (G-A11Y-DWELL) |
| dwell-threshold | Honored | Motion that cancels a dwell or a held secondary click, and a gesture's minimum (G-A11Y-DWELL) |
| dwell-time | Honored | Live dwell and gesture timeout (G-A11Y-DWELL) |
| secondary-click-enabled | Honored | A held primary button becomes a secondary click on release (G-A11Y-SECONDARY) |
| secondary-click-time | Honored | Live hold time (G-A11Y-SECONDARY) |
## org.gnome.desktop.app-folders

| Key | Status | Reason |
|---|---|---|
| folder-children | Honored | Read when app grid rebuilds |
## org.gnome.desktop.app-folders.folder

| Key | Status | Reason |
|---|---|---|
| apps | Honored | Read when app grid rebuilds |
| categories | Honored | Read when app grid rebuilds |
| excluded-apps | Honored | Read when app grid rebuilds |
| name | Honored | Read when app grid rebuilds |
| translate | Honored | Read when app grid rebuilds |
## org.gnome.desktop.background

| Key | Status | Reason |
|---|---|---|
| color-shading-type | Ignored | Tuna Desktop implements the wallpaper URI/placement/primary color subset; no slideshow, shading or metadata policy |
| picture-opacity | Ignored | Tuna Desktop implements the wallpaper URI/placement/primary color subset; no slideshow, shading or metadata policy |
| picture-options | Honored (partial) | none hides the picture; every other mode currently uses compositor zoom placement |
| picture-uri | Honored | Live wallpaper URI or solid-color fallback consumed by compositor |
| picture-uri-dark | Honored | Live wallpaper URI or solid-color fallback consumed by compositor |
| primary-color | Honored | Live wallpaper URI or solid-color fallback consumed by compositor |
| secondary-color | Ignored | Tuna Desktop implements the wallpaper URI/placement/primary color subset; no slideshow, shading or metadata policy |
| show-desktop-icons | Ignored | Tuna Desktop implements the wallpaper URI/placement/primary color subset; no slideshow, shading or metadata policy |
## org.gnome.desktop.break-reminders

| Key | Status | Reason |
|---|---|---|
| selected-breaks | Honored | Shell break scheduler; empty disables reminders |
## org.gnome.desktop.break-reminders.eyesight

| Key | Status | Reason |
|---|---|---|
| countdown | Honored (partial) | OSD countdown in the last 60 s before a break; StopCountdown cancels only its owned OSD display/timer; the added actual GTK regression awaits exact-source CI. Production GTK policy and installed-session acceptance remain open (#354) |
| delay-seconds | Honored (partial) | GTK wellbeing notice Delay callback queues the action; the 2 s break tick invokes delay_break with the live per-type delay setting. Production GTK policy and installed-session acceptance remain open (#354) |
| duration-seconds | Honored | Break length for scheduling and idle-take detection |
| fade-screen | Honored (partial) | GTK wellbeing effect hook → correlated protocol0.35 Applied → compositor wellbeing_effects brightness fade. Exact-source native renderer pixels independently verified; production GTK policy and installed DRM/GPU acceptance remain open (#354) |
| interval-seconds | Honored | Time between breaks |
| lock-screen | Honored | Locks the session when the break starts |
| notify | Honored | Due/overdue notices posted; tray-only urgency not distinguished yet |
| notify-overdue | Honored | Overdue notice 60 s after due |
| notify-upcoming | Honored | Upcoming notice 2 min before due |
| play-sound | Honored | Complete sound via canberra, event-sounds gated |
## org.gnome.desktop.break-reminders.movement

| Key | Status | Reason |
|---|---|---|
| countdown | Honored (partial) | OSD countdown in the last 60 s before a break; StopCountdown cancels only its owned OSD display/timer; the added actual GTK regression awaits exact-source CI. Production GTK policy and installed-session acceptance remain open (#354) |
| delay-seconds | Honored (partial) | GTK wellbeing notice Delay callback queues the action; the 2 s break tick invokes delay_break with the live per-type delay setting. Production GTK policy and installed-session acceptance remain open (#354) |
| duration-seconds | Honored | Break length for scheduling and idle-take detection |
| fade-screen | Honored (partial) | GTK wellbeing effect hook → correlated protocol0.35 Applied → compositor wellbeing_effects brightness fade. Exact-source native renderer pixels independently verified; production GTK policy and installed DRM/GPU acceptance remain open (#354) |
| interval-seconds | Honored | Time between breaks |
| lock-screen | Honored | Locks the session when the break starts |
| notify | Honored | Due/overdue notices posted; tray-only urgency not distinguished yet |
| notify-overdue | Honored | Overdue notice 60 s after due |
| notify-upcoming | Honored | Upcoming notice 2 min before due |
| play-sound | Honored | Complete sound via canberra, event-sounds gated |
## org.gnome.desktop.calendar

| Key | Status | Reason |
|---|---|---|
| show-weekdate | Honored | Live ISO week labels use each displayed row's Thursday; GTK gate G-CAL-PREFS |
| week-start-day | Honored | Live first-weekday enum; default and reset follow locale; GTK gate G-CAL-PREFS |
## org.gnome.desktop.datetime

| Key | Status | Reason |
|---|---|---|
| automatic-timezone | Honored (gsd-datetime, unqualified) | gnome-settings-daemon 51's datetime plugin, started by the Tuna session (V-GSD-PLUGINS), applies it through Geoclue and timedated; the time zone change itself is unproven |
## org.gnome.desktop.default-applications.office.calendar

| Key | Status | Reason |
|---|---|---|
| exec | Ignored | App launch resolves desktop-file/MIME handlers; these legacy command preferences are not read |
| needs-term | Ignored | App launch resolves desktop-file/MIME handlers; these legacy command preferences are not read |
## org.gnome.desktop.default-applications.office.tasks

| Key | Status | Reason |
|---|---|---|
| exec | Ignored | App launch resolves desktop-file/MIME handlers; these legacy command preferences are not read |
| needs-term | Ignored | App launch resolves desktop-file/MIME handlers; these legacy command preferences are not read |
## org.gnome.desktop.default-applications.terminal

| Key | Status | Reason |
|---|---|---|
| exec | Honored (GTK shell) | The terminal Ctrl+Return in the Run a Command dialog runs the line inside, as runDialog.js does; app launch still resolves desktop-file/MIME handlers (#357) |
| exec-arg | Honored (GTK shell) | Placed between `exec` and the line for Ctrl+Return in the Run a Command dialog, as runDialog.js does (#357) |
## org.gnome.desktop.input-sources

| Key | Status | Reason |
|---|---|---|
| current | Ignored | Deprecated and ignored in GNOME 51 (schema text); gnome-shell 51.0 never reads it, and the active source is the shell's MRU front instead |
| mru-sources | Honored (GTK shell) | GNOME 51's MRU order (keyboard.js `_updateMruSources`/`_updateMruSettings`): seeds the order at start, saved on every interactive switch; Super+Space flips between the two most recent sources and the held popup steps through it (G-INPUT-SOURCE-MRU, #346). XKB sources only: IBus sources take no part |
| per-window | Honored (GTK shell) | Each focused window, and the overview, restores the source last active in it; a window seen first starts on the first source; off forgets (keyboard.js `_setPerWindowInputSource`; G-INPUT-PER-WINDOW, #346) |
| show-all-sources | Ignored | Read only by GNOME Settings' input-source chooser (schema: "available for choosing in System Settings"); gnome-shell and mutter 51 never read it, so the desktop has nothing to do |
| sources | Honored | Live compositor XKB keymap; IBus sources use bridge |
| xkb-model | Ignored | gnome-shell 51 passes it into the keymap (`KeyboardManager.setKeyboardModel`); Tuna's `InputSettings` wire carries no model field, so the compositor compiles xkbcommon's default model. Needs `InputSettings.xkb_model` (#346) |
| xkb-options | Honored | Live compositor XKB keymap; IBus sources use bridge |
## org.gnome.desktop.interface

| Key | Status | Reason |
|---|---|---|
| accent-color | Honored | Live compositor overview accent and libadwaita style |
| avatar-directories | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| can-change-accels | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| clock-format | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| clock-show-date | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| clock-show-seconds | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| clock-show-weekday | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| color-scheme | Honored | Live dark tile/background selection; libadwaita apps follow style; shell chrome stays dark |
| cursor-blink | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| cursor-blink-time | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| cursor-blink-timeout | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| cursor-size | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| cursor-theme | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| document-font-name | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| enable-animations | Honored | Live idle shield fade duration, GTK transitions and compositor overview/strip motion; proofs G-ANIMATIONS-OFF and G-INTROSPECT-MOTION verify effective motion through actual GNOME clients |
| enable-hot-corners | Honored (partial) | Live enable/disable and output-aware LTR corner regions; candidate fullscreen guard has protocol/GTK proofs awaiting execution. Pressure barriers, RTL, retrigger/toggle policy and physical multi-output qualification remain in #336 |
| font-antialiasing | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| font-hinting | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| font-name | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| font-rendering | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| font-rgba-order | Ignored (shell) | Shell explicitly uses rgb for rgba antialiasing; GTK clients translate this key independently |
| gtk-color-palette | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| gtk-color-scheme | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| gtk-enable-primary-paste | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| gtk-im-module | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| gtk-im-preedit-style | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| gtk-im-status-style | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| gtk-key-theme | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| gtk-theme | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| gtk-timeout-initial | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| gtk-timeout-repeat | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| icon-theme | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| locate-pointer | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| menubar-accel | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| menubar-detachable | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| menus-have-tearoff | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| monospace-font-name | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| overlay-scrolling | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| scaling-factor | Ignored | Output scale uses Mutter DisplayConfig/monitors.xml, not this XSettings integer |
| show-battery-percentage | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| text-scaling-factor | Honored | Live shell CSS/GTK rendering, clock, input policy or UPower percentage |
| toolbar-detachable | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| toolbar-icons-size | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| toolbar-style | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
| toolkit-accessibility | Ignored | No Tuna Desktop consumer or verified toolkit translation for this preference |
## org.gnome.desktop.lockdown

Consumers re-checked against gnome-shell 51.0, gnome-session 51.0 and gnome-settings-daemon 51.0 (#357); mechanism and proofs in [parity-ledger.md](parity-ledger.md) P-ST-05.

| Key | Status | Reason |
|---|---|---|
| disable-application-handlers | Ignored | No GNOME 51 desktop consumer: gnome-shell, gnome-session and gnome-settings-daemon 51.0 never read it, so Tuna's shell no longer gates Settings or calendar launches on it (#357); applications that honor it read it themselves |
| disable-command-line | Honored (GTK shell) | Alt+F2 opens GNOME's Run a Command dialog; `RunDialog.open()` refuses while set, re-read per open (runDialog.js); G-LOCKDOWN-COMMAND-LINE (#357) |
| disable-lock-screen | Honored | As screenShield.js `lock()`/systemActions.js: Lock button hidden, keybinding, screensaver/D-Bus, sleep and idle locks refused by the shell, compositor backstop denies Lock and idle; never unlocks a locked session; G-LOCKDOWN-LOCK (#357) |
| disable-log-out | Honored (GTK shell) | Log Out…, Restart… and Power Off… hidden and refused live, as gnome-session 51 answers CanShutdown/CanReboot unavailable and refuses Logout; Suspend stays; G-LOCKDOWN-LOG-OUT (#357) |
| disable-print-setup | Ignored | No GNOME 51 desktop consumer; applications' Print Setup dialogs read it themselves; the shell exposes no print path |
| disable-printing | Ignored | No GNOME 51 desktop consumer; applications' Print dialogs read it themselves; the shell exposes no print path |
| disable-save-to-disk | Honored (shell captures) | As screenshot.js/screencastService.js: shell screenshots stay on the clipboard, screencasts refuse to start, the portal's org.gnome.Shell.Screenshot calls are refused; applications' Save As dialogs stay app-owned; G-LOCKDOWN-SAVE-TO-DISK (#357) |
| disable-show-password | Honored (GTK shell) | As StPasswordEntry: unlock prompt, polkit and network secret peek icons (and GTK's Show Text) drop live and re-hide revealed text (#357) |
| disable-user-switching | Ignored | Tuna ships no Switch User row or unlock-dialog user switch (gnome-shell 51.0's only consumers; gnome-session 51.0 defines but never reads the key), so nothing to restrict; the visibility gate is unit-tested for their landing |
| mount-removable-storage-devices-as-read-only | Ignored | Daemon-owned: removable mounts (udisks2); no GNOME 51 desktop consumer and Tuna ships no mount agent |
| user-administration-disabled | Ignored | App-owned: GNOME Settings' Users panel; Tuna ships no Users panel |
## org.gnome.desktop.media-handling

| Key | Status | Reason |
|---|---|---|
| automount | Ignored | Tuna Desktop has no removable-media autorun/automount agent |
| automount-open | Ignored | Tuna Desktop has no removable-media autorun/automount agent |
| autorun-never | Ignored | Tuna Desktop has no removable-media autorun/automount agent |
| autorun-x-content-ignore | Ignored | Tuna Desktop has no removable-media autorun/automount agent |
| autorun-x-content-open-folder | Ignored | Tuna Desktop has no removable-media autorun/automount agent |
| autorun-x-content-start-app | Ignored | Tuna Desktop has no removable-media autorun/automount agent |
## org.gnome.desktop.notifications

| Key | Status | Reason |
|---|---|---|
| application-children | Ignored | Tuna Desktop uses global banners and its notification model; per-application/lock-screen preference is not consumed |
| show-banners | Honored | Live Do Not Disturb policy |
| show-in-lock-screen | Ignored | Tuna Desktop uses global banners and its notification model; per-application/lock-screen preference is not consumed |
## org.gnome.desktop.notifications.application

| Key | Status | Reason |
|---|---|---|
| application-id | Ignored | Tuna Desktop uses global banners and its notification model; per-application/lock-screen preference is not consumed |
| details-in-lock-screen | Ignored | Tuna Desktop uses global banners and its notification model; per-application/lock-screen preference is not consumed |
| enable | Ignored | Tuna Desktop uses global banners and its notification model; per-application/lock-screen preference is not consumed |
| enable-sound-alerts | Ignored | Tuna Desktop uses global banners and its notification model; per-application/lock-screen preference is not consumed |
| force-expanded | Ignored | Tuna Desktop uses global banners and its notification model; per-application/lock-screen preference is not consumed |
| show-banners | Ignored | Tuna Desktop uses global banners and its notification model; per-application/lock-screen preference is not consumed |
| show-in-lock-screen | Ignored | Tuna Desktop uses global banners and its notification model; per-application/lock-screen preference is not consumed |
## org.gnome.desktop.peripherals.keyboard

| Key | Status | Reason |
|---|---|---|
| delay | Honored | Live compositor seat repeat policy |
| numlock-state | Honored | The compositor restores it at startup while remember-numlock-state is set and saves every Num Lock change into it, as mutter 51's meta-input-settings.c does; keymap reloads keep the lock (G-NUMLOCK, #346) |
| remember-numlock-state | Honored | Gates the compositor's Num Lock restore and saves, read per save as mutter 51 reads it (G-NUMLOCK, #346) |
| repeat | Honored | Live compositor seat repeat policy |
| repeat-interval | Honored | Live compositor seat repeat policy |
## org.gnome.desktop.peripherals.mouse

| Key | Status | Reason |
|---|---|---|
| accel-profile | Honored | `default`, `flat` and `adaptive` reach libinput live and on hotplug for mice only (trackballs and pointing sticks excluded, as in Mutter); a profile the device lacks and `custom` fall back to the device default, since the compositor's libinput bindings cannot set custom curves. Native VM proof V-POINTER-LIVE checks flat then adaptive on the live device |
| custom-accel-config | Ignored | The pinned libinput bindings (input-sys 1.19) cannot set custom acceleration curves; `accel-profile` `custom` falls back to the device default and each device reports `accel_custom_supported_by_compositor: false` |
| double-click | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| drag-threshold | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| left-handed | Honored | Live GNOME boolean reaches libinput; native VM proof V-MOUSE-HANDEDNESS covers false/true/false on the same compositor |
| middle-click-emulation | Honored | Live and on hotplug for mice only (not touchpads, trackballs or pointing sticks, as Mutter 51) where libinput offers it; native VM proof V-POINTER-LIVE checks the live device readback on and off |
| natural-scroll | Honored | Live libinput policy on hardware devices |
| scroll-wheel-emulation-button | Honored | Mice only, as Mutter 51: a nonzero GNOME button selects libinput's on-button-down scrolling with that button (Mutter's button-to-evdev mapping), 0 turns button scrolling off. Native VM proof V-POINTER-LIVE checks button 2 then off where the guest schema has the GNOME 51 key |
| scroll-wheel-emulation-button-lock | Honored | Applied with `scroll-wheel-emulation-button` to mice as libinput's scroll button lock; V-POINTER-LIVE checks it on then off where the guest schema has the GNOME 51 key |
| speed | Honored | Live libinput policy on hardware devices |
## org.gnome.desktop.peripherals.pointingstick

| Key | Status | Reason |
|---|---|---|
| accel-profile | Honored | `default`, `flat` and `adaptive` reach libinput live and on hotplug for pointing sticks (udev `ID_INPUT_POINTINGSTICK`, as Mutter 51); a profile the device lacks and `custom` fall back to the device default. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no pointing stick in the VM) |
| custom-accel-config | Ignored | The pinned libinput bindings (input-sys 1.19) cannot set custom acceleration curves; `custom` falls back to the device default and each device reports `accel_custom_supported_by_compositor: false` |
| scroll-method | Honored | `default` (device default), `none` and `on-button-down` reach libinput live and on hotplug for pointing sticks, as Mutter 51. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no pointing stick in the VM) |
| speed | Honored | Pointing sticks take this speed instead of the mouse speed, live and on hotplug, as Mutter 51. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no pointing stick in the VM) |
## org.gnome.desktop.peripherals.tablet

| Key | Status | Reason |
|---|---|---|
| area | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| keep-aspect | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| left-handed | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| mapping | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| output | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
## org.gnome.desktop.peripherals.tablet.deprecated

| Key | Status | Reason |
|---|---|---|
| display | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
## org.gnome.desktop.peripherals.tablet.pad-button

| Key | Status | Reason |
|---|---|---|
| action | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| keybinding | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
## org.gnome.desktop.peripherals.tablet.stylus

| Key | Status | Reason |
|---|---|---|
| button-action | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| button-keybinding | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| eraser-button-action | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| eraser-button-keybinding | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| eraser-button-mode | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| eraser-pressure-curve | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| eraser-pressure-range | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| pressure-curve | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| pressure-range | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| secondary-button-action | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| secondary-button-keybinding | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| tertiary-button-action | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| tertiary-button-keybinding | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
## org.gnome.desktop.peripherals.touchpad

| Key | Status | Reason |
|---|---|---|
| accel-profile | Honored | `default`, `flat` and `adaptive` reach libinput live and on hotplug for touchpads, as Mutter 51; a profile the device lacks and `custom` fall back to the device default. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no touchpad in the VM) |
| click-method | Honored | `default` (device default), `none`, `areas` and `fingers` reach libinput's click method live and on hotplug for touchpads, as Mutter 51. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no touchpad in the VM) |
| custom-accel-config | Ignored | The pinned libinput bindings (input-sys 1.19) cannot set custom acceleration curves; `custom` falls back to the device default and each device reports `accel_custom_supported_by_compositor: false` |
| disable-while-typing | Honored | Live libinput policy, including hotplug |
| disable-while-typing-timeout | Ignored | Mutter 51 sets it with `libinput_device_config_dwt_set_timeout`, which the pinned libinput bindings (input-sys 1.19) lack; libinput's own timeout applies and each device reports `dwt_timeout_supported_by_compositor: false` |
| edge-scrolling-enabled | Honored | Live and on hotplug for touchpads, replaying Mutter 51's two-finger-then-edge sequence: edge scrolling yields while both keys are on and any touchpad scrolls with two fingers, and settings reapply when the last such touchpad leaves. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no touchpad in the VM) |
| left-handed | Honored | Live left/right/mouse enum reaches libinput, including the hotplug path; physical touchpad and hotplug qualification remain open |
| middle-click-emulation | Honored | Live and on hotplug for touchpads where libinput offers it, as Mutter 51. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no touchpad in the VM) |
| natural-scroll | Honored | Live libinput policy, including hotplug |
| scroll-speed | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
| send-events | Honored | `enabled`, `disabled` and `disabled-on-external-mouse` reach libinput's send-events mode live and on hotplug for touchpads, as Mutter 51; libinput itself suspends and resumes the touchpad as external mice come and go. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no touchpad in the VM) |
| speed | Honored | Live libinput policy, including hotplug |
| tap-and-drag | Honored | Live and on hotplug for touchpads with tapping, as Mutter 51. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no touchpad in the VM) |
| tap-and-drag-lock | Honored (timeout drag lock) | Live and on hotplug for touchpads with tapping. The pinned bindings name only libinput's original timeout drag lock, while Mutter 51 asks libinput for sticky drag lock; each device reports the live `tap_drag_lock` mode and `tap_drag_lock_sticky_supported_by_compositor: false`. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no touchpad in the VM) |
| tap-button-map | Honored | `default` (device default), `lrm` and `lmr` reach libinput live and on hotplug for touchpads with tapping, as Mutter 51. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no touchpad in the VM) |
| tap-to-click | Honored | Live libinput policy, including hotplug |
| two-finger-scrolling-enabled | Honored | Live and on hotplug for touchpads, with `edge-scrolling-enabled` in Mutter 51's order (two-finger wins when both are on). G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no touchpad in the VM) |
## org.gnome.desktop.peripherals.touchscreen

| Key | Status | Reason |
|---|---|---|
| output | Ignored | No Tuna Desktop mapping for this libinput/tablet preference; supported subset is listed explicitly |
## org.gnome.desktop.peripherals.trackball

| Key | Status | Reason |
|---|---|---|
| accel-profile | Honored | `default`, `flat` and `adaptive` reach libinput live and on hotplug for trackballs (udev `ID_INPUT_TRACKBALL`, as Mutter 51); a profile the device lacks and `custom` fall back to the device default. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no trackball in the VM) |
| custom-accel-config | Ignored | The pinned libinput bindings (input-sys 1.19) cannot set custom acceleration curves; `custom` falls back to the device default and each device reports `accel_custom_supported_by_compositor: false` |
| middle-click-emulation | Honored | Live and on hotplug for trackballs where libinput offers it, as Mutter 51. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no trackball in the VM) |
| scroll-wheel-emulation-button | Honored | Trackballs take libinput's on-button-down scrolling with this GNOME button (0 turns it off), live and on hotplug, as Mutter 51. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no trackball in the VM) |
| scroll-wheel-emulation-button-lock | Honored | Applied with `scroll-wheel-emulation-button` to trackballs as libinput's scroll button lock. G-SETTINGS-POINTER proves the key reaches the compositor; device application is unit-tested only (no trackball in the VM) |
## org.gnome.desktop.privacy

| Key | Status | Reason |
|---|---|---|
| disable-camera | Honored | Shell host publishes the standard Lockdown+Access backend from this key, so xdg-desktop-portal denies new Camera sessions while set (live daemon proof); the GTK shell additionally destroys Video/Source nodes and camera-linked capture streams on the PipeWire graph, existing and newly appearing, and restores on explicit re-allow. See privacy-enforcement.md |
| disable-microphone | Honored | GTK shell mutes every PipeWire Audio/Source while set and re-mutes bypass attempts; explicit restore unmutes only tracker-muted nodes. GNOME publishes this key to Lockdown with no enforcer; enforcement here is Tuna-defined. See privacy-enforcement.md |
| disable-sound-output | Honored | GTK shell mutes every PipeWire Audio/Sink while set and re-mutes bypass attempts; explicit restore unmutes only tracker-muted nodes. GNOME publishes this key to Lockdown with no enforcer; enforcement here is Tuna-defined. See privacy-enforcement.md |
| hide-identity | Ignored | Tuna Desktop has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| old-files-age | Honored (gsd-housekeeping, unqualified) | gnome-settings-daemon 51's housekeeping plugin, started by the Tuna session (V-GSD-PLUGINS), purges old trash and temporary files; purging is unproven |
| privacy-screen | Ignored | Tuna Desktop has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| recent-files-max-age | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| remember-app-usage | Ignored | Tuna Desktop has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| remember-recent-files | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| remove-old-temp-files | Honored (gsd-housekeeping, unqualified) | gnome-settings-daemon 51's housekeeping plugin, started by the Tuna session (V-GSD-PLUGINS), purges old temporary files; purging is unproven |
| remove-old-trash-files | Honored (gsd-housekeeping, unqualified) | gnome-settings-daemon 51's housekeeping plugin, started by the Tuna session (V-GSD-PLUGINS), purges old trash; purging is unproven |
| report-technical-problems | Ignored | Tuna Desktop has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| send-software-usage-stats | Ignored | Tuna Desktop has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| show-full-name-in-top-bar | Ignored | Tuna Desktop has no privacy retention/location/cleanup policy consumer; applications may implement their own |
| usb-protection | Honored (gsd-usb-protection, unqualified) | gnome-settings-daemon 51's usb-protection plugin, started by the Tuna session (V-GSD-PLUGINS), applies it only through a running USBGuard daemon; unproven |
| usb-protection-level | Honored (gsd-usb-protection, unqualified) | gnome-settings-daemon 51's usb-protection plugin, started by the Tuna session (V-GSD-PLUGINS), applies it only through a running USBGuard daemon; unproven |
## org.gnome.desktop.remote-desktop.rdp

| Key | Status | Reason |
|---|---|---|
| auth-methods | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| enable | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| kerberos-keytab | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| negotiate-port | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| port | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| screen-share-mode | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| tls-cert | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| tls-key | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| view-only | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
## org.gnome.desktop.remote-desktop.rdp.headless

| Key | Status | Reason |
|---|---|---|
| auth-methods | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| enable | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| kerberos-keytab | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| negotiate-port | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| port | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
## org.gnome.desktop.remote-desktop.vnc

| Key | Status | Reason |
|---|---|---|
| auth-method | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| enable | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| encryption | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| negotiate-port | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| port | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| screen-share-mode | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| view-only | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
## org.gnome.desktop.remote-desktop.vnc.headless

| Key | Status | Reason |
|---|---|---|
| enable | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| negotiate-port | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
| port | Ignored | GNOME Remote Desktop owns these keys; Tuna Desktop does not start or configure that daemon |
## org.gnome.desktop.screen-time-limits

| Key | Status | Reason |
|---|---|---|
| daily-limit-enabled | Honored | Limit evaluated on the 30 s poll and on settings change |
| daily-limit-seconds | Honored | Daily allowance in seconds |
| grayscale | Honored (partial) | GTK screen-time tracker → correlated protocol0.35 Applied → compositor wellbeing_effects desaturation. Exact-source native renderer pixels independently verified; production GTK policy and installed DRM/GPU acceptance remain open (#354) |
| history-enabled | Honored | Recording on/off; off deletes the history file live |
## org.gnome.desktop.screensaver

| Key | Status | Reason |
|---|---|---|
| color-shading-type | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| embedded-keyboard-command | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| embedded-keyboard-enabled | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| idle-activation-enabled | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| lock-delay | Honored | Live idle lock policy or lock background |
| lock-enabled | Honored | Live idle lock policy or lock background |
| logout-command | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| logout-delay | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| logout-enabled | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| picture-opacity | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| picture-options | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| picture-uri | Honored | Live idle lock policy or lock background |
| primary-color | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| restart-enabled | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| secondary-color | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| show-full-name-in-top-bar | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| status-message-enabled | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
| user-switch-enabled | Ignored | Tuna Desktop applies idle lock and picture URI; no GNOME screensaver theme/status/switch-user policy |
## org.gnome.desktop.search-providers

| Key | Status | Reason |
|---|---|---|
| disable-external | Honored | Provider discovery/order policy |
| disabled | Honored | Provider discovery/order policy |
| enabled | Honored | Provider discovery/order policy |
| sort-order | Honored | Provider discovery/order policy |
## org.gnome.desktop.session

| Key | Status | Reason |
|---|---|---|
| idle-delay | Honored | Live compositor idle policy; gsd-power dims at half the delay and blanks after the shield (V-GSD-POWER-IDLE) |
| save-restore | Ignored | Tuna Desktop session does not consume this legacy session preference |
| session-name | Ignored | Tuna Desktop session does not consume this legacy session preference |
## org.gnome.desktop.sound

| Key | Status | Reason |
|---|---|---|
| allow-volume-above-100-percent | Ignored | No verified Tuna Desktop audio preference consumer beyond toolkit translation |
| event-sounds | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| input-feedback-sounds | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| theme-name | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
## org.gnome.desktop.thumbnail-cache

| Key | Status | Reason |
|---|---|---|
| maximum-age | Ignored | Thumbnail cache is external; Tuna Desktop has no cache retention consumer |
| maximum-size | Ignored | Thumbnail cache is external; Tuna Desktop has no cache retention consumer |
## org.gnome.desktop.thumbnailers

| Key | Status | Reason |
|---|---|---|
| disable | Ignored | Thumbnail generation is external; Tuna Desktop has no thumbnailer policy consumer |
| disable-all | Ignored | Thumbnail generation is external; Tuna Desktop has no thumbnailer policy consumer |
## org.gnome.desktop.wm.keybindings

| Key | Status | Reason |
|---|---|---|
| activate-window-menu | Honored | Live shell accelerator registration and compositor dispatch |
| always-on-top | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| begin-move | Honored | Live shell accelerator registration and compositor dispatch |
| begin-resize | Honored | Live shell accelerator registration and compositor dispatch |
| close | Honored | Live shell accelerator registration and compositor dispatch |
| cycle-group | Honored | Live SetSwitcherKeys registration; window MRU popup or immediate window/group cycling |
| cycle-group-backward | Honored | Live SetSwitcherKeys registration; window MRU popup or immediate window/group cycling |
| cycle-panels | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| cycle-panels-backward | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| cycle-windows | Honored | Live SetSwitcherKeys registration; window MRU popup or immediate window/group cycling |
| cycle-windows-backward | Honored | Live SetSwitcherKeys registration; window MRU popup or immediate window/group cycling |
| lower | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| maximize | Honored | Live shell accelerator registration and compositor dispatch |
| maximize-horizontally | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| maximize-vertically | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| minimize | Honored | Live shell accelerator registration and compositor dispatch |
| move-to-center | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-corner-ne | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-corner-nw | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-corner-se | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-corner-sw | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-monitor-down | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-monitor-left | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-monitor-right | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-monitor-up | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-side-e | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-side-n | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-side-s | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-side-w | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-workspace-1 | Honored | Live shell accelerator registration and compositor dispatch |
| move-to-workspace-10 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-workspace-11 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-workspace-12 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-workspace-2 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-workspace-3 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-workspace-4 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-workspace-5 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-workspace-6 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-workspace-7 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-workspace-8 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-workspace-9 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-workspace-down | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| move-to-workspace-last | Honored | Live shell accelerator registration and compositor dispatch |
| move-to-workspace-left | Honored | Live shell accelerator registration and compositor dispatch |
| move-to-workspace-right | Honored | Live shell accelerator registration and compositor dispatch |
| move-to-workspace-up | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| panel-main-menu | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| panel-run-dialog | Honored (GTK shell) | Opens GNOME's Run a Command dialog in normal and overview modes (main.js), read and rebound live through the shell's grabs; refused while `disable-command-line` is set (#357) |
| raise | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| raise-or-lower | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| set-spew-mark | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| show-desktop | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-applications | Honored | Live shell accelerator registration and compositor dispatch |
| switch-applications-backward | Honored | Live shell accelerator registration and compositor dispatch |
| switch-group | Honored | Live shell accelerator registration and compositor dispatch |
| switch-group-backward | Honored | Live shell accelerator registration and compositor dispatch |
| switch-input-source | Honored | Live shell accelerator: GNOME 51's MRU switcher popup (quick tap picks the second most recent source; held, each press steps and releasing the modifier picks; another shell popup or the lock screen steps by position) (#346) |
| switch-input-source-backward | Honored | Live shell accelerator: the MRU popup starting on the least recent source and stepping backward (#346) |
| switch-panels | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-panels-backward | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-1 | Honored | Live shell accelerator registration and compositor dispatch |
| switch-to-workspace-10 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-11 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-12 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-2 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-3 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-4 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-5 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-6 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-7 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-8 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-9 | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-down | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-to-workspace-last | Honored | Live shell accelerator registration and compositor dispatch |
| switch-to-workspace-left | Honored | Live shell accelerator registration and compositor dispatch |
| switch-to-workspace-right | Honored | Live shell accelerator registration and compositor dispatch |
| switch-to-workspace-up | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| switch-windows | Honored | Live SetSwitcherKeys registration; window MRU popup or immediate window/group cycling |
| switch-windows-backward | Honored | Live SetSwitcherKeys registration; window MRU popup or immediate window/group cycling |
| toggle-above | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| toggle-fullscreen | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| toggle-maximized | Honored | Live shell accelerator registration and compositor dispatch |
| toggle-on-all-workspaces | Ignored | This accelerator has no Tuna Desktop action/registration; only the listed live bindings are grabbed |
| unmaximize | Honored | Live shell accelerator registration and compositor dispatch |
## org.gnome.desktop.wm.preferences

| Key | Status | Reason |
|---|---|---|
| action-double-click-titlebar | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| action-middle-click-titlebar | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| action-right-click-titlebar | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| audible-bell | Ignored | Tuna Desktop uses click focus and dynamic workspaces; this WM policy has no consumer |
| auto-raise | Honored (compositor) | Owned GIO reader, live; in sloppy/mouse focus the entered window rises after auto-raise-delay if still under the pointer and not on top (Mutter 51 display.c); never in click focus. Proof G-FOCUS-AUTORAISE |
| auto-raise-delay | Honored (compositor) | Owned GIO reader, live; the auto-raise delay, clamped to the schema's 0–10000 ms. Proof G-FOCUS-AUTORAISE |
| button-layout | Via GTK | GTK Wayland settings translation; clients follow the portal or GSettings fallback |
| disable-workarounds | Ignored | Tuna Desktop uses click focus and dynamic workspaces; this WM policy has no consumer |
| focus-mode | Honored (compositor) | Owned GIO reader, live; click, sloppy (focus on enter, kept over the desktop) and mouse (also unfocused over the desktop and shell chrome), with focus-change-on-pointer-rest and Mutter's sticky activation focus; the scrollable strip keeps click focus. Proofs G-FOCUS-SLOPPY, G-FOCUS-MOUSE |
| focus-new-windows | Ignored | Tuna Desktop uses click focus and dynamic workspaces; this WM policy has no consumer |
| mouse-button-modifier | Honored (compositor) | Owned GIO reader, live; parsed like Mutter's meta_parse_modifier (empty/disabled turns modifier clicks off); with it held button 1 moves, the resize button resizes from the pressed third, the menu button opens the window menu. Mod2/Mod3/Mod5 contribute nothing. Proof G-FOCUS-MODIFIER |
| num-workspaces | Honored | The fixed workspace count (1–36) while `org.gnome.mutter dynamic-workspaces` is off, live; shrinking moves windows to the new last workspace as mutter 51. Proof G-WS-FIXED (P-WM-08) |
| raise-on-click | Honored (compositor) | Owned GIO reader, live; false focuses clicked windows and xdg-activation requests without raising, and only grabs under 8 px raise at release; shell activations still raise (Mutter's pager rule). Proof G-FOCUS-RAISE-ON-CLICK |
| resize-with-right-button | Honored (compositor) | Owned GIO reader, live; true makes the right button resize and the middle button open the window menu with mouse-button-modifier. Proof G-FOCUS-MODIFIER |
| theme | Ignored | Tuna Desktop uses click focus and dynamic workspaces; this WM policy has no consumer |
| titlebar-font | Ignored | Tuna Desktop uses click focus and dynamic workspaces; this WM policy has no consumer |
| titlebar-uses-system-font | Ignored | Tuna Desktop uses click focus and dynamic workspaces; this WM policy has no consumer |
| visual-bell | Ignored | Tuna Desktop uses click focus and dynamic workspaces; this WM policy has no consumer |
| visual-bell-type | Ignored | Tuna Desktop uses click focus and dynamic workspaces; this WM policy has no consumer |
| workspace-names | Ignored | Tuna Desktop uses click focus and dynamic workspaces; this WM policy has no consumer |
