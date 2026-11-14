#!/bin/sh
# Sourced by tuna-gtk-shell-proof, also used by its focused
# --datemenu-only run (P-PN-04, #1036). Requires that harness's private
# bus, nested window, GTK shell, AT-SPI and assertion helpers (pass,
# fail, showing, hidden, cstate, kr_focus_until, datemenu_mask,
# weather_fixture, $PY, $ROOT, $ARTIFACTS, $WID, $WIDTH, $HEIGHT) and
# its calendar-server stub.

# The date menu's own gates: G-CAL-OPEN, G-CAL-EVENTS, G-CAL-PREFS and
# G-CAL-LAUNCH. They leave the menu closed and its first tree in
# a11y-calendar.json, which datemenu_proof reads.
calendar_proof() {
xdotool mousemove --window "$WID" $((WIDTH / 2)) 16 click 1
sleep 2
scrot "$ARTIFACTS/02-calendar.png" >/dev/null
"$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-calendar.json" 20 ||
    fail G-CAL-OPEN "accessibility dump failed"
showing "$ARTIFACTS/a11y-calendar.json" "No Notifications" ||
    fail G-CAL-OPEN "calendar popover contents not showing"
pass G-CAL-OPEN "clock opens the calendar and notification list"
for name in "Design review" "Release day" "All Day" "10:00 – 10:30"; do
    showing "$ARTIFACTS/a11y-calendar.json" "$name" ||
        fail G-CAL-EVENTS "the events card does not show \"$name\""
done
if showing "$ARTIFACTS/a11y-calendar.json" "Retro"; then
    fail G-CAL-EVENTS "tomorrow's event shows under today"
fi
pass G-CAL-EVENTS "today's calendar-server events list in the events card, timed as GNOME does"
# The open popover must update from real GSettings changes without a restart.
"$PY" "$ROOT/scripts/lib/tuna-calendar-preferences-proof.py" "$ARTIFACTS/a11y-calendar.json" default false ||
    fail G-CAL-PREFS "initial locale calendar is invalid"
for first in Monday Thursday Sunday; do
    gsettings set org.gnome.desktop.calendar week-start-day "$(printf '%s' "$first" | tr '[:upper:]' '[:lower:]')"
    gsettings set org.gnome.desktop.calendar show-weekdate true
    sleep 1
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-calendar-$first.json" 20 ||
        fail G-CAL-PREFS "live calendar tree is unavailable"
    "$PY" "$ROOT/scripts/lib/tuna-calendar-preferences-proof.py" "$ARTIFACTS/a11y-calendar-$first.json" "$first" true ||
        fail G-CAL-PREFS "weekday order or ISO week numbers did not follow $first"
done
gsettings reset org.gnome.desktop.calendar week-start-day
gsettings reset org.gnome.desktop.calendar show-weekdate
sleep 1
"$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-calendar-reset.json" 20 ||
    fail G-CAL-PREFS "reset calendar tree is unavailable"
"$PY" "$ROOT/scripts/lib/tuna-calendar-preferences-proof.py" "$ARTIFACTS/a11y-calendar-reset.json" default false ||
    fail G-CAL-PREFS "locale reset or hiding week numbers failed"
jq '[.[] | select(.showing and .role == "label" and (.name | test("^(Sunday|Monday|Tuesday|Wednesday|Thursday|Friday|Saturday)$"))) | .name]' "$ARTIFACTS/a11y-calendar.json" >"$ARTIFACTS/calendar-initial-headings.json"
jq '[.[] | select(.showing and .role == "label" and (.name | test("^(Sunday|Monday|Tuesday|Wednesday|Thursday|Friday|Saturday)$"))) | .name]' "$ARTIFACTS/a11y-calendar-reset.json" >"$ARTIFACTS/calendar-reset-headings.json"
cmp "$ARTIFACTS/calendar-initial-headings.json" "$ARTIFACTS/calendar-reset-headings.json" ||
    fail G-CAL-PREFS "reset did not restore the initial locale headings"
scrot "$ARTIFACTS/02-calendar-reset.png" >/dev/null
pass G-CAL-PREFS "live weekday changes, six ISO week labels and locale reset are visible in the same shell"
"$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-calendar-launch.json" 20 --press "Open Calendar" ||
    fail G-CAL-LAUNCH "events card has no accessible launch action"
sleep 1
[ "$(cat "$ARTIFACTS/calendar-launch" 2>/dev/null)" = "calendar:///$(date +%F)" ] ||
    fail G-CAL-LAUNCH "default calendar did not launch at the selected day"
"$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-calendar-closed.json" 20
if showing "$ARTIFACTS/a11y-calendar-closed.json" "Open Calendar"; then
    fail G-CAL-LAUNCH "calendar popover stayed open after launch"
fi
pass G-CAL-LAUNCH "default calendar launches selected day through Tuna Desktop and closes the menu"
}

