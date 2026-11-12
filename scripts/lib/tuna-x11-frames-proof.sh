#!/bin/sh
# Sourced by tuna-gtk-shell-proof, also used by its focused X11 frames run
# (--x11-frames-only). Requires that harness's private bus, nested window,
# GTK shell, AT-SPI and assertion helpers. P-WM-10 (#1034).
#
# X11 windows get their title bars as in Mutter 51: the compositor runs a
# frames client (tuna-x11-frames, a GTK4 X11 program) that makes one frame
# window with a header bar per decorated window, and the window manager
# reparents the window into it. The gates read both sides: the X server
# (xprop, xwininfo on the session's own display), the compositor's state
# file, the frames client's accessible tree and the output's pixels, and
# they drive the header bar with the real pointer.

# The session's own X server (Xwayland), set by x11_frames_proof.
X11F_DISPLAY=""

# jq: the nodes of the frames client's frame titled $t (its header bar's
# label and buttons), from a tuna-a11y-dump of tuna-x11-frames.
# shellcheck disable=SC2016 # a jq program, not shell
X11F_JQ='def frame($t): . as $all
    | ([range(0; length) | select($all[.].role == "frame" and $all[.].name == $t and $all[.].showing)][0]) as $i
    | if $i == null then null
      else [label $out | $all[$i + 1:][] | if .depth <= $all[$i].depth then break $out else . end] end;
  def buttons: map(select(.showing and (.role == "push button" or .role == "button")));'

# Run a command as a client of the session's own X server. Clients kept
# running take DISPLAY themselves: `$!` must be the client, not a shell.
x11f() { DISPLAY="$X11F_DISPLAY" "$@"; }

# Retry "$@" for up to $1 seconds.
x11f_until() {
    x11f_deadline=$(($(date +%s) + $1))
    shift
    while :; do
        "$@" && return 0
        [ "$(date +%s)" -ge "$x11f_deadline" ] && return 1
        sleep 0.2
    done
}

# One field (jq expression $2) of compositor window $1.
x11f_win() { jq -r ".windows[] | select(.id == $1) | $2" "$CSTATE"; }

# The id of the one X11 window mapped since the id list $1 was taken.
x11f_new_id() {
    x11f_new="[.windows[] | select(.x11_window_id != null and (.id as \$i | $1 | index(\$i) == null))]"
    cstate "$x11f_new | length == 1" "$2" 20 || return 1
    jq -r "${x11f_new}[0].id" "$CSTATE"
}

# "X Y W H" of X11 window $1 in root coordinates, as the X server has it.
x11f_geometry() {
    x11f xwininfo -id "$1" 2>/dev/null | awk '
        /Absolute upper-left X:/ { x = $NF }
        /Absolute upper-left Y:/ { y = $NF }
        /^ *Width:/ { w = $NF }
        /^ *Height:/ { h = $NF }
        END { print x, y, w, h }'
}

# The four cardinals of property $2 on X11 window $1 as "a,b,c,d"; empty
# when the window has no such property.
x11f_cardinals() {
    x11f xprop -id "$1" "$2" 2>/dev/null | sed -n "s/^$2(CARDINAL) = //p" | tr -d ' '
}

# The X11 window id (decimal) that window $1's parent names in
# _MUTTER_FRAME_FOR; empty when the parent is no frames-client frame.
x11f_frame_for() {
    x11f_target="$(x11f xprop -id "$1" _MUTTER_FRAME_FOR 2>/dev/null | sed -n 's/.*window id # \(0x[0-9a-f]*\).*/\1/p')"
    [ -n "$x11f_target" ] && printf '%d\n' "$x11f_target"
}

# The parent window (hex) of X11 window $1.
x11f_parent() {
    x11f xwininfo -id "$1" -tree 2>/dev/null | sed -n 's/^ *Parent window id: \(0x[0-9a-f]*\).*/\1/p'
}

# Whether X11 window $1 is where the compositor's window $2 says it is:
# $3 picks the rectangle ("x11_buffer_rect" for the window carrying the
# surface; "client" for the application's own window inside the borders).
x11f_placed() {
    if [ "$3" = client ]; then
        x11f_want="$(x11f_win "$2" '"\(.rect[0] + .x11_frame_extents[0]) \(.rect[1] + .x11_frame_extents[2]) \(.rect[2] - .x11_frame_extents[0] - .x11_frame_extents[1]) \(.rect[3] - .x11_frame_extents[2] - .x11_frame_extents[3])"')"
    else
        x11f_want="$(x11f_win "$2" ".$3 | map(tostring) | join(\" \")")"
    fi
    [ -n "$x11f_want" ] && [ "$(x11f_geometry "$1")" = "$x11f_want" ]
}

# Whether _NET_WM_STATE of X11 window $1 lists atom $2.
x11f_state_has() { x11f xprop -id "$1" _NET_WM_STATE 2>/dev/null | grep -q "$2"; }
x11f_state_lacks() { ! x11f_state_has "$1" "$2"; }

# Dump the frames client's accessible tree to $1 until jq filter $2 holds.
x11f_a11y() {
    x11f_a11y_end=$(($(date +%s) + 20))
    while :; do
        if "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-x11-frames "$1" 10 >/dev/null 2>&1 &&
            jq -e "$X11F_JQ $2" "$1" >/dev/null 2>&1; then
            return 0
        fi
        [ "$(date +%s)" -gt "$x11f_a11y_end" ] && return 1
        sleep 0.3
    done
}

