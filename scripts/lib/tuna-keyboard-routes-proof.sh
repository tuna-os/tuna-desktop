#!/bin/sh
# Sourced by tuna-gtk-shell-proof, also used by its focused
# --keyboard-routes-only run (P-A11Y-02, #591). Requires that harness's
# private bus, GTK shell, AT-SPI and assertion helpers (pass, fail,
# showing, hidden, focused, cstate, $PY, $ROOT, $ARTIFACTS).
#
# Every step sends real X11 keys through the nested window and reads the
# result back from the shell's AT-SPI tree or the notification bus; no
# step presses through AT-SPI.

kr_dump() {
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$1" 20
}

# Press $2 until a showing, focused node matches the jq filter $1 (at
# most $3 presses); the last dump is left in $4.
kr_focus_until() {
    kr_i=0
    while :; do
        kr_dump "$4" || return 1
        jq -e "any(.[]; .showing and .focused and ($1))" "$4" >/dev/null && return 0
        [ "$kr_i" -lt "$3" ] || return 1
        xdotool key "$2"
        sleep 0.6
        kr_i=$((kr_i + 1))
    done
}

# Dump until the jq filter $1 holds over the tree (at most $2 seconds).
kr_until() {
    kr_end=$(($(date +%s) + $2))
    while :; do
        kr_dump "$3" || return 1
        jq -e "$1" "$3" >/dev/null && return 0
        [ "$(date +%s)" -lt "$kr_end" ] || return 1
        sleep 0.4
    done
}

# Shift+F10 by raw keycode: `xdotool key F10` taps Alt around F-keys on
# this server's keymap (see fkey_tap in the harness).
kr_shift_f10() {
    xdotool keydown Shift_L
    sleep 0.3
    "$PY" "$ROOT/scripts/lib/xkeycode-tap.py" "$DISPLAY" ffc7 >&2
    kr_status=$?
    xdotool keyup Shift_L
    return "$kr_status"
}

# Post a notification; prints its id.
kr_notify() {
    gdbus call --session --dest org.freedesktop.Notifications \
        --object-path /org/freedesktop/Notifications \
        --method org.freedesktop.Notifications.Notify \
        -- "Tuna Desktop Proof" 0 "" "$1" "$2" "[]" "{}" -1 |
        sed -n 's/^(uint32 \([0-9]*\),)$/\1/p'
}

# G-PANEL-MENU-KEYS: popupMenu.js `_onKeyPress` (Down on a focused
# panel button opens its menu and moves into it) and panelMenu.js
# `_onMenuKeyPress` with PopupMenuManager's `_changeMenu` (Left/Right
# that move nowhere inside an open menu go to the neighbouring panel
# menu and open it).
keyboard_panel_menu_proof() {
    xdotool key ctrl+alt+Tab
    sleep 1
    kr_focus_until '.name == "Date and Time"' Tab 8 "$ARTIFACTS/a11y-kr-clock.json" ||
        fail G-PANEL-MENU-KEYS "Ctrl+Alt+Tab and Tab did not reach the clock"
    xdotool key Down
    kr_until 'any(.[]; .name == "Previous month" and .showing)' 5 "$ARTIFACTS/a11y-kr-down.json" ||
        fail G-PANEL-MENU-KEYS "Down on the clock did not open the date menu"
    jq -e 'any(.[]; .showing and .focused and .name != "Date and Time")' "$ARTIFACTS/a11y-kr-down.json" >/dev/null ||
        fail G-PANEL-MENU-KEYS "Down opened the date menu without moving focus into it"
    # Right walks the menu's own controls first; at its right edge it
    # leaves for the next panel menu, quick settings.
    kr_n=0
    until showing "$ARTIFACTS/a11y-kr-right.json" "Dark Style" 2>/dev/null; do
        [ "$kr_n" -lt 20 ] || fail G-PANEL-MENU-KEYS "Right never left the date menu for quick settings"
        xdotool key Right
        sleep 0.6
        kr_dump "$ARTIFACTS/a11y-kr-right.json" || fail G-PANEL-MENU-KEYS "accessibility dump failed"
        kr_n=$((kr_n + 1))
    done
    hidden "$ARTIFACTS/a11y-kr-right.json" "Previous month" ||
        fail G-PANEL-MENU-KEYS "the date menu stayed open after switching"
    # Left at quick settings' left edge comes back to the date menu.
    kr_n=0
    until showing "$ARTIFACTS/a11y-kr-left.json" "Previous month" 2>/dev/null; do
        [ "$kr_n" -lt 20 ] || fail G-PANEL-MENU-KEYS "Left never left quick settings for the date menu"
        xdotool key Left
        sleep 0.6
        kr_dump "$ARTIFACTS/a11y-kr-left.json" || fail G-PANEL-MENU-KEYS "accessibility dump failed"
        kr_n=$((kr_n + 1))
    done
    hidden "$ARTIFACTS/a11y-kr-left.json" "Dark Style" ||
        fail G-PANEL-MENU-KEYS "quick settings stayed open after switching back"
    xdotool key Escape
    kr_until 'any(.[]; .name == "Previous month" and .showing) | not' 5 "$ARTIFACTS/a11y-kr-unwind.json" ||
        fail G-PANEL-MENU-KEYS "Escape did not close the switched-to menu"
    focused "$ARTIFACTS/a11y-kr-unwind.json" "Date and Time" ||
        fail G-PANEL-MENU-KEYS "Escape did not leave focus on the menu's button"
    xdotool key Escape
    sleep 1
    pass G-PANEL-MENU-KEYS "Down opens a focused panel menu with focus inside; Right/Left at a menu's edge switch to the neighbouring panel menu; Escape unwinds"
}

