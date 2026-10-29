# GNOME settings compatibility map

Tuna Desktop reads GNOME 51's own GSettings schemas. It ships no shim schemas
and runs no translating daemon.

The [exhaustive desktop inventory](settings-desktop-inventory.md) lists
every one of the 428 GNOME 51 desktop-schema keys. The sections below
explain common keys and related Shell/Mutter/settings-daemon settings. The Marlin image already carries
`gsettings-desktop-schemas`, so the keys exist and GNOME Settings can write them. Schema presence does not
establish functional compatibility: [the GNOME Settings audit](gnome-settings-audit.md)
tracks ineffective controls and unqualified service integrations. Ledger row P-ST-01 tracks this page.

This page records that decision for R9 (knowledge entry
`learnings/dconf-settings-interop.md`).

**Status words.**
- **Honored**: Tuna Desktop reads the key and reacts while it runs.
- **Via GTK**: GTK or libadwaita reads the key for every app and for the
  shell, so nothing in Tuna Desktop has to.
- **Ignored**: the key has no effect yet. The reason and owner are given.

## org.gnome.desktop.interface

| Key | Status | Notes |
|---|---|---|
| color-scheme | Honored | The Dark Style tile writes it. The shell and apps follow it through libadwaita. Proof G-QS-DARK |
| accent-color | Via GTK | libadwaita applies it |
| font-name | Honored | Live shell CSS family, base size, weight, style and stretch (with semantic emphasis preserved) and GTK client font settings; proof G-SETTINGS-FONT |
| document-font-name, monospace-font-name | Via GTK | Live for GTK apps reading the keys via Gio.Settings (as gnome-terminal/gedit do), with a visible repaint; Tuna Desktop chrome still uses font-name. Proof G-SET-FONTS |
| text-scaling-factor | Via GTK | Live GTK Wayland DPI translation; proof G-SETTINGS-FONT |
| gtk-theme, icon-theme, cursor-theme, cursor-size | Via GTK | Inside apps. The compositor's own cursor ignores them (#89) |
| clock-format | Honored | Panel clock, live 12h/24h rendering. Proof G-SET-CLOCK |
| clock-show-weekday, clock-show-date, clock-show-seconds | Honored | Panel clock, built as gnome-desktop's wall clock builds it. Proofs G-SETTINGS-CLOCK (seconds), G-SET-CLOCK (date) |
| enable-hot-corners | Honored | Live. Proof G-SETTINGS-INPUT |
| enable-animations | Honored | Live idle fade, GTK transitions and compositor overview/strip motion; disabled transitions finish immediately. Proofs G-ANIMATIONS-OFF, G-INTROSPECT-MOTION |
| show-battery-percentage | Honored | UPower DisplayDevice panel percentage; updates live |