# "X Y" of the centre of the showing button named $3 in the frame titled
# $2 of dump $1, in output coordinates: the frame's accessible bounds
# start at the window's rectangle ($4 $5).
x11f_button() {
    jq -r --arg t "$2" --arg n "$3" --argjson x "$4" --argjson y "$5" "$X11F_JQ"'
        frame($t) | buttons | map(select(.name == $n))[0].bounds
        | "\(($x + .[0] + .[2] / 2) | floor) \(($y + .[1] + .[3] / 2) | floor)"' "$1"
}

# Raw RGB of the $4 x $5 block at $2,$3 of PNG $1.
x11f_block() {
    ffmpeg -loglevel error -i "$1" -vf "crop=$4:$5:$2:$3" -f rawvideo -pix_fmt rgb24 -
}

# Whether the pixel at $2,$3 of PNG $1 is the probe's own background
# (xclock -bg '#c061cb').
x11f_magenta() {
    # shellcheck disable=SC2046 # three RGB channels
    set -- $(px "$1" "$2" "$3")
    [ "${1:-0}" -ge 150 ] && [ "${2:-255}" -le 130 ] && [ "${3:-0}" -ge 150 ]
}

# Press button 1 at $1,$2, move to $3,$4 in steps and release: enough
# distinct samples for GTK to recognize the drag before the last one.
x11f_drag() {
    xdotool mousemove --window "$WID" "$1" "$2"
    sleep 0.3
    xdotool mousedown 1
    sleep 0.1
    for x11f_step in 1 2 3 4 5 6 7 8; do
        xdotool mousemove --window "$WID" $(($1 + ($3 - $1) * x11f_step / 8)) $(($2 + ($4 - $2) * x11f_step / 8))
        sleep 0.05
    done
    sleep 0.2
    xdotool mouseup 1
}

# The frames client's records since log offset $1 that name a failure:
# an X error is fatal to a GDK program, and a critical is a bug.
x11f_log_errors() {
    tail -c +"$(($1 + 1))" "$LOG" |
        grep -E 'tuna-x11-frames.*(CRITICAL|ERROR)|X Window System error|tuna-compositor: x11 frames: (offer|XSETTINGS) refused' || true
}