# G-QS-MENU-KEY: St's popup-menu keys (st-widget.c: Menu, Shift+F10) on
# quick settings: ShutdownItem opens its menu (status/system.js) and a
# QuickMenuToggle's arrow opens its menu (quickSettings.js).
keyboard_quick_settings_proof() {
    xdotool key super+s
    kr_until 'any(.[]; .name == "Dark Style" and .showing)' 5 "$ARTIFACTS/a11y-kr-qs.json" ||
        fail G-QS-MENU-KEY "Super+S did not open quick settings"
    hidden "$ARTIFACTS/a11y-kr-qs.json" "Log Out…" ||
        fail G-QS-MENU-KEY "the power menu was already open"
    kr_focus_until '.name == "Power Off Menu"' Tab 40 "$ARTIFACTS/a11y-kr-qs-power.json" ||
        fail G-QS-MENU-KEY "Tab never reached Power Off Menu"
    kr_shift_f10 || fail G-QS-MENU-KEY "Shift+F10 tap failed"
    kr_until 'any(.[]; (.name == "Suspend" or .name == "Restart…" or .name == "Power Off…" or .name == "Log Out…") and .showing)' 5 \
        "$ARTIFACTS/a11y-kr-qs-power-open.json" ||
        fail G-QS-MENU-KEY "Shift+F10 on Power Off Menu did not open the power menu"
    # A quick toggle with a menu: its arrow button (named "<title> Menu").
    kr_focus_until '(.name | endswith(" Menu")) and .name != "Power Off Menu" and (.checked | not)' Tab 40 \
        "$ARTIFACTS/a11y-kr-qs-toggle.json" ||
        fail G-QS-MENU-KEY "Tab never reached a quick toggle's menu arrow"
    kr_toggle="$(jq -r '[.[] | select(.showing and .focused)][0].name' "$ARTIFACTS/a11y-kr-qs-toggle.json")"
    xdotool key Menu
    kr_until "any(.[]; .name == \"$kr_toggle\" and .showing and .checked)" 5 "$ARTIFACTS/a11y-kr-qs-toggle-open.json" ||
        fail G-QS-MENU-KEY "the Menu key on $kr_toggle did not open its menu"
    xdotool key Escape
    kr_until 'any(.[]; .name == "Dark Style" and .showing) | not' 5 "$ARTIFACTS/a11y-kr-qs-closed.json" ||
        fail G-QS-MENU-KEY "Escape did not close quick settings"
    pass G-QS-MENU-KEY "Shift+F10 opens the power menu and the Menu key opens $kr_toggle, as St's popup-menu keys do"
}

