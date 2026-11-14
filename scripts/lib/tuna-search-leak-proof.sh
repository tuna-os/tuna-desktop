#!/bin/sh
# Sourced by tuna-gtk-shell-proof for its focused --search-leak-only run
# (#1084, a v1 gate of #577). Requires that harness's private bus, GTK
# shell and helpers (pass, fail, cstate, $PY, $ROOT, $ARTIFACTS, $CSTATE,
# $COMPOSITOR_PID, $WID).
#
# Every step is real X11 input through the nested window; the counters
# are read from /proc (scripts/lib/tuna-resource-sample.py).

# Resources of the compositor and the shell, into $1.
sl_sample() {
    "$PY" "$ROOT/scripts/lib/tuna-resource-sample.py" sample \
        compositor="$COMPOSITOR_PID" shell="$SL_SHELL_PID" >"$1"
}

# Height of the overview's search surface as the compositor laid it out.
sl_search_height() {
    jq -r '[.layers[] | select(.namespace == "tuna-shell-overview") | .rect[3]][0] // 0' "$CSTATE"
}

# One search that shows results, then closed: Super, the query, wait for
# the search surface to grow past its entry-only height, Escape.
sl_search_cycle() {
    xdotool key Super_L
    cstate '.overview_open and .overview_progress == 1' "overview open" 10 ||
        fail G-SEARCH-LEAK "overview did not open (search $1)"
    cstate '.layers | any(.namespace == "tuna-shell-overview")' "search surface" 10 ||
        fail G-SEARCH-LEAK "the search surface never mapped (search $1)"
    sl_entry_height="$(sl_search_height)"
    xdotool type --delay 30 "tuna"
    cstate ".overview_search and ([.layers[] | select(.namespace == \"tuna-shell-overview\") | .rect[3]][0] // 0) > $sl_entry_height" \
        "search results" 10 ||
        fail G-SEARCH-LEAK "search $1 showed no results (search surface stayed $sl_entry_height high)"
    SL_RESULT_HEIGHT="$(sl_search_height)"
    xdotool key Escape
    cstate '.overview_open | not' "closed" 3 || xdotool key Escape
    cstate '(.overview_open | not) and .overview_progress == 0 and (.overview_search | not)' "overview closed" 10 ||
        fail G-SEARCH-LEAK "overview stayed open (search $1)"
}

# G-SEARCH-LEAK: searches with results leave the shell's GDK shm buffers
# and both processes' descriptors where they were.
#
# The warm-up is fixed: the first searches create what stays for the
# session (the result tiles' icon textures, the search provider's bus
# proxy, GDK's one cached buffer per surface), so the baseline is taken
# after SL_WARMUP searches and nothing may grow from there.
search_leak_proof() {
    SL_SHELL_PID="$(pgrep -P "$COMPOSITOR_PID" -x tuna-shell-gtk)" ||
        fail G-SEARCH-LEAK "GTK shell process absent"
    SL_WARMUP=5
    SL_SEARCHES="${TUNA_SEARCH_LEAK_SEARCHES:-50}"
    sl_i=1
    while [ "$sl_i" -le "$SL_WARMUP" ]; do
        sl_search_cycle "warm-up $sl_i"
        sl_i=$((sl_i + 1))
    done
    sleep 1
    sl_sample "$ARTIFACTS/search-leak-before.json"
    sl_i=1
    while [ "$sl_i" -le "$SL_SEARCHES" ]; do
        sl_search_cycle "$sl_i"
        sl_i=$((sl_i + 1))
    done
    sleep 1
    sl_sample "$ARTIFACTS/search-leak-after.json"
    [ "$(pgrep -P "$COMPOSITOR_PID" -x tuna-shell-gtk)" = "$SL_SHELL_PID" ] ||
        fail G-SEARCH-LEAK "the shell restarted during the searches"
    sl_grown="$("$PY" "$ROOT/scripts/lib/tuna-resource-sample.py" compare \
        "$ARTIFACTS/search-leak-before.json" "$ARTIFACTS/search-leak-after.json" \
        "shell.shm:memfd:gdk-wayland=0" "shell.fds=0" "compositor.fds=0")" ||
        fail G-SEARCH-LEAK "$SL_SEARCHES searches with results leaked: $(echo "$sl_grown" | tr '\n' ';')"
    pass G-SEARCH-LEAK "$SL_SEARCHES searches with results (search surface $sl_entry_height -> $SL_RESULT_HEIGHT high) left the shell's GDK shm mappings and both processes' descriptors where they were after $SL_WARMUP warm-up searches"
}
