# GNOME 51 and Tuna Desktop accessibility comparison

Captured from GNOME Shell 51.0 through AT-SPI on 2026-10-04. These fixtures preserve every visible node, including labels and unnamed controls. The clock text is normalized. GNOME’s `button` role name is shown as `push button` below to match the older AT-SPI role names used by the Tuna Desktop runner.

Tuna Desktop columns come from its live proof goldens. Native scenes use power profiles and logind; the Tuna Desktop proof also supplies network, Bluetooth, audio and backlight services. The native overview has the reference image’s favorites alongside three test windows, while Tuna Desktop’s proof pins no favorites. The two columns are independent ordered catalogs; controls on the same row are not asserted to be equivalent.

## panel

GNOME: 41 visible nodes; 2 named controls. Tuna Desktop: 6 named nodes in the proof golden.

| GNOME named control | Tuna Desktop named node |
| --- | --- |
| toggle button: Activities | frame: Top Bar |
| menu: System | toggle button: Activities |
| — | push button: Date and Time |
| — | menu: Date and Time |
| — | push button: System |
| — | menu: System |

## overview

GNOME: 136 visible nodes; 9 named controls. Tuna Desktop: 15 named nodes in the proof golden.

| GNOME named control | Tuna Desktop named node |
| --- | --- |
| push button: Files | frame: Top Bar |
| push button: Text Editor | toggle button: Activities |
| push button: Calculator | push button: Date and Time |
| push button: tuna-test-beta | menu: Date and Time |
| push button: tuna-test-gamma | push button: System |
| push button: tuna-test-alpha | menu: System |
| toggle button: Show Apps | frame: Search |
| toggle button: Activities | entry: Search |
| menu: System | frame: Dash |
| — | push button: tuna-test-alpha |
| — | push button: tuna-test-beta |
| — | push button: tuna-test-gamma |
| — | toggle button: Show Apps |
| — | frame: Window Previews |
| — | push button: Close |

## quick-settings

GNOME: 93 visible nodes; 7 named controls. Tuna Desktop: 29 named nodes in the proof golden.

| GNOME named control | Tuna Desktop named node |
| --- | --- |
| toggle button: Activities | frame: Top Bar |
| menu: System | toggle button: Activities |
| push button: Take Screenshot | push button: Date and Time |
| push button: Settings | menu: Date and Time |
| push button: Lock Screen | push button: System |
| push button: Power Off Menu | menu: System |
| push button: Open power profiles menu | push button: Take Screenshot |
| — | push button: Settings |
| — | push button: Lock Screen |
| — | push button: Power Off Menu |
| — | push button: Mute |
| — | slider: Volume |
| — | toggle button: Open sound output menu |
| — | slider: Brightness |
| — | toggle button: Wi-Fi |
| — | toggle button: Wi-Fi Menu |
| — | toggle button: Wired |
| — | toggle button: Wired Menu |
| — | toggle button: Bluetooth |
| — | toggle button: Bluetooth Menu |
| — | toggle button: Printers |
| — | toggle button: Printers Menu |
| — | toggle button: Mobile Network |
| — | toggle button: Mobile Network Menu |
| — | toggle button: Power Mode |
| — | toggle button: Power Mode Menu |
| — | toggle button: Night Light |
| — | toggle button: Dark Style |
| — | toggle button: Do Not Disturb |

## Intended differences

- GNOME has one compositor stage with many `panel` containers. Tuna Desktop exposes the GTK layer windows as named frames: Top Bar, Search, Dash and Window Previews.
- Activities is a toggle button on both sides, pressed while the overview is open (panel.js).
- GNOME’s date control is an unnamed menu with a clock label child; Tuna Desktop names it Date and Time. Each Tuna Desktop panel menu is a GTK menu button: an outer push button around the focusable control, which takes GNOME’s menu role (panelMenu.js).
- GNOME’s search field is an unnamed text node. Tuna Desktop exposes a named Search entry. Both scenes retain their search field in the visible tree.
- Dash app names depend on the installed favorites and window fixtures. Show Apps is a toggle button on both sides. GNOME’s preview buttons are unnamed in the native tree; Tuna Desktop’s hovered-preview golden names the Close action.
- Open power profiles menu in GNOME maps to Power Mode Menu in Tuna Desktop. The shared screenshot, settings, lock screen and power menu actions keep GNOME’s names.
- GNOME’s Power Mode, Dark Style and Do Not Disturb toggles use visible label children instead of a name on the toggle. Tuna Desktop names the toggles directly. The native fixture retains those label children.
- Tuna Desktop’s network, Bluetooth, volume and brightness controls are present because its live proof supplies those services. The GNOME headless reference has no usable network adapter, audio server or hardware backlight.
- Tuna Desktop adds a Printers tile and menu for the CUPS queue (#362); GNOME 51 has no quick-settings printers control.

This comparison describes visible accessible structure and labels. The live Tuna Desktop proof checks actions, focus and slider operation; what Orca speaks on these surfaces is checked by scripts/tuna-orca-speech-proof.