# G-NOTIFICATION-KEYS: messageList.js binds Delete/BackSpace to close
# the focused message; messageTray.js binds Escape to expire the focused
# banner; calendar.js's message list collapses an expanded group on
# Escape before the menu closes. The reasons are read back from the
# bus's own NotificationClosed signals.
keyboard_notification_proof() {
    gdbus monitor --session --dest org.freedesktop.Notifications \
        --object-path /org/freedesktop/Notifications >"$ARTIFACTS/kr-notification-signals.log" 2>&1 &
    kr_monitor=$!
    sleep 1
    kr_delete_id="$(kr_notify "Delete Proof Banner" "Closed by keyboard")"
    [ -n "$kr_delete_id" ] || fail G-NOTIFICATION-KEYS "Notify failed"
    kr_until 'any(.[]; .name == "Delete Proof Banner" and .showing)' 5 "$ARTIFACTS/a11y-kr-banner.json" ||
        fail G-NOTIFICATION-KEYS "no banner to focus"
    xdotool key super+n
    # Layer activation and AT-SPI focus arrive asynchronously; wait for
    # the exact card, without sending the focus chord a second time.
    kr_until 'any(.[]; .name == "Delete Proof Banner" and .showing and .focused)' 5 \
        "$ARTIFACTS/a11y-kr-banner-focus.json" ||
        fail G-NOTIFICATION-KEYS "Super+N did not focus the banner"
    xdotool key Delete
    kr_until 'any(.[]; .name == "Delete Proof Banner" and .showing) | not' 5 "$ARTIFACTS/a11y-kr-banner-deleted.json" ||
        fail G-NOTIFICATION-KEYS "Delete did not close the focused banner"
    kr_escape_id="$(kr_notify "Escape Proof Banner" "Expired by keyboard")"
    [ -n "$kr_escape_id" ] || fail G-NOTIFICATION-KEYS "Notify failed"
    kr_until 'any(.[]; .name == "Escape Proof Banner" and .showing)' 5 "$ARTIFACTS/a11y-kr-banner2.json" ||
        fail G-NOTIFICATION-KEYS "no second banner to focus"
    xdotool key super+n
    kr_until 'any(.[]; .name == "Escape Proof Banner" and .showing and .focused)' 5 \
        "$ARTIFACTS/a11y-kr-banner2-focus.json" ||
        fail G-NOTIFICATION-KEYS "Super+N did not focus the second banner"
    xdotool key Escape
    kr_until 'any(.[]; .name == "Escape Proof Banner" and .showing) | not' 5 "$ARTIFACTS/a11y-kr-banner-expired.json" ||
        fail G-NOTIFICATION-KEYS "Escape did not expire the focused banner"
    sleep 1
    grep -q "NotificationClosed (uint32 $kr_delete_id, uint32 2)" "$ARTIFACTS/kr-notification-signals.log" ||
        fail G-NOTIFICATION-KEYS "Delete did not close the banner as dismissed (reason 2)"
    grep -q "NotificationClosed (uint32 $kr_escape_id, uint32 1)" "$ARTIFACTS/kr-notification-signals.log" ||
        fail G-NOTIFICATION-KEYS "Escape did not expire the banner (reason 1)"
    # A second message from the same source makes a collapsed group in
    # the date menu's list.
    kr_notify "Group Proof Message" "Second in the group" >/dev/null ||
        fail G-NOTIFICATION-KEYS "Notify failed"
    sleep 1
    xdotool key super+v
    kr_until 'any(.[]; .name == "Previous month" and .showing)' 5 "$ARTIFACTS/a11y-kr-list.json" ||
        fail G-NOTIFICATION-KEYS "Super+V did not open the date menu"
    kr_group='.name == "Tuna Desktop Proof" and (.description | endswith("notifications, collapsed"))'
    kr_focus_until "$kr_group" Tab 80 "$ARTIFACTS/a11y-kr-group.json" ||
        fail G-NOTIFICATION-KEYS "Tab never reached the collapsed notification group"
    xdotool key Return
    kr_until 'any(.[]; .name == "Collapse" and .showing)' 5 "$ARTIFACTS/a11y-kr-expanded.json" ||
        fail G-NOTIFICATION-KEYS "Enter did not expand the group"
    focused "$ARTIFACTS/a11y-kr-expanded.json" "Collapse" ||
        fail G-NOTIFICATION-KEYS "Enter left the keyboard focus outside the expanded group"
    xdotool key Escape
    kr_until 'any(.[]; .name == "Collapse" and .showing) | not' 5 "$ARTIFACTS/a11y-kr-collapsed.json" ||
        fail G-NOTIFICATION-KEYS "Escape did not collapse the expanded group"
    showing "$ARTIFACTS/a11y-kr-collapsed.json" "Previous month" ||
        fail G-NOTIFICATION-KEYS "Escape closed the date menu instead of collapsing the group"
    jq -e "any(.[]; ($kr_group) and .showing and .focused)" "$ARTIFACTS/a11y-kr-collapsed.json" >/dev/null ||
        fail G-NOTIFICATION-KEYS "Escape did not return the keyboard focus to the collapsed group"
    xdotool key Delete
    kr_until "any(.[]; ($kr_group) and .showing) | not" 5 "$ARTIFACTS/a11y-kr-group-deleted.json" ||
        fail G-NOTIFICATION-KEYS "Delete did not close the focused group"
    hidden "$ARTIFACTS/a11y-kr-group-deleted.json" "Group Proof Message" ||
        fail G-NOTIFICATION-KEYS "the group's messages stayed listed"
    xdotool key Escape
    kr_until 'any(.[]; .name == "Previous month" and .showing) | not' 5 "$ARTIFACTS/a11y-kr-list-closed.json" ||
        fail G-NOTIFICATION-KEYS "Escape did not close the date menu"
    kill "$kr_monitor" 2>/dev/null || true
    wait "$kr_monitor" 2>/dev/null || true
    pass G-NOTIFICATION-KEYS "Delete closes a focused banner (dismissed) and a focused list group; Escape expires a focused banner; Enter expands a group with focus inside, Escape collapses it back onto the group before the date menu closes"
}

