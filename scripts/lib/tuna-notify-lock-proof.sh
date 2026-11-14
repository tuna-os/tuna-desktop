#!/bin/sh
# Sourced by tuna-gtk-shell-proof, also used by its focused
# --notify-lock-only run. Requires that harness's private bus, GTK shell,
# AT-SPI, pam_wrapper unlock, libcanberra player stand-in
# ($ARTIFACTS/canberra-calls) and assertion helpers.
#
# GNOME 51's lock-screen notifications (js/ui/unlockDialog.js
# NotificationsBox) and notification sounds (js/ui/messageTray.js
# Notification.playSound), #348:
#   G-NOTIFY-LOCK        while locked no banner shows; each installed app
#                        with unseen notifications and show-in-lock-screen
#                        shows its name and count, its titles and bodies
#                        only with details-in-lock-screen; a critical one
#                        reveals nothing more; an app kept off the lock
#                        screen, or disabled, is absent from the tree
#   G-NOTIFY-LOCK-PIXELS a redacted source's pixels do not depend on its
#                        text (a replaced title and body change nothing);
#                        allowing details changes them, and taking them
#                        back restores the redacted frame exactly
#   G-NOTIFY-LOCK-LIVE   arrivals and the global and per-app keys reach
#                        the locked screen at once; unlocking takes the
#                        list away and the queued banners, with their
#                        sound, follow
#   G-NOTIFY-SOUND       a banner plays its sound-name (else sound-file)
#                        through the event-sound player as it shows,
#                        unless enable-sound-alerts or event-sounds is off
#   G-NOTIFY-RESTORE     what a disabled app sent stays out of the date
#                        menu's list; enabling the app brings it back
#                        there at once, with no banner (messageTray.js
#                        _onSourceEnableChanged hides, never drops)
#   G-NOTIFY-EXPANDED    a banner opens collapsed (one body line, no
#                        action buttons) unless critical or its app's
#                        force-expanded is on, read as it shows; the
#                        pointer coming onto it expands and holds it
#                        (messageTray.js _expandBanner)

NL_SCHEMA=org.gnome.desktop.notifications
NL_APP_SCHEMA=org.gnome.desktop.notifications.application
NL_APPS="tuna-lock-private tuna-lock-detailed tuna-lock-hidden tuna-lock-off tuna-sound-on tuna-sound-muted tuna-expand-plain tuna-expand-forced"

nl_app() { printf '%s:/org/gnome/desktop/notifications/application/%s/' "$NL_APP_SCHEMA" "$1"; }

# Notify as installed app $1: summary $2, body $3, urgency byte $4, more
# hints $5, replacing $6, actions $7. Prints the id.
nl_notify() {
    nl_out=$(gdbus call --session --dest org.freedesktop.Notifications \
        --object-path /org/freedesktop/Notifications \
        --method org.freedesktop.Notifications.Notify \
        -- "Tuna Lock Proof" "${6:-0}" "" "$2" "$3" "${7:-[]}" \
        "{'desktop-entry': <'$1'>, 'urgency': <byte $4>${5:+, $5}}" -1) || return 1
    printf '%s %s\n' "$1" "$nl_out" >>"$ARTIFACTS/notify-lock.log"
    printf '%s\n' "$nl_out" | sed -n 's/.*uint32 \([0-9][0-9]*\).*/\1/p'
}

nl_close() {
    gdbus call --session --dest org.freedesktop.Notifications \
        --object-path /org/freedesktop/Notifications \
        --method org.freedesktop.Notifications.CloseNotification "$1" >/dev/null 2>&1 || true
}

# Dump the shell's accessible tree as a11y-notify-$1.json.
nl_dump() {
    nl_tree="$ARTIFACTS/a11y-notify-$1.json"
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$nl_tree" 20 ||
        fail "$2" "accessibility dump failed ($1)"
}

# Any node, showing or not, whose name or description holds $2.
nl_mentions() {
    jq -e --arg s "$2" 'any(.[]; ((.name // "") + " " + (.description // "")) | contains($s))' "$1" >/dev/null
}

# Dump until jq filter $2 holds (the shell redraws on its main loop).
nl_until() {
    nl_end=$(($(date +%s) + 10))
    while :; do
        nl_dump "$1" "$3"
        jq -e "$2" "$nl_tree" >/dev/null && return 0
        [ "$(date +%s)" -lt "$nl_end" ] || return 1
        sleep 0.5
    done
}

