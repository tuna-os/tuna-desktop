# Tuna Desktop v1 visual audit — 2026-10-12

## Verdict

**Incomplete; not a v1 visual sign-off.** Reviewed all 113 downloaded image entries in contact sheets (including duplicate PNG/PPM frames, boot screens and a title-card preview), inspected calendar, quick settings, grid drag and lock recovery at original size, and inspected 20 tour samples at four-second intervals. These are not 113 features. Video sampling does not establish animation smoothness or intermediate-frame parity.

The checklist enumerates all 72 ledger capabilities and 46 walkthrough states. 16 capabilities have an observed visual subset; none is promoted to full visual qualification. Only 11/46 required walkthrough filenames are present in the downloaded GTK artifact. Missing or unimplemented states stay open.

## Provenance

- PR #633 branch head: `b1c929aa55b0f3286458035fbc81ecd62d168496`.
- [CI run 38217542137](https://github.com/tuna-os/tuna-desktop/actions/runs/38217542137), attempts 1 and 2; artifacts must not be treated as one uninterrupted capture session.
- GTK artifact: `11738581711` (attempt 2); installed Marlin artifact: `11734091402` (attempt 1); tour artifact: `11733986549`; scroll artifact: `11732747854`; journey artifact: `11733053754`.
- Marlin manifest records tested synthetic PR merge commit `7b6c85922af14f4c239a4f958262b657adc6b03f`, image ID `e38a739f7a0bc94fce9e55123ff364eef5dbae46fc6f31a6e4307168757bdd47`, package `tuna-desktop-0.1.0-1-x86_64.pkg.tar.zst`, DRM/KMS with virtio and CI-only greetd initial session at 1280×800. This is not real hardware/full production greeter qualification.
- The newer panel/privacy/Night Light branches are not included in this integration head. Their captures need separate provenance until integrated.
- No matched current GNOME 51 reference captures were inspected in this audit; no pixel-parity claim is made.

## Findings

1. **Tour stills fail their named state.** `T-app-grid.png`, `T-calendar.png`, and `T-quick-settings.png` show wallpaper without the named UI. `T-overview.png` has search/dash but no workspace overview card. Replace these with state-confirmed captures. The video sample at 28 seconds does show quick settings, so a bad still does not prove that feature is broken.
2. **Grid visual and persistence review fails.** Latest GTK assertions end with `G-GRID-DRAG-CANCEL fail cancelled folder drag changed the saved layout`; the `11h-folder-page-peek.png` capture shows overlapping/neighbor page content during drag. Verify revert and persisted order after Escape, and collect the settled post-cancel frame. A transition frame alone cannot prove the final layout is correct.
3. **Full capture coverage is blocked by early exit.** Later notification, input-method, screenshot, scaling, workspace, authentication and submenu captures are absent from this GTK run. Use independent feature stages for collection while preserving ordinary failure gates.
4. **Nested screenshots are fixture views.** Blue wallpaper, probe apps and synthetic service state are useful for layout inspection but cannot qualify shipped apps, real daemon failure handling or installed settings.
5. **Tour title card still says “Roost on TunaOS Marlin”.** Refresh release media branding after the capture-state defects are fixed. Title cards do not count as rendered feature evidence.

## Observations with limited scope

Calendar text, event list, selected date, quick-settings labels/sliders and lock prompt appear legible in inspected full-resolution frames. Overview windows, switcher thumbnails, tile preview, installed Files/System Monitor windows and strip geometry render in the sampled states. No whole-feature pass follows from these observations. Static lock images cannot prove absence of leakage across transitions.

## Required completion

For every checklist row: capture one frozen integrated source/package/image identity, normal/active/disabled/error and keyboard focus states where applicable, long labels and light/dark appearance, 100/125/150/200% scale and multi-output behavior. Record actual consumer output for nonvisual features. Animation rows need real transition clips plus intermediate frames against the matched GNOME baseline. Mark unavailable device tests explicitly; do not infer a pass. Capture installed-image/Corral behavior independently of nested fixtures.

Muse acknowledged the parallel visual-capture lane request. Initial gallery and frame index live in `/tmp/tuna-v1-visual-review/review/`; retain/download the GitHub originals for durable evidence.

## All-capability checklist

| ID | Capability | Visual audit | Reviewed files | Missing named walkthrough states |
|---|---|---|---|---|
| P-OV-01 | Super opens the overview with live window previews for the current workspace | Observed subset; qualification open | `gtk-shell/05-overview.png`, `marlin-vm/V-OVERVIEW.png` | — |
| P-OV-02 | Overview shows the workspace strip with thumbnails; drag or click switches | No adequate capture reviewed | — | Workspaces, Moving windows |
| P-OV-03 | Overview shows the dash with favorites and running apps | Observed subset; qualification open | `gtk-shell/09-overview-shell.png` | — |
| P-OV-04 | Typing in the overview searches apps and launches the top hit with Enter | Observed subset; qualification open | `gtk-shell/10-search.png` | Search providers |
| P-OV-05 | App grid with pages and folders | FAIL: drag-cancel layout assertion; subset visible | `gtk-shell/11c-app-grid-pages.png` | App grid, App folders, Making folders, Reordering |
| P-OV-06 | Escape returns to the previously focused window | No adequate capture reviewed | — | — |
| P-PN-01 | Top panel: Activities, centered clock, system menu right | Observed subset; qualification open | `gtk-shell/01-panel.png` | — |
| P-PN-02 | Clock opens calendar plus notification list | Observed subset; qualification open | `gtk-shell/02-calendar.png` | — |
| P-PN-03 | Quick settings grid: Wi-Fi, Bluetooth, power mode, night light, dark style, volume, brightness | No adequate capture reviewed | — | Quick settings, Wi-Fi menu, Wi-Fi password, Wired connections, Bluetooth menu, Microphone |
| P-NT-01 | Notification banners stack top-center with actions and dismissal | No adequate capture reviewed | — | Banners |
| P-NT-02 | Do Not Disturb toggle and notification history | No adequate capture reviewed | — | Notification list, Do Not Disturb, Notification groups, Expanded group |
| P-WM-01 | Alt-Tab switcher with app icons and window previews | Observed subset; qualification open | `gtk-shell/08b-switcher.png`, `gtk-shell/08c-switcher-thumbnails.png` | — |
| P-WM-02 | Super+Up maximizes, Super+Left/Right tiles halves | Observed subset; qualification open | `gtk-shell/08-tile-preview.png` | — |
| P-WM-03 | Dynamic workspaces, Super+PgUp/PgDn switch, Shift moves window | No adequate capture reviewed | — | Workspace switcher |
| P-WM-04 | Interactive drag-to-move and resize with pointer | Observed subset; qualification open | `gtk-shell/08-dragged.png` | — |
| P-WM-06 | App menus and popovers open as xdg popups and close on outside click | Observed subset; qualification open | `gtk-shell/07-app-popover.png` | — |
| P-WM-05 | Window close, minimize, maximize via server-side or client decorations | Observed subset; qualification open | `gtk-shell/04-windows.png` | Window menu |
| P-WM-07 | Scrollable tiling (niri mode, Super+Shift+T): a strip of columns, Super+R widths, Super+arrows along the strip | Observed subset; qualification open | `scroll/02-strip.png`, `scroll/03-preset.png`, `scroll/04-wheel.png` | The strip, Following focus, Column widths |
| P-LK-01 | Idle blanks then locks; password unlocks; nothing leaks while locked | Observed subset; qualification open | `marlin-vm/lifecycle/shell-recovered.png`, `marlin-vm/lifecycle/first-resume.png`, `tour/T-lock.png` | Lock screen, Unlock |
| P-SY-01 | Session starts from the display manager on hardware | Observed subset; qualification open | `marlin-vm/V-SESSION.png`, `marlin-vm/lifecycle/logout-greeter.png` | — |
| P-SY-02 | Multi-monitor with per-output panel and hotplug | No adequate capture reviewed | — | — |
| P-SY-03 | Fractional scaling and mixed DPI | No adequate capture reviewed | — | Display scale |
| P-SY-04 | XWayland apps run by default | No adequate capture reviewed | — | X11 apps |
| P-SY-06 | Clients find the protocols Mutter offers (dmabuf, activation, viewporter, fractional scale, cursor shape, idle inhibit, presentation time, fifo, shortcuts inhibit, xdg-foreign, dialogs) | No adequate capture reviewed | — | — |
| P-SY-07 | End session: Suspend, Restart…, Power Off…, Log Out with GNOME's 60 s confirmation dialog | No adequate capture reviewed | — | Power menu, End session |
| P-SY-05 | Screen capture and sharing through portals with consent | No adequate capture reviewed | — | Screenshot UI |
| P-A11Y-01 | Screen reader reads panel, overview, quick settings | No adequate capture reviewed | — | — |
| P-A11Y-02 | Every pointer action has a keyboard equivalent | No adequate capture reviewed | — | — |
| P-IN-01 | IME composition in GTK apps | No adequate capture reviewed | — | Input methods, Candidate window |
| P-IN-02 | Touchpad gestures (three-finger overview, workspace swipe) | No adequate capture reviewed | — | — |
| P-ST-01 | Settings written by GNOME Settings take effect (wallpaper, clock, fonts) | Observed subset; qualification open | `gtk-shell/01b-wallpaper-file-uri.png` | Volume OSD, Brightness OSD |
| P-TR-01 | AppIndicator tray items show icons and menus | Observed subset; qualification open | `gtk-shell/03b-tray-menu.png` | — |
| P-WM-08 | Multitasking preferences: fixed workspace count, workspaces on primary only, edge tiling and current-workspace-only app switching | No adequate capture reviewed | — | — |
| P-WM-09 | The full GNOME window-manager shortcut list and focus policy (focus mode, auto-raise, raise on click, mouse-button modifier) | No adequate capture reviewed | — | — |
| P-SY-08 | Session save and restore (opt-in window restoration across logout and reboot) | No adequate capture reviewed | — | — |
| P-SY-09 | Clients find the remaining Mutter protocols and versions, including native DRM lease, explicit sync and dmabuf feedback | No adequate capture reviewed | — | — |
| P-SY-10 | Screenshot UI keyboard selection: arrows resize, Alt moves, Shift/Ctrl coarse and fine steps, R resets | No adequate capture reviewed | — | — |
| P-DS-01 | Night Light: colour temperature, schedule, manual times and temporary disable | No adequate capture reviewed | — | — |
| P-DS-02 | Display modes, refresh rate, rotation, mirroring and output disabling from GNOME Settings | No adequate capture reviewed | — | — |
| P-DS-03 | Colour management: ICC profiles, calibration, HDR and YUV colour conversion | No adequate capture reviewed | — | — |
| P-DS-04 | Touchscreens, touch output mapping and sensor-driven orientation with orientation lock | No adequate capture reviewed | — | — |
| P-IN-03 | Compositor cursor themes, sizes, requested cursor shapes and locate-pointer | No adequate capture reviewed | — | — |
| P-IN-04 | Mouse, touchpad, pointing-stick and trackball settings from GNOME Settings | No adequate capture reviewed | — | — |
| P-IN-05 | Graphics tablets and GNOME Wacom settings (mapping, calibration, pressure, stylus and pad buttons) | No adequate capture reviewed | — | — |
| P-IN-06 | Keyboard input-source policy (per-window sources, MRU), remembered Num Lock and custom shortcuts | No adequate capture reviewed | — | — |
| P-NT-03 | Per-app notification preferences and lock-screen notification privacy | No adequate capture reviewed | — | — |
| P-A11Y-03 | Screen magnifier and on-screen keyboard | No adequate capture reviewed | — | — |
| P-A11Y-04 | Keyboard and pointer input aids: sticky/slow/bounce/toggle keys, Mouse Keys, dwell and simulated secondary click | No adequate capture reviewed | — | — |
| P-A11Y-05 | Reduced motion, keyboard focus visibility and the always-shown accessibility menu | No adequate capture reviewed | — | — |
| P-A11Y-06 | Screen reader activation from Settings and complete spoken shell navigation with Orca | No adequate capture reviewed | — | — |
| P-SN-01 | Sound preferences: over-amplification, feedback sounds, visual and audible bell | No adequate capture reviewed | — | — |
| P-ST-02 | Wellbeing: screen-time history and limits, grayscale and break reminders | No adequate capture reviewed | — | — |
| P-ST-03 | Power panel: GNOME 51 brightness interface, power saving, idle dim/blank and power policies | No adequate capture reviewed | — | — |
| P-ST-04 | Privacy device controls: camera, microphone and sound-output disable, privacy screen and USB protection | No adequate capture reviewed | — | — |
| P-ST-05 | Administrative lockdown (`org.gnome.desktop.lockdown`): disable command line, log out, user switching, lock screen and others | No adequate capture reviewed | — | — |
| P-ST-06 | Remote desktop: GNOME Settings Desktop Sharing, Remote Control and remote login over RDP | No adequate capture reviewed | — | — |
| P-ST-07 | Date & Time: calendar preferences, automatic time and time zone on the shipped image | No adequate capture reviewed | — | — |
| P-ST-08 | Background placement modes, shading and slideshow (dynamic XML) wallpapers | No adequate capture reviewed | — | — |
| P-ST-09 | Apps panel: permissions, background apps, global shortcuts, default handlers and removable-media autorun | No adequate capture reviewed | — | — |
| P-ST-10 | Service-backed Settings panels (network, Bluetooth, online accounts, printers, sharing, users, region, updates) on the shipped image | No adequate capture reviewed | — | — |
| P-ST-11 | Legacy and app-owned GNOME settings are classified without false parity claims | No adequate capture reviewed | — | — |
| P-LK-02 | Authentication beyond one password: fingerprint, smartcard/PIN, multi-step and web login on the lock screen | No adequate capture reviewed | — | — |
| P-AN-01 | Motion preferences: animations off is instant, reduced motion keeps fades only, slow-down factor scales durations | Transition clip + intermediate frames required | — | — |
| P-AN-02 | Window open and close animations (150 ms map/destroy, 100 ms attached dialogs) | Transition clip + intermediate frames required | — | — |
| P-AN-03 | Minimize and unminimize toward the dash icon (400 ms) | Transition clip + intermediate frames required | — | — |
| P-AN-04 | Maximize, unmaximize, tile and fullscreen size-change transition (250 ms crossfade) | Transition clip + intermediate frames required | — | — |
| P-AN-05 | Workspace switch slide, finger-following swipe, switcher popup fade and Activities dots | Transition clip + intermediate frames required | — | — |
| P-AN-06 | Parent window dims behind an attached modal dialog (500 ms in, 250 ms out) | Transition clip + intermediate frames required | — | — |
| P-AN-07 | Overview transitions: open/close easing, gesture momentum, picker and app grid, hover, thumbnails, drag, search, dash and panel | Transition clip + intermediate frames required | — | — |
| P-AN-08 | App grid animations: folder zoom, icon scale-in, launch zoom, page switch curve and page arrows | Transition clip + intermediate frames required | — | — |
| P-AN-09 | Menus, quick-settings submenus, OSD, notification banners and list, Alt+Tab, modal dialogs and lightbox | Transition clip + intermediate frames required | — | — |
| P-AN-10 | Lock curtain slide, wallpaper crossfade, screenshot flash, hot-corner ripples, startup and monitor-change transitions | Transition clip + intermediate frames required | — | — |

## All walkthrough states

| State | Required frame | Initial audit |
|---|---|---|
| panel | `01-panel.png` | Observed subset |
| calendar | `02-calendar.png` | Observed subset |
| tray | `03b-tray-menu.png` | Observed subset |
| quick-settings | `15-quick-settings-services.png` | Missing from this GTK artifact |
| wifi-menu | `15b-wifi-menu.png` | Missing from this GTK artifact |
| network-agent | `15d-network-agent.png` | Missing from this GTK artifact |
| wired-menu | `15e-wired-menu.png` | Missing from this GTK artifact |
| bluetooth-menu | `15c-bluetooth-menu.png` | Missing from this GTK artifact |
| microphone | `16b-microphone.png` | Missing from this GTK artifact |
| power-menu | `21-power-menu.png` | Missing from this GTK artifact |
| osd-volume | `24b-osd.png` | Missing from this GTK artifact |
| osd-brightness | `24c-brightness-osd.png` | Missing from this GTK artifact |
| windows | `04-windows.png` | Observed subset |
| popover | `07-app-popover.png` | Observed subset |
| tile-preview | `08-tile-preview.png` | Observed subset |
| window-menu | `24d-window-menu.png` | Missing from this GTK artifact |
| x11 | `20-x11-app.png` | Missing from this GTK artifact |
| scroll-strip | `24j-scroll-strip.png` | Missing from this GTK artifact |
| scroll-follow | `24k-scroll-follow.png` | Missing from this GTK artifact |
| scroll-presets | `24k-scroll-preset.png` | Missing from this GTK artifact |
| switcher | `08b-switcher.png` | Observed subset |
| switcher-thumbnails | `08c-switcher-thumbnails.png` | Observed subset |
| overview | `05-overview.png` | Observed subset |
| workspaces | `24g-workspace-thumbnails.png` | Missing from this GTK artifact |
| overview-drag | `24h-overview-drag.png` | Missing from this GTK artifact |
| workspace-popup | `24f-workspace-popup.png` | Missing from this GTK artifact |
| search | `10-search.png` | Observed subset |
| search-provider | `10b-search-provider.png` | Missing from this GTK artifact |
| app-grid | `11-app-grid.png` | Missing from this GTK artifact |
| app-folder | `11b-app-folder.png` | Missing from this GTK artifact |
| app-grid-pages | `11c-app-grid-pages.png` | Observed subset |
| folder-made | `11f-folder-dialog.png` | Missing from this GTK artifact |
| grid-reorder | `11g-grid-reordered.png` | Missing from this GTK artifact |
| banner | `12-banner.png` | Missing from this GTK artifact |
| notification-list | `13-notification-list.png` | Missing from this GTK artifact |
| do-not-disturb | `14-dnd-list.png` | Missing from this GTK artifact |
| notification-group | `14b-notification-group.png` | Missing from this GTK artifact |
| notification-group-expanded | `14c-notification-group-expanded.png` | Missing from this GTK artifact |
| ime | `24i-ime-preedit.png` | Missing from this GTK artifact |
| ime-candidates | `24j-ime-candidates.png` | Missing from this GTK artifact |
| screenshot-ui | `24e-screenshot-ui.png` | Missing from this GTK artifact |
| polkit | `24g-polkit.png` | Missing from this GTK artifact |
| end-session | `22-end-session.png` | Missing from this GTK artifact |
| lock | `23-locked-after-suspend.png` | Missing from this GTK artifact |
| unlock | `24-unlock-prompt.png` | Missing from this GTK artifact |
| scale | `24-scale-125.png` | Missing from this GTK artifact |
