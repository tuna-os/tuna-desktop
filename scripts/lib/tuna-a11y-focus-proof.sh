#!/bin/sh
# Sourced by tuna-gtk-shell-proof, also used by its focused accessibility run
# (--a11y-focus-only). Requires that harness's private bus, nested window,
# GTK shell, AT-SPI and assertion helpers. P-A11Y-05 (#351).

A11Y_FOCUS_KEY=keyboard-focus-visible-timeout
A11Y_INTERFACE=org.gnome.desktop.a11y.interface
# Log byte range of the one deliberate shell restart G-SHELL-STABLE admits.
A11Y_RESTART_FROM=""
A11Y_RESTART_TO=""

# "x,y,w,h" of the first showing node named $2 with role $3 in dump $1:
# a panel control's focusable pill. The panel is the top layer, so window
# coordinates are screen coordinates.
a11y_focus_rect() {
    jq -r --arg n "$2" --arg r "$3" '[.[] | select(.name == $n and .role == $r and .showing and .bounds != null)][0].bounds
        | select(. != null) | map(tostring) | join(",")' "$1"
}

# One real key, then the panel's focus indicator timeline for $2 seconds.
# $1 label, $3 key, then NAME=RECT pairs.
a11y_focus_timeline() {
    _label="$1" _seconds="$2" _key="$3"
    shift 3
    "$PY" "$ROOT/scripts/lib/tuna-focus-ring-timeline.py" "$DISPLAY" \
        "$ARTIFACTS/focus-ring-$_label.json" "$_seconds" "$_key" "$@" >&2 ||
        fail G-FOCUS-RING "frame sampler failed for $_label"
}

# jq over a timeline's per-control summary.
a11y_focus_check() {
    if jq -e --arg c "$2" ".controls[\$c] | $3" "$ARTIFACTS/focus-ring-$1.json" >/dev/null; then
        return 0
    fi
    jq --arg c "$2" '{control: $c, observation: .controls[$c]}' "$ARTIFACTS/focus-ring-$1.json" >&2
    return 1
}

a11y_focus_dump() {
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-focus-$1.json" 20 ${2:+--pid "$2"} ||
        fail "${3:-G-FOCUS-RING}" "accessibility dump failed ($1)"
}

# Real keys drive panel focus (switch-panels, Tab, Shift+Tab); frames read
# from the X server show GNOME 51's focus fill on exactly the focused
# control, and keyboard-focus-visible-timeout decides when it clears
# while the control keeps keyboard focus.
a11y_focus_ring_proof() {
    xdotool mousemove --window "$WID" $((WIDTH / 2)) $((HEIGHT / 2))
    cstate '(.overview_open | not) and (.locked | not)' "desktop for panel focus" ||
        fail G-FOCUS-RING "the overview or lock screen was still up"
    gsettings reset "$A11Y_INTERFACE" "$A11Y_FOCUS_KEY"
    # Nothing from an earlier stage may still be lit.
    sleep 4
    a11y_focus_dump start
    act="$(a11y_focus_rect "$ARTIFACTS/a11y-focus-start.json" Activities "toggle button")"
    clock="$(a11y_focus_rect "$ARTIFACTS/a11y-focus-start.json" "Date and Time" menu)"
    [ -n "$act" ] && [ -n "$clock" ] || fail G-FOCUS-RING "no Activities or clock bounds"

    # Default (-1): the toolkit's three seconds (GTK 4.14 VISIBLE_FOCUS_DURATION).
    a11y_focus_timeline default 5 ctrl+alt+Tab "act=$act" "clock=$clock"
    a11y_focus_dump default
    focused "$ARTIFACTS/a11y-focus-default.json" Activities ||
        fail G-FOCUS-RING "Ctrl+Alt+Tab did not focus Activities"
    a11y_focus_check default act '(.lit_before | not) and .lit_from != null and .lit_from <= 0.8' ||
        fail G-FOCUS-RING "Activities showed no focus fill after Ctrl+Alt+Tab"
    a11y_focus_check default clock '.lit_from == null' ||
        fail G-FOCUS-RING "the unfocused clock was lit"
    a11y_focus_check default act '.lit_until != null and .lit_until >= 2.8 and .lit_until <= 3.9' ||
        fail G-FOCUS-RING-TIMEOUT "the default focus indication did not clear after about 3 s"

    # Two seconds: shorter than the toolkit timer, on the next control.
    gsettings set "$A11Y_INTERFACE" "$A11Y_FOCUS_KEY" 2
    sleep 0.5
    a11y_focus_timeline two 4 Tab "act=$act" "clock=$clock"
    a11y_focus_dump two
    focused "$ARTIFACTS/a11y-focus-two.json" "Date and Time" ||
        fail G-FOCUS-RING "Tab did not move focus to the clock"
    focused "$ARTIFACTS/a11y-focus-two.json" Activities &&
        fail G-FOCUS-RING "Activities kept focus after Tab"
    a11y_focus_check two clock '(.lit_before | not) and .lit_from != null and .lit_from <= 0.8' ||
        fail G-FOCUS-RING "Tab did not light the clock"
    a11y_focus_check two act '.lit_from == null' ||
        fail G-FOCUS-RING "Activities stayed lit after focus left it"
    a11y_focus_check two clock '.lit_until != null and .lit_until >= 1.8 and .lit_until <= 2.9' ||
        fail G-FOCUS-RING-TIMEOUT "a 2 s timeout did not clear the indication after 2 s"
    pass G-FOCUS-RING "real Ctrl+Alt+Tab and Tab light GNOME's 17% focus fill on exactly the focused panel control"

    # Five seconds: outlives the toolkit's own three-second timer.
    gsettings set "$A11Y_INTERFACE" "$A11Y_FOCUS_KEY" 5
    sleep 0.5
    a11y_focus_timeline five 7 shift+Tab "act=$act" "clock=$clock"
    a11y_focus_dump five
    focused "$ARTIFACTS/a11y-focus-five.json" Activities ||
        fail G-FOCUS-RING "Shift+Tab did not return focus to Activities"
    a11y_focus_check five act '.lit_from != null and .lit_from <= 0.8' ||
        fail G-FOCUS-RING "Shift+Tab did not light Activities within 0.8 s (5 s setting)"
    a11y_focus_check five act '.lit_until != null and .lit_until >= 4.8 and .lit_until <= 5.9' ||
        fail G-FOCUS-RING-TIMEOUT "a 5 s timeout did not keep the indication past the toolkit's 3 s"

    # Zero: forever.
    gsettings set "$A11Y_INTERFACE" "$A11Y_FOCUS_KEY" 0
    sleep 0.5
    a11y_focus_timeline forever 8 Tab "act=$act" "clock=$clock"
    a11y_focus_check forever clock '.lit_from != null and .lit_from <= 0.8' ||
        fail G-FOCUS-RING "Tab did not light the clock within 0.8 s (zero timeout)"
    a11y_focus_check forever clock '.lit_until == null' ||
        fail G-FOCUS-RING-TIMEOUT "a zero timeout let the indication clear within 8 s"
    a11y_focus_dump forever
    focused "$ARTIFACTS/a11y-focus-forever.json" "Date and Time" ||
        fail G-FOCUS-RING "Tab did not move focus to the clock"
    xdotool key Escape
    sleep 1
    gsettings reset "$A11Y_INTERFACE" "$A11Y_FOCUS_KEY"
    pass G-FOCUS-RING-TIMEOUT "keyboard-focus-visible-timeout clears a real key's indication at 3 s by default, 2 s and 5 s when set, never at 0, with focus kept"
}

