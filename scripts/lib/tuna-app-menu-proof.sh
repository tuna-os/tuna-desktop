#!/bin/sh
# Sourced by tuna-gtk-shell-proof for its focused --app-menu-only run
# (#1182 and #1183, parity rows P-OV-03 and P-OV-05). Requires that
# harness's private buses, GTK shell and helpers (pass, fail, cstate, $PY,
# $ROOT, $ARTIFACTS, $CSTATE, $WID, $SOCKET_NAME, $XDG_DATA_HOME).
#
# Every step is real input through the nested window (pointer clicks,
# presses and drags, Shift+F10 and Menu) or assistive technology (a menu
# row pressed through its AT-SPI action, as a screen reader user does);
# the dash and the menus are read from the shell's accessible tree.
#
#   G-APP-MENU      an app icon's menu in the dash and the grid: GNOME's
#                   rows in GNOME's order, from a right click, a long
#                   press and the keyboard; Pin to Dash and Unpin write
#                   favorite-apps; New Window opens a window, a desktop
#                   action runs, an open window raises, Quit closes the
#                   app, App Details asks GNOME Software, and the launch
#                   on the discrete GPU follows switcheroo-control
#   G-DASH-PIN-DND  an app dragged from the grid pins at the drop
#                   position, one dragged within the dash reorders, and
#                   one dropped on Show Apps (which reads "Unpin")
#                   unpins; a drop anywhere else changes nothing
#   G-FAVORITES-LIVE favorite-apps written by gsettings reaches the dash
#                   without a restart, open or closed
#   G-FAVORITES-IMPORT the pins an older Tuna Desktop kept in
#                   ~/.local/share/tuna-shell/favorites.json are imported
#                   once into an unset favorite-apps when the shell starts

AM_APP=tuna-test-menu

# The shell's accessibility tree into $ARTIFACTS/$1.json.
am_dump() {
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/$1.json" 20 >/dev/null ||
        fail "$AM_GATE" "accessibility dump failed ($1)"
}

# A showing menu item named $2 in dump $1.
am_has_item() {
    jq -e --arg n "$2" 'any(.[]; .role == "menu item" and .name == $n and .showing)' "$1" >/dev/null
}

# The showing menu items of dump $1, in order, comma-separated.
am_items() {
    jq -r '[.[] | select(.role == "menu item" and .showing)] | map(.name) | join(",")' "$1"
}

# Dump into $1 until a menu item named $2 shows.
am_wait_item() {
    am_try=0
    while [ "$am_try" -lt 12 ]; do
        am_dump "$1"
        am_has_item "$ARTIFACTS/$1.json" "$2" && return 0
        am_try=$((am_try + 1))
        sleep 0.5
    done
    return 1
}

# Press the control named $2 through its accessibility action, dump to $1.
am_press() {
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/$1.json" 20 --press "$2" >/dev/null ||
        fail "$AM_GATE" "could not press $2"
}

# "x y" of the layer surface $1's top-left corner.
am_origin() {
    jq -r --arg ns "$1" '[.layers[] | select(.namespace == $ns)][0] | "\(.rect[0]) \(.rect[1])"' "$CSTATE"
}

# The output position of the button named $3 in the window titled $2 of
# dump $1 (layer surface $4), at fraction $5 (default the middle) of its width.
am_pt() {
    am_o="$(am_origin "$4")"
    jq -er --arg frame "$2" --arg name "$3" --arg o "$am_o" --argjson fx "${5:-0.5}" '
        ($o | split(" ") | map(tonumber)) as [$ox, $oy]
        | [foreach .[] as $n (null; if $n.role == "frame" then $n.name else . end; [., $n])]
        | map(select(.[0] == $frame) | .[1]
            | select(.showing and (.role == "push button" or .role == "button" or .role == "toggle button")
                and .name == $name and .bounds[2] > 0 and .bounds[3] > 0))
        | first | .bounds
        | "\((.[0] + $ox + .[2] * $fx) | floor) \((.[1] + $oy + .[3] / 2) | floor)"' "$1"
}

am_dash_pt() { am_pt "$1" Dash "$2" tuna-shell-dash "${3:-0.5}"; }
am_grid_pt() { am_pt "$1" "App Grid" "$2" tuna-shell-appgrid "${3:-0.5}"; }

# The dash's items of dump $1, left to right, comma-separated (Show Apps
# aside).
am_dash_names() {
    jq -r '[foreach .[] as $n (null; if $n.role == "frame" then $n.name else . end; [., $n])]
        | map(select(.[0] == "Dash") | .[1]
            | select(.showing and (.role == "push button" or .role == "button") and .bounds[2] > 0))
        | sort_by(.bounds[0]) | map(.name) | join(",")' "$1"
}

# The name of the button holding the keyboard focus in window $2 of dump $1.
am_focus_in() {
    jq -r --arg frame "$2" '[foreach .[] as $n (null; if $n.role == "frame" then $n.name else . end; [., $n])]
        | map(select(.[0] == $frame) | .[1]
            | select(.focused and (.role == "push button" or .role == "button" or .role == "toggle button")))
        | first | .name // empty' "$1"
}

am_favorites() { gsettings get org.gnome.shell favorite-apps; }