# How many notification sounds the player was asked for.
nl_sounds() {
    if [ -f "$ARTIFACTS/canberra-calls" ]; then
        grep -c -- '--description Notification sound' "$ARTIFACTS/canberra-calls" || true
    else
        echo 0
    fi
}

# The bottom of the screen from row $3 down: 0 when $1 and $2 match
# there pixel for pixel, 1 when they differ.
nl_band_same() {
    "$PY" - "$1" "$2" "$3" <<'NL_BAND'
import sys
from PIL import Image, ImageChops
a = Image.open(sys.argv[1]).convert("RGB")
b = Image.open(sys.argv[2]).convert("RGB")
top = int(sys.argv[3])
box = (0, top, a.size[0], a.size[1])
diff = ImageChops.difference(a.crop(box), b.crop(box)).getbbox()
print("same" if diff is None else f"differs in {diff}")
sys.exit(0 if diff is None else 1)
NL_BAND
}

nl_shot() {
    sleep 1
    scrot "$ARTIFACTS/notify-lock-$1.png" >/dev/null
}

notify_lock_proof() {
    nl_desktop_dir="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
    mkdir -p "$nl_desktop_dir"
    for nl_id in $NL_APPS; do
        nl_name=$(printf '%s' "$nl_id" | sed 's/^tuna-/Tuna /; s/-/ /g; s/\b\(.\)/\u\1/g')
        printf '[Desktop Entry]\nName=%s\nExec=true\nType=Application\n' "$nl_name" \
            >"$nl_desktop_dir/$nl_id.desktop"
    done
    nl_children_before=$(gsettings get "$NL_SCHEMA" application-children)
    nl_banners_before=$(gsettings get "$NL_SCHEMA" show-banners)
    nl_event_sounds_before=$(gsettings get org.gnome.desktop.sound event-sounds)
    gsettings set "$NL_SCHEMA" show-banners true
    gsettings set "$NL_SCHEMA" show-in-lock-screen true
    gsettings set org.gnome.desktop.sound event-sounds true
    gsettings set "$(nl_app tuna-lock-detailed)" details-in-lock-screen true
    gsettings set "$(nl_app tuna-lock-hidden)" show-in-lock-screen false
    gsettings set "$(nl_app tuna-lock-off)" enable false
    gsettings set "$(nl_app tuna-sound-muted)" enable-sound-alerts false
    sleep 1
    nl_ids=""

    # Lock, then notify: nothing is seen, so everything waits for the
    # lock screen and, after it, the banners.
    xdotool key super+l
    cstate '.locked' "locked for notifications" 10 || fail G-NOTIFY-LOCK "Super+L did not lock"
    sleep 2
    xdotool mousemove --window "$WID" 40 80
    nl_sounds_locked=$(nl_sounds)
    nl_private=$(nl_notify tuna-lock-private "Private Summary" "PRIVATE-BODY-ONE secret" 1) ||
        fail G-NOTIFY-LOCK "Notify failed"
    nl_critical=$(nl_notify tuna-lock-private "Private Critical" "PRIVATE-BODY-TWO secret" 2 \
        "'sound-name': <'message-new-instant'>") || fail G-NOTIFY-LOCK "Notify failed"
    nl_detailed=$(nl_notify tuna-lock-detailed "Detailed Summary" "Detailed body text" 1) ||
        fail G-NOTIFY-LOCK "Notify failed"
    nl_hidden=$(nl_notify tuna-lock-hidden "Hidden Summary" "HIDDEN-BODY" 2) ||
        fail G-NOTIFY-LOCK "Notify failed"
    nl_off=$(nl_notify tuna-lock-off "Off Summary" "OFF-BODY" 2) || fail G-NOTIFY-LOCK "Notify failed"
    nl_ids="$nl_private $nl_critical $nl_detailed $nl_hidden $nl_off"
    nl_until locked '[.[] | select(.showing)] | (any(.name == "Tuna Lock Private") and any(.name == "Detailed Summary Detailed body text"))' G-NOTIFY-LOCK ||
        fail G-NOTIFY-LOCK "the lock screen does not list the two allowed sources"
    nl_locked="$nl_tree"
    showing "$nl_locked" "Tuna Lock Detailed" || fail G-NOTIFY-LOCK "the detailed source is not named"
    # Two unseen, one of them critical: a count, nothing more.
    showing "$nl_locked" "2" || fail G-NOTIFY-LOCK "the redacted source does not count its two notifications"
    # No title or body of a redacted, hidden or disabled source anywhere
    # in the tree, shown or not.
    for nl_secret in "Private Summary" "Private Critical" "PRIVATE-BODY" \
        "Hidden Summary" "HIDDEN-BODY" "Off Summary" "OFF-BODY"; do
        if nl_mentions "$nl_locked" "$nl_secret"; then
            fail G-NOTIFY-LOCK "the locked shell's accessible tree holds \"$nl_secret\""
        fi
    done
    # App names also label the (unshown) app grid: only what shows counts.
    for nl_secret in "Tuna Lock Hidden" "Tuna Lock Off"; do
        if showing "$nl_locked" "$nl_secret"; then
            fail G-NOTIFY-LOCK "\"$nl_secret\" shows on the lock screen"
        fi
    done
    [ "$(nl_sounds)" = "$nl_sounds_locked" ] ||
        fail G-NOTIFY-LOCK "a notification sound played while locked (no banner shows there)"
    cstate '.locked' "still locked with notifications" 2 || fail G-NOTIFY-LOCK "a notification unlocked the session"
    pass G-NOTIFY-LOCK "locked: no banner or sound; allowed apps show name and count, details only with details-in-lock-screen, critical reveals nothing; show-in-lock-screen off and disabled apps are absent from the tree"

    # Pixels: below the clock, the redacted source draws the same frame
    # whatever its text says.
    nl_top=$(jq -r '[.[] | select(.showing and .name == "Click or press a key" and .bounds != null) | .bounds[1] + .bounds[3]][0] // empty' "$nl_locked")
    [ -n "$nl_top" ] || nl_top=$((HEIGHT * 2 / 3 + 10))
    nl_shot redacted
    nl_replaced=$(nl_notify tuna-lock-private "Replaced With A Much Longer Title" \
        "WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW MMMMMMMMMMMMMMMMMMMMMMMMMM" 2 \
        "'sound-name': <'message-new-instant'>" "$nl_critical") || fail G-NOTIFY-LOCK-PIXELS "Notify failed"
    [ "$nl_replaced" = "$nl_critical" ] || fail G-NOTIFY-LOCK-PIXELS "the replacement got a new id"
    nl_until replaced 'any(.[]; .showing and .name == "Tuna Lock Private")' G-NOTIFY-LOCK-PIXELS ||
        fail G-NOTIFY-LOCK-PIXELS "the redacted source left after a replacement"
    nl_mentions "$nl_tree" "Much Longer" && fail G-NOTIFY-LOCK-PIXELS "the replaced title reached the tree"
    nl_shot replaced
    nl_band_same "$ARTIFACTS/notify-lock-redacted.png" "$ARTIFACTS/notify-lock-replaced.png" "$nl_top" ||
        fail G-NOTIFY-LOCK-PIXELS "the redacted source's pixels changed with its text"
    gsettings set "$(nl_app tuna-lock-private)" details-in-lock-screen true
    nl_until detailed 'any(.[]; .showing and .name == "Replaced With A Much Longer Title WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW MMMMMMMMMMMMMMMMMMMMMMMMMM")' G-NOTIFY-LOCK-PIXELS ||
        fail G-NOTIFY-LOCK-PIXELS "details-in-lock-screen on did not show the details at once"
    nl_shot detailed
    if nl_band_same "$ARTIFACTS/notify-lock-replaced.png" "$ARTIFACTS/notify-lock-detailed.png" "$nl_top"; then
        fail G-NOTIFY-LOCK-PIXELS "allowing details did not change the lock screen's pixels"
    fi
    gsettings set "$(nl_app tuna-lock-private)" details-in-lock-screen false
    nl_until redacted-again '(any(.[]; .showing and .name == "Tuna Lock Private")) and (any(.[]; (.name // "") | contains("Much Longer")) | not)' G-NOTIFY-LOCK-PIXELS ||
        fail G-NOTIFY-LOCK-PIXELS "details-in-lock-screen off did not take the details back"
    nl_shot redacted-again
    nl_band_same "$ARTIFACTS/notify-lock-replaced.png" "$ARTIFACTS/notify-lock-redacted-again.png" "$nl_top" ||
        fail G-NOTIFY-LOCK-PIXELS "taking details back did not restore the redacted frame"
    pass G-NOTIFY-LOCK-PIXELS "a redacted source's pixels are the same whatever its text; details change them and taking them back restores the frame exactly"

    # Live: a new arrival, the global switch.
    nl_second=$(nl_notify tuna-lock-detailed "Detailed Second" "Second body" 1) ||
        fail G-NOTIFY-LOCK-LIVE "Notify failed"
    nl_ids="$nl_ids $nl_second"
    nl_until arrival 'any(.[]; .showing and .name == "Detailed Second Second body")' G-NOTIFY-LOCK-LIVE ||
        fail G-NOTIFY-LOCK-LIVE "a notification arriving while locked did not reach the lock screen"
    gsettings set "$NL_SCHEMA" show-in-lock-screen false
    nl_until global-off '(any(.[]; .showing and ((.name // "") | startswith("Tuna Lock") or startswith("Detailed")))) | not' G-NOTIFY-LOCK-LIVE ||
        fail G-NOTIFY-LOCK-LIVE "the global show-in-lock-screen off left sources on the lock screen"
    gsettings set "$NL_SCHEMA" show-in-lock-screen true
    nl_until global-on 'any(.[]; .showing and .name == "Tuna Lock Private")' G-NOTIFY-LOCK-LIVE ||
        fail G-NOTIFY-LOCK-LIVE "the global show-in-lock-screen on did not bring the sources back"
    [ "$(nl_sounds)" = "$nl_sounds_locked" ] ||
        fail G-NOTIFY-LOCK-LIVE "a notification sound played while locked"
    xdotool mousemove --window "$WID" $((WIDTH / 2)) $((HEIGHT * 3 / 4)) click 1
    sleep 1
    xdotool type --delay 30 "$UNLOCK_PASSWORD"
    xdotool key Return
    cstate '(.locked | not)' "unlocked after notifications" 15 || fail G-NOTIFY-LOCK-LIVE "PAM did not unlock"
    # The critical one leads the queue; its banner plays its sound.
    nl_until unlocked 'any(.[]; .showing and .name == "Replaced With A Much Longer Title")' G-NOTIFY-LOCK-LIVE ||
        fail G-NOTIFY-LOCK-LIVE "the queued critical banner did not show after unlocking"
    nl_mentions "$nl_tree" "Detailed Second Second body" &&
        fail G-NOTIFY-LOCK-LIVE "the lock screen's list outlived the unlock"
    [ "$(nl_sounds)" -gt "$nl_sounds_locked" ] ||
        fail G-NOTIFY-LOCK-LIVE "the critical banner shown after unlocking played no sound"
    grep -q -- '--id message-new-instant --description Notification sound' "$ARTIFACTS/canberra-calls" ||
        fail G-NOTIFY-LOCK-LIVE "the banner's sound-name did not reach the event-sound player"
    pass G-NOTIFY-LOCK-LIVE "arrivals and the global and per-app keys reach the locked screen at once; unlocking takes the list away and the queued critical banner shows with its sound"
    # Take every banner down, earlier gates' included (history keeps
    # them), so each sound below belongs to the banner it waits for. Ids
    # only grow, and history holds the last 50.
    nl_last=$nl_second
    nl_first=$((nl_last > 100 ? nl_last - 100 : 1))
    for nl_id in $(seq "$nl_first" "$nl_last"); do nl_close "$nl_id"; done
    sleep 1

    # Sounds as banners show (unlocked).
    nl_sound_step() { # app summary hints expect(+1|0) message
        nl_before=$(nl_sounds)
        nl_id=$(nl_notify "$1" "$2" "sound body" 1 "$3") || fail G-NOTIFY-SOUND "Notify failed"
        nl_until "sound-$nl_before-$1" "any(.[]; .showing and .name == \"$2\")" G-NOTIFY-SOUND ||
            fail G-NOTIFY-SOUND "no banner for \"$2\""
        sleep 0.5
        [ "$(nl_sounds)" = $((nl_before + $4)) ] || fail G-NOTIFY-SOUND "$5"
        nl_close "$nl_id"
        sleep 1
    }
    nl_sound_step tuna-sound-muted "Muted Sound" "'sound-name': <'bell'>" 0 \
        "enable-sound-alerts off still played a sound"
    nl_sound_step tuna-sound-on "Named Sound" "'sound-name': <'dialog-information'>" 1 \
        "a banner with sound-name played nothing"
    tail -1 "$ARTIFACTS/canberra-calls" | grep -q -- '--id dialog-information --description Notification sound' ||
        fail G-NOTIFY-SOUND "sound-name did not play from the theme"
    nl_sound_step tuna-sound-on "File Sound" "'sound-file': <'/usr/share/sounds/tuna-proof.oga'>" 1 \
        "a banner with sound-file played nothing"
    tail -1 "$ARTIFACTS/canberra-calls" | grep -q -- '--file /usr/share/sounds/tuna-proof.oga --description Notification sound' ||
        fail G-NOTIFY-SOUND "sound-file did not play the file"
    gsettings set org.gnome.desktop.sound event-sounds false
    sleep 1
    nl_sound_step tuna-sound-on "Quiet Sound" "'sound-name': <'dialog-information'>" 0 \
        "event-sounds off still played a notification sound"
    gsettings set org.gnome.desktop.sound event-sounds true
    pass G-NOTIFY-SOUND "a banner plays its sound-name, else its sound-file, through the event-sound player as it shows; enable-sound-alerts or event-sounds off keeps it silent"

    # CloseNotification dismissed the earlier banners but retained history,
    # including its long-word cards. Add fresh summaries so this gate can
    # distinguish an enabled source from the app still switched off.
    nl_listed=$(nl_notify tuna-lock-private "Listed Summary" "listed body" 1) ||
        fail G-NOTIFY-RESTORE "Notify failed"
    nl_off_again=$(nl_notify tuna-lock-off "Off Again" "OFF-AGAIN-BODY" 1) ||
        fail G-NOTIFY-RESTORE "Notify failed"
    sleep 1
    # The last sound banner can still own a pointer grab. Open through the
    # actual message-tray binding rather than clicking through that grab.
    xdotool key super+v
    sleep 2
    nl_shot restore-menu
    nl_until restore-disabled '[.[] | select(.showing)] | (any(.name == "Clear") and any(.name == "Listed Summary"))' G-NOTIFY-RESTORE ||
        fail G-NOTIFY-RESTORE "the date menu does not list the history"
    nl_mentions "$nl_tree" "Off Again" &&
        fail G-NOTIFY-RESTORE "a disabled app's notification reached the list"
    gsettings set "$(nl_app tuna-lock-off)" enable true
    nl_until restored '[.[] | select(.showing)] | (any(.name == "Clear") and any(.name == "Off Again"))' G-NOTIFY-RESTORE ||
        fail G-NOTIFY-RESTORE "enabling the app did not bring its notification back to the list"
    nl_shot restored
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-notify-restore-cleared.json" 20 \
        --press "Clear" || fail G-NOTIFY-RESTORE "could not press Clear"
    xdotool key Escape
    sleep 2
    nl_dump restored-closed G-NOTIFY-RESTORE
    showing "$nl_tree" "Off Again" && fail G-NOTIFY-RESTORE "a restored notification showed a banner"
    nl_close "$nl_listed"
    nl_close "$nl_off_again"
    pass G-NOTIFY-RESTORE "a disabled app's notification stays out of the list; enabling the app brings it back at once, with no banner"
    # Expanded banners: collapsed is one body line and no actions.
    gsettings set "$(nl_app tuna-expand-forced)" force-expanded true
    nl_long="A long body that runs on well past the single line a collapsed banner keeps, so only an expanded banner shows the rest of it."
    nl_body_h() { # tree: the body label's height
        jq -r --arg b "$nl_long" '[.[] | select(.showing and .name == $b and .role == "label" and .bounds != null) | .bounds[3]][0] // empty' "$1"
    }
    nl_expand_step() { # app summary urgency expect(collapsed|expanded) message
        xdotool mousemove --window "$WID" $((WIDTH / 3)) $((HEIGHT / 2))
        nl_id=$(nl_notify "$1" "$2" "$nl_long" "$3" "" 0 "['reply', 'Reply']") ||
            fail G-NOTIFY-EXPANDED "Notify failed"
        nl_until "expand-$1-$3" "any(.[]; .showing and .name == \"$2\")" G-NOTIFY-EXPANDED ||
            fail G-NOTIFY-EXPANDED "no banner for \"$2\""
        sleep 0.5
        nl_dump "expand-$1-$3" G-NOTIFY-EXPANDED
        nl_h=$(nl_body_h "$nl_tree")
        [ -n "$nl_h" ] || fail G-NOTIFY-EXPANDED "\"$2\" shows no body"
        if [ "$4" = expanded ]; then
            button_showing "$nl_tree" "Reply" || fail G-NOTIFY-EXPANDED "$5: no action button"
            [ "$nl_h" -gt $((nl_line_h * 3 / 2)) ] ||
                fail G-NOTIFY-EXPANDED "$5: the body is $nl_h px, one line is $nl_line_h px"
        else
            button_showing "$nl_tree" "Reply" && fail G-NOTIFY-EXPANDED "$5: the action shows collapsed"
            nl_line_h=$nl_h
        fi
        nl_shot "expand-$1-$3"
    }
    nl_expand_step tuna-expand-plain "Plain Banner" 1 collapsed "a plain banner opened expanded"
    # The pointer onto the banner expands it, and holds it there.
    xdotool mousemove --window "$WID" $((WIDTH / 2)) 60
    nl_until expand-hover 'any(.[]; .showing and .name == "Reply" and (.role == "push button" or .role == "button"))' G-NOTIFY-EXPANDED ||
        fail G-NOTIFY-EXPANDED "the pointer on a banner did not expand it"
    nl_hover_h=$(nl_body_h "$nl_tree")
    [ "${nl_hover_h:-0}" -gt $((nl_line_h * 3 / 2)) ] ||
        fail G-NOTIFY-EXPANDED "hover showed the actions but the body stayed ${nl_hover_h:-0} px"
    nl_shot expand-hover
    sleep 5
    nl_dump expand-held G-NOTIFY-EXPANDED
    showing "$nl_tree" "Plain Banner" || fail G-NOTIFY-EXPANDED "the banner under the pointer timed out"
    # Off it, the banner goes.
    xdotool mousemove --window "$WID" $((WIDTH / 3)) $((HEIGHT / 2))
    sleep 1.5
    nl_dump expand-left G-NOTIFY-EXPANDED
    showing "$nl_tree" "Plain Banner" && fail G-NOTIFY-EXPANDED "the banner stayed once the pointer left it"
    nl_close "$nl_id"
    sleep 1
    nl_expand_step tuna-expand-forced "Forced Banner" 1 expanded "force-expanded"
    nl_close "$nl_id"
    sleep 1
    nl_expand_step tuna-expand-plain "Critical Banner" 2 expanded "a critical banner"
    nl_close "$nl_id"
    sleep 1
    # The key is read as each banner shows.
    gsettings set "$(nl_app tuna-expand-forced)" force-expanded false
    sleep 1
    nl_expand_step tuna-expand-forced "Unforced Banner" 1 collapsed "force-expanded off still opened expanded"
    nl_close "$nl_id"
    sleep 1
    pass G-NOTIFY-EXPANDED "banners open collapsed (one body line, no actions) unless critical or force-expanded, read as each shows; the pointer expands a banner and holds it until it leaves"

    # As found for the gates after.
    for nl_id in $NL_APPS; do
        for nl_key in enable enable-sound-alerts show-in-lock-screen details-in-lock-screen force-expanded application-id; do
            gsettings reset "$(nl_app "$nl_id")" "$nl_key"
        done
        rm -f "$nl_desktop_dir/$nl_id.desktop"
    done
    gsettings set "$NL_SCHEMA" application-children "$nl_children_before"
    gsettings reset "$NL_SCHEMA" show-in-lock-screen
    gsettings set "$NL_SCHEMA" show-banners "$nl_banners_before"
    gsettings set org.gnome.desktop.sound event-sounds "$nl_event_sounds_before"
}
