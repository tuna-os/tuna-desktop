#!/bin/sh
# Sourced by tuna-gtk-shell-proof after tuna-x11-frames-proof.sh, whose
# pointer drag (x11f_drag) it uses. Run by the full journey next to the
# other window gates and by --x11-frames-only, whose frame gates resize an
# X11 window from the same invisible border. P-WM-04 (#1079).
#
# A client-decorated window (every GTK4 and libadwaita application) is a
# surface larger than the window it shows: the shadow lies around the
# window's rectangle, and the part of the shadow next to the window is in
# the surface's input region. That is where the resize handles are. Mutter
# picks a surface by its input region, so the pointer just outside a
# window's visible edge belongs to that window, and the pointer in the rest
# of the shadow to whatever is beneath. The gate drives two real GTK4
# windows with the real pointer and reads the compositor's state file.

# One field (jq expression $2) of compositor window $1.
rb_win() { jq -r ".windows[] | select(.id == $1) | $2" "$CSTATE"; }

# Start a libadwaita test window titled $1 coloured $2 and leave its
# compositor id in $rb_id and its process in $rb_pid.
rb_open() {
    rb_before="$(jq -c '[.windows[] | .id]' "$CSTATE")"
    WAYLAND_DISPLAY="$rb_socket" "$PY" "$ROOT/scripts/lib/tuna-test-window.py" "$1" "$2" \
        >"$ARTIFACTS/window-$1.log" 2>&1 &
    rb_pid=$!
    cstate "[.windows[] | select(.id as \$i | $rb_before | index(\$i) == null)] | length == 1" "$1 mapped" ||
        fail G-RESIZE-BORDER "the window $1 never mapped"
    rb_id="$(jq -r "[.windows[] | select(.id as \$i | $rb_before | index(\$i) == null)][0].id" "$CSTATE")"
    cstate ".focused == $rb_id and (.windows[] | select(.id == $rb_id) | .surface_rect != null)" "$1 focused and drawn" ||
        fail G-RESIZE-BORDER "the window $1 was not focused and drawn"
}

