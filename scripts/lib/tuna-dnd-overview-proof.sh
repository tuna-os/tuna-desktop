#!/bin/sh
# Sourced by tuna-gtk-shell-proof for its focused --dnd-overview-only run
# (#1189). Requires that harness's private bus, GTK shell and helpers (pass,
# fail, cstate, $PY, $ROOT, $ARTIFACTS, $CSTATE, $WID, $SOCKET_NAME).
#
# The drags are real: scripts/lib/tuna-test-window.py with TUNA_TEST_DRAG is
# a GTK 4 window whose label starts a Gtk.DragSource drag (wl_data_device
# start_drag) and takes its drop; the button and every move are real X11
# input through the nested window. GNOME's own timings (overview.js
# DND_WINDOW_SWITCH_TIMEOUT, panel.js BUTTON_DND_ACTIVATION_TIMEOUT) are
# measured from the last motion xdotool sent: a timer can fire no earlier
# than that, so the elapsed time is a lower bound the compositor cannot
# beat; the state file is polled, so nothing tighter is claimed.

# Pointer position as the proof last left it.
DND_X=0
DND_Y=0
# Monotonic nanoseconds just before the last motion of dnd_glide.
DND_LAST_MOVE_NS=0

# One drag-capable GTK window: TITLE COLOR LOG.
dnd_window() {
    WAYLAND_DISPLAY="$SOCKET_NAME" TUNA_TEST_DRAG="$3" "$PY" \
        "$ROOT/scripts/lib/tuna-test-window.py" "$1" "$2" >>"$ARTIFACTS/dnd-apps.log" 2>&1 &
    DND_PIDS="$DND_PIDS $!"
}

# The pointer to X Y in ten steps (as a hand does), without lifting the button.
dnd_glide() {
    dnd_from_x="$DND_X"
    dnd_from_y="$DND_Y"
    dnd_step=1
    while [ "$dnd_step" -le 10 ]; do
        dnd_next_x=$((dnd_from_x + ($1 - dnd_from_x) * dnd_step / 10))
        dnd_next_y=$((dnd_from_y + ($2 - dnd_from_y) * dnd_step / 10))
        if [ "$dnd_step" = 10 ]; then
            DND_LAST_MOVE_NS="$(date +%s%N)"
        fi
        xdotool mousemove --window "$WID" "$dnd_next_x" "$dnd_next_y"
        sleep 0.03
        dnd_step=$((dnd_step + 1))
    done
    DND_X="$1"
    DND_Y="$2"
}

# Milliseconds since the last motion of dnd_glide.
dnd_elapsed_ms() {
    echo $((($(date +%s%N) - DND_LAST_MOVE_NS) / 1000000))
}

# A point on the label of window APP that no window above it (APP_OVER)
# covers: where a press starts that window's drag.
dnd_free_point() {
    jq -r --arg a "$1" --arg b "$2" '
        ([.windows[] | select(.app_id == $a)][0].rect) as $r
        | ([.windows[] | select(.app_id == $b)][0].rect) as $o
        | [range(0; 5) as $i | (0, 50, 100, 150) as $j
            | [(($r[0] + 20 + ($r[2] - 40) * $i / 4) | floor), ($r[1] + 60 + $j)]]
        | map(select(. as $p
            | ($p[0] >= $o[0] and $p[0] < $o[0] + $o[2] and $p[1] >= $o[1] and $p[1] < $o[1] + $o[3]) | not))
        | .[0] | "\(.[0]) \(.[1])"' "$CSTATE"
}

# Press at "X Y" and move until GTK has started a real drag (the button
# stays held).
dnd_begin() {
    case "$1" in null*) fail G-DND-OVERVIEW "no point on the source's label is clear of the target" ;; esac
    # shellcheck disable=SC2086
    set -- $1
    DND_X="$1"
    DND_Y="$2"
    xdotool mousemove --window "$WID" "$DND_X" "$DND_Y"
    sleep 0.3
    xdotool mousedown 1
    dnd_i=1
    while [ "$dnd_i" -le 8 ]; do
        xdotool mousemove --window "$WID" $((DND_X + dnd_i * 4)) $((DND_Y + dnd_i * 2))
        sleep 0.05
        dnd_i=$((dnd_i + 1))
    done
    DND_X=$((DND_X + 32))
    DND_Y=$((DND_Y + 16))
    cstate '.client_dnd_active' "client drag" 10 ||
        fail G-DND-OVERVIEW "GTK never started a real drag from $1"
}

# The centre of a rectangle given as jq path into the state: "x y".
dnd_centre() {
    jq -r "$1 | \"\\(.[0] + .[2] / 2 | floor) \\(.[1] + .[3] / 2 | floor)\"" "$CSTATE"
}

