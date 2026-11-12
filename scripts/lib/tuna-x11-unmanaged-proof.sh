#!/bin/sh
# Sourced by tuna-gtk-shell-proof after tuna-x11-frames-proof.sh, whose
# helpers (x11f, x11f_until, x11f_win, x11f_new_id, x11f_geometry) and
# X11F_DISPLAY it uses. Run by the full journey and by --x11-frames-only.
# P-SY-04 (#1076).
#
# Override-redirect X11 windows are the windows no window manager manages:
# menus, tooltips, dropdowns. The client maps them where it wants them and
# drives them through its own pointer and keyboard grabs. As Mutter 51
# does for its unmanaged windows, the compositor only draws them where
# they are and gives them the pointer; it never frames, focuses or lists
# them. The gates use a GTK3 program (tuna-x11-menu-window.py): GTK3's
# menus and tooltips on X11 are such windows, and an open menu holds
# GTK's grabs. They read the X server (xwininfo, xprop), the client's own
# record of what it did, the compositor's state file and the output's
# pixels, and they drive the menu with the real pointer.

X11U_EVENTS=""
X11U_LAYOUT=""

# The viewable override-redirect root children of the menu probe, one
# X11 window id (hex) per line.
x11u_windows() {
    for x11u_child in $(x11f xwininfo -root -children 2>/dev/null | sed -n 's/^ *\(0x[0-9a-f]*\) .*/\1/p'); do
        x11u_info="$(x11f xwininfo -id "$x11u_child" 2>/dev/null)" || continue
        printf '%s\n' "$x11u_info" | grep -q 'Override Redirect State: yes' || continue
        printf '%s\n' "$x11u_info" | grep -q 'Map State: IsViewable' || continue
        x11f xprop -id "$x11u_child" WM_CLASS 2>/dev/null | grep -q '"tuna-x11-menu"' || continue
        printf '%s\n' "$x11u_child"
    done
}

# Whether the probe has exactly $1 such windows; they are left in
# $x11u_found.
x11u_count() {
    x11u_found="$(x11u_windows)"
    [ "$(printf '%s\n' "$x11u_found" | grep -c .)" = "$1" ]
}

# The probe's override-redirect window of EWMH type $1 (POPUP_MENU,
# DROPDOWN_MENU, TOOLTIP), left in $x11u_typed.
x11u_of_type() {
    x11u_typed=""
    for x11u_candidate in $(x11u_windows); do
        if x11f xprop -id "$x11u_candidate" _NET_WM_WINDOW_TYPE 2>/dev/null | grep -q "_NET_WM_WINDOW_TYPE_$1"; then
            x11u_typed="$x11u_candidate"
            return 0
        fi
    done
    return 1
}

# "X Y W H" of the client's widget $1 in root coordinates, from the
# layout the client writes; fails while the widget is not mapped.
x11u_rect() {
    jq -er --arg n "$1" '.[$n] | select(. != null) | map(tostring) | join(" ")' "$X11U_LAYOUT" 2>/dev/null
}

# "X Y" of a point of widget $1 clear of its label: $2 px in from its
# right edge, at mid height.
x11u_point() {
    # shellcheck disable=SC2046 # "X Y W H"
    set -- "$2" $(x11u_rect "$1")
    [ "$#" = 5 ] || return 1
    echo "$(($2 + $4 - $1)) $(($3 + $5 / 2))"
}

# Whether the client has recorded line $1 at least $2 times.
x11u_recorded() { [ "$(grep -cx "$1" "$X11U_EVENTS" || true)" -ge "${2:-1}" ]; }

# How often the client has recorded line $1.
x11u_times() { grep -cx "$1" "$X11U_EVENTS" || true; }

# Whether the compositor draws exactly the override-redirect windows
# $x11u_found, each where the X server has it, owned by window $1.
x11u_drawn() {
    x11u_want="[]"
    for x11u_xid in $x11u_found; do
        # shellcheck disable=SC2046 # "X Y W H"
        set -- "$1" $(x11f_geometry "$x11u_xid")
        [ "$#" = 5 ] || return 1
        x11u_want="$(jq -c --argjson xid "$(printf '%d' "$x11u_xid")" --argjson owner "$1" \
            --argjson r "[$2, $3, $4, $5]" '. + [{x11_window_id: $xid, rect: $r, owner: $owner, shown: true}]' <<EOF
$x11u_want
EOF
)"
    done
    jq -e --argjson want "$x11u_want" \
        '(.x11_unmanaged | map({x11_window_id, rect, owner, shown}) | sort_by(.x11_window_id)) == ($want | sort_by(.x11_window_id))' \
        "$CSTATE" >/dev/null 2>&1
}

