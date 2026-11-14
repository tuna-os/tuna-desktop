#!/bin/sh
# Sourced by tuna-gtk-shell-proof, also used by its focused --sound-alerts-only
# run. Requires that harness's private bus, nested session, AT-SPI, fake
# wpctl/pw-dump, the canberra-gtk-play stand-in and assertion helpers.
#
#   G-VISUAL-BELL   a mapped Wayland client rings xdg-system-bell; host
#                   pixels show Mutter's frame flash over its window,
#                   the whole-output flash for no window and for
#                   fullscreen-flash, nothing with visual-bell off, and
#                   always the original pixels afterwards; audible-bell
#                   and event-sounds decide the bell-window-system sound
#   G-X11-BELL      an X11 client's XkbBell reaches the same policy: frame
#                   flash on the window it names, on the focused framed
#                   X11 window when it names none, the sound, and Mutter's
#                   100 ms drop of a repeated bell
#   G-XSETTINGS-SOUNDS the window manager leaves XSETTINGS to
#                   gnome-settings-daemon's xsettings plugin, which then
#                   publishes input-feedback-sounds live to X11 clients
#                   as Net/EnableInputFeedbackSounds
#   G-QS-VOLUME-FEEDBACK dragging the output slider plays no feedback
#                   until the drag ends, then exactly one; a scroll plays
#                   one; a playing sink or event-sounds off plays none
sound_alerts_feedback() {
    awk -v id="$1" '$2 == "--id" && $3 == id' "$ARTIFACTS/canberra-calls" | wc -l
}

# Wait for the compositor to hold the policy the shell just published.
sound_alerts_policy() {
    cstate ".bell_policy.audible == $1 and .bell_policy.visual == $2 and .bell_policy.frame_flash == $3 and .bell_policy.event_sounds == $4" \
        "bell policy $*" 10 || fail "$5" "the compositor never received bell policy $*"
}

sound_alerts_set_policy() {
    gsettings set org.gnome.desktop.wm.preferences audible-bell "$1"
    gsettings set org.gnome.desktop.wm.preferences visual-bell "$2"
    if [ "$3" = true ]; then
        gsettings set org.gnome.desktop.wm.preferences visual-bell-type frame-flash
    else
        gsettings set org.gnome.desktop.wm.preferences visual-bell-type fullscreen-flash
    fi
    gsettings set org.gnome.desktop.sound event-sounds "$4"
    sound_alerts_policy "$@"
}

# One bell case: CASE GATE FIFO LINE RECT EXPECT SOUNDS RINGS
# SOUNDS is the expected number of new bell-window-system plays, RINGS of
# bells the compositor received.
sound_alerts_case() {
    case_sounds="$(sound_alerts_feedback bell-window-system)"
    case_rings="$(jq '.bell_rings' "$CSTATE")"
    # The photosensitivity limit admits one visual alert per 500 ms.
    sleep 0.6
    WIDTH="$WIDTH" HEIGHT="$HEIGHT" "$PY" "$ROOT/scripts/lib/tuna-bell-pixels.py" \
        "$ARTIFACTS/bell" "$1" "$3" "$4" "$SOUND_ALERTS_ORIGIN" "$5" "$6" --top "$SOUND_ALERTS_TOP" ||
        fail "$2" "$1: host pixels (see bell/$1.json)"
    cstate ".bell_rings == $((case_rings + $8))" "$1 rings" 5 ||
        fail "$2" "$1: the compositor received $(jq '.bell_rings' "$CSTATE") bells, not $((case_rings + $8))"
    # The stand-in exits at once; give a spawned player time to log.
    sleep 0.5
    [ "$(sound_alerts_feedback bell-window-system)" = "$((case_sounds + $7))" ] ||
        fail "$2" "$1: bell-window-system played $(($(sound_alerts_feedback bell-window-system) - case_sounds)) times, not $7"
}

sound_alerts_window_rect() {
    jq -er --arg app "$1" '[.windows[] | select(.app_id == $app)][0].rect | "\(.[0]),\(.[1]),\(.[2]),\(.[3])"' "$CSTATE"
}