# Real keys: switch-panels, Tab to $1, Enter opens its menu.
a11y_menu_open() {
    xdotool key ctrl+alt+Tab
    sleep 1
    for _try in $(seq 1 10); do
        a11y_focus_dump "menu-walk-$2" "$3" G-A11Y-MENU-RESTART
        focused "$ARTIFACTS/a11y-focus-menu-walk-$2.json" Accessibility && break
        xdotool key Tab
        sleep 0.5
    done
    focused "$ARTIFACTS/a11y-focus-menu-walk-$2.json" Accessibility ||
        fail G-A11Y-MENU-RESTART "Tab never reached the accessibility menu ($2)"
    xdotool key Return
    sleep 1
    for _try in $(seq 1 14); do
        a11y_focus_dump "menu-row-$2" "$3" G-A11Y-MENU-RESTART
        focused "$ARTIFACTS/a11y-focus-menu-row-$2.json" "$1" && return 0
        xdotool key Tab
        sleep 0.5
    done
    fail G-A11Y-MENU-RESTART "Tab never reached $1 in the accessibility menu ($2)"
}

a11y_bell_is() {
    end=$(($(date +%s) + 10))
    until [ "$(gsettings get org.gnome.desktop.wm.preferences visual-bell)" = "$1" ]; do
        [ "$(date +%s)" -le "$end" ] || return 1
        sleep 0.2
    done
}

# $1 wanted ("showing"/"hidden"), $2 label, $3 shell pid.
a11y_indicator_wait() {
    end=$(($(date +%s) + 15))
    while :; do
        a11y_focus_dump "indicator-$2" "$3" G-A11Y-MENU-RESTART
        "$1" "$ARTIFACTS/a11y-focus-indicator-$2.json" Accessibility && return 0
        [ "$(date +%s)" -le "$end" ] || return 1
        sleep 0.5
    done
}

a11y_shell_pid() {
    _pid="$(pgrep -P "$COMPOSITOR_PID" -x tuna-shell-gtk || true)"
    case "$_pid" in ''|*[!0-9]*) return 1 ;; esac
    echo "$_pid"
}