# Whether the pixel at $2,$3 of PNG $1 is the menu's green (#26a269),
# the hovered item's yellow (#e5a50a) or the tooltip's red (#a51d2d).
x11u_is() {
    x11u_colour="$4"
    # shellcheck disable=SC2046 # three RGB channels
    set -- $(px "$1" "$2" "$3")
    case "$x11u_colour" in
        green) [ "${1:-255}" -le 80 ] && [ "${2:-0}" -ge 130 ] && [ "${3:-255}" -le 150 ] && [ "${3:-0}" -ge 60 ] ;;
        yellow) [ "${1:-0}" -ge 190 ] && [ "${2:-0}" -ge 130 ] && [ "${3:-255}" -le 60 ] ;;
        red) [ "${1:-0}" -ge 130 ] && [ "${2:-255}" -le 70 ] && [ "${3:-255}" -le 90 ] ;;
        *) return 1 ;;
    esac
}

# Take a screenshot to $1 until the pixel at $2,$3 is colour $4.
x11u_shot() {
    x11u_shot_end=$(($(date +%s) + 6))
    while :; do
        scrot -o "$1" >/dev/null
        x11u_is "$1" "$2" "$3" "$4" && return 0
        [ "$(date +%s)" -ge "$x11u_shot_end" ] && return 1
        sleep 0.3
    done
}

# Take a screenshot to $1 until the pixel at $2,$3 is no longer colour $4.
x11u_shot_without() {
    x11u_shot_end=$(($(date +%s) + 6))
    while :; do
        scrot -o "$1" >/dev/null
        x11u_is "$1" "$2" "$3" "$4" || return 0
        [ "$(date +%s)" -ge "$x11u_shot_end" ] && return 1
        sleep 0.3
    done
}

# Open the probe's menu with a click on its menu bar item and wait for
# the client to show it. $1 is the gate that fails otherwise.
x11u_open_menu() {
    x11u_shown="$(x11u_times 'shown menu')"
    # shellcheck disable=SC2046 # "X Y" of the menu bar item
    xdotool mousemove --window "$WID" $(x11u_point probe 10)
    sleep 0.3
    xdotool click 1
    x11f_until 10 x11u_recorded 'shown menu' $((x11u_shown + 1)) ||
        fail "$1" "a click on the menu bar never opened the client's menu"
    x11f_until 10 x11u_count 1 ||
        fail "$1" "the open menu is not one override-redirect window: '$(x11u_windows | tr '\n' ' ')'"
    # The items are placed once the menu is mapped.
    x11f_until 5 x11u_rect bravo >/dev/null || fail "$1" "the client never laid out its menu"
    sleep 0.3
}

# The menu closed: the client hid it, the X server has no window of it
# left and the compositor draws none.
x11u_closed() {
    x11u_recorded 'hidden menu' "$1" && x11u_count 0 &&
        jq -e '.x11_unmanaged == []' "$CSTATE" >/dev/null 2>&1
}

