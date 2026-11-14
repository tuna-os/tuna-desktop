#!/bin/sh
# Sourced by tuna-gtk-shell-proof for its focused --window-leak-only run
# (#1088, a v1 gate of #577). Requires that harness's private bus, GTK
# shell and helpers (pass, fail, cstate, $PY, $ROOT, $ARTIFACTS, $CSTATE,
# $COMPOSITOR_PID, $SOCKET_NAME).
#
# Windows are real libadwaita apps (scripts/lib/tuna-test-window.py, which
# GTK gives an xdg_dialog_v1, a popover and shm buffers); closes are real
# X11 input through the nested window or the app being killed. The
# counters are read from /proc (scripts/lib/tuna-resource-sample.py).

# Resources of the compositor, into $1.
wl_sample() {
    "$PY" "$ROOT/scripts/lib/tuna-resource-sample.py" sample \
        compositor="$COMPOSITOR_PID" >"$1"
}

# Wait until no window close or map effect still holds its snapshot.
wl_settle() {
    cstate '(.window_lifecycle.destroying | length) == 0 and (.window_lifecycle.opening | length) == 0' \
        "window effects done" 10 ||
        fail G-WINDOW-LEAK "a window effect never finished ($1)"
}

# One window opened and closed. Odd cycles close it as its close button
# would (Alt+F4: xdg_toplevel.close, the app destroys its window, the close
# effect plays); even cycles kill the app, whose connection just closes
# with every object still alive.
wl_cycle() {
    wl_before="$(jq '.windows | length' "$CSTATE")"
    WAYLAND_DISPLAY="$SOCKET_NAME" "$PY" "$ROOT/scripts/lib/tuna-test-window.py" \
        "Leak $1" "#c01c28" >>"$ARTIFACTS/window-leak-apps.log" 2>&1 &
    wl_pid=$!
    cstate "(.windows | length) == $((wl_before + 1))" "window $1 mapped" 20 ||
        fail G-WINDOW-LEAK "window $1 never mapped"
    # Let it draw: its frames are uploaded and its map effect runs.
    sleep 0.4
    if [ $(($1 % 2)) = 1 ]; then
        cstate '.focused != null' "window $1 focused" 10 ||
            fail G-WINDOW-LEAK "window $1 never took focus"
        xdotool key alt+F4
    else
        kill "$wl_pid"
    fi
    cstate "(.windows | length) == $wl_before" "window $1 closed" 20 ||
        fail G-WINDOW-LEAK "window $1 never closed"
    wait "$wl_pid" 2>/dev/null || true
}

# G-WINDOW-LEAK: windows opened and closed leave the compositor's
# anonymous memory within a fixed bound of its warm state, however many
# there were (each used to keep its surface and textures, about 1.65 MB
# for this window).
#
# The warm-up is fixed: the first windows create what stays for the
# session (shader programs, the icon cache, allocator arenas), so the
# baseline is taken after WL_WARMUP windows.
window_leak_proof() {
    WL_WARMUP=5
    WL_WINDOWS="${TUNA_WINDOW_LEAK_WINDOWS:-100}"
    # Fixed allowances, independent of the number of windows: allocator
    # and cache noise, and the shell's own shm pools (GDK keeps up to two
    # per surface, so the panel's second pool may or may not be mapped at
    # either sample). A leak of whole windows exceeds them within a few.
    WL_BOUND_KIB=8192
    WL_FDS=2
    : >"$ARTIFACTS/window-leak-apps.log"
    wl_i=1
    while [ "$wl_i" -le "$WL_WARMUP" ]; do
        wl_cycle "$wl_i"
        wl_i=$((wl_i + 1))
    done
    wl_settle "warm-up"
    sleep 1.5
    wl_sample "$ARTIFACTS/window-leak-before.json"
    wl_n=1
    while [ "$wl_n" -le "$WL_WINDOWS" ]; do
        wl_cycle "$wl_i"
        wl_i=$((wl_i + 1))
        wl_n=$((wl_n + 1))
    done
    wl_settle "after $WL_WINDOWS windows"
    sleep 1.5
    wl_sample "$ARTIFACTS/window-leak-after.json"
    "$PY" "$ROOT/scripts/lib/tuna-resource-sample.py" diff \
        "$ARTIFACTS/window-leak-before.json" "$ARTIFACTS/window-leak-after.json" \
        >"$ARTIFACTS/window-leak-diff.txt"
    wl_grown="$("$PY" "$ROOT/scripts/lib/tuna-resource-sample.py" compare \
        "$ARTIFACTS/window-leak-before.json" "$ARTIFACTS/window-leak-after.json" \
        "compositor.pss_anon_kib=$WL_BOUND_KIB" "compositor.fds=$WL_FDS")" ||
        fail G-WINDOW-LEAK "$WL_WINDOWS windows opened and closed leaked: $(echo "$wl_grown" | tr '\n' ';')"
    pass G-WINDOW-LEAK "$WL_WINDOWS windows opened and closed (half by Alt+F4, half by killing the app) left the compositor's anonymous memory within ${WL_BOUND_KIB} KiB and its descriptors within $WL_FDS of its state after $WL_WARMUP warm-up windows ($(tr '\n' ';' <"$ARTIFACTS/window-leak-diff.txt"))"
}