x11_frames_proof() {
for x11f_tool in Xwayland xclock xeyes xev xprop xwininfo; do
    command -v "$x11f_tool" >/dev/null 2>&1 ||
        fail G-X11-FRAME "the X11 frame gates need $x11f_tool"
done
cstate '.x11_display != null' "xwayland" 30 || fail G-X11-FRAME "XWayland never came up"
X11F_DISPLAY="$(jq -r '.x11_display' "$CSTATE")"
x11f_mark="$(wc -c <"$LOG")"
# The header bar's buttons are GNOME's button-layout; the journey sets
# it, whatever the host's default is.
x11f_layout="$(gsettings get org.gnome.desktop.wm.preferences button-layout)"
gsettings set org.gnome.desktop.wm.preferences button-layout 'appmenu:close'

# G-X11-FRAME: a plain X11 window (xclock sets no Motif hints) asks the
# window manager for its decoration. Mutter's handshake, read from the X
# server: _MUTTER_NEEDS_FRAME on the client, a frames-client window
# naming it in _MUTTER_FRAME_FOR as its parent, the borders that frame
# reports published as the client's _NET_FRAME_EXTENTS.
x11f_before="$(jq -c '[.windows[] | .id]' "$CSTATE")"
DISPLAY="$X11F_DISPLAY" xclock -geometry 600x200 -bg '#c061cb' -title 'Tuna Frame Probe' >/dev/null 2>&1 &
x11f_probe_pid=$!
x11f_probe="$(x11f_new_id "$x11f_before" "frame probe mapped")" ||
    fail G-X11-FRAME "the X11 probe never mapped"
cstate ".windows[] | select(.id == $x11f_probe) | .x11_framed and .x11_frame_extents[2] > 0" "probe framed" ||
    fail G-X11-FRAME "the X11 probe got no frame"
x11f_frames_pid="$(pgrep -P "$COMPOSITOR_PID" -x tuna-x11-frames || true)"
[ "$(printf '%s\n' "$x11f_frames_pid" | grep -c .)" = 1 ] ||
    fail G-X11-FRAME "expected one frames client owned by the compositor, found '$x11f_frames_pid'"
x11f_client="$(x11f_win "$x11f_probe" .x11_window_id)"
x11f_top="$(x11f_win "$x11f_probe" '.x11_frame_extents[2]')"
x11f_extents="$(x11f_win "$x11f_probe" '.x11_frame_extents | map(tostring) | join(",")')"
[ "$(x11f_cardinals "$x11f_client" _MUTTER_NEEDS_FRAME)" = 1 ] ||
    fail G-X11-FRAME "the client does not carry _MUTTER_NEEDS_FRAME"
x11f_frame="$(x11f_parent "$x11f_client")"
[ "$(x11f_frame_for "$x11f_frame")" = "$x11f_client" ] ||
    fail G-X11-FRAME "the client's parent $x11f_frame does not name it in _MUTTER_FRAME_FOR"
x11f xprop -id "$x11f_frame" _NET_WM_PID | grep -q " = $x11f_frames_pid\$" ||
    fail G-X11-FRAME "the frame window is not the frames client's"
[ "$(x11f_cardinals "$x11f_frame" _MUTTER_FRAME_EXTENTS)" = "$x11f_extents" ] ||
    fail G-X11-FRAME "the frame reports $(x11f_cardinals "$x11f_frame" _MUTTER_FRAME_EXTENTS), the window manager read $x11f_extents"
[ "$(x11f_cardinals "$x11f_client" _NET_FRAME_EXTENTS)" = "$x11f_extents" ] ||
    fail G-X11-FRAME "_NET_FRAME_EXTENTS is '$(x11f_cardinals "$x11f_client" _NET_FRAME_EXTENTS)', not $x11f_extents"
# The window is the client plus its title bar, and nothing else: the
# application keeps the size it asked for, right under the header, and
# the frame's shadow (GDK's _GTK_FRAME_EXTENTS on the frame window) lies
# outside the window's rectangle.
cstate ".windows[] | select(.id == $x11f_probe) | .rect[2] == 600 and .rect[3] == 200 + $x11f_top" "probe size" 5 ||
    fail G-X11-FRAME "the framed window is not the 600x200 client plus a $x11f_top px title bar"
x11f_shadow="$(x11f_cardinals "$x11f_frame" _GTK_FRAME_EXTENTS)"
[ -n "$x11f_shadow" ] || fail G-X11-FRAME "the frame window reports no invisible border"
cstate ".windows[] | select(.id == $x11f_probe) | [$x11f_shadow] as [\$l, \$r, \$t, \$b]
    | .x11_buffer_rect == [.rect[0] - \$l, .rect[1] - \$t, .rect[2] + \$l + \$r, .rect[3] + \$t + \$b]" "shadow outside the window" 5 ||
    fail G-X11-FRAME "the frame's shadow ($x11f_shadow) is counted as part of the window"
x11f_until 5 x11f_placed "$x11f_frame" "$x11f_probe" x11_buffer_rect ||
    fail G-X11-FRAME "the frame window is at '$(x11f_geometry "$x11f_frame")', not where the compositor has it"
x11f_until 5 x11f_placed "$x11f_client" "$x11f_probe" client ||
    fail G-X11-FRAME "the client window is at '$(x11f_geometry "$x11f_client")', not under the title bar"
# The header bar is GTK's own: the title and the buttons a screen reader
# finds are what is drawn.
x11f_a11y "$ARTIFACTS/a11y-x11-frame.json" 'frame("Tuna Frame Probe") | any(.[]; .role == "label" and .name == "Tuna Frame Probe" and .showing) and (buttons | map(.name) == ["Close"])' ||
    fail G-X11-FRAME "the frame's header bar does not show the window title and a lone Close button"
gsettings set org.gnome.desktop.wm.preferences button-layout 'close:minimize,maximize'
x11f_a11y "$ARTIFACTS/a11y-x11-frame-layout.json" 'frame("Tuna Frame Probe") | buttons | sort_by(.bounds[0]) | map(.name) == ["Close", "Minimize", "Maximize"]' ||
    fail G-X11-FRAME "button-layout 'close:minimize,maximize' did not reach the header bar"
gsettings set org.gnome.desktop.wm.preferences button-layout 'appmenu:close'
x11f_a11y "$ARTIFACTS/a11y-x11-frame.json" 'frame("Tuna Frame Probe") | buttons | map(.name) == ["Close"]' ||
    fail G-X11-FRAME "button-layout 'appmenu:close' did not come back"
x11f xprop -id "$x11f_client" -f _NET_WM_NAME 8u -set _NET_WM_NAME 'Tuna Frame Renamed'
x11f_a11y "$ARTIFACTS/a11y-x11-frame-renamed.json" 'frame("Tuna Frame Renamed") | any(.[]; .role == "label" and .name == "Tuna Frame Renamed" and .showing)' ||
    fail G-X11-FRAME "the title bar did not follow _NET_WM_NAME"
# Pixels: the header band is drawn above the client, the client starts on
# the row after it and ends on the window's last row and column.
sleep 1
scrot -o "$ARTIFACTS/20-x11-frame.png" >/dev/null
read -r x11f_x x11f_y x11f_w x11f_h <<EOF
$(x11f_win "$x11f_probe" '.rect | map(tostring) | join(" ")')
EOF
x11f_magenta "$ARTIFACTS/20-x11-frame.png" $((x11f_x + 5)) $((x11f_y + x11f_top)) ||
    fail G-X11-FRAME "the client's first row is not right under the $x11f_top px header"
if x11f_magenta "$ARTIFACTS/20-x11-frame.png" $((x11f_x + 5)) $((x11f_y + x11f_top - 1)) ||
    x11f_magenta "$ARTIFACTS/20-x11-frame.png" $((x11f_x + 5)) $((x11f_y + 5)); then
    fail G-X11-FRAME "no header band is drawn above the client"
fi
x11f_magenta "$ARTIFACTS/20-x11-frame.png" $((x11f_x + x11f_w - 1)) $((x11f_y + x11f_h - 1)) ||
    fail G-X11-FRAME "the client does not reach the window's bottom right corner"
if x11f_magenta "$ARTIFACTS/20-x11-frame.png" $((x11f_x + x11f_w + 1)) $((x11f_y + x11f_h / 2)) ||
    x11f_magenta "$ARTIFACTS/20-x11-frame.png" $((x11f_x + x11f_w / 2)) $((x11f_y + x11f_h + 1)); then
    fail G-X11-FRAME "the client is drawn outside the window"
fi
# Focus: the window manager mirrors _NET_WM_STATE onto the frame window,
# and GTK draws the header bar focused or backdrop from it. Another X11
# window takes the focus; a block of the header bar clear of that
# window and its shadow (the Close button, else the bar's other end)
# must be drawn differently.
cstate ".focused == $x11f_probe" "probe focused" 5 || fail G-X11-FRAME "the mapped probe is not focused"
x11f_until 5 x11f_state_has "$x11f_frame" _NET_WM_STATE_FOCUSED ||
    fail G-X11-FRAME "the focused window's frame is not marked _NET_WM_STATE_FOCUSED"
# shellcheck disable=SC2046 # "X Y" of the button's centre
set -- $(x11f_button "$ARTIFACTS/a11y-x11-frame-renamed.json" "Tuna Frame Renamed" Close "$x11f_x" "$x11f_y")
x11f_close_x=$(($1 - 12)) x11f_close_y=$(($2 - 12))
x11f_before="$(jq -c '[.windows[] | .id]' "$CSTATE")"
DISPLAY="$X11F_DISPLAY" xeyes >/dev/null 2>&1 &
x11f_other_pid=$!
x11f_other="$(x11f_new_id "$x11f_before" "second X11 window mapped")" ||
    fail G-X11-FRAME "a second X11 window never mapped"
cstate ".focused == $x11f_other and (.windows[] | select(.id == $x11f_other) | .x11_framed)" "second window focused" ||
    fail G-X11-FRAME "the second X11 window was not framed and focused"
x11f_until 5 x11f_state_lacks "$x11f_frame" _NET_WM_STATE_FOCUSED ||
    fail G-X11-FRAME "the unfocused window's frame is still marked _NET_WM_STATE_FOCUSED"
# Whether a 24 px block at $1,$2 is outside the second window's surface.
x11f_clear_of_other() {
    jq -e ".windows[] | select(.id == $x11f_other) | .x11_buffer_rect
        | ($1 + 24 <= .[0]) or ($1 >= .[0] + .[2]) or ($2 + 24 <= .[1]) or ($2 >= .[1] + .[3])" "$CSTATE" >/dev/null
}
if x11f_clear_of_other "$x11f_close_x" "$x11f_close_y"; then
    x11f_block_x="$x11f_close_x" x11f_block_y="$x11f_close_y"
elif x11f_clear_of_other $((x11f_x + 6)) $((x11f_y + 6)); then
    x11f_block_x=$((x11f_x + 6)) x11f_block_y=$((x11f_y + 6))
else
    fail G-X11-FRAME "the second window covers both ends of the probe's header bar; the backdrop frame cannot be read"
fi
x11f_block "$ARTIFACTS/20-x11-frame.png" "$x11f_block_x" "$x11f_block_y" 24 24 >"$ARTIFACTS/x11-frame-close-focused.rgb"
sleep 1
scrot -o "$ARTIFACTS/20-x11-frame-backdrop.png" >/dev/null
x11f_block "$ARTIFACTS/20-x11-frame-backdrop.png" "$x11f_block_x" "$x11f_block_y" 24 24 >"$ARTIFACTS/x11-frame-close-backdrop.rgb"
if cmp -s "$ARTIFACTS/x11-frame-close-focused.rgb" "$ARTIFACTS/x11-frame-close-backdrop.rgb"; then
    fail G-X11-FRAME "the header bar is drawn the same focused and in the backdrop"
fi
x11f_magenta "$ARTIFACTS/20-x11-frame-backdrop.png" $((x11f_x + 5)) $((x11f_y + x11f_top)) ||
    fail G-X11-FRAME "the backdrop window lost its client content"
pass G-X11-FRAME "an X11 window is reparented into a tuna-x11-frames frame: _NET_FRAME_EXTENTS $x11f_extents, title and button-layout in a header bar drawn above the client, focused and backdrop"

# G-X11-FRAME-ACTIONS: the header bar asks the window manager through
# GDK's X11 requests on the frame window (_NET_WM_MOVERESIZE,
# _NET_WM_STATE, _GTK_SHOW_WINDOW_MENU); the close button asks the
# client (WM_DELETE_WINDOW). All from the real pointer.
x11f_grab_x=$((x11f_x + 40)) x11f_grab_y=$((x11f_y + x11f_top / 2))
x11f_drag "$x11f_grab_x" "$x11f_grab_y" 100 $((80 + x11f_top / 2))
x11f_x=60 x11f_y=80
cstate ".focused == $x11f_probe and .interactive_move_window == null and (.windows[] | select(.id == $x11f_probe) | .rect == [$x11f_x, $x11f_y, $x11f_w, $x11f_h])" "title bar drag" 10 ||
    fail G-X11-FRAME-ACTIONS "dragging the title bar did not move the window with the pointer to $x11f_x,$x11f_y"
# The X11 windows follow: clients place menus by root coordinates.
x11f_until 5 x11f_placed "$x11f_frame" "$x11f_probe" x11_buffer_rect ||
    fail G-X11-FRAME-ACTIONS "the frame window stayed at '$(x11f_geometry "$x11f_frame")' after the move"
x11f_until 5 x11f_placed "$x11f_client" "$x11f_probe" client ||
    fail G-X11-FRAME-ACTIONS "the client window stayed at '$(x11f_geometry "$x11f_client")' after the move"
x11f_until 5 x11f_state_has "$x11f_frame" _NET_WM_STATE_FOCUSED ||
    fail G-X11-FRAME-ACTIONS "pressing the title bar did not focus the frame again"
# Double click: GNOME's action-double-click-titlebar, toggle-maximize.
x11f_grab_x=$((x11f_x + 40)) x11f_grab_y=$((x11f_y + x11f_top / 2))
xdotool mousemove --window "$WID" "$x11f_grab_x" "$x11f_grab_y"
sleep 0.3
xdotool click --repeat 2 --delay 80 1
cstate ".windows[] | select(.id == $x11f_probe) | .rect[0] == 0 and .rect[1] >= 32 and .rect[2] == $WIDTH and .x11_framed and .x11_frame_extents[2] > 0 and .x11_buffer_rect == .rect" "maximized" 10 ||
    fail G-X11-FRAME-ACTIONS "a double click on the title bar did not maximize the framed window (without shadow)"
x11f_until 5 x11f_state_has "$x11f_client" _NET_WM_STATE_MAXIMIZED_VERT ||
    fail G-X11-FRAME-ACTIONS "the maximized client is not marked _NET_WM_STATE_MAXIMIZED_VERT"
x11f_until 5 x11f_placed "$x11f_client" "$x11f_probe" client ||
    fail G-X11-FRAME-ACTIONS "the maximized client is at '$(x11f_geometry "$x11f_client")', not under the title bar"
sleep 1
scrot -o "$ARTIFACTS/20-x11-frame-maximized.png" >/dev/null
x11f_max_y="$(x11f_win "$x11f_probe" '.rect[1]')"
x11f_max_top="$(x11f_win "$x11f_probe" '.x11_frame_extents[2]')"
if ! x11f_magenta "$ARTIFACTS/20-x11-frame-maximized.png" 5 $((x11f_max_y + x11f_max_top)) ||
    ! x11f_magenta "$ARTIFACTS/20-x11-frame-maximized.png" $((WIDTH - 1)) $((HEIGHT - 1)); then
    fail G-X11-FRAME-ACTIONS "the maximized client does not fill the work area under its title bar"
fi
xdotool mousemove --window "$WID" 40 $((x11f_max_y + x11f_max_top / 2))
sleep 0.3
xdotool click --repeat 2 --delay 80 1
cstate ".windows[] | select(.id == $x11f_probe) | .rect == [$x11f_x, $x11f_y, $x11f_w, $x11f_h] and .x11_buffer_rect != .rect" "restored" 10 ||
    fail G-X11-FRAME-ACTIONS "a second double click did not restore the window to $x11f_x,$x11f_y ${x11f_w}x$x11f_h"
x11f_until 5 x11f_state_lacks "$x11f_client" _NET_WM_STATE_MAXIMIZED_VERT ||
    fail G-X11-FRAME-ACTIONS "the restored client is still marked maximized"
x11f_until 5 x11f_placed "$x11f_client" "$x11f_probe" client ||
    fail G-X11-FRAME-ACTIONS "the restored client is at '$(x11f_geometry "$x11f_client")'"
# The resize handle in the frame's shadow, right of the window.
sleep 0.5
x11f_drag $((x11f_x + x11f_w + 3)) $((x11f_y + x11f_h / 2)) $((x11f_x + x11f_w + 53)) $((x11f_y + x11f_h / 2))
x11f_w=$((x11f_w + 50))
cstate ".windows[] | select(.id == $x11f_probe) | .rect == [$x11f_x, $x11f_y, $x11f_w, $x11f_h]" "edge resize" 10 ||
    fail G-X11-FRAME-ACTIONS "dragging the right edge did not widen the window by 50 px"
x11f_until 5 x11f_placed "$x11f_client" "$x11f_probe" client ||
    fail G-X11-FRAME-ACTIONS "the client was not resized with its frame: '$(x11f_geometry "$x11f_client")'"
# Secondary click: GNOME's action-right-click-titlebar, the window menu.
# The compositor says when the shell's menu surface is there; its tree
# is read once it has settled, never while it is torn down.
sleep 0.5
xdotool mousemove --window "$WID" $((x11f_x + 40)) $((x11f_y + x11f_top / 2)) click 3
cstate '.layers | any(.namespace == "tuna-window-menu")' "window menu" 10 ||
    fail G-X11-FRAME-ACTIONS "a right click on the title bar opened no window menu"
sleep 1.5
"$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-x11-frame-menu.json" 20 ||
    fail G-X11-FRAME-ACTIONS "accessibility dump failed"
showing "$ARTIFACTS/a11y-x11-frame-menu.json" "Hide" ||
    fail G-X11-FRAME-ACTIONS "the right click's menu is not the window menu"
scrot -o "$ARTIFACTS/20-x11-frame-menu.png" >/dev/null
xdotool key Escape
cstate '.layers | any(.namespace == "tuna-window-menu") | not' "window menu closed" 10 ||
    fail G-X11-FRAME-ACTIONS "Escape did not close the window menu"
sleep 1
# Close: the button the header bar draws, found where GTK reports it.
x11f_a11y "$ARTIFACTS/a11y-x11-frame-close.json" 'frame("Tuna Frame Renamed") | buttons | map(.name) == ["Close"]' ||
    fail G-X11-FRAME-ACTIONS "the frame lost its Close button"
# shellcheck disable=SC2046 # "X Y" of the button's centre
set -- $(x11f_button "$ARTIFACTS/a11y-x11-frame-close.json" "Tuna Frame Renamed" Close "$x11f_x" "$x11f_y")
xdotool mousemove --window "$WID" "$1" "$2"
sleep 0.3
xdotool click 1
cstate "all(.windows[]; .id != $x11f_probe)" "probe closed" 10 ||
    fail G-X11-FRAME-ACTIONS "the Close button did not close the window"
x11f_close_end=$(($(date +%s) + 10))
while kill -0 "$x11f_probe_pid" 2>/dev/null; do
    [ "$(date +%s)" -le "$x11f_close_end" ] ||
        fail G-X11-FRAME-ACTIONS "the client did not exit on WM_DELETE_WINDOW"
    sleep 0.2
done
pass G-X11-FRAME-ACTIONS "the frame's title bar moves the window with the pointer, maximizes and restores on double click, resizes from its edge, opens the window menu, and its Close button closes the client"

# G-X11-FRAME-CSD: a window that draws its own decoration (GTK sets
# _MOTIF_WM_HINTS decorations to 0) keeps it, and its popover, an
# override-redirect window, is no window manager's business.
# The closed probe's exit animation must not be in the baseline frame.
sleep 1
scrot -o "$ARTIFACTS/20-x11-frame-csd-before.png" >/dev/null
x11f_before="$(jq -c '[.windows[] | .id]' "$CSTATE")"
DISPLAY="$X11F_DISPLAY" GDK_BACKEND=x11 TUNA_TEST_POPOVER=1 "$PY" "$ROOT/scripts/lib/tuna-test-window.py" \
    "Framecheck" "#1c71d8" >"$ARTIFACTS/window-x11-csd.log" 2>&1 &
x11f_csd_pid=$!
x11f_csd="$(x11f_new_id "$x11f_before" "client-decorated X11 window mapped")" ||
    fail G-X11-FRAME-CSD "the client-decorated X11 window never mapped"
x11f_csd_client="$(x11f_win "$x11f_csd" .x11_window_id)"
# Its popover opens 1.5 s after the window: by then a frame would have
# come. The frames client never heard of either window.
# shellcheck disable=SC2329 # run by x11f_until
x11f_popover() {
    x11f_or=""
    for x11f_child in $(x11f xwininfo -root -children 2>/dev/null | sed -n 's/^ *\(0x[0-9a-f]*\) .*/\1/p'); do
        x11f_info="$(x11f xwininfo -id "$x11f_child" 2>/dev/null)" || continue
        printf '%s\n' "$x11f_info" | grep -q 'Override Redirect State: yes' || continue
        printf '%s\n' "$x11f_info" | grep -q 'Map State: IsViewable' || continue
        x11f xprop -id "$x11f_child" WM_CLASS 2>/dev/null | grep -q '"tuna-test-framecheck"' || continue
        x11f_or="$x11f_child"
        return 0
    done
    return 1
}
x11f_until 15 x11f_popover || fail G-X11-FRAME-CSD "the client's override-redirect popover never mapped"
sleep 1
cstate ".windows[] | select(.id == $x11f_csd) | .x11_framed == false and .x11_frame_extents == [0, 0, 0, 0]" "no frame" 5 ||
    fail G-X11-FRAME-CSD "a client-decorated X11 window was framed"
[ -z "$(x11f_cardinals "$x11f_csd_client" _MUTTER_NEEDS_FRAME)" ] ||
    fail G-X11-FRAME-CSD "the client-decorated window was offered to the frames client"
[ -z "$(x11f_cardinals "$x11f_or" _MUTTER_NEEDS_FRAME)" ] ||
    fail G-X11-FRAME-CSD "the override-redirect window was offered to the frames client"
[ "$(x11f_parent "$x11f_or")" = "$(x11f xwininfo -root | sed -n 's/^xwininfo: Window id: \(0x[0-9a-f]*\).*/\1/p')" ] ||
    fail G-X11-FRAME-CSD "the override-redirect window was reparented"
jq -e --argjson xid "$(printf '%d' "$x11f_or")" 'all(.windows[]; .x11_window_id != $xid)' "$CSTATE" >/dev/null ||
    fail G-X11-FRAME-CSD "the override-redirect window became a managed window"
for x11f_child in "$(x11f_parent "$x11f_csd_client")" $(x11f xwininfo -root -children 2>/dev/null | sed -n 's/^ *\(0x[0-9a-f]*\) .*/\1/p'); do
    x11f_named="$(x11f_frame_for "$x11f_child" || true)"
    if [ "$x11f_named" = "$x11f_csd_client" ] || [ "$x11f_named" = "$(printf '%d' "$x11f_or")" ]; then
        fail G-X11-FRAME-CSD "frame window $x11f_child was made for an undecorated window"
    fi
done
x11f_a11y "$ARTIFACTS/a11y-x11-frame-csd.json" '(frame("Framecheck") == null) and any(.[]; .role == "frame" and .showing)' ||
    fail G-X11-FRAME-CSD "the frames client drew a header bar for the client-decorated window"
# The window's rectangle is what the client draws as its window: its own
# shadow (_GTK_FRAME_EXTENTS) is outside it and stays see-through.
x11f_shadow="$(x11f_cardinals "$x11f_csd_client" _GTK_FRAME_EXTENTS)"
[ -n "$x11f_shadow" ] || fail G-X11-FRAME-CSD "the client-decorated window reports no shadow"
cstate ".windows[] | select(.id == $x11f_csd) | [$x11f_shadow] as [\$l, \$r, \$t, \$b]
    | .x11_buffer_rect == [.rect[0] - \$l, .rect[1] - \$t, .rect[2] + \$l + \$r, .rect[3] + \$t + \$b]" "client shadow outside the window" 5 ||
    fail G-X11-FRAME-CSD "the client's own shadow ($x11f_shadow) is counted as part of the window"
scrot -o "$ARTIFACTS/20-x11-frame-csd.png" >/dev/null
read -r x11f_bx x11f_by <<EOF
$(x11f_win "$x11f_csd" '.x11_buffer_rect | "\(.[0] + 2) \(.[1] + 2)"')
EOF
[ "$(px "$ARTIFACTS/20-x11-frame-csd.png" "$x11f_bx" "$x11f_by")" = "$(px "$ARTIFACTS/20-x11-frame-csd-before.png" "$x11f_bx" "$x11f_by")" ] ||
    fail G-X11-FRAME-CSD "the corner of the client's shadow is opaque: $(px "$ARTIFACTS/20-x11-frame-csd.png" "$x11f_bx" "$x11f_by")"
kill "$x11f_csd_pid" 2>/dev/null || true
cstate "all(.windows[]; .id != $x11f_csd)" "client-decorated window closed" 10 ||
    fail G-X11-FRAME-CSD "the client-decorated window stayed after its client exited"
pass G-X11-FRAME-CSD "a client-decorated X11 window and its override-redirect popover get no frame; the client's own shadow stays outside the window"

# G-X11-FRAME-RECOVERY: the frames client dies. The X server hands its
# clients back (the save set); the window manager keeps them mapped,
# bare, where they were, and the supervisor's next frames client frames
# them again. xev prints the events it receives: the pointer reaches the
# client bare and framed.
x11f_before="$(jq -c '[.windows[] | .id]' "$CSTATE")"
DISPLAY="$X11F_DISPLAY" xev -geometry 300x200 >"$ARTIFACTS/x11-frame-xev.log" 2>&1 &
x11f_xev_pid=$!
x11f_xev="$(x11f_new_id "$x11f_before" "recovery probe mapped")" ||
    fail G-X11-FRAME-RECOVERY "the recovery probe never mapped"
cstate ".focused == $x11f_xev and all(.windows[] | select(.id == $x11f_xev or .id == $x11f_other); .x11_framed and .x11_frame_extents[2] > 0)" "both framed" ||
    fail G-X11-FRAME-RECOVERY "the two X11 windows were not framed before the frames client died"
x11f_xev_client="$(x11f_win "$x11f_xev" .x11_window_id)"
x11f_xev_rect="$(x11f_win "$x11f_xev" '.rect | map(tostring) | join(",")')"
x11f_other_rect="$(x11f_win "$x11f_other" '.rect | map(tostring) | join(",")')"
x11f_top="$(x11f_win "$x11f_xev" '.x11_frame_extents[2]')"
x11f_extents="$(x11f_win "$x11f_xev" '.x11_frame_extents | map(tostring) | join(",")')"
read -r x11f_x x11f_y x11f_w x11f_h <<EOF
$(x11f_win "$x11f_xev" '.rect | map(tostring) | join(" ")')
EOF
# A point of the client both framed and bare (the window keeps its
# origin and loses the title bar's height).
x11f_click_x=$((x11f_x + x11f_w / 2)) x11f_click_y=$((x11f_y + x11f_top + (x11f_h - 2 * x11f_top) / 2))
xdotool mousemove --window "$WID" "$x11f_click_x" "$x11f_click_y"
sleep 0.5
x11f_presses() { grep -c '^ButtonPress event' "$ARTIFACTS/x11-frame-xev.log" || true; }
# shellcheck disable=SC2329 # run by x11f_until
x11f_pressed() { [ "$(x11f_presses)" -gt "$1" ]; }
DISPLAY="$X11F_DISPLAY" xprop -spy -id "$x11f_xev_client" _NET_FRAME_EXTENTS >"$ARTIFACTS/x11-frame-extents-spy.log" 2>&1 &
x11f_spy_pid=$!
sleep 0.5
x11f_recovery_mark="$(wc -c <"$LOG")"
kill -KILL "$x11f_frames_pid"
# The bare interval lasts the supervisor's restart delay: sample fast.
# The frame's X11 window carried the Wayland surface and took it along:
# each window has none until Xwayland made one for its bare wrapper (a
# few frames; the compositor reports it as surface_rect), and a press
# has nowhere to go before that. The click below is made once, on a
# window seen bare with its surface.
x11f_bare=""
x11f_bare_end=$(($(date +%s) + 10))
while [ "$(date +%s)" -le "$x11f_bare_end" ]; do
    if cp "$CSTATE" "$ARTIFACTS/x11-frame-bare.json" 2>/dev/null &&
        jq -e "[.windows[] | select(.id == $x11f_xev or .id == $x11f_other)] | length == 2 and all(.[]; .x11_framed == false and .surface_rect != null)" "$ARTIFACTS/x11-frame-bare.json" >/dev/null 2>&1; then
        x11f_bare=1
        break
    fi
    sleep 0.05
done
[ -n "$x11f_bare" ] || fail G-X11-FRAME-RECOVERY "the windows were never seen bare, each with its surface again, after the frames client was killed"
x11f_count="$(x11f_presses)"
xdotool click 1
scrot -o "$ARTIFACTS/20-x11-frame-bare.png" >/dev/null
jq -e ".windows[] | select(.id == $x11f_xev) | .rect == [$x11f_x, $x11f_y, $x11f_w, $x11f_h - $x11f_top] and .x11_frame_extents == [0, 0, 0, 0] and .x11_buffer_rect == .rect" "$ARTIFACTS/x11-frame-bare.json" >/dev/null ||
    fail G-X11-FRAME-RECOVERY "the bare window did not keep its origin and client size: $(jq -c ".windows[] | select(.id == $x11f_xev)" "$ARTIFACTS/x11-frame-bare.json")"
x11f_until 5 x11f_pressed "$x11f_count" ||
    fail G-X11-FRAME-RECOVERY "a click on the window after its frame went never reached the client"
# The supervisor's next frames client, and the same windows framed again
# at the same rectangles.
# shellcheck disable=SC2329 # run by x11f_until
x11f_restarted() {
    x11f_new_frames_pid="$(pgrep -P "$COMPOSITOR_PID" -x tuna-x11-frames || true)"
    [ -n "$x11f_new_frames_pid" ] && [ "$x11f_new_frames_pid" != "$x11f_frames_pid" ]
}
x11f_until 15 x11f_restarted || fail G-X11-FRAME-RECOVERY "the frames client was not restarted"
cstate "[.windows[] | select(.id == $x11f_xev or .id == $x11f_other)] | length == 2 and all(.[]; .x11_framed and .x11_frame_extents[2] > 0)" "framed again" 15 ||
    fail G-X11-FRAME-RECOVERY "the windows were not framed again by the restarted frames client"
cstate "(.windows[] | select(.id == $x11f_xev) | .rect == [$x11f_xev_rect]) and (.windows[] | select(.id == $x11f_other) | .rect == [$x11f_other_rect])" "same rectangles" 5 ||
    fail G-X11-FRAME-RECOVERY "the reframed windows did not come back to their rectangles"
x11f_frame="$(x11f_parent "$x11f_xev_client")"
[ "$(x11f_frame_for "$x11f_frame")" = "$x11f_xev_client" ] ||
    fail G-X11-FRAME-RECOVERY "the client's new parent does not name it in _MUTTER_FRAME_FOR"
x11f xprop -id "$x11f_frame" _NET_WM_PID | grep -q " = $x11f_new_frames_pid\$" ||
    fail G-X11-FRAME-RECOVERY "the new frame is not the restarted frames client's"
x11f_until 5 x11f_placed "$x11f_xev_client" "$x11f_xev" client ||
    fail G-X11-FRAME-RECOVERY "the reframed client is at '$(x11f_geometry "$x11f_xev_client")', not under its title bar"
# The client saw its borders go and come back; it was never withdrawn.
# shellcheck disable=SC2329 # run by x11f_until
x11f_spied() {
    grep -n '_NET_FRAME_EXTENTS(CARDINAL) = ' "$ARTIFACTS/x11-frame-extents-spy.log" | tr -d ' ' |
        awk -F= -v want="$x11f_extents" '
            $2 == "0,0,0,0" && !bare { bare = NR }
            bare && NR > bare && $2 == want { again = 1 }
            END { exit !(bare && again) }'
}
x11f_until 5 x11f_spied ||
    fail G-X11-FRAME-RECOVERY "_NET_FRAME_EXTENTS did not go to 0,0,0,0 and back to $x11f_extents: $(tr '\n' ' ' <"$ARTIFACTS/x11-frame-extents-spy.log")"
kill "$x11f_spy_pid" 2>/dev/null || true
if tail -c +"$((x11f_recovery_mark + 1))" "$LOG" | grep -Eq "tuna-compositor: window ($x11f_xev|$x11f_other) unmapped"; then
    fail G-X11-FRAME-RECOVERY "a window was unmapped when the frames client died"
fi
x11f_a11y "$ARTIFACTS/a11y-x11-frame-recovered.json" 'frame("Event Tester") | buttons | map(.name) == ["Close"]' ||
    fail G-X11-FRAME-RECOVERY "the restarted frames client shows no header bar for the window"
sleep 0.5
x11f_count="$(x11f_presses)"
xdotool mousemove --window "$WID" "$x11f_click_x" "$x11f_click_y" click 1
x11f_until 5 x11f_pressed "$x11f_count" ||
    fail G-X11-FRAME-RECOVERY "a click on the reframed window never reached the client"
scrot -o "$ARTIFACTS/20-x11-frame-recovered.png" >/dev/null
pass G-X11-FRAME-RECOVERY "killing tuna-x11-frames leaves X11 windows mapped bare where they were and taking the pointer; the restarted client frames them again"

# Leave the session as it was found.
kill "$x11f_xev_pid" "$x11f_other_pid" 2>/dev/null || true
cstate "all(.windows[]; .id != $x11f_xev and .id != $x11f_other)" "probes closed" 10 ||
    fail G-X11-FRAME-RECOVERY "the probes stayed after their clients exited"
gsettings set org.gnome.desktop.wm.preferences button-layout "$x11f_layout"
x11f_errors="$(x11f_log_errors "$x11f_mark")"
[ -z "$x11f_errors" ] || fail G-X11-FRAME "the frames client logged a failure: $x11f_errors"
}