# G-WINDOW-MENU-KEYS: the window menu is a PopupMenu, whose rows St's
# focus manager walks with Up and Down (st-focus-manager.c). $1 is the
# dump of the menu Alt+Space just opened.
keyboard_window_menu_proof() {
    kr_first="$(jq -r '[.[] | select(.showing and .focused)][0].name // empty' "$1")"
    [ -n "$kr_first" ] || fail G-WINDOW-MENU-KEYS "Alt+Space opened the window menu with no focused row"
    xdotool key Down
    sleep 0.6
    kr_dump "$ARTIFACTS/a11y-kr-wm-down.json" || fail G-WINDOW-MENU-KEYS "accessibility dump failed"
    kr_second="$(jq -r '[.[] | select(.showing and .focused)][0].name // empty' "$ARTIFACTS/a11y-kr-wm-down.json")"
    if [ -z "$kr_second" ] || [ "$kr_second" = "$kr_first" ]; then
        fail G-WINDOW-MENU-KEYS "Down did not move from '$kr_first' to the next row"
    fi
    xdotool key Up
    sleep 0.6
    kr_dump "$ARTIFACTS/a11y-kr-wm-up.json" || fail G-WINDOW-MENU-KEYS "accessibility dump failed"
    focused "$ARTIFACTS/a11y-kr-wm-up.json" "$kr_first" ||
        fail G-WINDOW-MENU-KEYS "Up did not return to '$kr_first'"
    pass G-WINDOW-MENU-KEYS "Down and Up walk the window menu's rows ('$kr_first', '$kr_second')"
}

# G-SCREENSHOT-CTRL-C: screenshot.js binds Ctrl+C to `activate`, as
# Enter and Space: the screenshot UI captures and closes.
keyboard_screenshot_proof() {
    kr_marker="$ARTIFACTS/kr-ctrl-c.marker"
    sleep 1.1 # GNOME filenames have one-second resolution.
    touch "$kr_marker"
    xdotool key Print
    cstate 'any(.layers[]; .namespace == "tuna-screenshot-ui")' "screenshot overlay mapped" ||
        fail G-SCREENSHOT-CTRL-C "Print did not map the screenshot UI"
    kr_until 'any(.[]; .name == "Capture" and .showing)' 10 "$ARTIFACTS/a11y-kr-shot.json" ||
        fail G-SCREENSHOT-CTRL-C "Print did not open the screenshot UI"
    xdotool key --clearmodifiers c
    sleep 0.5
    xdotool key ctrl+c
    kr_end=$(($(date +%s) + 15))
    until [ -n "$(find "$XDG_PICTURES_DIR/Screenshots" -name 'Screenshot From *.png' -newer "$kr_marker" 2>/dev/null)" ] ||
        [ "$(date +%s)" -gt "$kr_end" ]; do sleep 0.3; done
    kr_shot="$(find "$XDG_PICTURES_DIR/Screenshots" -name 'Screenshot From *.png' -newer "$kr_marker" 2>/dev/null | head -1)"
    [ -n "$kr_shot" ] || fail G-SCREENSHOT-CTRL-C "Ctrl+C saved nothing"
    kr_dims="$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=p=0 "$kr_shot")"
    [ "$kr_dims" = "$WIDTH,$HEIGHT" ] || fail G-SCREENSHOT-CTRL-C "the Ctrl+C capture is $kr_dims, not $WIDTH,$HEIGHT"
    cstate 'any(.layers[]; .namespace == "tuna-screenshot-ui") | not' "screenshot overlay gone" ||
        fail G-SCREENSHOT-CTRL-C "the screenshot UI stayed open after Ctrl+C"
    pass G-SCREENSHOT-CTRL-C "C picks Screen and Ctrl+C captures the whole screen, as screenshot.js binds them"
}
