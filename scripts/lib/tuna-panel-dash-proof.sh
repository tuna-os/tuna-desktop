# Sourced by tuna-gtk-shell-proof: the --panel-dash-only slice (#1198,
# #1199). The session runs at TUNA_SCALE (unset is 1) in the locale the
# proof was started in; each gate's name carries that configuration:
# -FRACTIONAL at a scale other than 1, -RTL in a right-to-left locale.

# Right to left when the session locale's language is written so, as
# GTK and Clutter decide from the locale's script (gtkmain.c and
# clutter-context.c, gtk_get_locale_direction/clutter_get_text_direction).
panel_dash_locale="${LC_ALL:-${LC_CTYPE:-${LANG:-C}}}"
case "$panel_dash_locale" in
    ar_* | ar.* | he_* | he.* | fa_* | fa.* | ur_* | ur.* | yi_* | yi.* | ps_* | ps.* | dv_* | dv.* | ckb_* | ckb.* | ug_* | ug.* | syr_* | syr.*)
        PANEL_DASH_DIRECTION=rtl ;;
    *) PANEL_DASH_DIRECTION=ltr ;;
esac
PANEL_DASH_SUFFIX=""
case "${TUNA_SCALE:-1}" in
    1 | 1.0) ;;
    *) PANEL_DASH_SUFFIX="-FRACTIONAL" ;;
esac
[ "$PANEL_DASH_DIRECTION" = rtl ] && PANEL_DASH_SUFFIX="$PANEL_DASH_SUFFIX-RTL"

# Before the shell starts, which reads favorite-apps once: no Tuna pins
# (they would replace GNOME's list), three visible apps the proof can
# launch, and a favorite-apps list with a missing app between the two
# favourites (GNOME skips it).
panel_dash_setup() {
    # An unavailable locale leaves every process in C, left to right.
    "$PY" -c 'import locale; locale.setlocale(locale.LC_ALL, "")' 2>/dev/null ||
        fail "G-PANEL-LAYOUT$PANEL_DASH_SUFFIX" "the locale $panel_dash_locale is not available"
    rm -f "$XDG_DATA_HOME/tuna-shell/favorites.json"
    for panel_dash_app in Zeta Epsilon Eta; do
        panel_dash_id="tuna-test-$(echo "$panel_dash_app" | tr '[:upper:]' '[:lower:]')"
        printf '[Desktop Entry]\nName=Tuna Dash %s\nExec=%s %s/scripts/lib/tuna-test-window.py %s\nType=Application\n' \
            "$panel_dash_app" "$PY" "$ROOT" "$panel_dash_app" >"$XDG_DATA_HOME/applications/$panel_dash_id.desktop"
    done
    gsettings set org.gnome.shell favorite-apps "['tuna-test-zeta.desktop', 'org.tuna.ProofMissing.desktop', 'tuna-test-epsilon.desktop']"
}