resize_border_proof() {
rb_socket="$(sed -n 's/.*nested session on \([^ ]*\) .*/\1/p' "$LOG" | head -1)"
rb_open Borderlow '#986a44'
rb_low="$rb_id" rb_low_pid="$rb_pid"
rb_open Borderhigh '#2190a4'
rb_high="$rb_id" rb_high_pid="$rb_pid"
sleep 1
# The upper window moves down and right by its header bar, so its left
# shadow lies over the lower window.
read -r rb_x rb_y rb_w rb_h <<EOF
$(rb_win "$rb_high" '.rect | map(tostring) | join(" ")')
EOF
x11f_drag $((rb_x + 120)) $((rb_y + 20)) $((rb_x + 120 + 140)) $((rb_y + 20 + 40))
cstate ".interactive_move_window == null and (.windows[] | select(.id == $rb_high) | .rect[2] == $rb_w and .rect[3] == $rb_h and .rect[0] - $rb_x >= 100 and .rect[0] - $rb_x <= 140 and .rect[1] - $rb_y >= 20 and .rect[1] - $rb_y <= 40)" "upper window moved" 10 ||
    fail G-RESIZE-BORDER "dragging the upper window's header bar did not move it: $(rb_win "$rb_high" '.rect | tostring')"
sleep 0.5
read -r rb_x rb_y rb_w rb_h <<EOF
$(rb_win "$rb_high" '.rect | map(tostring) | join(" ")')
EOF
# The client draws a shadow around its window and takes input on the part
# next to the window only: at least 4 px of handle past each edge, and
# shadow past the handle.
cstate ".windows[] | select(.id == $rb_high) | .rect as \$r | .input_rect as \$i | .surface_rect as \$s
    | \$i != null and \$i[0] <= \$r[0] - 5 and \$i[0] + \$i[2] >= \$r[0] + \$r[2] + 5 and \$i[1] + \$i[3] >= \$r[1] + \$r[3] + 5
    and \$s[0] <= \$i[0] - 4" "shadow, handles and window" 5 ||
    fail G-RESIZE-BORDER "the GTK4 window has no resize handles in a shadow wider than them: $(rb_win "$rb_high" '{rect, input_rect, surface_rect} | tostring')"
scrot -o "$ARTIFACTS/07b-resize-border-before.png" >/dev/null

# The right edge, from 4 px outside the window: the pointer there is the
# window's, GTK starts the resize from its handle, and the window grows
# by exactly the pointer's travel.
x11f_drag $((rb_x + rb_w + 4)) $((rb_y + rb_h / 2)) $((rb_x + rb_w + 4 + 60)) $((rb_y + rb_h / 2))
cstate ".windows[] | select(.id == $rb_high) | .rect == [$rb_x, $rb_y, $((rb_w + 60)), $rb_h]" "right edge resize" 10 ||
    fail G-RESIZE-BORDER "a drag from 4 px right of the window did not widen it by 60 px: $(rb_win "$rb_high" '.rect | tostring'), was [$rb_x,$rb_y,$rb_w,$rb_h]"
rb_w=$((rb_w + 60))
sleep 0.5
# The bottom right corner, from 4 px outside both edges. The pointer
# must be able to travel the whole way on the output.
[ $((rb_x + rb_w + 4 + 40)) -lt "$WIDTH" ] && [ $((rb_y + rb_h + 4 + 30)) -lt "$HEIGHT" ] ||
    fail G-RESIZE-BORDER "the window at [$rb_x,$rb_y,$rb_w,$rb_h] leaves no room on the output for the corner drag"
x11f_drag $((rb_x + rb_w + 4)) $((rb_y + rb_h + 4)) $((rb_x + rb_w + 4 + 40)) $((rb_y + rb_h + 4 + 30))
cstate ".windows[] | select(.id == $rb_high) | .rect == [$rb_x, $rb_y, $((rb_w + 40)), $((rb_h + 30))]" "corner resize" 10 ||
    fail G-RESIZE-BORDER "a drag from 4 px outside the bottom right corner did not grow the window by 40x30 px: $(rb_win "$rb_high" '.rect | tostring'), was [$rb_x,$rb_y,$rb_w,$rb_h]"
rb_w=$((rb_w + 40)) rb_h=$((rb_h + 30))
cstate ".focused == $rb_high and (.stacking | last) == $rb_high" "upper window still on top" 5 ||
    fail G-RESIZE-BORDER "the resized window is not the focused top window"
sleep 0.5
scrot -o "$ARTIFACTS/07b-resize-border-after.png" >/dev/null

# The far shadow: a point of the upper window's surface left of its
# handles, over the lower window. The click is the lower window's.
rb_point="$(jq -r --argjson high "$rb_high" --argjson low "$rb_low" '
    (.windows[] | select(.id == $high)) as $h | (.windows[] | select(.id == $low) | .rect) as $l
    | [(($h.surface_rect[0] + $h.input_rect[0]) / 2 | floor), ($h.rect[1] + 60)]
    | select(.[0] >= $h.surface_rect[0] and .[0] < $h.input_rect[0]
        and .[0] >= $l[0] and .[0] < $l[0] + $l[2] and .[1] >= $l[1] and .[1] < $l[1] + $l[3])
    | map(tostring) | join(" ")' "$CSTATE")"
[ -n "$rb_point" ] ||
    fail G-RESIZE-BORDER "the upper window's far shadow is not over the lower window: $(jq -c "[.windows[] | select(.id == $rb_high or .id == $rb_low) | {id, rect, input_rect, surface_rect}]" "$CSTATE")"
# shellcheck disable=SC2086 # "X Y"
xdotool mousemove --window "$WID" $rb_point
sleep 0.3
xdotool click 1
cstate ".focused == $rb_low and (.stacking | last) == $rb_low" "lower window focused" 10 ||
    fail G-RESIZE-BORDER "a click in the upper window's far shadow at $rb_point did not reach the window underneath"
jq -e ".windows[] | select(.id == $rb_high) | .rect == [$rb_x, $rb_y, $rb_w, $rb_h]" "$CSTATE" >/dev/null ||
    fail G-RESIZE-BORDER "the click in the far shadow changed the upper window"
pass G-RESIZE-BORDER "a GTK4 window resizes by the pointer's travel from 4 px outside its right edge and its bottom right corner; a click in its shadow past the handles reaches the window underneath"

# Leave the session as it was found.
kill "$rb_high_pid" "$rb_low_pid" 2>/dev/null || true
cstate "all(.windows[]; .id != $rb_high and .id != $rb_low)" "border probes closed" 10 ||
    fail G-RESIZE-BORDER "the probes stayed after their clients exited"
}