dnd_overview_proof() {
    DND_PIDS=""
    DND_SOURCE_LOG="$ARTIFACTS/dnd-source.log"
    DND_TARGET_LOG="$ARTIFACTS/dnd-target.log"
    : >"$DND_SOURCE_LOG"
    : >"$DND_TARGET_LOG"
    : >"$ARTIFACTS/dnd-apps.log"
    dnd_open="$(jq '.windows | length' "$CSTATE")"
    # Target first, then source over it, then a third window sent one
    # workspace on: three workspaces (the trailing empty one included) put
    # GNOME's thumbnails strip in the overview.
    dnd_window Dnd-Target '#2ec27e' "$DND_TARGET_LOG"
    cstate ".windows | length == $((dnd_open + 1))" "target mapped" ||
        fail G-DND-OVERVIEW "the drop target window never mapped"
    dnd_window Dnd-Source '#3584e4' "$DND_SOURCE_LOG"
    cstate ".windows | length == $((dnd_open + 2))" "source mapped" ||
        fail G-DND-OVERVIEW "the drag source window never mapped"
    dnd_window Dnd-Away '#e66100' "$ARTIFACTS/dnd-away.log"
    cstate '.focused_app_id == "tuna-test-dnd-away"' "away focused" ||
        fail G-DND-OVERVIEW "the third window never took focus"
    sleep 1.5
    dnd_home="$(jq '.active_workspace' "$CSTATE")"
    xdotool key super+shift+Page_Down
    cstate ".active_workspace == $dnd_home + 1 and any(.windows[]; .app_id == \"tuna-test-dnd-away\" and .workspace == $dnd_home + 1)" \
        "away moved on" ||
        fail G-DND-OVERVIEW "Super+Shift+Page Down did not move the third window one workspace on"
    xdotool key super+Page_Up
    cstate ".active_workspace == $dnd_home" "home again" ||
        fail G-DND-OVERVIEW "could not return to the drag's workspace"
    sleep 1.5
    xdotool mousemove --window "$WID" $((WIDTH / 2)) $((HEIGHT - 20))
    cstate '(.overview_open | not) and (.client_dnd_active | not)' "quiet desktop" ||
        fail G-DND-OVERVIEW "the desktop was not quiet before the first drag"
    dnd_target_id="$(jq '.windows[] | select(.app_id == "tuna-test-dnd-target") | .id' "$CSTATE")"

    # --- The hot corner: a real drag, the pointer to the corner. ---------
    dnd_begin "$(dnd_free_point tuna-test-dnd-source tuna-test-dnd-target)"
    grep -q drag-begin "$DND_SOURCE_LOG" ||
        fail G-DND-HOT-CORNER "the GTK source never reported its drag"
    dnd_glide 1 1
    cstate '.overview_open and .client_dnd_active' "corner opened under the drag" ||
        fail G-DND-HOT-CORNER "hovering the hot corner with a client drag did not open the overview"
    cp "$CSTATE" "$ARTIFACTS/dnd-hot-corner.json"
    pass G-DND-HOT-CORNER "a real GTK drag held over the hot corner opens the overview and keeps the drag"

    # --- A window preview held 750 ms is activated: the drop lands. ------
    cstate '.overview_progress == 1' "overview settled" ||
        fail G-DND-WINDOW-SWITCH "the overview never settled open"
    dnd_preview="$(dnd_centre ".previews[] | select(.app_id == \"tuna-test-dnd-target\") | .rect")"
    [ -n "$dnd_preview" ] || fail G-DND-WINDOW-SWITCH "the overview shows no preview of the target"
    cstate '.focused_app_id != "tuna-test-dnd-target"' "target not yet up" ||
        fail G-DND-WINDOW-SWITCH "the target was focused before the drag reached its preview"
    # shellcheck disable=SC2086
    dnd_glide $dnd_preview
    cstate '(.overview_open | not) and .focused_app_id == "tuna-test-dnd-target"' "target activated" 10 ||
        fail G-DND-WINDOW-SWITCH "resting on the target's preview did not activate it and close the overview"
    dnd_waited="$(dnd_elapsed_ms)"
    [ "$dnd_waited" -ge 750 ] ||
        fail G-DND-WINDOW-SWITCH "the preview was activated after ${dnd_waited} ms, before GNOME's 750 ms"
    [ "$dnd_waited" -lt 6000 ] ||
        fail G-DND-WINDOW-SWITCH "the preview took ${dnd_waited} ms to activate"
    cstate ".client_dnd_active and .stacking[-1] == $dnd_target_id and .active_workspace == $dnd_home" "target raised" ||
        fail G-DND-WINDOW-SWITCH "the activated window was not raised with the drag still running"
    cp "$CSTATE" "$ARTIFACTS/dnd-window-switch.json"
    # The drop lands on it.
    # shellcheck disable=SC2046 # exactly two numeric coordinates
    dnd_glide $(dnd_centre ".windows[] | select(.app_id == \"tuna-test-dnd-target\") | .rect")
    sleep 0.5
    xdotool mouseup 1
    dnd_end=$(($(date +%s) + 10))
    until grep -q '^drop ' "$DND_TARGET_LOG" || [ "$(date +%s)" -gt "$dnd_end" ]; do sleep 0.2; done
    grep -q '^drop ' "$DND_TARGET_LOG" ||
        fail G-DND-WINDOW-SWITCH "the drop never reached the activated window"
    cstate '(.client_dnd_active | not) and (.overview_open | not) and .active_workspace == '"$dnd_home" "dropped" ||
        fail G-DND-WINDOW-SWITCH "the drop left the overview or the workspace changed"
    pass G-DND-WINDOW-SWITCH "a drag resting ${dnd_waited} ms (GNOME: 750) on a window preview activates and raises that window, and the drop lands there"
    sleep 1

    # --- The Activities button, then a thumbnail, then a cancel. ---------
    dnd_begin "$(dnd_free_point tuna-test-dnd-source tuna-test-dnd-target)"
    dnd_glide $((WIDTH / 2)) $((HEIGHT / 2))
    cstate '.overview_open | not' "still closed" ||
        fail G-DND-ACTIVITIES "the overview opened away from the corner and the button"
    dnd_glide 40 10
    cstate '.overview_open' "button opened" 10 ||
        fail G-DND-ACTIVITIES "a drag resting on the Activities button did not open the overview"
    dnd_waited="$(dnd_elapsed_ms)"
    [ "$dnd_waited" -ge 250 ] ||
        fail G-DND-ACTIVITIES "the button opened the overview after ${dnd_waited} ms, before GNOME's 250 ms"
    cstate '.client_dnd_active and .overview_progress == 1' "open under the drag" ||
        fail G-DND-ACTIVITIES "the overview did not settle open under the drag"
    pass G-DND-ACTIVITIES "a real drag resting ${dnd_waited} ms (GNOME: 250) on the Activities button opens the overview"
    dnd_thumb="$(dnd_centre ".thumbnails[] | select(.workspace == $dnd_home + 1) | .rect")"
    [ -n "$dnd_thumb" ] || fail G-DND-CANCEL-RETURNS "the overview shows no thumbnail of the next workspace"
    # shellcheck disable=SC2086
    dnd_glide $dnd_thumb
    cstate ".overview_open and .active_workspace == $dnd_home + 1 and .client_dnd_active" "thumbnail activated" ||
        fail G-DND-CANCEL-RETURNS "hovering the next workspace's thumbnail did not activate it"
    xdotool key Escape
    cstate ".client_dnd_active | not" "cancelled" ||
        fail G-DND-CANCEL-RETURNS "Escape did not cancel the drag"
    cstate "(.overview_open | not) and .active_workspace == $dnd_home" "returned" ||
        fail G-DND-CANCEL-RETURNS "cancelling the drag did not return to the starting workspace and close the overview"
    cstate ".focused as \$f | \$f != null and any(.windows[]; .id == \$f and .workspace == $dnd_home)" "focus returned" ||
        fail G-DND-CANCEL-RETURNS "no window of the starting workspace took focus after the cancel"
    cp "$CSTATE" "$ARTIFACTS/dnd-cancel.json"
    xdotool mouseup 1
    pass G-DND-CANCEL-RETURNS "cancelling a drag that switched workspace in the overview returns to the starting workspace and closes it"
    sleep 1

    # --- A drop does the same. -------------------------------------------
    dnd_begin "$(dnd_free_point tuna-test-dnd-source tuna-test-dnd-target)"
    dnd_glide 1 1
    cstate '.overview_open and .overview_progress == 1' "corner open" ||
        fail G-DND-DROP-RETURNS "the corner did not open the overview for the second drag"
    # shellcheck disable=SC2086
    dnd_glide $dnd_thumb
    cstate ".overview_open and .active_workspace == $dnd_home + 1" "thumbnail again" ||
        fail G-DND-DROP-RETURNS "hovering the thumbnail did not activate the workspace again"
    xdotool mouseup 1
    cstate "(.client_dnd_active | not) and (.overview_open | not) and .active_workspace == $dnd_home" "drop returned" ||
        fail G-DND-DROP-RETURNS "dropping the drag did not return to the starting workspace and close the overview"
    cstate ".focused as \$f | \$f != null and any(.windows[]; .id == \$f and .workspace == $dnd_home)" "focus returned" ||
        fail G-DND-DROP-RETURNS "no window of the starting workspace took focus after the drop"
    cp "$CSTATE" "$ARTIFACTS/dnd-drop.json"
    pass G-DND-DROP-RETURNS "dropping a drag that switched workspace in the overview returns to the starting workspace and closes it"

    # shellcheck disable=SC2086
    kill $DND_PIDS 2>/dev/null || true
    # shellcheck disable=SC2086
    wait $DND_PIDS 2>/dev/null || true
    cstate ".windows | length == $dnd_open" "proof windows closed" ||
        fail G-DND-OVERVIEW "the proof's windows did not close"
    pass G-DND-OVERVIEW "client drags open the overview from the corner and the Activities button, activate a window preview after 750 ms, and return to the starting workspace on cancel and drop"
}