x11_unmanaged_proof() {
command -v xwininfo >/dev/null 2>&1 || fail G-X11-MENU "the X11 menu gates need xwininfo"
"$PY" -c 'import gi; gi.require_version("Gtk", "3.0")' 2>/dev/null ||
    fail G-X11-MENU "the X11 menu gates need GTK 3 for Python (gir1.2-gtk-3.0)"
X11F_DISPLAY="$(jq -r '.x11_display' "$CSTATE")"
X11U_EVENTS="$ARTIFACTS/x11-menu-events.log"
X11U_LAYOUT="$ARTIFACTS/x11-menu-layout.json"
rm -f "$X11U_EVENTS" "$X11U_LAYOUT"

# A native window first, under the probe: one of the clicks outside the
# menu lands on it.
x11u_before="$(jq -c '[.windows[] | .id]' "$CSTATE")"
x11u_socket="$(sed -n 's/.*nested session on \([^ ]*\) .*/\1/p' "$LOG" | head -1)"
WAYLAND_DISPLAY="$x11u_socket" "$PY" "$ROOT/scripts/lib/tuna-test-window.py" "Menuaside" '#613583' \
    >"$ARTIFACTS/window-x11-menu-aside.log" 2>&1 &
x11u_native_pid=$!
cstate "[.windows[] | select(.id as \$i | $x11u_before | index(\$i) == null)] | length == 1" "native window mapped" ||
    fail G-X11-MENU "the native window never mapped"
x11u_native="$(jq -r "[.windows[] | select(.id as \$i | $x11u_before | index(\$i) == null)][0].id" "$CSTATE")"
cstate ".focused == $x11u_native" "native window focused" || fail G-X11-MENU "the native window was not focused"
sleep 1

x11u_before="$(jq -c '[.windows[] | .id]' "$CSTATE")"
DISPLAY="$X11F_DISPLAY" "$PY" "$ROOT/scripts/lib/tuna-x11-menu-window.py" "$X11U_EVENTS" "$X11U_LAYOUT" \
    >"$ARTIFACTS/x11-menu-window.log" 2>&1 &
x11u_pid=$!
x11u_win="$(x11f_new_id "$x11u_before" "menu probe mapped")" ||
    fail G-X11-MENU "the GTK3 menu probe never mapped"
cstate ".focused == $x11u_win and (.windows[] | select(.id == $x11u_win) | .x11_framed)" "menu probe framed and focused" ||
    fail G-X11-MENU "the menu probe was not framed and focused"
x11u_client="$(x11f_win "$x11u_win" .x11_window_id)"
x11f_until 5 x11f_placed "$x11u_client" "$x11u_win" client ||
    fail G-X11-MENU "the probe's window is at '$(x11f_geometry "$x11u_client")', not where the compositor has it"
# The client knows where it is: its widgets' root coordinates are the
# output's.
# shellcheck disable=SC2329 # run by x11f_until
x11u_laid_out() {
    [ "$(x11u_rect window | cut -d' ' -f1,2)" = "$(x11f_geometry "$x11u_client" | cut -d' ' -f1,2)" ] &&
        x11u_rect probe >/dev/null && x11u_rect hint >/dev/null
}
x11f_until 10 x11u_laid_out ||
    fail G-X11-MENU "the client places itself at '$(x11u_rect window || true)', the X server at '$(x11f_geometry "$x11u_client")'"
x11u_windows_before="$(jq -c '[.windows[] | .id]' "$CSTATE")"
x11u_stacking_before="$(jq -c '.stacking' "$CSTATE")"
x11u_focus_before="$(x11f xdotool getwindowfocus)"
jq -e '.x11_unmanaged == []' "$CSTATE" >/dev/null ||
    fail G-X11-MENU "an override-redirect window is drawn before any menu opened: $(jq -c '.x11_unmanaged' "$CSTATE")"
sleep 1
scrot -o "$ARTIFACTS/21-x11-menu-closed.png" >/dev/null

# G-X11-MENU: the menu is one override-redirect window (GTK3 gives it a
# menu type and holds its pointer and keyboard grabs while it is open).
# It is drawn where the X server has it, which is where the client laid
# its items out.
x11u_open_menu G-X11-MENU
x11u_menu="$x11u_found"
x11f_until 5 x11u_drawn "$x11u_win" ||
    fail G-X11-MENU "the compositor does not draw the menu $x11u_menu at '$(x11f_geometry "$x11u_menu")': $(jq -c '.x11_unmanaged' "$CSTATE")"
read -r x11u_mx x11u_my x11u_mw x11u_mh <<EOF
$(x11f_geometry "$x11u_menu")
EOF
# The points whose pixels are read: clear of the items' labels and of
# the pointer's own image when it rests further right on an item.
# shellcheck disable=SC2046 # "X Y" of a point of the item
set -- $(x11u_point alpha 90)
x11u_ax="$1" x11u_ay="$2"
# shellcheck disable=SC2046 # "X Y" of a point of the item
set -- $(x11u_point bravo 90)
x11u_bx="$1" x11u_by="$2"
if [ "$x11u_bx" -lt "$x11u_mx" ] || [ "$x11u_bx" -ge $((x11u_mx + x11u_mw)) ] ||
    [ "$x11u_by" -lt "$x11u_my" ] || [ "$x11u_by" -ge $((x11u_my + x11u_mh)) ]; then
    fail G-X11-MENU "the client's item at $x11u_bx,$x11u_by is outside its menu window $x11u_mx,$x11u_my ${x11u_mw}x$x11u_mh"
fi
x11u_shot "$ARTIFACTS/21-x11-menu-open.png" "$x11u_bx" "$x11u_by" green ||
    fail G-X11-MENU "the open menu is not on screen: pixel $x11u_bx,$x11u_by is '$(px "$ARTIFACTS/21-x11-menu-open.png" "$x11u_bx" "$x11u_by")', not the menu's green"
x11u_is "$ARTIFACTS/21-x11-menu-closed.png" "$x11u_bx" "$x11u_by" green &&
    fail G-X11-MENU "the menu's place was green before the menu opened"
# It is no window of the window manager's: no model entry, no place in
# the stacking, no frame, and the focus stays where it was, in the
# compositor and in the X server.
jq -e --argjson xid "$(printf '%d' "$x11u_menu")" --argjson ids "$x11u_windows_before" --argjson stacking "$x11u_stacking_before" \
    "([.windows[] | .id] == \$ids) and all(.windows[]; .x11_window_id != \$xid) and .stacking == \$stacking and .focused == $x11u_win and .previews == []" "$CSTATE" >/dev/null ||
    fail G-X11-MENU "the menu changed the window list, the stacking or the focus"
[ "$(x11f xdotool getwindowfocus)" = "$x11u_focus_before" ] ||
    fail G-X11-MENU "the X input focus moved from $x11u_focus_before to $(x11f xdotool getwindowfocus) when the menu opened"
[ -z "$(x11f_cardinals "$x11u_menu" _MUTTER_NEEDS_FRAME)" ] ||
    fail G-X11-MENU "the menu was offered to the frames client"
# Hovering an item highlights it: pointer motion reaches the menu.
# shellcheck disable=SC2046 # "X Y" of a point of the item
xdotool mousemove --window "$WID" $(x11u_point alpha 20)
x11u_shot "$ARTIFACTS/21-x11-menu-hover.png" "$x11u_ax" "$x11u_ay" yellow ||
    fail G-X11-MENU "hovering the first item did not highlight it: pixel $x11u_ax,$x11u_ay is '$(px "$ARTIFACTS/21-x11-menu-hover.png" "$x11u_ax" "$x11u_ay")'"
x11u_is "$ARTIFACTS/21-x11-menu-hover.png" "$x11u_bx" "$x11u_by" green ||
    fail G-X11-MENU "the item not hovered lost the menu's colour"
# Clicking it activates it; the client closes its menu and the window
# goes from the screen.
x11u_hidden="$(x11u_times 'hidden menu')"
xdotool click 1
x11f_until 10 x11u_recorded 'activate alpha' ||
    fail G-X11-MENU "a click on the menu's first item never activated it: $(tr '\n' ';' <"$X11U_EVENTS")"
x11f_until 10 x11u_closed $((x11u_hidden + 1)) ||
    fail G-X11-MENU "the menu stayed after its item was activated: X has '$(x11u_windows | tr '\n' ' ')', the compositor $(jq -c '.x11_unmanaged' "$CSTATE")"
x11u_shot_without "$ARTIFACTS/21-x11-menu-activated.png" "$x11u_bx" "$x11u_by" green ||
    fail G-X11-MENU "the closed menu is still on screen"

# A submenu is a second override-redirect window: hovering the item it
# hangs off opens it beside the first, and its own item can be clicked.
x11u_open_menu G-X11-MENU
# shellcheck disable=SC2046 # "X Y" of a point of the item
xdotool mousemove --window "$WID" $(x11u_point more 30)
x11f_until 10 x11u_recorded 'shown submenu' ||
    fail G-X11-MENU "hovering the submenu's item never opened the submenu"
x11f_until 10 x11u_count 2 ||
    fail G-X11-MENU "the open submenu is not a second override-redirect window: '$(x11u_windows | tr '\n' ' ')'"
x11f_until 5 x11u_drawn "$x11u_win" ||
    fail G-X11-MENU "the compositor does not draw the menu and its submenu where the X server has them: $(jq -c '.x11_unmanaged' "$CSTATE")"
x11f_until 5 x11u_rect charlie >/dev/null || fail G-X11-MENU "the client never laid out its submenu"
sleep 0.3
# The pointer goes to the submenu's part past the client's own window:
# there the submenu's surface is the only one of the X server's under it,
# so the highlight and the click prove the pointer is given to the
# override-redirect window itself.
# shellcheck disable=SC2046 # "X Y" of a point of the item
set -- $(x11u_point charlie 20)
jq -e --argjson x "$1" --argjson y "$2" \
    ".windows[] | select(.id == $x11u_win) | .x11_buffer_rect | \$x < .[0] or \$x >= .[0] + .[2] or \$y < .[1] or \$y >= .[1] + .[3]" "$CSTATE" >/dev/null ||
    fail G-X11-MENU "the submenu's item at $1,$2 does not reach past the client's window"
xdotool mousemove --window "$WID" "$1" "$2"
# shellcheck disable=SC2046 # "X Y" of a point of the item
set -- $(x11u_point charlie 90)
x11u_shot "$ARTIFACTS/21-x11-submenu-hover.png" "$1" "$2" yellow ||
    fail G-X11-MENU "hovering the submenu's item did not highlight it: pixel $1,$2 is '$(px "$ARTIFACTS/21-x11-submenu-hover.png" "$1" "$2")'"
x11u_hidden="$(x11u_times 'hidden menu')"
xdotool click 1
x11f_until 10 x11u_recorded 'activate charlie' ||
    fail G-X11-MENU "a click on the submenu's item never activated it: $(tr '\n' ';' <"$X11U_EVENTS")"
x11f_until 10 x11u_closed $((x11u_hidden + 1)) ||
    fail G-X11-MENU "the menus stayed after the submenu's item was activated"

# Clicking outside dismisses the menu without activating anything. On the
# client's own window the X server hands the press to the grabbing
# client, as on any X server.
x11u_activations="$(grep -c '^activate ' "$X11U_EVENTS" || true)"
x11u_open_menu G-X11-MENU
read -r x11u_wx x11u_wy x11u_ww x11u_wh <<EOF
$(x11u_rect window)
EOF
x11u_hidden="$(x11u_times 'hidden menu')"
xdotool mousemove --window "$WID" $((x11u_wx + x11u_ww - 20)) $((x11u_wy + x11u_wh - 20))
sleep 0.3
xdotool click 1
x11f_until 10 x11u_closed $((x11u_hidden + 1)) ||
    fail G-X11-MENU "a click on the client's own window, outside its menu, did not dismiss the menu"
# Where no surface of the X server is under the pointer the X server
# never hears of a press: the compositor hands it over for the client
# holding the pointer grab. On the bare desktop, where the session has
# some clear of every window and its shadow:
x11u_desktop="$(jq -r --argjson w "$WIDTH" --argjson h "$HEIGHT" '
    . as $state
    | [[$w - 40, $h - 40], [40, $h - 40], [$w - 40, 120], [40, 120], [$w / 2, $h - 40]]
    | map(select(. as [$x, $y] | all($state.windows[]; (.x11_buffer_rect // .rect) as $r
        | $x < $r[0] - 48 or $x >= $r[0] + $r[2] + 48 or $y < $r[1] - 48 or $y >= $r[1] + $r[3] + 48)))[0]
    | select(. != null) | map(floor | tostring) | join(" ")' "$CSTATE")"
if [ -n "$x11u_desktop" ]; then
    x11u_open_menu G-X11-MENU
    x11u_hidden="$(x11u_times 'hidden menu')"
    # shellcheck disable=SC2086 # "X Y"
    xdotool mousemove --window "$WID" $x11u_desktop
    sleep 0.3
    xdotool click 1
    x11f_until 10 x11u_closed $((x11u_hidden + 1)) ||
        fail G-X11-MENU "a click on the desktop at $x11u_desktop did not dismiss the menu: $(tr '\n' ';' <"$X11U_EVENTS")"
    x11u_desktop="the desktop, "
fi
# On the native window under the probe: the same, and the press is the
# menu's alone, as under an X11 pointer grab (it neither focuses nor
# reaches the window).
x11u_native_point="$(jq -r --argjson native "$x11u_native" --argjson probe "$x11u_win" '
    (.windows[] | select(.id == $probe) | .x11_buffer_rect) as $p
    | (.windows[] | select(.id == $native) | .rect) as $n
    | [[$n[0] + 30, $n[1] + $n[3] - 120], [$n[0] + $n[2] - 30, $n[1] + $n[3] - 120], [$n[0] + 30, $n[1] + 120], [$n[0] + $n[2] - 30, $n[1] + 120]]
    | map(select(.[0] < $p[0] or .[0] >= $p[0] + $p[2] or .[1] < $p[1] or .[1] >= $p[1] + $p[3]))[0]
    | select(. != null) | map(tostring) | join(" ")' "$CSTATE")"
[ -n "$x11u_native_point" ] || fail G-X11-MENU "the native window has no point clear of the probe"
x11u_open_menu G-X11-MENU
x11u_hidden="$(x11u_times 'hidden menu')"
# shellcheck disable=SC2086 # "X Y"
xdotool mousemove --window "$WID" $x11u_native_point
sleep 0.3
xdotool click 1
x11f_until 10 x11u_closed $((x11u_hidden + 1)) ||
    fail G-X11-MENU "a click on a native window did not dismiss the menu: $(tr '\n' ';' <"$X11U_EVENTS")"
sleep 0.5
jq -e ".focused == $x11u_win" "$CSTATE" >/dev/null ||
    fail G-X11-MENU "the press that dismissed the menu also focused the native window"
[ "$(grep -c '^activate ' "$X11U_EVENTS" || true)" = "$x11u_activations" ] ||
    fail G-X11-MENU "a click outside the menu activated an item: $(tr '\n' ';' <"$X11U_EVENTS")"
# With no menu open the same click is the native window's.
xdotool click 1
cstate ".focused == $x11u_native" "native window focused by a click" 10 ||
    fail G-X11-MENU "after the menu closed a click on the native window did not focus it"
kill "$x11u_native_pid" 2>/dev/null || true
cstate "all(.windows[]; .id != $x11u_native) and .focused == $x11u_win" "native window closed" 10 ||
    fail G-X11-MENU "the native window stayed, or the probe did not take the focus back"
pass G-X11-MENU "a GTK3 X11 menu and its submenu are drawn where the client put them, highlight under the pointer, activate on click and close; a click outside (the client's window, ${x11u_desktop}a native window) dismisses the menu; the window list, stacking and focus never change"
x11u_windows_before="$(jq -c '[.windows[] | .id]' "$CSTATE")"
x11u_stacking_before="$(jq -c '.stacking' "$CSTATE")"
# shellcheck disable=SC2329 # run by x11f_until
x11u_focus_back() { [ "$(x11f xdotool getwindowfocus)" = "$x11u_focus_before" ]; }
x11f_until 5 x11u_focus_back ||
    fail G-X11-TOOLTIP "the X input focus did not come back to the probe: $(x11f xdotool getwindowfocus), not $x11u_focus_before"

# G-X11-TOOLTIP: resting the pointer on the button shows GTK3's tooltip,
# an override-redirect window of type _NET_WM_WINDOW_TYPE_TOOLTIP, next
# to the pointer. It never takes the focus and goes when the pointer
# leaves the button.
sleep 0.5
read -r x11u_hx x11u_hy x11u_hw x11u_hh <<EOF
$(x11u_rect hint)
EOF
x11u_tip_x=$((x11u_hx + x11u_hw / 2)) x11u_tip_y=$((x11u_hy + x11u_hh / 2))
xdotool mousemove --window "$WID" $((x11u_tip_x - 6)) "$x11u_tip_y"
sleep 0.2
xdotool mousemove --window "$WID" "$x11u_tip_x" "$x11u_tip_y"
x11f_until 10 x11u_of_type TOOLTIP ||
    fail G-X11-TOOLTIP "resting the pointer on the button never mapped a tooltip window"
x11u_tip="$x11u_typed"
x11f_until 5 x11u_count 1 || fail G-X11-TOOLTIP "the tooltip is not the client's only override-redirect window"
x11f_until 5 x11u_drawn "$x11u_win" ||
    fail G-X11-TOOLTIP "the compositor does not draw the tooltip $x11u_tip at '$(x11f_geometry "$x11u_tip")': $(jq -c '.x11_unmanaged' "$CSTATE")"
read -r x11u_tx x11u_ty x11u_tw x11u_th <<EOF
$(x11f_geometry "$x11u_tip")
EOF
# Near the pointer: within 64 px of it on each axis.
if [ "$x11u_tip_x" -lt $((x11u_tx - 64)) ] || [ "$x11u_tip_x" -ge $((x11u_tx + x11u_tw + 64)) ] ||
    [ "$x11u_tip_y" -lt $((x11u_ty - 64)) ] || [ "$x11u_tip_y" -ge $((x11u_ty + x11u_th + 64)) ]; then
    fail G-X11-TOOLTIP "the tooltip at $x11u_tx,$x11u_ty ${x11u_tw}x$x11u_th is not near the pointer at $x11u_tip_x,$x11u_tip_y"
fi
# Its pixels, read at its left end: the pointer's own image is drawn
# over its middle.
x11u_sx=$((x11u_tx + 8)) x11u_sy=$((x11u_ty + x11u_th / 2))
x11u_shot "$ARTIFACTS/21-x11-tooltip.png" "$x11u_sx" "$x11u_sy" red ||
    fail G-X11-TOOLTIP "the tooltip is not on screen: pixel $x11u_sx,$x11u_sy is '$(px "$ARTIFACTS/21-x11-tooltip.png" "$x11u_sx" "$x11u_sy")', not the tooltip's red"
jq -e --argjson xid "$(printf '%d' "$x11u_tip")" --argjson ids "$x11u_windows_before" --argjson stacking "$x11u_stacking_before" \
    "([.windows[] | .id] == \$ids) and all(.windows[]; .x11_window_id != \$xid) and .stacking == \$stacking and .focused == $x11u_win and .previews == []" "$CSTATE" >/dev/null ||
    fail G-X11-TOOLTIP "the tooltip changed the window list, the stacking or the focus"
[ "$(x11f xdotool getwindowfocus)" = "$x11u_focus_before" ] ||
    fail G-X11-TOOLTIP "the X input focus moved from $x11u_focus_before to $(x11f xdotool getwindowfocus) when the tooltip showed"
[ -z "$(x11f_cardinals "$x11u_tip" _MUTTER_NEEDS_FRAME)" ] ||
    fail G-X11-TOOLTIP "the tooltip was offered to the frames client"
# The pointer leaves the button, staying on the client's window.
xdotool mousemove --window "$WID" $((x11u_wx + 20)) $((x11u_wy + x11u_wh - 20))
# shellcheck disable=SC2329 # run by x11f_until
x11u_tooltip_gone() { x11u_count 0 && jq -e '.x11_unmanaged == []' "$CSTATE" >/dev/null 2>&1; }
x11f_until 10 x11u_tooltip_gone ||
    fail G-X11-TOOLTIP "the tooltip stayed after the pointer left the button: X has '$(x11u_windows | tr '\n' ' ')', the compositor $(jq -c '.x11_unmanaged' "$CSTATE")"
x11u_shot_without "$ARTIFACTS/21-x11-tooltip-gone.png" "$x11u_sx" "$x11u_sy" red ||
    fail G-X11-TOOLTIP "the hidden tooltip is still on screen"
pass G-X11-TOOLTIP "a GTK3 X11 tooltip is drawn next to the pointer where the client put it, takes no focus and no place in the window list, and goes when the pointer leaves"

kill "$x11u_pid" 2>/dev/null || true
cstate "all(.windows[]; .id != $x11u_win) and .x11_unmanaged == []" "menu probe closed" 10 ||
    fail G-X11-MENU "the menu probe stayed after its client exited"
}