# The accessibility menu across one deliberate shell restart: a row
# toggled with real keys keeps the indicator up and persists into the
# restarted shell's menu, which still follows the settings live.
a11y_menu_restart_proof() {
    xdotool mousemove --window "$WID" $((WIDTH / 2)) $((HEIGHT / 2))
    # The ten rows' features off: the indicator then shows only on request.
    gsettings reset "$A11Y_INTERFACE" high-contrast
    for _key in screen-magnifier-enabled screen-reader-enabled screen-keyboard-enabled; do
        gsettings reset org.gnome.desktop.a11y.applications "$_key"
    done
    for _key in stickykeys-enable slowkeys-enable bouncekeys-enable mousekeys-enable; do
        gsettings reset org.gnome.desktop.a11y.keyboard "$_key"
    done
    gsettings reset org.gnome.desktop.interface text-scaling-factor
    gsettings reset org.gnome.desktop.a11y always-show-universal-access-status
    gsettings reset org.gnome.desktop.wm.preferences visual-bell
    old_shell="$(a11y_shell_pid)" || fail G-A11Y-MENU-RESTART "expected exactly one owned shell"
    a11y_indicator_wait hidden off "$old_shell" ||
        fail G-A11Y-MENU-RESTART "the indicator showed with nothing enabled and always-show off"
    gsettings set org.gnome.desktop.a11y always-show-universal-access-status true
    a11y_indicator_wait showing always "$old_shell" ||
        fail G-A11Y-MENU-RESTART "always-show-universal-access-status did not show the indicator"
    a11y_menu_open "Visual Alerts" before "$old_shell"
    xdotool key space
    a11y_bell_is true || fail G-A11Y-MENU-RESTART "Space on Visual Alerts did not set visual-bell"
    xdotool key Escape
    sleep 0.5
    xdotool key Escape
    sleep 0.5
    gsettings set org.gnome.desktop.a11y always-show-universal-access-status false
    sleep 1
    a11y_indicator_wait showing active "$old_shell" ||
        fail G-A11Y-MENU-RESTART "an enabled feature did not keep the indicator up"

    A11Y_RESTART_FROM="$(wc -c <"$LOG")"
    kill -TERM "$old_shell"
    end=$(($(date +%s) + 20))
    while :; do
        new_shell="$(a11y_shell_pid || true)"
        [ -n "$new_shell" ] && [ "$new_shell" != "$old_shell" ] && break
        [ "$(date +%s)" -le "$end" ] || fail G-A11Y-MENU-RESTART "the supervisor did not restart the shell"
        sleep 0.2
    done
    ! kill -0 "$old_shell" 2>/dev/null || fail G-A11Y-MENU-RESTART "the old shell survived"
    a11y_indicator_wait showing restarted "$new_shell" ||
        fail G-A11Y-MENU-RESTART "the restarted shell did not show the indicator for the enabled feature"
    A11Y_RESTART_TO="$(wc -c <"$LOG")"
    [ "$(gsettings get org.gnome.desktop.wm.preferences visual-bell)" = true ] ||
        fail G-A11Y-MENU-RESTART "visual-bell did not persist across the restart"
    a11y_menu_open "Visual Alerts" after "$new_shell"
    jq -e 'any(.[]; .name == "Visual Alerts" and .showing and .focused and .checked)' \
        "$ARTIFACTS/a11y-focus-menu-row-after.json" >/dev/null ||
        fail G-A11Y-MENU-RESTART "the restarted menu did not show Visual Alerts checked"
    xdotool key space
    a11y_bell_is false || fail G-A11Y-MENU-RESTART "Space after the restart did not clear visual-bell"
    xdotool key Escape
    sleep 0.5
    xdotool key Escape
    a11y_indicator_wait hidden cleared "$new_shell" ||
        fail G-A11Y-MENU-RESTART "the restarted indicator did not hide once nothing was enabled"
    pass G-A11Y-MENU-RESTART "a menu row toggled with real keys persists across one shell restart; the restarted menu shows it checked and still follows the settings live"
}

# The shell must survive the journey: a crash the supervisor quietly
# restarts is still a failure (the overview-close teardown bug hid
# behind exactly that). The one exception is the restart
# a11y_menu_restart_proof makes on purpose, inside its own log range.
shell_stable_gate() {
    shell_exits="$(grep -c ShellExited "$LOG" || true)"
    shell_allowed=0
    if [ -n "$A11Y_RESTART_TO" ]; then
        shell_allowed="$(tail -c +"$((A11Y_RESTART_FROM + 1))" "$LOG" |
            head -c "$((A11Y_RESTART_TO - A11Y_RESTART_FROM))" | grep -c ShellExited || true)"
        [ "$shell_allowed" = 1 ] ||
            fail G-SHELL-STABLE "the deliberate accessibility-menu restart logged $shell_allowed shell exits, not one"
    fi
    [ "$shell_exits" = "$shell_allowed" ] ||
        fail G-SHELL-STABLE "the shell exited during the journey: $((shell_exits - shell_allowed)) time(s) outside the deliberate restart"
    if [ "$shell_allowed" = 1 ]; then
        pass G-SHELL-STABLE "the shell never exited or restarted except the one deliberate accessibility-menu restart"
    else
        pass G-SHELL-STABLE "the shell never exited or restarted"
    fi
}