GNOME 51 `org.gnome.desktop.a11y.interface reduced-motion` is read live and combines with `enable-animations` into one motion policy shared by the shell and compositor: `full`, `fade-only` or `off`. `enable-animations=false` is `off`: every transition finishes immediately. `reduce` with animations enabled is `fade-only`, as in GNOME 51: opacity fades (the idle shield) keep running, while translation and scale (compositor overview and strip motion, the shell's GTK transitions) finish immediately. `enable-animations` itself, and Introspect's `AnimationsEnabled`, stay true under Reduced Motion, as GNOME reports them. GNOME Shell's `GNOME_SHELL_SLOWDOWN_FACTOR` environment variable scales every duration as GNOME's `adjustAnimationTime` does. Older schema sets keep the existing animation policy. This controls the desktop shell; application toolkits remain responsible for their own transitions. Proofs G-ANIMATIONS-OFF, G-INTROSPECT-MOTION.

## Session, lock and notifications

| Schema and key | Status | Notes |
|---|---|---|
| org.gnome.desktop.session idle-delay | Honored | Live, through the shell's `SetIdleTimeout` command (G-SETTINGS-IDLE); gsd-power dims at half the delay and blanks 30 s into idleness once the shield is up (V-GSD-POWER-IDLE) |
| org.gnome.desktop.screensaver lock-enabled, lock-delay | Honored | Idle fades to black over ten seconds; lock waits for the larger of fade duration and lock-delay. Activity cancels blanking; an authenticated lock remains latched |
| org.gnome.desktop.screensaver picture-uri | Honored | Blurred, dimmed lock background; empty or missing files fall back to the desktop wallpaper |
| org.gnome.desktop.notifications show-banners | Honored | Mirrors Do Not Disturb live in both directions. Proof G-NOTIFY-DND and G-SETTINGS-NOTIFICATIONS (external writes and banner policy) |
| org.gnome.desktop.search-providers (all keys) | Honored | disable-external, disabled, enabled and sort-order. Proof G-SEARCH-PROVIDER |
| org.gnome.desktop.background picture-uri, picture-uri-dark | Honored | Wallpaper: live publish, file identity, decode and visible repaint, with file-replacement refresh, an ignored invalid URI and revert. Proof G-SET-WALLPAPER |
| org.gnome.shell favorite-apps | Honored | Used for the dash when Tuna Desktop has no pins of its own |
| org.gnome.desktop.app-folders (folder-children and each folder's name, apps, categories, excluded-apps, translate) | Honored | App-grid folders, read on each grid open. Proof G-APP-FOLDERS |
| org.gnome.shell enabled-extensions | Ignored | GNOME Shell extensions are JavaScript. Tuna Desktop has its own extension point |
| org.gnome.settings-daemon.plugins.color night-light-enabled | Honored | The Night Light tile writes it; the compositor reads it live and tints every output in GL. It owns `org.gnome.SettingsDaemon.Color`, so gsd-color is not started. Proof G-QS-NIGHT (#339) |
| org.gnome.settings-daemon.plugins.color night-light-temperature, night-light-schedule-automatic, night-light-schedule-from, night-light-schedule-to, night-light-last-coordinates | Honored | Read live by the compositor: gsd 51's clamp, local-time automatic and from/to schedules (midnight wrap, DST, runtime zone change), one-hour edge smear and 5 s jump smoothing. Geoclue never updates night-light-last-coordinates, so automatic mode uses the stored value or the from/to window. Proof G-QS-NIGHT drives NightLightPreview against night-light-temperature (#339) |
| org.gnome.settings-daemon.plugins.power idle-dim, idle-brightness | Honored | gsd-power calls the shell's `org.gnome.Shell.Brightness.SetDimming`; the shell clips to `idle-brightness`. Proof V-GSD-POWER-IDLE (call observed; no physical backlight in the VM) |
| org.gnome.settings-daemon.plugins.power sleep-inactive-ac-timeout, sleep-inactive-ac-type, sleep-inactive-battery-timeout, sleep-inactive-battery-type, power-saver-profile-on-low-battery, ambient-enabled | Honored (unqualified) | Consumed by the running gsd-power; automatic suspend, battery policy and ambient light are not proven (gsd-power never suspends a VM) |
| org.gnome.settings-daemon.plugins.power power-button-action | Ignored | GNOME 51's consumer is gsd-media-keys, which the Tuna session does not start because the shell binds the media keys; logind's own HandlePowerKey applies |

## GNOME Settings Daemon plugins

GNOME 51 starts gnome-settings-daemon's plugins from gnome-session's
`gnome-session-basic-services.target` and `gnome-session-services.target`
(gnome-session 51 `data/`), each through the plugin's own
`org.gnome.SettingsDaemon.<Plugin>.target`, which requires
`gnome-session-initialized.target`. `tuna-session.target` wants
`gnome-session.target`, `gnome-session-initialized.target` and the plugin
targets below; the compositor holds gnome-session's leader FIFO so the targets
and plugins stop with the session, and starts
`gnome-session-x11-services-ready.target` when Xwayland is up, as GNOME Shell
51 does. The compositor serves the `org.gnome.SessionManager` subset the
plugins read (`InhibitedActions`, `SessionIsActive`, `Inhibit`, `Uninhibit`,
`IsInhibited`, `GetInhibitors`); idle inhibitors also hold off Tuna's idle lock.
Proof V-GSD-PLUGINS checks every started plugin's process, user, unit and bus name.

| Plugin (bus name `org.gnome.SettingsDaemon.*`) | GNOME 51 target | Tuna session |
|---|---|---|
| Power | basic-services | Started by gsd |
| Keyboard | basic-services | Started by gsd |
| Rfkill | basic-services | Started by gsd |
| Sharing | basic-services | Started by gsd |
| Smartcard | basic-services | Started by gsd |
| UsbProtection | basic-services | Started by gsd (acts only with USBGuard) |
| Datetime | services | Started by gsd |
| Housekeeping | services | Started by gsd |
| PrintNotifications | services | Started by gsd |
| ScreensaverProxy | services | Started by gsd (forwards `org.freedesktop.ScreenSaver` inhibits to the compositor's session manager) |
| Sound | services | Started by gsd |
| Wwan | services | Started by gsd |
| XSettings | x11-services | Started by gsd when Xwayland is ready (`DISPLAY` published then); `org.gnome.Mutter.X11` is not served, so X11 scale falls back to 1 |
| Color | services | Reimplemented by Tuna: the compositor owns the bus name (Night Light); ICC profiles are missing (P-DS-03) |
| MediaKeys | basic-services | Reimplemented by Tuna for volume, brightness and lock keys; the other media bindings, including `power-button-action`, are missing |
| A11ySettings | basic-services | Reimplemented by Tuna: the compositor runs Orca and the shell sets `toolkit-accessibility`; gsd would start a second Orca |

## Displays

| File | Status | Notes |
|---|---|---|
| ~/.config/monitors.xml | Honored | The arrangement for the lit connectors sets each output's scale, position and primary monitor (hardware sessions). Proof D-SCALE |
| GNOME Settings' Displays panel | Partial | ApplyMonitorsConfig over org.gnome.Mutter.DisplayConfig changes scale and position live; "keep changes" saves to ~/.config/tuna/monitors.xml, which Tuna Desktop reads before GNOME's file (GNOME's own file is never rewritten). Proof G-DISPLAY-SETTINGS |

## Window management and input

| Schema and key | Status | Notes |
|---|---|---|
| org.gnome.shell.keybindings show-screenshot-ui, toggle-overview, toggle-application-view, toggle-message-tray, toggle-quick-settings, switch-to-application-1..9, open-new-window-application-1..9, screenshot, screenshot-window, screen-brightness-up, screen-brightness-down | Honored | read live, GNOME 51's defaults when the schema is missing; grabbed through the compositor like any accelerator (`crates/shell-gtk/src/keybindings.rs`) |
| org.gnome.shell.keybindings show-screen-recording-ui, focus-active-notification, shift-overview-up/down | Honored | recording UI; focus the current banner (Escape returns input); step session/picker/app grid in both directions; read and rebound live through shell grabs |
| org.gnome.shell.keybindings screen-brightness-up/down-monitor, screen-brightness-cycle(-monitor) | Honored | up/down/cycle through logind; monitor keys select the DRM connector under the pointer and never substitute another screen's backlight. Devices without a kernel connector association are unsupported and unchanged |
| org.gnome.settings-daemon.plugins.media-keys screensaver | Honored | Read live by the shell, with Super+L as the default; works without gsd-media-keys. Proof G-LOCK-KEY |
| org.gnome.settings-daemon.plugins.media-keys volume-up/down/mute, quiet/precise variants, mic-mute, their -static bindings, volume-step | Honored | Native async PipeWire controls and OSD, including while locked; reads both editable and static arrays live, 6% default / 2% precise. Range capped at 100%; feedback sounds and amplification are not implemented. Proof G-MEDIA-VOLUME / V-MEDIA-VOLUME |
| org.gnome.desktop.wm.keybindings activate-window-menu, toggle-maximized, maximize, unmaximize, minimize, close, begin-move, begin-resize, switch-input-source(-backward), switch-to-workspace-1/last/left/right, move-to-workspace-1/last/left/right; org.gnome.mutter.keybindings toggle-tiled-left/right | Honored | read live (GNOME 51's defaults without the schema), grabbed like any accelerator; once the shell grabs them the compositor's built-in defaults stand aside, so a rebound key moves and an emptied one reaches apps |
| org.gnome.desktop.wm.keybindings switch-applications(-backward), switch-group(-backward) | Honored | read live (GNOME 51's defaults without the schema) and handed to the compositor, which holds the switcher open while the chord's modifiers are held and commits when they are released; Shift steps backward, `Above_Tab` is the key above Tab |
| org.gnome.desktop.wm.keybindings switch-windows(-backward), cycle-windows(-backward), cycle-group(-backward) | Honored | compositor drives bound keys through SetSwitcherKeys: a window-only MRU popup (current workspace), or immediate window/app-group cycling without a popup. GNOME's switch-windows defaults are empty |
| org.gnome.desktop.wm.preferences button-layout | Via GTK | Client-side decorations read it |
| org.gnome.desktop.wm.preferences focus-mode, auto-raise, auto-raise-delay, raise-on-click, mouse-button-modifier, resize-with-right-button; org.gnome.mutter focus-change-on-pointer-rest | Honored | Read live by the compositor on an owned GIO thread, as Mutter 51 reads them in-process; GNOME 51's defaults (pointer rest on, as GNOME Shell's override) when a schema is missing. Sloppy/mouse focus on enter without raising, auto-raise in those modes only, raise-on-click for clicks, client activations and short grabs, the modifier and button for move/resize/window menu. Tuna differs: the scrollable strip keeps click focus; Mod2/Mod3/Mod5 modifiers contribute nothing. Proofs G-FOCUS-SLOPPY, G-FOCUS-AUTORAISE, G-FOCUS-MOUSE, G-FOCUS-RAISE-ON-CLICK, G-FOCUS-MODIFIER |
| org.gnome.desktop.wm.preferences num-workspaces | Ignored | Dynamic workspaces, as GNOME's default |
| org.gnome.desktop.wm.preferences audible-bell, visual-bell | Ignored | The bell always sounds (GNOME's default audible bell) and never flashes (xdg-system-bell, #89) |
| org.gnome.mutter dynamic-workspaces, edge-tiling | Ignored | Always on, as GNOME's defaults |
| org.gnome.desktop.peripherals.keyboard repeat, delay, repeat-interval | Honored | Seat key repeat, live. Proof G-SETTINGS-INPUT |
| org.gnome.desktop.a11y.keyboard (Sticky, Slow, Bounce, Toggle and Mouse Keys, their beeps and delays, `enable`) | Honored | Live; applied in the compositor's input pipeline before keybindings, the lock screen and every client, XWayland included. The keyboard shortcuts' switches are saved by the shell, which shows GNOME's confirmation. `timeout-enable` and `disable-timeout` are ignored, as in Mutter. Proofs G-A11Y-STICKY, G-A11Y-SHORTCUTS, G-A11Y-SLOW, G-A11Y-BOUNCE, G-A11Y-MOUSEKEYS |
| org.gnome.desktop.a11y.mouse (secondary click, hover click: time, threshold, mode, gesture directions) | Honored | Live; applied in the compositor's input pipeline ahead of every client. Window mode shows GNOME's click-type chooser in the panel; the shell draws GNOME's pie timer during each timeout. `click-type-window-visible` is ignored, as in GNOME Shell 51. Proofs G-A11Y-DWELL, G-A11Y-SECONDARY |
| org.gnome.desktop.peripherals.touchpad tap-to-click, natural-scroll, speed, disable-while-typing, left-handed, send-events, click-method, edge-scrolling-enabled, two-finger-scrolling-enabled, tap-and-drag, tap-and-drag-lock, tap-button-map, middle-click-emulation, accel-profile | Honored | libinput on hardware sessions, live and on hotplug, with Mutter 51's touchpad rules; left/right/mouse enum resolved in the shell. `disabled-on-external-mouse` is libinput's own mode; edge scrolling yields to two-finger scrolling as in Mutter; `tap-and-drag-lock` gets libinput's timeout drag lock (the pinned bindings cannot ask for Mutter's sticky lock); `custom` acceleration and `disable-while-typing-timeout` are unsupported by the pinned bindings and keep the device default. Proofs V-MOUSE-HANDEDNESS, V-POINTER-LIVE, V-POINTER-HOTPLUG, G-SETTINGS-POINTER; the VM has no touchpad, so touchpad application is unit-tested and physical qualification remains (#343) |
| org.gnome.desktop.peripherals.mouse natural-scroll, speed, left-handed, accel-profile, middle-click-emulation, scroll-wheel-emulation-button, scroll-wheel-emulation-button-lock | Honored | libinput on hardware sessions, live and on hotplug. `accel-profile`, middle-click and scroll-wheel emulation apply to mice only (not trackballs or pointing sticks, as Mutter 51); `custom` acceleration falls back to the device default. Proofs V-MOUSE-HANDEDNESS, V-POINTER-LIVE, V-POINTER-HOTPLUG, G-SETTINGS-POINTER (#343) |
| org.gnome.desktop.peripherals.trackball accel-profile, middle-click-emulation, scroll-wheel-emulation-button, scroll-wheel-emulation-button-lock; org.gnome.desktop.peripherals.pointingstick speed, accel-profile, scroll-method | Honored | libinput on hardware sessions, live and on hotplug, for devices udev tags `ID_INPUT_TRACKBALL` / `ID_INPUT_POINTINGSTICK` (device or parent), as Mutter 51; trackballs keep the mouse speed, pointing sticks take their own. `custom` acceleration falls back to the device default. Proof G-SETTINGS-POINTER; the VM has neither device, so application is unit-tested and physical qualification remains (#343) |
| org.gnome.settings-daemon.peripherals.touchscreen orientation-lock | Ignored | No GSettings reader yet: the compositor's `OrientationPolicy` honors `InputSettings.orientation_lock` and serves `PanelOrientationManaged` live, but nothing publishes the key (mutter 51 `CONF_SCHEMA`; #345 follow-up) |
| org.gnome.desktop.input-sources sources, xkb-options | Honored | xkb sources become one keymap in order; Super+Space switches; IBus sources use the supervised bridge (#60). Proof G-SETTINGS-INPUT |
| org.gnome.desktop.input-sources mru-sources, per-window | Honored | GNOME 51's InputSourceManager in the GTK shell: MRU order seeded from and saved to `mru-sources`, the Super+Space switcher popup over it, per-window sources (the overview counts as a window). XKB sources only (#346). Proofs G-INPUT-SOURCE-MRU, G-INPUT-PER-WINDOW |
| org.gnome.desktop.peripherals.keyboard remember-numlock-state, numlock-state | Honored | The compositor restores Num Lock at startup and saves each change, as mutter 51 does; keymap reloads keep it (#346). Proof G-NUMLOCK |
| org.gnome.desktop.a11y.* | Partial / missing | Some keys reach GTK and the keyboard and pointer aids are honored (above); zoom, screen keyboard and several modern preferences lack consumers. Spoken screen-reader acceptance remains unqualified; see the control-center audit |

`J-SETTINGS-LIVE` changes `clock-format` with `gsettings set` during the
nested journey and asserts the shell’s consumed settings snapshot before
restoring it. GTK font/scale, dark style, input, idle and rebound keybinding
changes have separate live graphical gates, as do wallpaper
identity/refresh (`G-SET-WALLPAPER`), clock format/date rendering
(`G-SET-CLOCK`) and document/monospace app reads (`G-SET-FONTS`); each
reverts its keys and carries a negative control.