# G-PANEL-LAYOUT (P-PN-01): the bar's accessible tree reads Activities,
# the clock, the system menu in the reading direction with the clock
# centred on the output; each control's hover fill (its drawn pill) puts
# Activities at the leading edge, the clock's centre within a physical
# pixel of the output's, the system menu at the trailing edge (mirrored
# right to left).
panel_layout_gate() {
    panel_gate="G-PANEL-LAYOUT$PANEL_DASH_SUFFIX"
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-panel-layout.json" 20 ||
        fail "$panel_gate" "accessibility dump failed"
    snapshot_state "$CSTATE" "$ARTIFACTS/panel-layout-state.json"
    scrot "$ARTIFACTS/panel-layout.png" >/dev/null
    panel_points="$("$PY" "$ROOT/scripts/lib/tuna-panel-dash-check.py" panel-points \
        "$ARTIFACTS/a11y-panel-layout.json" "$ARTIFACTS/panel-layout-state.json" \
        --physical-width "$WIDTH" 2>&1)" || fail "$panel_gate" "$panel_points"
    panel_frames=""
    while read -r panel_name panel_x panel_y; do
        xdotool mousemove --window "$WID" "$panel_x" "$panel_y"
        # Past the fill's hover transition.
        sleep 0.6
        scrot "$ARTIFACTS/panel-layout-hover-$panel_name.png" >/dev/null
        panel_frames="$panel_frames $ARTIFACTS/panel-layout-hover-$panel_name.png"
    done <<EOF
$panel_points
EOF
    xdotool mousemove --window "$WID" $((WIDTH / 2)) $((HEIGHT / 2))
    # shellcheck disable=SC2086
    panel_detail="$("$PY" "$ROOT/scripts/lib/tuna-panel-dash-check.py" panel \
        "$ARTIFACTS/a11y-panel-layout.json" "$ARTIFACTS/panel-layout-state.json" $panel_frames \
        --physical-width "$WIDTH" --direction "$PANEL_DASH_DIRECTION" 2>&1)" ||
        fail "$panel_gate" "$panel_detail"
    pass "$panel_gate" "Activities at the leading edge, the clock centred, the system menu at the trailing edge ($panel_detail)"
}

# G-DASH-FAVORITES-RUNNING (P-OV-03): favourite Epsilon and non-favourite
# Eta run; the overview's dash lists Zeta and Epsilon in favorite-apps
# order, then Eta, then Show Apps, along the reading direction, with the
# running dot under Epsilon and Eta only.
dash_favorites_running_gate() {
    dash_gate="G-DASH-FAVORITES-RUNNING$PANEL_DASH_SUFFIX"
    for dash_app in Epsilon Eta; do
        dash_count="$(jq '.windows | length' "$CSTATE")"
        WAYLAND_DISPLAY="$SOCKET_NAME" "$PY" "$ROOT/scripts/lib/tuna-test-window.py" "$dash_app" \
            >"$ARTIFACTS/window-dash-$dash_app.log" 2>&1 &
        cstate "(.windows | length) == $((dash_count + 1)) and any(.windows[]; .app_id == \"tuna-test-$(echo "$dash_app" | tr '[:upper:]' '[:lower:]')\")" "$dash_app mapped" ||
            fail "$dash_gate" "$dash_app never mapped"
    done
    xdotool mousemove --window "$WID" $((WIDTH / 2)) $((HEIGHT / 2))
    xdotool key Super_L
    cstate '.overview_open and .overview_progress == 1' "overview for the dash" ||
        fail "$dash_gate" "Super did not open the overview"
    # The dash's items zoom in over GNOME's 200 ms.
    sleep 1.5
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-dash-favorites.json" 20 ||
        fail "$dash_gate" "accessibility dump failed"
    snapshot_state "$CSTATE" "$ARTIFACTS/dash-favorites-state.json"
    scrot "$ARTIFACTS/dash-favorites.png" >/dev/null
    dash_detail="$("$PY" "$ROOT/scripts/lib/tuna-panel-dash-check.py" dash \
        "$ARTIFACTS/a11y-dash-favorites.json" "$ARTIFACTS/dash-favorites-state.json" \
        "$ARTIFACTS/dash-favorites.png" --physical-width "$WIDTH" --direction "$PANEL_DASH_DIRECTION" \
        --expect "Tuna Dash Zeta" "Tuna Dash Epsilon" "Tuna Dash Eta" "Show Apps" \
        --running "Tuna Dash Epsilon" "Tuna Dash Eta" --separator-after "Tuna Dash Epsilon" 2>&1)" ||
        fail "$dash_gate" "$dash_detail"
    pass "$dash_gate" "favourites in favorite-apps order, then the running non-favourite, then Show Apps; running dots under exactly the running apps ($dash_detail)"
    xdotool key Escape
    cstate '.overview_open | not' "overview closed" || fail "$dash_gate" "Escape left the overview open"
}