# Wait for favorite-apps to read $1 exactly.
am_wait_favorites() {
    am_try=0
    while [ "$am_try" -lt 20 ]; do
        [ "$(am_favorites)" = "$1" ] && return 0
        am_try=$((am_try + 1))
        sleep 0.4
    done
    return 1
}

am_overview_open() {
    xdotool key Super_L
    cstate '.overview_open and .overview_progress == 1' "overview" ||
        fail "$AM_GATE" "Super did not open the overview"
    sleep 1
}

am_grid_open() {
    xdotool key super+a
    cstate '.overview_open and .overview_app_grid' "app grid" ||
        fail "$AM_GATE" "Super+A did not show the app grid"
    sleep 1.5
}

am_overview_close() {
    cstate '.overview_open' "overview still open" 2 || return 0
    xdotool key Escape
    cstate '.overview_open | not' "closed" 3 ||
        { xdotool key Escape; cstate '.overview_open | not' "closed" || fail "$AM_GATE" "the overview stayed open"; }
    sleep 0.5
}

# Right-click the point in $1 ("x y" output coordinates).
am_right_click() {
    read -r am_click_x am_click_y <<EOF
$1
EOF
    xdotool mousemove --window "$WID" "$am_click_x" "$am_click_y"
    sleep 0.4
    xdotool click 3
}

# The ids of $AM_APP's windows, lowest first.
am_app_windows() {
    jq -r --arg a "$AM_APP" '[.windows[] | select(.app_id == $a) | .id] | sort | .[]' "$CSTATE"
}

# Start one of the probe app's windows (title $1) as a process of its own.
am_window() {
    WAYLAND_DISPLAY="$SOCKET_NAME" TUNA_TEST_PRGNAME="$AM_APP" "$PY" "$ROOT/scripts/lib/tuna-test-window.py" \
        "$1" "#613583" >>"$ARTIFACTS/am-windows.log" 2>&1 &
    AM_PIDS="$AM_PIDS $!"
}

# Close every window the probes opened and wait for them to go.
am_clear_windows() {
    pkill -f tuna-test-window.py 2>/dev/null || true
    cstate '[.windows[] | select((.app_id // "") | startswith("tuna-test-"))] | length == 0' "no probe windows" ||
        fail "$AM_GATE" "the probe windows never closed"
}

am_fixtures() {
    mkdir -p "$XDG_DATA_HOME/applications"
    am_dir="$XDG_DATA_HOME/applications"
    # A multi-window app with a desktop action: every launch opens a
    # window titled by its process (so each window's row has its own name).
    am_exec="sh -c 'exec env TUNA_TEST_PRGNAME=$AM_APP $PY $ROOT/scripts/lib/tuna-test-window.py \"Menu \$\$\" \"#613583\"'"
    cat >"$am_dir/$AM_APP.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=AAA Menu Probe
Exec=$am_exec
Actions=private;

[Desktop Action private]
Name=New Private Window
Exec=touch $ARTIFACTS/am-private
EOF
    # A single-window app: GNOME offers no New Window while it runs.
    cat >"$am_dir/tuna-test-solo.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=AAA Menu Solo
SingleMainWindow=true
Exec=sh -c 'exec env TUNA_TEST_PRGNAME=tuna-test-solo $PY $ROOT/scripts/lib/tuna-test-window.py "Solo \$\$" "#26a269"'
EOF
    # A stopped app that records the GPU environment it was launched with.
    cat >"$am_dir/tuna-menu-gpu.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=AAA Menu Gpu
Exec=sh -c 'echo "\$DRI_PRIME \$TUNA_PROOF_GPU" >$ARTIFACTS/am-gpu'
EOF
    sleep 1.5
}