# World clocks and weather (P-PN-04; dateMenu.js WorldClocksSection and
# WeatherSection). Clocks and Weather were masked until now, which hid
# both cards; installing probe entries shows them in the running shell.
# The clocks come from gnome-shell's own setting through GWeather's
# locations database, the forecast from the TUNA_WEATHER_FIXTURE file.
# Runs after calendar_proof, with the menu closed, and leaves it open
# without either card.
datemenu_proof() {
    datemenu_dump() {
        "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-datemenu-$1.json" 20 ||
            fail "$2" "accessibility dump failed ($1)"
    }
    datemenu_open() {
        xdotool mousemove --window "$WID" $((WIDTH / 2)) 16 click 1
        sleep 2
        datemenu_dump "$1" "$2"
    }
    datemenu_close() {
        xdotool key Escape
        sleep 1
    }
    # An installed app whose launch leaves a marker. NoDisplay keeps it
    # out of the app grid, as it does not stop GNOME finding the app.
    datemenu_app() {
        rm -f "$ARTIFACTS/$1-launch"
        printf '#!/bin/sh\necho launched >"%s"\n' "$ARTIFACTS/$1-launch" >"$ARTIFACTS/$1-probe"
        chmod +x "$ARTIFACTS/$1-probe"
        printf '[Desktop Entry]\nType=Application\nName=%s\nExec=%s\nNoDisplay=true\n' \
            "$2" "$ARTIFACTS/$1-probe" >"$ARTIFACTS/$1.desktop"
        mv "$ARTIFACTS/$1.desktop" "$XDG_DATA_HOME/applications/$1.desktop"
    }
    # A showing node whose name matches the regular expression.
    showing_like() {
        jq -e --arg re "$2" 'any(.[]; .showing and (.name | test($re)))' "$1" >/dev/null
    }
    # How many showing labels match the regular expression.
    showing_labels() {
        jq --arg re "$2" '[.[] | select(.showing and .role == "label" and (.name | test($re)))] | length' "$1"
    }
    datemenu_layout() {
        "$PY" "$ROOT/scripts/lib/tuna-datemenu-layout.py" "$@"
    }
    # The labels inside the showing card named $2, as a JSON array (null
    # without such a card).
    card_labels() {
        jq -c --arg n "$2" '. as $all
            | [range(length) | select($all[.].showing and $all[.].name == $n
                and ($all[.].role == "push button" or $all[.].role == "button"))][0] as $i
            | if $i == null then null else
                [$all[($i + 1):] | label $card | .[]
                    | if .depth <= $all[$i].depth then break $card else . end
                    | select(.showing and .role == "label") | .name]
              end' "$1"
    }
    datemenu_tokyo="<(uint32 2, <('Tokyo', 'RJTI', true, [(0.62191898430954862, 2.4408429589140699)], [(0.62282074357417661, 2.4391218722853854)])>)>"
    datemenu_new_york="<(uint32 2, <('New York', 'KNYC', true, [(0.71180344078725644, -1.2909618758762367)], [(0.71059804659265924, -1.2916478949920254)])>)>"
    datemenu_kathmandu="<(uint32 2, <('Kathmandu', 'VNKT', true, [(0.48345620280242924, 1.4899294106035903)], [(0.48374709682885919, 1.489056745977593)])>)>"
    for name in "World Clocks" "Add World Clocks…"; do
        hidden "$ARTIFACTS/a11y-calendar.json" "$name" ||
            fail G-DATEMENU-WORLD-CLOCKS "the world clocks card shows without GNOME Clocks installed"
    done
    for name in "Weather" "Select Weather Location…"; do
        hidden "$ARTIFACTS/a11y-calendar.json" "$name" ||
            fail G-DATEMENU-WEATHER "the weather card shows without GNOME Weather installed"
    done
    # The closed menu's frame, with the pointer where datemenu_open leaves
    # it: what the open menu is measured against.
    xdotool mousemove --window "$WID" $((WIDTH / 2)) 16
    sleep 1
    scrot "$ARTIFACTS/02a-datemenu-closed.png" >/dev/null
    datemenu_app org.gnome.clocks "Clocks"
    datemenu_app org.gnome.Weather "Weather"
    sleep 1
    datemenu_open empty G-DATEMENU-WORLD-CLOCKS
    scrot "$ARTIFACTS/02b-datemenu-prompts.png" >/dev/null
    showing "$ARTIFACTS/a11y-datemenu-empty.json" "Add World Clocks…" ||
        fail G-DATEMENU-WORLD-CLOCKS "installing Clocks did not show the card asking for world clocks"
    showing "$ARTIFACTS/a11y-datemenu-empty.json" "Select Weather Location…" ||
        fail G-DATEMENU-WEATHER "installing Weather did not show the card asking for a location"
    # Each prompt is all its card holds: no clock rows, and neither a
    # location nor a status line.
    [ "$(card_labels "$ARTIFACTS/a11y-datemenu-empty.json" "Add World Clocks…")" = '["Add World Clocks…"]' ] ||
        fail G-DATEMENU-WORLD-CLOCKS "the card without world clocks holds more than its prompt"
    [ "$(card_labels "$ARTIFACTS/a11y-datemenu-empty.json" "Select Weather Location…")" = '["Select Weather Location…"]' ] ||
        fail G-DATEMENU-WEATHER "the card without a location holds more than its prompt"
    # Stored east first: GNOME lists the clocks by UTC offset. The menu
    # stays open, so the rows must follow the setting live.
    gsettings set org.gnome.shell.world-clocks locations "[$datemenu_tokyo, $datemenu_new_york]"
    sleep 1
    clocks_from="$(date +%s)"
    datemenu_dump clocks G-DATEMENU-WORLD-CLOCKS
    clocks_to="$(date +%s)"
    clocks="$ARTIFACTS/a11y-datemenu-clocks.json"
    showing "$clocks" "World Clocks" ||
        fail G-DATEMENU-WORLD-CLOCKS "the header did not become World Clocks"
    hidden "$clocks" "Add World Clocks…" ||
        fail G-DATEMENU-WORLD-CLOCKS "the header still asks for world clocks"
    jq -e '[.[] | select(.showing) | .name] | index("New York") as $west | index("Tokyo") as $east
        | $west != null and $east != null and $west < $east' "$clocks" >/dev/null ||
        fail G-DATEMENU-WORLD-CLOCKS "New York and Tokyo are not listed in UTC-offset order"
    [ "$(showing_labels "$clocks" '^[+-][0-9]{1,2}(:[0-9]{1,2})?$')" = 2 ] ||
        fail G-DATEMENU-WORLD-CLOCKS "each clock needs one offset from local time"
    # A 12-hour time pads a single-digit hour with a figure space.
    [ "$(showing_labels "$clocks" '^[\s\x{2007}]*[0-9]{1,2}:[0-9]{2}( [AP]M)?$')" = 2 ] ||
        fail G-DATEMENU-WORLD-CLOCKS "each clock needs its time"
    # City, time there and offset from here, as gnome-shell writes them,
    # in the grid's three columns.
    datemenu_layout clocks "$clocks" 24h "$clocks_from" "$clocks_to" \
        "New York=America/New_York" "Tokyo=Asia/Tokyo" ||
        fail G-DATEMENU-WORLD-CLOCKS "the clocks do not show each city's time and offset in aligned columns"
    clocks_minute="$(date -d "@$clocks_to" +%M)"
    clocks_before="$(card_labels "$clocks" "World Clocks")"
    # A third clock a fraction of an hour away, between entries that are
    # no locations: GNOME skips those. Still the same open menu.
    gsettings set org.gnome.shell.world-clocks locations "[<'not a location'>, $datemenu_tokyo, <(uint32 9, <'unknown format'>)>, $datemenu_kathmandu, <(uint32 2, <('Nowhere', 'ZZZZ', true, [(0.1, 0.1)], [(0.1, 0.1)])>)>, $datemenu_new_york]"
    sleep 1
    clocks_from="$(date +%s)"
    datemenu_dump clocks-three G-DATEMENU-WORLD-CLOCKS
    clocks_to="$(date +%s)"
    datemenu_layout clocks "$ARTIFACTS/a11y-datemenu-clocks-three.json" 24h "$clocks_from" "$clocks_to" \
        "New York=America/New_York" "Kathmandu=Asia/Kathmandu" "Tokyo=Asia/Tokyo" ||
        fail G-DATEMENU-WORLD-CLOCKS "a live third clock among entries that are no locations is not listed in offset order"
    scrot "$ARTIFACTS/02b-datemenu-clocks.png" >/dev/null
    # clock-format reaches the open card: %l:%M %p.
    clock_format_before="$(gsettings get org.gnome.desktop.interface clock-format)"
    gsettings set org.gnome.desktop.interface clock-format '12h'
    sleep 1
    clocks_from="$(date +%s)"
    datemenu_dump clocks-12h G-DATEMENU-WORLD-CLOCKS
    clocks_to="$(date +%s)"
    datemenu_layout clocks "$ARTIFACTS/a11y-datemenu-clocks-12h.json" 12h "$clocks_from" "$clocks_to" \
        "New York=America/New_York" "Kathmandu=Asia/Kathmandu" "Tokyo=Asia/Tokyo" ||
        fail G-DATEMENU-WORLD-CLOCKS "the open card did not follow clock-format 12h"
    gsettings set org.gnome.desktop.interface clock-format "$clock_format_before"
    gsettings set org.gnome.shell.world-clocks locations "[$datemenu_tokyo, $datemenu_new_york]"
    # The open card ticks over with the minute, as the panel clock does.
    while [ "$(date +%M)" = "$clocks_minute" ]; do sleep 1; done
    sleep 2
    clocks_from="$(date +%s)"
    datemenu_dump clocks-tick G-DATEMENU-WORLD-CLOCKS
    clocks_to="$(date +%s)"
    datemenu_layout clocks "$ARTIFACTS/a11y-datemenu-clocks-tick.json" 24h "$clocks_from" "$clocks_to" \
        "New York=America/New_York" "Tokyo=Asia/Tokyo" ||
        fail G-DATEMENU-WORLD-CLOCKS "the open card's clocks did not move on with the minute"
    [ "$(card_labels "$ARTIFACTS/a11y-datemenu-clocks-tick.json" "World Clocks")" != "$clocks_before" ] ||
        fail G-DATEMENU-WORLD-CLOCKS "a minute later the open card still shows the old times"
    # Without clocks the open card asks for them again.
    gsettings set org.gnome.shell.world-clocks locations "[]"
    sleep 1
    datemenu_dump clocks-none G-DATEMENU-WORLD-CLOCKS
    [ "$(card_labels "$ARTIFACTS/a11y-datemenu-clocks-none.json" "Add World Clocks…")" = '["Add World Clocks…"]' ] ||
        fail G-DATEMENU-WORLD-CLOCKS "removing every clock did not bring the prompt back"
    # GNOME Clocks itself: when it appears on the bus the shell copies the
    # Locations it exports (org.gnome.Shell.ClocksIntegration) into its
    # own setting, follows their changes, and keeps them once Clocks has
    # gone. The Clocks here is a stand-in that never gets started by the
    # shell.
    printf '%s\n' "{'Locations': <[$datemenu_kathmandu]>}" >"$ARTIFACTS/clocks-exported"
    "$PY" "$ROOT/scripts/lib/tuna-shell-integration-stub.py" clocks "$ARTIFACTS/clocks-exported" \
        >"$ARTIFACTS/clocks-stub.log" 2>&1 &
    DATEMENU_STUB_PID=$!
    for _ in $(seq 1 20); do grep -q ready "$ARTIFACTS/clocks-stub.log" 2>/dev/null && break; sleep 0.5; done
    grep -q ready "$ARTIFACTS/clocks-stub.log" 2>/dev/null ||
        fail G-DATEMENU-WORLD-CLOCKS "the GNOME Clocks stand-in did not start"
    sleep 1
    case "$(gsettings get org.gnome.shell.world-clocks locations)" in
        *"'VNKT'"*) ;;
        *) fail G-DATEMENU-WORLD-CLOCKS "the shell did not store the locations a running Clocks exports" ;;
    esac
    clocks_from="$(date +%s)"
    datemenu_dump clocks-exported G-DATEMENU-WORLD-CLOCKS
    clocks_to="$(date +%s)"
    datemenu_layout clocks "$ARTIFACTS/a11y-datemenu-clocks-exported.json" 24h "$clocks_from" "$clocks_to" \
        "Kathmandu=Asia/Kathmandu" ||
        fail G-DATEMENU-WORLD-CLOCKS "the open card does not show the clock a running Clocks exports"
    printf '%s\n' "{'Locations': <[$datemenu_tokyo, $datemenu_new_york]>}" >"$ARTIFACTS/clocks-exported.tmp"
    mv "$ARTIFACTS/clocks-exported.tmp" "$ARTIFACTS/clocks-exported"
    sleep 2
    kill "$DATEMENU_STUB_PID" 2>/dev/null || true
    wait "$DATEMENU_STUB_PID" 2>/dev/null || true
    DATEMENU_STUB_PID=""
    sleep 1
    clocks_from="$(date +%s)"
    datemenu_dump clocks-kept G-DATEMENU-WORLD-CLOCKS
    clocks_to="$(date +%s)"
    datemenu_layout clocks "$ARTIFACTS/a11y-datemenu-clocks-kept.json" 24h "$clocks_from" "$clocks_to" \
        "New York=America/New_York" "Tokyo=Asia/Tokyo" ||
        fail G-DATEMENU-WORLD-CLOCKS "the card did not follow Clocks' changed locations, or lost them when Clocks went"
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-datemenu-clocks-launch.json" 20 --press "World Clocks" ||
        fail G-DATEMENU-WORLD-CLOCKS "the world clocks card has no accessible action"
    sleep 1
    [ -f "$ARTIFACTS/org.gnome.clocks-launch" ] ||
        fail G-DATEMENU-WORLD-CLOCKS "the card did not activate GNOME Clocks"
    if showing "$ARTIFACTS/a11y-datemenu-clocks-launch.json" "World Clocks"; then
        fail G-DATEMENU-WORLD-CLOCKS "the date menu stayed open after activating Clocks"
    fi
    pass G-DATEMENU-WORLD-CLOCKS "installing Clocks shows its world clocks live, sorted by UTC offset in aligned columns, each with GNOME's time and offset; they follow the setting, a running Clocks' exported locations, clock-format and the minute while the menu is open; the card activates Clocks and closes the menu"

    # Each state reads exactly as gnome-shell 51.0 has it: the title, the
    # location, and one status line in place of the forecasts.
    for state in "loading=Loading…" "offline=Go Online for Weather Information" \
        "unavailable=Weather Information Unavailable"; do
        weather_fixture "{\"state\": \"${state%%=*}\", \"location\": \"Lisbon\"}"
        datemenu_open "${state%%=*}" G-DATEMENU-WEATHER
        scrot "$ARTIFACTS/02b-datemenu-weather-${state%%=*}.png" >/dev/null
        [ "$(card_labels "$ARTIFACTS/a11y-datemenu-${state%%=*}.json" "Weather" |
            jq -c --arg s "${state#*=}" '. == ["Weather", "Lisbon", $s]')" = true ] ||
            fail G-DATEMENU-WEATHER "the ${state%%=*} state does not read Weather, Lisbon, \"${state#*=}\""
        hidden "$ARTIFACTS/a11y-datemenu-${state%%=*}.json" "Select Weather Location…" ||
            fail G-DATEMENU-WEATHER "the header still asks for a location"
        datemenu_close
    done
    # Eight forecasts: one already past, one under an hour after the
    # first shown, and one beyond GNOME's five.
    weather_fixture '{"state": "forecast", "location": "Lisbon", "forecasts": [
 {"in_minutes": -30, "icon": "weather-clear-symbolic", "temp": 40},
 {"in_minutes": 30, "icon": "weather-few-clouds-symbolic", "temp": 21.4},
 {"in_minutes": 50, "icon": "weather-overcast-symbolic", "temp": 41},
 {"in_minutes": 90, "icon": "weather-showers-symbolic", "temp": 18.5},
 {"in_minutes": 150, "icon": "weather-fog-symbolic", "temp": -0.4},
 {"in_minutes": 210, "icon": "weather-snow-symbolic", "temp": -2.6},
 {"in_minutes": 270, "icon": "weather-clear-night-symbolic", "temp": 7},
 {"in_minutes": 330, "icon": "weather-storm-symbolic", "temp": 42}]}'
    forecast_from="$(date +%s)"
    datemenu_open forecast G-DATEMENU-WEATHER
    forecast_to="$(date +%s)"
    scrot "$ARTIFACTS/02c-datemenu-displays.png" >/dev/null
    forecast="$ARTIFACTS/a11y-datemenu-forecast.json"
    for temp in 21 19 0 7; do
        showing_like "$forecast" '^\s*'"$temp"'°$' ||
            fail G-DATEMENU-WEATHER "the forecast lacks the rounded temperature $temp°"
    done
    showing_like "$forecast" '^-3°$' ||
        fail G-DATEMENU-WEATHER "the forecast lacks the rounded temperature -3°"
    [ "$(showing_labels "$forecast" '°$')" = 5 ] ||
        fail G-DATEMENU-WEATHER "the card does not show exactly five forecasts"
    for temp in 40 41 42; do
        if showing_like "$forecast" '^\s*'"$temp"'°$'; then
            fail G-DATEMENU-WEATHER "a past, too-close or sixth forecast ($temp°) is shown"
        fi
    done
    [ "$(showing_labels "$forecast" '^[\s\x{2007}]*[0-9]{1,2}:[0-9]{2}$')" -ge 5 ] ||
        fail G-DATEMENU-WEATHER "the forecasts lack their times"
    # Time (without AM/PM) over a 32px icon over the rounded temperature,
    # a space where a minus sign would stand, in five even columns.
    datemenu_layout forecast "$forecast" 24h "$forecast_from" "$forecast_to" \
        "30= 21°" "90= 19°" "150= 0°" "210=-3°" "270= 7°" ||
        fail G-DATEMENU-WEATHER "the forecasts are not GNOME's five columns of time, icon and temperature"
    # Events, world clocks and weather together are taller than a
    # 1280x800 output has room for: the menu stays whole on the output
    # and its displays section scrolls, as GNOME's does.
    menu_box="$(datemenu_layout fits "$ARTIFACTS/02a-datemenu-closed.png" "$ARTIFACTS/02c-datemenu-displays.png" \
        "$WIDTH" "$HEIGHT" 32 6)" ||
        fail G-DATEMENU-WEATHER "the date menu with all three cards does not fit the output: $menu_box"
    datemenu_layout overflows "$forecast" "Weather" ||
        fail G-DATEMENU-WEATHER "three cards on a ${WIDTH}x${HEIGHT} output did not leave the displays section to scroll"
    # shellcheck disable=SC2086 # left top right bottom
    set -- $menu_box
    xdotool mousemove --window "$WID" $(($3 - 150)) $(($4 - 60))
    for _ in 1 2 3 4 5 6 7 8; do xdotool click 5; sleep 0.1; done
    sleep 1
    scrot "$ARTIFACTS/02d-datemenu-scrolled.png" >/dev/null
    datemenu_dump scrolled G-DATEMENU-WEATHER
    datemenu_layout reached "$ARTIFACTS/a11y-datemenu-scrolled.json" "Weather" 4 ||
        fail G-DATEMENU-WEATHER "scrolling the displays section did not bring the whole Weather card into view"
    # Forecast times follow clock-format live, still without AM/PM.
    gsettings set org.gnome.desktop.interface clock-format '12h'
    sleep 1
    datemenu_dump forecast-12h G-DATEMENU-WEATHER
    forecast_to="$(date +%s)"
    datemenu_layout forecast "$ARTIFACTS/a11y-datemenu-forecast-12h.json" 12h "$forecast_from" "$forecast_to" \
        "30= 21°" "90= 19°" "150= 0°" "210=-3°" "270= 7°" ||
        fail G-DATEMENU-WEATHER "the open card's forecast times did not follow clock-format 12h"
    gsettings set org.gnome.desktop.interface clock-format "$clock_format_before"
    sleep 1
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-datemenu-weather-launch.json" 20 --press "Weather" ||
        fail G-DATEMENU-WEATHER "the weather card has no accessible action"
    sleep 1
    [ -f "$ARTIFACTS/org.gnome.Weather-launch" ] ||
        fail G-DATEMENU-WEATHER "the card did not activate GNOME Weather"
    if showing "$ARTIFACTS/a11y-datemenu-weather-launch.json" "Lisbon"; then
        fail G-DATEMENU-WEATHER "the date menu stayed open after activating Weather"
    fi
    # The keyboard reaches the card as well: the cards end the menu's
    # focus chain, focus scrolls the section to show the whole card, and
    # Enter activates it.
    rm -f "$ARTIFACTS/org.gnome.Weather-launch"
    datemenu_open forecast-reopened G-DATEMENU-WEATHER
    kr_focus_until '.name == "Weather" and (.role == "push button" or .role == "button")' shift+Tab 4 \
        "$ARTIFACTS/a11y-datemenu-weather-focus.json" ||
        fail G-DATEMENU-WEATHER "Shift+Tab from the open menu did not reach the Weather card"
    datemenu_layout reached "$ARTIFACTS/a11y-datemenu-weather-focus.json" "Weather" 4 ||
        fail G-DATEMENU-WEATHER "focusing the Weather card did not scroll all of it, margin included, into view"
    scrot "$ARTIFACTS/02d-datemenu-focused.png" >/dev/null
    xdotool key Return
    sleep 1
    [ -f "$ARTIFACTS/org.gnome.Weather-launch" ] ||
        fail G-DATEMENU-WEATHER "Enter on the focused card did not activate GNOME Weather"
    datemenu_dump weather-keys G-DATEMENU-WEATHER
    if showing "$ARTIFACTS/a11y-datemenu-weather-keys.json" "Lisbon"; then
        fail G-DATEMENU-WEATHER "the date menu stayed open after Enter on the Weather card"
    fi
    # Removing the apps hides both cards again, and the rest of the
    # journey sees the date menu it always has.
    datemenu_mask org.gnome.clocks
    datemenu_mask org.gnome.Weather
    gsettings reset org.gnome.shell.world-clocks locations
    weather_fixture '{"state": "none"}'
    sleep 1
    datemenu_open removed G-DATEMENU-WEATHER
    for name in "World Clocks" "Add World Clocks…"; do
        hidden "$ARTIFACTS/a11y-datemenu-removed.json" "$name" ||
            fail G-DATEMENU-WORLD-CLOCKS "the world clocks card outlived GNOME Clocks"
    done
    for name in "Weather" "Select Weather Location…"; do
        hidden "$ARTIFACTS/a11y-datemenu-removed.json" "$name" ||
            fail G-DATEMENU-WEATHER "the weather card outlived GNOME Weather"
    done
    showing "$ARTIFACTS/a11y-datemenu-removed.json" "No Notifications" ||
        fail G-DATEMENU-WEATHER "the date menu did not reopen after the apps were removed"
    # Without the two apps the menu is, pixel for pixel, the one the
    # journey opened first (the panel's clock aside).
    scrot "$ARTIFACTS/02e-datemenu-removed.png" >/dev/null
    "$PY" - "$ARTIFACTS/02-calendar.png" "$ARTIFACTS/02e-datemenu-removed.png" <<'PYSAME' ||
import sys
from PIL import Image, ImageChops
first, last = (Image.open(path).convert("RGB") for path in sys.argv[1:3])
box = ImageChops.difference(first, last).crop((0, 32, *first.size)).getbbox()
print(f"date menu frames differ under the panel in {box}")
raise SystemExit(0 if box is None else 1)
PYSAME
        fail G-DATEMENU-WEATHER "with both apps gone the date menu is not drawn as it was before they came"
    pass G-DATEMENU-WEATHER "installing Weather shows its card live: location prompt, GNOME's loading, offline and unavailable lines, and five hourly forecasts from now in even columns; all three cards keep the menu on the output with the displays section scrolling; the card activates Weather by its accessible action and by Shift+Tab and Enter, and closes the menu; both cards hide when their apps go and the menu is drawn as before"
}