sound_alerts_proof() {
    touch "$ARTIFACTS/canberra-calls"
    mkdir -p "$ARTIFACTS/bell"
    if [ -z "${SOCKET_NAME:-}" ]; then
        SOCKET_NAME="$(sed -n 's/.*nested session on \([^ ]*\) .*/\1/p' "$LOG" | head -1)"
    fi
    xdotool key Escape
    cstate '.locked == false and (.overview_open | not)' "plain desktop" 10 ||
        fail G-VISUAL-BELL "the desktop is not unlocked without the overview"
    eval "$(xdotool getwindowgeometry --shell "$WID")"
    SOUND_ALERTS_ORIGIN="$X,$Y"
    # The panel is black: a black flash cannot change it, and its clock
    # may tick. Every case compares the output below it.
    SOUND_ALERTS_TOP="$(jq '[.layers[] | select(.namespace == "tuna-panel" or .namespace == "tuna-shell-panel") | .rect[1] + .rect[3]] | max // 32' "$CSTATE")"
    # The pointer stays off the probes so the cursor is in no case.
    xdotool mousemove --window "$WID" 6 $((HEIGHT - 6))

    # --- G-VISUAL-BELL: a real xdg-system-bell client.
    bell_wl_fifo="$ARTIFACTS/bell/wayland.ctl"
    rm -f "$bell_wl_fifo"
    mkfifo "$bell_wl_fifo"
    WAYLAND_DISPLAY="$SOCKET_NAME" "$COMPANIONS/tuna-bell-client" wayland org.tuna.BellProbe \
        <"$bell_wl_fifo" >"$ARTIFACTS/bell/wayland.log" 2>&1 &
    SOUND_ALERTS_WL_PID=$!
    # Hold the writing end: the client lives until the gate closes it.
    exec 7>"$bell_wl_fifo"
    cstate '.focused_app_id == "org.tuna.BellProbe"' "bell probe mapped" ||
        fail G-VISUAL-BELL "the bell client never mapped and focused"
    sleep 1
    bell_rect="$(sound_alerts_window_rect org.tuna.BellProbe)" ||
        fail G-VISUAL-BELL "no rectangle for the bell client"
    sound_alerts_set_policy true false true true G-VISUAL-BELL
    sound_alerts_case wl-visual-off G-VISUAL-BELL "$bell_wl_fifo" window "$bell_rect" none 1 1
    sound_alerts_set_policy false true true true G-VISUAL-BELL
    sound_alerts_case wl-frame-window G-VISUAL-BELL "$bell_wl_fifo" window "$bell_rect" frame 0 1
    sound_alerts_case wl-frame-nowindow G-VISUAL-BELL "$bell_wl_fifo" whole "$bell_rect" full 0 1
    sound_alerts_set_policy true true false true G-VISUAL-BELL
    sound_alerts_case wl-fullscreen-window G-VISUAL-BELL "$bell_wl_fifo" window "$bell_rect" full 1 1
    sound_alerts_set_policy true false false false G-VISUAL-BELL
    sound_alerts_case wl-event-sounds-off G-VISUAL-BELL "$bell_wl_fifo" window "$bell_rect" none 0 1
    grep -q -- "--id bell-window-system --description Bell event --property canberra.xdg-theme.name=freedesktop" \
        "$ARTIFACTS/canberra-calls" || fail G-VISUAL-BELL "the bell did not play GNOME's bell-window-system from the theme"
    exec 7>&-
    wait "$SOUND_ALERTS_WL_PID" 2>/dev/null || true
    pass G-VISUAL-BELL "a mapped Wayland client's xdg-system-bell flashes its frame or the whole output per visual-bell-type, nothing with visual-bell off, restores the pixels, and sounds bell-window-system only with audible-bell and event-sounds"

    # --- G-X11-BELL: XkbBellNotify from a real X11 client through Xwayland.
    bell_x11_fifo="$ARTIFACTS/bell/x11.ctl"
    rm -f "$bell_x11_fifo"
    mkfifo "$bell_x11_fifo"
    cstate '.x11_display != null' "X11 display" 30 || fail G-X11-BELL "no X11 display"
    bell_x11_display="$(jq -r '.x11_display' "$CSTATE")"
    DISPLAY="$bell_x11_display" "$COMPANIONS/tuna-bell-client" x11 \
        <"$bell_x11_fifo" >"$ARTIFACTS/bell/x11.log" 2>&1 &
    SOUND_ALERTS_X11_PID=$!
    exec 8>"$bell_x11_fifo"
    cstate '.focused_app_id == "TunaX11Bell"' "X11 bell probe focused" 30 ||
        fail G-X11-BELL "the X11 bell client never mapped and focused"
    sleep 1
    bell_rect="$(sound_alerts_window_rect TunaX11Bell)" ||
        fail G-X11-BELL "no rectangle for the X11 bell client"
    sound_alerts_set_policy false true true true G-X11-BELL
    sound_alerts_case x11-frame-window G-X11-BELL "$bell_x11_fifo" window "$bell_rect" frame 0 1
    # XBell names no window: Mutter flashes the focused framed X11 window.
    sound_alerts_case x11-frame-focus G-X11-BELL "$bell_x11_fifo" none "$bell_rect" frame 0 1
    sound_alerts_set_policy false true false true G-X11-BELL
    sound_alerts_case x11-fullscreen G-X11-BELL "$bell_x11_fifo" window "$bell_rect" full 0 1
    sound_alerts_set_policy true false true true G-X11-BELL
    sound_alerts_case x11-audible G-X11-BELL "$bell_x11_fifo" window "$bell_rect" none 1 1
    sound_alerts_set_policy false false true true G-X11-BELL
    sound_alerts_case x11-silent G-X11-BELL "$bell_x11_fifo" window "$bell_rect" none 0 1
    # Two bells within 100 ms of X server time are one (Mutter's
    # handle_other_xevent).
    x11_rings="$(jq '.bell_rings' "$CSTATE")"
    x11_seen="$(grep -c '^RING' "$ARTIFACTS/bell/x11.log")"
    printf 'window\nwindow\n' >&8
    end=$(($(date +%s) + 10))
    until [ "$(grep -c '^RING' "$ARTIFACTS/bell/x11.log")" -ge $((x11_seen + 2)) ] || [ "$(date +%s)" -gt "$end" ]; do sleep 0.1; done
    sleep 1
    [ "$(jq '.bell_rings' "$CSTATE")" = $((x11_rings + 1)) ] ||
        fail G-X11-BELL "two X11 bells within 100 ms rang $(($(jq '.bell_rings' "$CSTATE") - x11_rings)) times, not once"
    exec 8>&-
    wait "$SOUND_ALERTS_X11_PID" 2>/dev/null || true
    pass G-X11-BELL "an X11 client's XKB bell through Xwayland flashes the window it names, the focused framed X11 window for none, or the whole output, sounds per audible-bell, and drops a repeat within 100 ms"
    gsettings reset org.gnome.desktop.wm.preferences audible-bell
    gsettings reset org.gnome.desktop.wm.preferences visual-bell
    gsettings reset org.gnome.desktop.wm.preferences visual-bell-type
    gsettings reset org.gnome.desktop.sound event-sounds

    # --- G-XSETTINGS-SOUNDS: GNOME's input-feedback-sounds consumer for
    # X11 clients is gsd-xsettings (Net/EnableInputFeedbackSounds).
    gsd_xsettings=""
    for candidate in /usr/libexec/gsd-xsettings /usr/lib/gsd-xsettings; do
        if [ -x "$candidate" ]; then gsd_xsettings="$candidate"; break; fi
    done
    if [ -z "$gsd_xsettings" ]; then
        if [ "${TUNA_REQUIRE_GSD_MEDIA_KEYS:-}" = 1 ]; then
            fail G-XSETTINGS-SOUNDS "required but missing: gsd-xsettings"
        fi
        echo "tuna-gtk-shell-proof: G-XSETTINGS-SOUNDS not run, missing gsd-xsettings" >&2
    else
        xsettings_value() {
            DISPLAY="$bell_x11_display" "$COMPANIONS/tuna-bell-client" xsettings Net/EnableInputFeedbackSounds
        }
        [ "$(xsettings_value)" = "owner none" ] ||
            fail G-XSETTINGS-SOUNDS "the window manager still owns XSETTINGS: $(xsettings_value)"
        gsettings set org.gnome.desktop.sound input-feedback-sounds false
        DISPLAY="$bell_x11_display" "$gsd_xsettings" >"$ARTIFACTS/gsd-xsettings.log" 2>&1 &
        GSD_XSETTINGS_PID=$!
        xsettings_wait() {
            end=$(($(date +%s) + 15))
            until xsettings_value | grep -q " Net/EnableInputFeedbackSounds $1\$" || [ "$(date +%s)" -gt "$end" ]; do
                kill -0 "$GSD_XSETTINGS_PID" 2>/dev/null ||
                    fail G-XSETTINGS-SOUNDS "gsd-xsettings exited: $(tail -3 "$ARTIFACTS/gsd-xsettings.log")"
                sleep 0.3
            done
            xsettings_value | grep -q " Net/EnableInputFeedbackSounds $1\$"
        }
        xsettings_wait 0 || fail G-XSETTINGS-SOUNDS "gsd-xsettings did not publish input-feedback-sounds off: $(xsettings_value)"
        gsettings set org.gnome.desktop.sound input-feedback-sounds true
        xsettings_wait 1 || fail G-XSETTINGS-SOUNDS "input-feedback-sounds on did not reach X11 clients: $(xsettings_value)"
        gsettings set org.gnome.desktop.sound input-feedback-sounds false
        xsettings_wait 0 || fail G-XSETTINGS-SOUNDS "input-feedback-sounds off did not reach X11 clients: $(xsettings_value)"
        kill "$GSD_XSETTINGS_PID" 2>/dev/null || true
        wait "$GSD_XSETTINGS_PID" 2>/dev/null || true
        gsettings reset org.gnome.desktop.sound input-feedback-sounds
        pass G-XSETTINGS-SOUNDS "the X11 window manager leaves XSETTINGS free; the distribution gsd-xsettings owns it and publishes input-feedback-sounds live as Net/EnableInputFeedbackSounds"
    fi

    # --- G-QS-VOLUME-FEEDBACK: GNOME's drag-end feedback on the real slider.
    xdotool mousemove --window "$WID" $((WIDTH - 50)) 16 click 1
    sleep 2
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-qs-feedback.json" 20 \
        --set Volume 100 || fail G-QS-VOLUME-FEEDBACK "could not set Volume"
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-qs-feedback-brightness.json" 20 \
        --set Brightness 20 || fail G-QS-VOLUME-FEEDBACK "could not set Brightness"
    sleep 1
    scrot -o "$ARTIFACTS/bell/qs-volume.png" >/dev/null
    # The slider whose accent fill spans its whole trough is Volume at 100%.
    volume_trough="$("$PY" - "$ARTIFACTS/bell/qs-volume.png" "$SOUND_ALERTS_ORIGIN" <<'TROUGH'
import sys
from PIL import Image
image = Image.open(sys.argv[1]).convert('RGB')
ox, oy = (int(v) for v in sys.argv[2].split(','))
best = None
for y in range(image.height):
    run = start = 0
    for x in range(image.width):
        r, g, b = image.getpixel((x, y))
        if b > 150 and b > r + 60 and b > g + 20:
            if not run:
                start = x
            run += 1
            if best is None or run > best[0]:
                best = (run, start, x, y)
        else:
            run = 0
run, x0, x1, y = best
print(x0 - ox, x1 - ox, y - oy)
TROUGH
)" || fail G-QS-VOLUME-FEEDBACK "no accent slider fill in quick settings"
    # shellcheck disable=SC2086 # three numbers
    set -- $volume_trough
    trough_x0=$1 trough_x1=$2 trough_y=$3
    [ $((trough_x1 - trough_x0)) -gt 100 ] ||
        fail G-QS-VOLUME-FEEDBACK "the volume trough found is only $((trough_x1 - trough_x0)) px wide"
    volume_drag() {
        xdotool mousemove --window "$WID" "$trough_x1" "$trough_y"
        sleep 0.3
        xdotool mousedown 1
        for i in 1 2 3 4 5 6 7 8 9 10; do
            xdotool mousemove --window "$WID" $((trough_x1 - (trough_x1 - trough_x0) * i * $1 / 1000)) "$trough_y"
            sleep 0.08
        done
        # Still held: the drag has written levels but sounded nothing.
        sleep 0.8
        drag_volume="$(cat "$VOLUME_FILE")"
        drag_sounds="$(sound_alerts_feedback audio-volume-change)"
        xdotool mouseup 1
        sleep 1.5
    }
    feedback_set_sink() {
        echo "$1" >"$ARTIFACTS/sink-state"
        "$PY" "$ROOT/scripts/lib/tuna-audio-fixture.py" "$ARTIFACTS" --event
        sleep 1
    }
    gsettings set org.gnome.desktop.sound event-sounds true
    feedback_set_sink idle
    before="$(sound_alerts_feedback audio-volume-change)"
    volume_drag 6
    [ "$drag_volume" != 1.00 ] || fail G-QS-VOLUME-FEEDBACK "the drag never moved the sink volume"
    [ "$drag_sounds" = "$before" ] ||
        fail G-QS-VOLUME-FEEDBACK "the slider sounded $((drag_sounds - before)) times during the drag"
    [ "$(sound_alerts_feedback audio-volume-change)" = $((before + 1)) ] ||
        fail G-QS-VOLUME-FEEDBACK "drag end sounded $(($(sound_alerts_feedback audio-volume-change) - before)) times, not once"
    # A scroll moves the level outside a drag: one sound.
    before="$(sound_alerts_feedback audio-volume-change)"
    scroll_volume="$(cat "$VOLUME_FILE")"
    xdotool mousemove --window "$WID" $(((trough_x0 + trough_x1) / 2)) "$trough_y" click 5
    sleep 1.5
    [ "$(cat "$VOLUME_FILE")" != "$scroll_volume" ] || fail G-QS-VOLUME-FEEDBACK "the scroll did not move the volume"
    [ "$(sound_alerts_feedback audio-volume-change)" = $((before + 1)) ] ||
        fail G-QS-VOLUME-FEEDBACK "a scroll sounded $(($(sound_alerts_feedback audio-volume-change) - before)) times, not once"
    # GNOME: no feedback while the sink is playing, nor with event-sounds off.
    feedback_set_sink running
    before="$(sound_alerts_feedback audio-volume-change)"
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-qs-feedback-reset.json" 20 \
        --set Volume 100 || fail G-QS-VOLUME-FEEDBACK "could not reset Volume"
    sleep 1
    volume_drag 5
    [ "$(sound_alerts_feedback audio-volume-change)" = "$before" ] ||
        fail G-QS-VOLUME-FEEDBACK "the slider sounded over a playing sink"
    feedback_set_sink idle
    gsettings set org.gnome.desktop.sound event-sounds false
    before="$(sound_alerts_feedback audio-volume-change)"
    volume_drag 4
    [ "$drag_volume" != 1.00 ] || fail G-QS-VOLUME-FEEDBACK "the negative-control drag never moved the volume"
    [ "$(sound_alerts_feedback audio-volume-change)" = "$before" ] ||
        fail G-QS-VOLUME-FEEDBACK "the slider sounded with event-sounds off"
    grep -q -- "--id audio-volume-change --description Volume changed --property canberra.xdg-theme.name=freedesktop" \
        "$ARTIFACTS/canberra-calls" || fail G-QS-VOLUME-FEEDBACK "the feedback did not play audio-volume-change from the theme"
    gsettings reset org.gnome.desktop.sound event-sounds
    rm -f "$ARTIFACTS/sink-state"
    "$PY" "$ROOT/scripts/lib/tuna-audio-fixture.py" "$ARTIFACTS" --event
    xdotool key Escape
    pass G-QS-VOLUME-FEEDBACK "the real output slider sounds audio-volume-change once when a drag ends and once per scroll, never mid-drag, over a playing sink or with event-sounds off"
}