app_menu_proof() {
    AM_GATE=G-APP-MENU
    AM_PIDS=""
    am_fixtures
    gsettings set org.gnome.shell favorite-apps "['org.tuna.ProofNoFavorite.desktop']"

    # --- a running app's icon in the dash, opened by a right click ---
    am_window "Menu 1"
    cstate "[.windows[] | select(.app_id == \"$AM_APP\")] | length == 1" "probe window" ||
        fail G-APP-MENU "the probe app's window never mapped"
    am_overview_open
    am_dump am-dash-closed
    am_point="$(am_dash_pt "$ARTIFACTS/am-dash-closed.json" "AAA Menu Probe")" ||
        fail G-APP-MENU "the running app has no dash icon"
    am_right_click "$am_point"
    am_wait_item am-menu-dash Quit ||
        fail G-APP-MENU "a right click on the dash icon opened no menu"
    scrot "$ARTIFACTS/am-menu-dash.png" >/dev/null
    jq -e 'any(.[]; .role == "label" and .name == "Open Windows" and .showing)' "$ARTIFACTS/am-menu-dash.json" >/dev/null ||
        fail G-APP-MENU "the menu has no Open Windows heading"
    [ "$(am_items "$ARTIFACTS/am-menu-dash.json")" = "Menu 1,New Window,New Private Window,Pin to Dash,Quit" ] ||
        fail G-APP-MENU "a running unpinned app's rows are '$(am_items "$ARTIFACTS/am-menu-dash.json")', not GNOME's (its window, New Window, its desktop action, Pin to Dash, Quit; no App Details without GNOME Software, no GPU row for a running app)"
    # Escape closes the menu alone.
    xdotool key Escape
    sleep 1
    am_dump am-menu-escaped
    [ -z "$(am_items "$ARTIFACTS/am-menu-escaped.json")" ] ||
        fail G-APP-MENU "Escape left the menu open"
    cstate '.overview_open' "overview" ||
        fail G-APP-MENU "Escape closed the overview with the menu instead of the menu alone"

    # --- New Window ---
    am_right_click "$am_point"
    am_wait_item am-menu-new "New Window" || fail G-APP-MENU "the menu did not open again"
    am_press am-pressed-new "New Window"
    cstate "([.windows[] | select(.app_id == \"$AM_APP\")] | length) == 2 and (.overview_open | not)" "second window" 20 ||
        fail G-APP-MENU "New Window opened no second window of the app, or left the overview open"

    # --- its two windows, and raising one ---
    am_first="$(am_app_windows | head -1)"
    am_overview_open
    am_dump am-dash-two
    am_point="$(am_dash_pt "$ARTIFACTS/am-dash-two.json" "AAA Menu Probe")" ||
        fail G-APP-MENU "the running app lost its dash icon"
    am_right_click "$am_point"
    am_wait_item am-menu-two Quit || fail G-APP-MENU "the menu did not open (two windows)"
    am_windows_listed="$(jq -r '[.[] | select(.role == "menu item" and .showing and (.name | startswith("Menu ")))] | length' "$ARTIFACTS/am-menu-two.json")"
    [ "$am_windows_listed" = 2 ] ||
        fail G-APP-MENU "the menu lists $am_windows_listed of the app's 2 windows"
    am_press am-pressed-raise "Menu 1"
    cstate ".focused == $am_first and (.overview_open | not)" "window raised" 15 ||
        fail G-APP-MENU "choosing the first window's row did not raise it and leave the overview"

    # --- the app's own desktop action ---
    rm -f "$ARTIFACTS/am-private"
    am_overview_open
    am_right_click "$am_point"
    am_wait_item am-menu-action "New Private Window" || fail G-APP-MENU "the menu did not open (desktop action)"
    am_press am-pressed-action "New Private Window"
    am_try=0
    until [ -e "$ARTIFACTS/am-private" ] || [ "$am_try" -gt 25 ]; do am_try=$((am_try + 1)); sleep 0.4; done
    [ -e "$ARTIFACTS/am-private" ] || fail G-APP-MENU "the New Private Window action did not run"

    # --- Pin to Dash, then Unpin, each written to favorite-apps ---
    am_overview_open
    am_right_click "$am_point"
    am_wait_item am-menu-pin "Pin to Dash" || fail G-APP-MENU "the menu did not open (pin)"
    am_press am-pressed-pin "Pin to Dash"
    am_wait_favorites "['$AM_APP.desktop']" ||
        fail G-APP-MENU "Pin to Dash left favorite-apps as $(am_favorites), not only the pinned app"
    jq -e 'any(.[]; (.name // "") | test("AAA Menu Probe Pinned to Dash"))' "$ARTIFACTS/am-pressed-pin.json" >/dev/null ||
        fail G-APP-MENU "pinning posted no \"Pinned to Dash\" notification"
    cstate '.overview_open' "overview" ||
        fail G-APP-MENU "Pin to Dash closed the overview"
    sleep 1
    am_dump am-dash-pinned
    am_point="$(am_dash_pt "$ARTIFACTS/am-dash-pinned.json" "AAA Menu Probe")" ||
        fail G-APP-MENU "the pinned app has no dash icon"
    am_right_click "$am_point"
    am_wait_item am-menu-unpin "Unpin" || fail G-APP-MENU "a pinned app's menu offers no Unpin"
    am_has_item "$ARTIFACTS/am-menu-unpin.json" "Pin to Dash" &&
        fail G-APP-MENU "a pinned app's menu still offers Pin to Dash"
    am_press am-pressed-unpin "Unpin"
    am_wait_favorites "@as []" ||
        fail G-APP-MENU "Unpin left favorite-apps as $(am_favorites)"

    # --- App Details asks GNOME Software; its row follows its install ---
    cat >"$XDG_DATA_HOME/applications/org.gnome.Software.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=AAA Menu Software
Exec=true
NoDisplay=true
EOF
    rm -f "$ARTIFACTS/am-software"
    "$PY" "$ROOT/scripts/lib/tuna-software-stub.py" "$ARTIFACTS/am-software" >"$ARTIFACTS/am-software-stub.log" 2>&1 &
    AM_PIDS="$AM_PIDS $!"
    am_try=0
    until grep -q ready "$ARTIFACTS/am-software-stub.log" 2>/dev/null || [ "$am_try" -gt 25 ]; do am_try=$((am_try + 1)); sleep 0.4; done
    grep -q ready "$ARTIFACTS/am-software-stub.log" || fail G-APP-MENU "the GNOME Software stub did not start"
    sleep 1.5
    am_right_click "$am_point"
    am_wait_item am-menu-details "App Details" || fail G-APP-MENU "installing GNOME Software added no App Details row"
    am_second="$(am_items "$ARTIFACTS/am-menu-details.json" | cut -d, -f2)"
    [ "$(am_items "$ARTIFACTS/am-menu-details.json")" = "Menu 1,$am_second,New Window,New Private Window,Pin to Dash,App Details,Quit" ] ||
        fail G-APP-MENU "with GNOME Software the rows are '$(am_items "$ARTIFACTS/am-menu-details.json")'"
    am_press am-pressed-details "App Details"
    am_try=0
    until [ -s "$ARTIFACTS/am-software" ] || [ "$am_try" -gt 25 ]; do am_try=$((am_try + 1)); sleep 0.4; done
    grep -q "^details .*$AM_APP.desktop" "$ARTIFACTS/am-software" ||
        fail G-APP-MENU "App Details did not ask GNOME Software for the app's details (got: $(cat "$ARTIFACTS/am-software" 2>/dev/null))"
    cstate '.overview_open | not' "overview closed" ||
        fail G-APP-MENU "App Details left the overview open"

    # --- Quit closes every window of the app ---
    am_overview_open
    am_right_click "$am_point"
    am_wait_item am-menu-quit Quit || fail G-APP-MENU "the menu did not open (quit)"
    am_press am-pressed-quit Quit
    cstate "[.windows[] | select(.app_id == \"$AM_APP\")] | length == 0" "app closed" 20 ||
        fail G-APP-MENU "Quit left $(jq "[.windows[] | select(.app_id == \"$AM_APP\")] | length" "$CSTATE") of the app's windows open"
    am_overview_close

    # --- a single-window app has no New Window while it runs ---
    WAYLAND_DISPLAY="$SOCKET_NAME" TUNA_TEST_PRGNAME=tuna-test-solo "$PY" "$ROOT/scripts/lib/tuna-test-window.py" "Solo 1" "#26a269" >>"$ARTIFACTS/am-windows.log" 2>&1 &
    AM_PIDS="$AM_PIDS $!"
    cstate '[.windows[] | select(.app_id == "tuna-test-solo")] | length == 1' "solo window" ||
        fail G-APP-MENU "the single-window app never mapped"
    am_overview_open
    am_dump am-dash-solo
    am_point="$(am_dash_pt "$ARTIFACTS/am-dash-solo.json" "AAA Menu Solo")" ||
        fail G-APP-MENU "the single-window app has no dash icon"
    am_right_click "$am_point"
    am_wait_item am-menu-solo Quit || fail G-APP-MENU "the menu did not open (single window)"
    [ "$(am_items "$ARTIFACTS/am-menu-solo.json")" = "Solo 1,Pin to Dash,App Details,Quit" ] ||
        fail G-APP-MENU "a running single-window app's rows are '$(am_items "$ARTIFACTS/am-menu-solo.json")', not its window, Pin to Dash, App Details and Quit"
    xdotool key Escape
    sleep 0.8
    am_overview_close
    pkill -f tuna-test-window.py 2>/dev/null || true
    am_clear_windows

    # --- the grid: a stopped app, the discrete GPU, long press, keyboard ---
    rm -f "$ARTIFACTS/am-gpu"
    am_grid_open
    am_dump am-grid
    am_point="$(am_grid_pt "$ARTIFACTS/am-grid.json" "AAA Menu Gpu")" ||
        fail G-APP-MENU "the grid has no tile for the stopped probe app"
    am_right_click "$am_point"
    am_wait_item am-menu-grid "New Window" || fail G-APP-MENU "a right click on a grid tile opened no menu"
    scrot "$ARTIFACTS/am-menu-grid.png" >/dev/null
    [ "$(am_items "$ARTIFACTS/am-menu-grid.json")" = "New Window,Pin to Dash,App Details" ] ||
        fail G-APP-MENU "a stopped app's grid menu rows are '$(am_items "$ARTIFACTS/am-menu-grid.json")', not New Window, Pin to Dash and App Details (no windows, no Quit, no GPU row with one GPU)"
    xdotool key Escape
    sleep 0.8
    # switcheroo-control appears with a second GPU: the row follows it.
    "$PY" "$ROOT/scripts/lib/tuna-switcheroo-stub.py" >"$ARTIFACTS/am-switcheroo-stub.log" 2>&1 &
    am_switcheroo=$!
    AM_PIDS="$AM_PIDS $am_switcheroo"
    am_try=0
    until grep -q ready "$ARTIFACTS/am-switcheroo-stub.log" 2>/dev/null || [ "$am_try" -gt 25 ]; do am_try=$((am_try + 1)); sleep 0.4; done
    grep -q ready "$ARTIFACTS/am-switcheroo-stub.log" || fail G-APP-MENU "the switcheroo-control stub did not start"
    sleep 1.5
    am_right_click "$am_point"
    am_wait_item am-menu-gpu "Launch Using Discrete Graphics Card" ||
        fail G-APP-MENU "a second GPU added no Launch Using Discrete Graphics Card row"
    [ "$(am_items "$ARTIFACTS/am-menu-gpu.json")" = "New Window,Launch Using Discrete Graphics Card,Pin to Dash,App Details" ] ||
        fail G-APP-MENU "with two GPUs the rows are '$(am_items "$ARTIFACTS/am-menu-gpu.json")'"
    scrot "$ARTIFACTS/am-menu-gpu.png" >/dev/null
    am_press am-pressed-gpu "Launch Using Discrete Graphics Card"
    am_try=0
    until [ -s "$ARTIFACTS/am-gpu" ] || [ "$am_try" -gt 25 ]; do am_try=$((am_try + 1)); sleep 0.4; done
    [ "$(cat "$ARTIFACTS/am-gpu" 2>/dev/null)" = "1 discrete" ] ||
        fail G-APP-MENU "the discrete launch ran with the environment '$(cat "$ARTIFACTS/am-gpu" 2>/dev/null)', not switcheroo-control's DRI_PRIME=1 and TUNA_PROOF_GPU=discrete"
    # The service leaves: the row goes.
    kill "$am_switcheroo"
    sleep 1.5
    am_grid_open
    am_dump am-grid-nogpu
    am_point="$(am_grid_pt "$ARTIFACTS/am-grid-nogpu.json" "AAA Menu Gpu")" ||
        fail G-APP-MENU "the grid lost the probe tile"
    am_right_click "$am_point"
    am_wait_item am-menu-nogpu "New Window" || fail G-APP-MENU "the menu did not open (service gone)"
    am_has_item "$ARTIFACTS/am-menu-nogpu.json" "Launch Using Discrete Graphics Card" &&
        fail G-APP-MENU "the discrete GPU row stayed after switcheroo-control left"
    xdotool key Escape
    sleep 0.8

    # Long press: the menu opens and the press launches nothing.
    rm -f "$ARTIFACTS/am-gpu"
    read -r am_hold_x am_hold_y <<EOF
$am_point
EOF
    xdotool mousemove --window "$WID" "$am_hold_x" "$am_hold_y"
    sleep 0.4
    xdotool mousedown 1
    sleep 1.6
    am_dump am-menu-long
    am_has_item "$ARTIFACTS/am-menu-long.json" "New Window" ||
        fail G-APP-MENU "a long press on a grid tile opened no menu"
    xdotool mouseup 1
    sleep 1
    am_dump am-menu-long-held
    am_has_item "$ARTIFACTS/am-menu-long-held.json" "New Window" ||
        fail G-APP-MENU "releasing a long press closed the menu"
    [ ! -e "$ARTIFACTS/am-gpu" ] ||
        fail G-APP-MENU "the long press also activated (launched) the app"
    cstate '.overview_open' "overview" ||
        fail G-APP-MENU "the long press closed the overview"
    xdotool key Escape
    sleep 0.8

    # Keyboard on the grid: Down onto a tile, Shift+F10, the first row has
    # the focus, Down moves it, Escape returns to the tile.
    xdotool key Down
    sleep 0.6
    xdotool key shift+F10
    am_wait_item am-menu-kbd "New Window" || fail G-APP-MENU "Shift+F10 on a focused grid tile opened no menu"
    am_first_focus="$(jq -r '[.[] | select(.role == "menu item" and .showing and .focused)] | first | .name // empty' "$ARTIFACTS/am-menu-kbd.json")"
    [ -n "$am_first_focus" ] || fail G-APP-MENU "no row took the focus when the menu opened from the keyboard"
    xdotool key Down
    sleep 0.5
    am_dump am-menu-kbd-down
    am_next_focus="$(jq -r '[.[] | select(.role == "menu item" and .showing and .focused)] | first | .name // empty' "$ARTIFACTS/am-menu-kbd-down.json")"
    { [ -n "$am_next_focus" ] && [ "$am_next_focus" != "$am_first_focus" ]; } ||
        fail G-APP-MENU "Down did not move the focus from '$am_first_focus' ('$am_next_focus')"
    xdotool key Escape
    sleep 0.8
    am_dump am-menu-kbd-closed
    [ -z "$(am_items "$ARTIFACTS/am-menu-kbd-closed.json")" ] || fail G-APP-MENU "Escape left the keyboard menu open"
    [ -n "$(am_focus_in "$ARTIFACTS/am-menu-kbd-closed.json" "App Grid")" ] ||
        fail G-APP-MENU "the grid tile lost the focus when its menu closed"
    cstate '.overview_open' "overview" || fail G-APP-MENU "Escape closed the overview with the keyboard menu instead of the menu alone"
    xdotool key Menu
    am_wait_item am-menu-kbd2 "New Window" || fail G-APP-MENU "the Menu key on a focused grid tile opened no menu"
    xdotool key Escape
    sleep 0.8

    # Keyboard on the dash: Down past the grid's last row reaches it, and
    # Menu opens an icon's menu there. (A running app has a dash icon.)
    am_window "Menu 2"
    cstate "[.windows[] | select(.app_id == \"$AM_APP\")] | length == 1" "probe window" ||
        fail G-APP-MENU "the probe app's window never mapped"
    am_overview_close
    am_grid_open
    xdotool key Down
    am_dash_focus=""
    for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14; do
        xdotool key Down
        sleep 0.5
        am_dump am-kbd-dash
        am_dash_focus="$(am_focus_in "$ARTIFACTS/am-kbd-dash.json" Dash)"
        [ -n "$am_dash_focus" ] && break
    done
    [ -n "$am_dash_focus" ] || fail G-APP-MENU "Down never moved the keyboard from the grid on to the dash"
    # A running icon first; walk right until the menu has something to open.
    xdotool key Menu
    sleep 1
    am_dump am-menu-dash-kbd
    [ -n "$(am_items "$ARTIFACTS/am-menu-dash-kbd.json")" ] ||
        fail G-APP-MENU "Menu on the focused dash icon '$am_dash_focus' opened no menu"
    scrot "$ARTIFACTS/am-menu-dash-kbd.png" >/dev/null
    xdotool key Escape
    sleep 0.8
    am_overview_close
    am_clear_windows
    rm -f "$XDG_DATA_HOME/applications/org.gnome.Software.desktop"
    pass G-APP-MENU "a right click, a long press and Shift+F10 or Menu on a dash or grid icon open GNOME's app menu: its windows, New Window, desktop actions, the discrete-GPU launch (only while switcheroo-control reports two), Pin to Dash and Unpin written to favorite-apps, App Details (only with GNOME Software) and Quit; every row works"
}

# Dash drag and drop: pins at the drop position, reorders, unpins.
am_fixtures_pin() {
    for am_n in One Two Three; do
        printf '[Desktop Entry]\nType=Application\nName=AAA Pin %s\nExec=true\n' "$am_n" \
            >"$XDG_DATA_HOME/applications/tuna-pin-$am_n.desktop"
    done
    sleep 1.5
}

# Before the shell starts, with favorite-apps left unset: the pins an
# older Tuna Desktop kept in its own file (one twice, one without
# ".desktop", one not installed).
am_seed_legacy_favorites() {
    mkdir -p "$XDG_DATA_HOME/applications" "$XDG_DATA_HOME/tuna-shell"
    am_fixtures_pin
    printf '%s\n' '["tuna-pin-Three", "org.example.Gone.desktop", "tuna-pin-One.desktop", "tuna-pin-Three.desktop"]' \
        >"$XDG_DATA_HOME/tuna-shell/favorites.json"
    cp "$XDG_DATA_HOME/tuna-shell/favorites.json" "$ARTIFACTS/am-legacy-favorites.json"
}

favorites_import_proof() {
    AM_GATE=G-FAVORITES-IMPORT
    am_wait_favorites "['tuna-pin-Three.desktop', 'org.example.Gone.desktop', 'tuna-pin-One.desktop']" ||
        fail G-FAVORITES-IMPORT "the legacy pins were not imported: favorite-apps reads $(am_favorites)"
    [ -e "$XDG_DATA_HOME/tuna-shell/favorite-apps-imported" ] ||
        fail G-FAVORITES-IMPORT "the import left no stamp, so it would run again"
    cmp -s "$XDG_DATA_HOME/tuna-shell/favorites.json" "$ARTIFACTS/am-legacy-favorites.json" ||
        fail G-FAVORITES-IMPORT "the import changed the legacy shell's favorites.json"
    am_overview_open
    am_dump am-import
    [ "$(am_dash_names "$ARTIFACTS/am-import.json")" = "AAA Pin Three,AAA Pin One" ] ||
        fail G-FAVORITES-IMPORT "the dash shows '$(am_dash_names "$ARTIFACTS/am-import.json")', not the imported pins"
    am_overview_close
    pass G-FAVORITES-IMPORT "the pins an older Tuna Desktop kept in tuna-shell/favorites.json reach favorite-apps once (desktop ids, in order, each once) and the dash when the shell starts with the key unset; the file stays for the legacy shell"
}

# Press at the point in $1 ("x y"), drag to the point in $2, and hold
# with the button down.
am_drag_hold() {
    read -r am_from_x am_from_y <<EOF
$1
EOF
    read -r am_to_x am_to_y <<EOF
$2
EOF
    xdotool mousemove --window "$WID" "$am_from_x" "$am_from_y"
    sleep 0.4
    xdotool mousedown 1
    sleep 0.3
    am_dx=$(((am_to_x - am_from_x) / 10))
    am_dy=$(((am_to_y - am_from_y) / 10))
    for am_s in 1 2 3 4 5 6 7 8 9 10; do
        xdotool mousemove --window "$WID" "$((am_from_x + am_dx * am_s))" "$((am_from_y + am_dy * am_s))"
        sleep 0.08
    done
    xdotool mousemove --window "$WID" "$am_to_x" "$am_to_y"
    sleep 0.7
}

am_drag_drop() {
    xdotool mouseup 1
    sleep 1.5
}

dash_pin_dnd_proof() {
    AM_GATE=G-DASH-PIN-DND
    am_fixtures_pin
    gsettings set org.gnome.shell favorite-apps "['tuna-pin-One.desktop', 'tuna-pin-Three.desktop']"
    sleep 1
    am_grid_open
    am_dump am-dnd-start
    [ "$(am_dash_names "$ARTIFACTS/am-dnd-start.json")" = "AAA Pin One,AAA Pin Three" ] ||
        fail G-DASH-PIN-DND "the dash starts with '$(am_dash_names "$ARTIFACTS/am-dnd-start.json")', not its two favorites"
    am_src="$(am_grid_pt "$ARTIFACTS/am-dnd-start.json" "AAA Pin Two")" ||
        fail G-DASH-PIN-DND "the grid has no tile for the app to pin"

    # Dragged from the grid onto the second favorite's left half: pinned
    # between the two.
    am_dst="$(am_dash_pt "$ARTIFACTS/am-dnd-start.json" "AAA Pin Three" 0.3)"
    am_mark="$(wc -c <"$LOG")"
    am_drag_hold "$am_src" "$am_dst"
    tail -c +"$((am_mark + 1))" "$LOG" | grep -q "dash dnd placeholder slot=1" ||
        fail G-DASH-PIN-DND "dragging over the second favorite showed no placeholder in slot 1"
    scrot "$ARTIFACTS/am-dnd-placeholder.png" >/dev/null
    am_drag_drop
    tail -c +"$((am_mark + 1))" "$LOG" | grep -q "dash dnd drop tuna-pin-Two.desktop slot=1 position=1" ||
        fail G-DASH-PIN-DND "the drop did not take position 1 among the favorites"
    am_wait_favorites "['tuna-pin-One.desktop', 'tuna-pin-Two.desktop', 'tuna-pin-Three.desktop']" ||
        fail G-DASH-PIN-DND "dropping the app between the favorites wrote $(am_favorites), not a pin at position 1"
    sleep 1
    am_dump am-dnd-pinned
    [ "$(am_dash_names "$ARTIFACTS/am-dnd-pinned.json")" = "AAA Pin One,AAA Pin Two,AAA Pin Three" ] ||
        fail G-DASH-PIN-DND "the dash shows '$(am_dash_names "$ARTIFACTS/am-dnd-pinned.json")' after the pin"
    jq -e 'any(.[]; (.name // "") | test("AAA Pin Two Pinned to Dash"))' "$ARTIFACTS/am-dnd-pinned.json" >/dev/null ||
        fail G-DASH-PIN-DND "the pin posted no \"Pinned to Dash\" notification"

    # Within the dash: the last favorite onto the first one's left half.
    am_src="$(am_dash_pt "$ARTIFACTS/am-dnd-pinned.json" "AAA Pin Three")"
    am_dst="$(am_dash_pt "$ARTIFACTS/am-dnd-pinned.json" "AAA Pin One" 0.25)"
    am_mark="$(wc -c <"$LOG")"
    am_drag_hold "$am_src" "$am_dst"
    tail -c +"$((am_mark + 1))" "$LOG" | grep -q "dash dnd placeholder slot=0" ||
        fail G-DASH-PIN-DND "dragging a favorite over the first showed no placeholder in slot 0"
    am_drag_drop
    am_wait_favorites "['tuna-pin-Three.desktop', 'tuna-pin-One.desktop', 'tuna-pin-Two.desktop']" ||
        fail G-DASH-PIN-DND "dragging the last favorite to the front wrote $(am_favorites)"
    sleep 1
    am_dump am-dnd-reordered
    [ "$(am_dash_names "$ARTIFACTS/am-dnd-reordered.json")" = "AAA Pin Three,AAA Pin One,AAA Pin Two" ] ||
        fail G-DASH-PIN-DND "the dash shows '$(am_dash_names "$ARTIFACTS/am-dnd-reordered.json")' after the reorder"

    # A favorite dropped on its own place changes nothing.
    am_src="$(am_dash_pt "$ARTIFACTS/am-dnd-reordered.json" "AAA Pin One" 0.4)"
    am_dst="$(am_dash_pt "$ARTIFACTS/am-dnd-reordered.json" "AAA Pin One" 0.6)"
    am_drag_hold "$am_src" "$am_dst"
    am_drag_drop
    [ "$(am_favorites)" = "['tuna-pin-Three.desktop', 'tuna-pin-One.desktop', 'tuna-pin-Two.desktop']" ] ||
        fail G-DASH-PIN-DND "a drop on the favorite's own place changed favorite-apps to $(am_favorites)"

    # Dropped away from the dash: nothing (GNOME unpins only on Show Apps).
    am_dump am-dnd-away-before
    am_src="$(am_dash_pt "$ARTIFACTS/am-dnd-away-before.json" "AAA Pin Two")"
    am_drag_hold "$am_src" "640 300"
    am_drag_drop
    [ "$(am_favorites)" = "['tuna-pin-Three.desktop', 'tuna-pin-One.desktop', 'tuna-pin-Two.desktop']" ] ||
        fail G-DASH-PIN-DND "a drop away from the dash changed favorite-apps to $(am_favorites)"
    am_dump am-dnd-away
    [ "$(am_dash_names "$ARTIFACTS/am-dnd-away.json")" = "AAA Pin Three,AAA Pin One,AAA Pin Two" ] ||
        fail G-DASH-PIN-DND "the dash shows '$(am_dash_names "$ARTIFACTS/am-dnd-away.json")' after a drop away from it"

    # Show Apps reads "Unpin" under a dragged favorite; dropping unpins.
    am_src="$(am_dash_pt "$ARTIFACTS/am-dnd-away.json" "AAA Pin Two")"
    am_dst="$(am_dash_pt "$ARTIFACTS/am-dnd-away.json" "Show Apps")"
    am_mark="$(wc -c <"$LOG")"
    am_drag_hold "$am_src" "$am_dst"
    am_dump am-dnd-unpin-hover
    jq -e 'any(.[]; .role == "toggle button" and .name == "Unpin" and .showing)' "$ARTIFACTS/am-dnd-unpin-hover.json" >/dev/null ||
        fail G-DASH-PIN-DND "Show Apps did not read \"Unpin\" under a dragged favorite"
    scrot "$ARTIFACTS/am-dnd-unpin.png" >/dev/null
    am_drag_drop
    tail -c +"$((am_mark + 1))" "$LOG" | grep -q "dash dnd unpin tuna-pin-Two.desktop" ||
        fail G-DASH-PIN-DND "the drop on Show Apps did not unpin"
    am_wait_favorites "['tuna-pin-Three.desktop', 'tuna-pin-One.desktop']" ||
        fail G-DASH-PIN-DND "dropping on Show Apps wrote $(am_favorites)"
    sleep 1
    am_dump am-dnd-unpinned
    [ "$(am_dash_names "$ARTIFACTS/am-dnd-unpinned.json")" = "AAA Pin Three,AAA Pin One" ] ||
        fail G-DASH-PIN-DND "the dash shows '$(am_dash_names "$ARTIFACTS/am-dnd-unpinned.json")' after the unpin"
    jq -e 'any(.[]; .role == "toggle button" and .name == "Show Apps" and .showing)' "$ARTIFACTS/am-dnd-unpinned.json" >/dev/null ||
        fail G-DASH-PIN-DND "Show Apps kept reading \"Unpin\" after the drop"
    # The unpinned app is a grid tile again.
    am_grid_pt "$ARTIFACTS/am-dnd-unpinned.json" "AAA Pin Two" >/dev/null ||
        fail G-DASH-PIN-DND "the unpinned app did not return to the grid"
    am_overview_close
    pass G-DASH-PIN-DND "an app dragged from the grid pins at the drop position (favorite-apps written), a favorite dragged within the dash reorders, a drop on its own place or away from the dash changes nothing, and a drop on Show Apps (which reads Unpin) unpins"
}

favorites_live_proof() {
    AM_GATE=G-FAVORITES-LIVE
    am_shell="$(pgrep -P "$COMPOSITOR_PID" -x tuna-shell-gtk)" || fail G-FAVORITES-LIVE "GTK shell process absent"
    gsettings set org.gnome.shell favorite-apps "['tuna-pin-One.desktop']"
    am_overview_open
    am_dump am-live-1
    [ "$(am_dash_names "$ARTIFACTS/am-live-1.json")" = "AAA Pin One" ] ||
        fail G-FAVORITES-LIVE "the dash shows '$(am_dash_names "$ARTIFACTS/am-live-1.json")', not the one favorite"
    # Written while the overview shows: the dash follows with no restart.
    gsettings set org.gnome.shell favorite-apps "['tuna-pin-One.desktop', 'tuna-pin-Two.desktop']"
    am_try=0
    while [ "$am_try" -lt 12 ]; do
        am_dump am-live-2
        [ "$(am_dash_names "$ARTIFACTS/am-live-2.json")" = "AAA Pin One,AAA Pin Two" ] && break
        am_try=$((am_try + 1))
        sleep 0.5
    done
    [ "$(am_dash_names "$ARTIFACTS/am-live-2.json")" = "AAA Pin One,AAA Pin Two" ] ||
        fail G-FAVORITES-LIVE "a second favorite written by gsettings did not reach the open dash ('$(am_dash_names "$ARTIFACTS/am-live-2.json")')"
    # Reordered and one dropped, an app that is not installed ignored.
    gsettings set org.gnome.shell favorite-apps "['tuna-pin-Three.desktop', 'org.example.Gone.desktop', 'tuna-pin-Two.desktop']"
    am_try=0
    while [ "$am_try" -lt 12 ]; do
        am_dump am-live-3
        [ "$(am_dash_names "$ARTIFACTS/am-live-3.json")" = "AAA Pin Three,AAA Pin Two" ] && break
        am_try=$((am_try + 1))
        sleep 0.5
    done
    [ "$(am_dash_names "$ARTIFACTS/am-live-3.json")" = "AAA Pin Three,AAA Pin Two" ] ||
        fail G-FAVORITES-LIVE "a reordered list with an uninstalled app showed '$(am_dash_names "$ARTIFACTS/am-live-3.json")'"
    scrot "$ARTIFACTS/am-live-dash.png" >/dev/null
    # Written while the overview is closed: it shows when it opens.
    am_overview_close
    gsettings set org.gnome.shell favorite-apps "['tuna-pin-Two.desktop', 'tuna-pin-One.desktop']"
    sleep 1
    am_overview_open
    am_dump am-live-4
    [ "$(am_dash_names "$ARTIFACTS/am-live-4.json")" = "AAA Pin Two,AAA Pin One" ] ||
        fail G-FAVORITES-LIVE "a list written while the overview was closed showed '$(am_dash_names "$ARTIFACTS/am-live-4.json")' on opening"
    # And emptied.
    gsettings set org.gnome.shell favorite-apps "[]"
    am_try=0
    while [ "$am_try" -lt 12 ]; do
        am_dump am-live-5
        [ -z "$(am_dash_names "$ARTIFACTS/am-live-5.json")" ] && break
        am_try=$((am_try + 1))
        sleep 0.5
    done
    [ -z "$(am_dash_names "$ARTIFACTS/am-live-5.json")" ] ||
        fail G-FAVORITES-LIVE "an emptied favorite-apps left '$(am_dash_names "$ARTIFACTS/am-live-5.json")' in the dash"
    am_overview_close
    [ "$(pgrep -P "$COMPOSITOR_PID" -x tuna-shell-gtk)" = "$am_shell" ] ||
        fail G-FAVORITES-LIVE "the shell restarted instead of following the key"
    pass G-FAVORITES-LIVE "favorite-apps written by gsettings (added, reordered, with an uninstalled app, written while the overview was closed, emptied) reaches the dash with the same shell process"
}
