#!/usr/bin/env bash
set -euo pipefail
if [ -z "${TUNA_REMOTE_BUS:-}" ]; then TUNA_REMOTE_BUS=1 exec dbus-run-session -- "$0"; fi
# Every component writes to a log under /out, so a failed step used to end the
# job without one line. Name the failing command or the expired wait here;
# scripts/tuna-remote-security then prints the log tails.
# -E: name failures inside the helper functions too.
set -E
trap 'echo "remote-desktop-reference: line $LINENO: $BASH_COMMAND exited $?" >&2' ERR
expired() { echo "remote-desktop-reference: deadline expired waiting for $1" >&2; exit 1; }
export DISPLAY=:99 XDG_RUNTIME_DIR=/out/runtime XDG_CONFIG_HOME=/out/config XDG_DATA_HOME=/out/data XDG_STATE_HOME=/out/state
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME GSETTINGS_BACKEND=memory LIBGL_ALWAYS_SOFTWARE=1 GTK_A11Y=atspi
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME/xdg-desktop-portal" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"
printf '[preferred]\ndefault=gnome;gtk;\norg.freedesktop.impl.portal.ScreenCast=gnome\n' >"$XDG_CONFIG_HOME/xdg-desktop-portal/portals.conf"
rpm -q xdg-desktop-portal-gnome xdg-desktop-portal pipewire gtk4 libadwaita libei > /out/runtime-versions.txt
[ "$(rpm -q --qf '%{VERSION}' xdg-desktop-portal-gnome | cut -d. -f1)" = 51 ] || { echo 'GNOME51 backend required' >&2; exit 1; }
# pkg-config deliberately returns separate compiler arguments.
# shellcheck disable=SC2046
gcc -Wall -Wextra -Werror /repo/scripts/lib/tuna-remote-ei.c -o /out/remote-ei $(pkg-config --cflags --libs libei-1.0)
# This independent real-library regression delays consumption past the old
# sender sleep, then requires delivery before the acknowledged disconnect.
# shellcheck disable=SC2046
gcc -Wall -Wextra -Werror /repo/tests/remote-desktop-reference/eis-lifetime.c -o /out/eis-lifetime $(pkg-config --cflags --libs libeis-1.0)
/out/eis-lifetime /out/remote-ei >/out/eis-lifetime.log 2>&1
pids=""
cleanup() { for pid in $pids; do kill "$pid" 2>/dev/null || true; done; }
trap cleanup EXIT
Xvfb :99 -screen 0 1280x800x24 -ac -nolisten tcp >/out/xvfb.log 2>&1 & pids="$pids $!"
sleep 1
pipewire >/out/pipewire.log 2>&1 & pids="$pids $!"
sleep 1
wireplumber >/out/wireplumber.log 2>&1 & pids="$pids $!"
export WAYLAND_DISPLAY=remote-proof GDK_BACKEND=wayland
dbus-update-activation-environment DISPLAY WAYLAND_DISPLAY GDK_BACKEND GTK_A11Y XDG_SESSION_TYPE XDG_CURRENT_DESKTOP XDG_RUNTIME_DIR XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME
export TUNA_COMPOSITOR_STATE=/out/compositor-state.json
# The compositor reads gnome-remote-desktop's sharing switches where GNOME
# Settings writes them, dconf (R-GRD-ENABLE); everything else stays on the
# memory backend.
env -u WAYLAND_DISPLAY GSETTINGS_BACKEND=dconf /candidate/usr/bin/tuna-compositor --backend winit --socket remote-proof --width 1280 --height 800 --shell-bin /candidate/usr/bin/tuna-shell-gtk >/out/compositor.log 2>&1 & pids="$pids $!"
end=$((SECONDS + 30))
until [ -S "$XDG_RUNTIME_DIR/remote-proof" ] && [ -s "$TUNA_COMPOSITOR_STATE" ]; do [ "$SECONDS" -lt "$end" ] || expired 'the compositor socket and state file (30 s)'; sleep .1; done
export WAYLAND_DISPLAY=remote-proof GDK_BACKEND=wayland
python3 /repo/scripts/lib/tuna-remote-input-window.py /out/input.json >/out/window.log 2>&1 & pids="$pids $!"
python3 /repo/scripts/lib/tuna-a11y-dump.py tuna-shell-gtk /out/a11y-shell.json 30
sleep 1
python3 /repo/scripts/lib/tuna-remote-security-client.py > /out/untrusted-denial.log
# R-GRD-ENABLE (#1162 F1 mitigation 3): the genuine GNOME 51
# gnome-remote-desktop daemon is admitted only while the user's own sharing
# switch for it is on, whatever grd itself was told. grd's Desktop Sharing
# (user) daemon runs on a private keyfile settings backend that has RDP and
# remote control on, as a same-user process could arrange; a real FreeRDP
# client connects, and grd's session asks the compositor for RemoteDesktop.
user_settings() { GSETTINGS_BACKEND=dconf gsettings "$@"; }
grd_env() { env GSETTINGS_BACKEND=keyfile XDG_CONFIG_HOME=/out/grd/config "$@"; }
# R-REMOTE-INDICATOR: whether GNOME 51's "Stop Screen Sharing" panel button
# (ScreenSharingIndicator) shows: wait up to $2 s for $1 (true or false).
sharing_button() {
    local end=$((SECONDS + $2)) dump="/out/a11y-$3.json"
    until python3 /repo/scripts/lib/tuna-a11y-dump.py tuna-shell-gtk "$dump" 5 >/dev/null 2>&1 &&
        [ "$(jq 'any(.[]; .name == "Stop Screen Sharing" and .showing and (.role == "push button" or .role == "button"))' "$dump")" = "$1" ]; do
        [ "$SECONDS" -lt "$end" ] || expired "the Stop Screen Sharing button to be showing=$1 ($3, $2 s)"
        sleep .5
    done
}
rdp_client() {
    # No clipboard: grd serves its file clipboard over FUSE, which a
    # container has not.
    DISPLAY=:99 timeout 90 xfreerdp /v:127.0.0.1:3389 /u:tuna-proof /p:tuna-proof-rdp /cert:ignore -clipboard /log-level:INFO >"/out/$1.log" 2>&1 & rdp_pid=$!; pids="$pids $rdp_pid"
}
exited() { local end=$((SECONDS + $2)); while kill -0 "$1" 2>/dev/null; do [ "$SECONDS" -lt "$end" ] || expired "$3 ($2 s)"; sleep .1; done; }
# grd's user daemon keeps its RDP credentials in the Secret Service. As root
# in a container gnome-keyring cannot drop capabilities it does not need,
# so it starts without any; its keyring files stay off the /out bind mount,
# where its write never completed.
printf tuna-proof-keyring | XDG_DATA_HOME=/tmp/tuna-keyring setpriv --bounding-set=-all gnome-keyring-daemon --foreground --unlock --components=secrets >/out/keyring.log 2>&1 & pids="$pids $!"
end=$((SECONDS + 15))
until gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.NameHasOwner org.freedesktop.secrets | grep -q true; do [ "$SECONDS" -lt "$end" ] || expired 'the Secret Service (15 s)'; sleep .1; done
mkdir -p /out/grd/config
winpr-makecert -silent -rdp -path /out/grd grd-tls >/out/grd-makecert.log 2>&1
grd_env grdctl rdp set-tls-cert /out/grd/grd-tls.crt
grd_env grdctl rdp set-tls-key /out/grd/grd-tls.key
grd_env grdctl rdp set-credentials tuna-proof tuna-proof-rdp
grd_env grdctl rdp disable-view-only
# grdctl's enable also starts grd's systemd user unit; there is none here.
grd_env gsettings set org.gnome.desktop.remote-desktop.rdp enable true
[ "$(user_settings get org.gnome.desktop.remote-desktop.rdp enable)" = false ]
env GSETTINGS_BACKEND=keyfile XDG_CONFIG_HOME=/out/grd/config G_MESSAGES_DEBUG=all /usr/libexec/gnome-remote-desktop-daemon >/out/grd.log 2>&1 & grd_pid=$!; pids="$pids $grd_pid"
end=$((SECONDS + 30))
until grep -q 'RDP server started' /out/grd.log; do [ "$SECONDS" -lt "$end" ] || expired 'the genuine grd RDP server (30 s)'; sleep .1; done
sharing_button false 10 grd-before
# Sharing off for the user: grd's session is refused and ends the client.
rdp_client rdp-disabled
end=$((SECONDS + 30))
until grep -q 'Failed to start remote desktop session: .*AccessDenied' /out/grd.log; do [ "$SECONDS" -lt "$end" ] || expired 'grd to be refused while sharing is off (30 s)'; sleep .1; done
exited "$rdp_pid" 30 'grd to end the refused RDP client'
sharing_button false 5 grd-refused
refusals=$(grep -c 'Failed to start remote desktop session' /out/grd.log)
# Sharing on: the same daemon is admitted and its remote control session
# shows GNOME's indicator (R-REMOTE-INDICATOR); the client stays connected.
user_settings set org.gnome.desktop.remote-desktop.rdp enable true
rdp_client rdp-enabled
sharing_button true 30 grd-sharing
# Still streaming two seconds on: nothing but the switch ends it below.
sleep 2
kill -0 "$rdp_pid"
grep -q 'Surface .* Metrics: First frame' /out/grd.log
[ "$(grep -c 'Remote desktop session closed' /out/grd.log || true)" = 0 ]
[ "$(grep -c 'Failed to start remote desktop session' /out/grd.log)" = "$refusals" ]
# Sharing switched off mid-session: the compositor revokes the grant, grd
# drops the client, and the indicator hides after GNOME's five seconds.
switched_off=$(date +%s%N)
user_settings set org.gnome.desktop.remote-desktop.rdp enable false
exited "$rdp_pid" 10 'the RDP client to be dropped once sharing is off'
grd_dropped_ms=$((($(date +%s%N) - switched_off) / 1000000))
grep -q 'Remote desktop session closed' /out/grd.log
sharing_button false 12 grd-revoked
kill "$grd_pid"
wait "$grd_pid" || true
# The remote-login Handover daemon is never admitted, even with every
# sharing switch on; the headless daemon follows only its own switches and
# the user daemon also VNC's. The genuine grd binary carries the in-binary
# probe (ADR 0010 F1), owning each name in turn; the admitted controls show
# that identity passes.
# shellcheck disable=SC2046
gcc -Wall -Wextra -Werror -shared -fPIC /repo/scripts/lib/tuna-service-identity-probe.c -o /out/identity-probe.so $(pkg-config --cflags --libs gio-2.0)
grd_probe() {
    TUNA_PROBE_OUT="/out/grd-probe-$1.txt" TUNA_PROBE_NAME="org.gnome.RemoteDesktop.$2" TUNA_PROBE_DEST=org.gnome.Mutter.RemoteDesktop \
        TUNA_PROBE_PATH=/org/gnome/Mutter/RemoteDesktop TUNA_PROBE_METHOD=org.gnome.Mutter.RemoteDesktop.CreateSession TUNA_PROBE_ARGS='()' \
        LD_PRELOAD=/out/identity-probe.so /usr/libexec/gnome-remote-desktop-daemon >"/out/grd-probe-$1.log" 2>&1
    grep -q "^$3 " "/out/grd-probe-$1.txt"
}
for schema in rdp vnc rdp.headless; do user_settings set "org.gnome.desktop.remote-desktop.$schema" enable true; done
grd_probe handover Handover 'error org.freedesktop.DBus.Error.AccessDenied'
grd_probe headless Headless accepted
user_settings set org.gnome.desktop.remote-desktop.rdp.headless enable false
grd_probe headless-off Headless 'error org.freedesktop.DBus.Error.AccessDenied'
user_settings set org.gnome.desktop.remote-desktop.rdp enable false
grd_probe user-vnc User accepted
user_settings set org.gnome.desktop.remote-desktop.vnc enable false
grd_probe user-off User 'error org.freedesktop.DBus.Error.AccessDenied'
/usr/libexec/xdg-desktop-portal-gnome --replace >/out/backend.log 2>&1 & portal_backend_pid=$!; pids="$pids $portal_backend_pid"
/usr/libexec/xdg-desktop-portal --replace >/out/frontend.log 2>&1 & pids="$pids $!"
sleep 2
for decision in cancel grant eis disconnect; do
    python3 /repo/scripts/lib/tuna-portal-remote-client.py "/out/$decision.png" "$decision" >"/out/$decision.log" 2>&1 & client=$!; pids="$pids $client"
    python3 /repo/scripts/lib/tuna-portal-consent.py "/out/a11y-$decision.json" "$decision" remote
    wait "$client"
done
# One command each: `set -e` ignores every failure in an AND list but the last.
[ -s /out/grant.png ]
[ -s /out/disconnect.png ]
[ ! -f /out/cancel.png ]
# Losing the trusted backend process revokes the compositor grant itself.
python3 /repo/scripts/lib/tuna-portal-remote-client.py /out/backend-disconnect.png backend-disconnect >/out/backend-disconnect.log 2>&1 & client=$!; pids="$pids $client"
python3 /repo/scripts/lib/tuna-portal-consent.py /out/a11y-backend-disconnect.json grant remote
end=$((SECONDS + 30))
until [ -s /out/backend-disconnect.active ]; do [ "$SECONDS" -lt "$end" ] || expired 'the backend-disconnect grant to become active (30 s)'; sleep .1; done
kill "$portal_backend_pid"
touch /out/backend-disconnect.revoke
wait "$client"
/usr/libexec/xdg-desktop-portal-gnome --replace >/out/backend-restarted.log 2>&1 & pids="$pids $!"
/usr/libexec/xdg-desktop-portal --replace >/out/frontend-restarted.log 2>&1 & pids="$pids $!"
sleep 2
# Keep a genuine external portal stream active across the lock transition.
python3 /repo/scripts/lib/tuna-portal-remote-client.py /out/lock-grant.png revoke >/out/lock-grant.log 2>&1 & client=$!; pids="$pids $client"
python3 /repo/scripts/lib/tuna-portal-consent.py /out/a11y-lock-grant.json grant remote
end=$((SECONDS + 30))
until [ -s /out/lock-grant.active ] && jq -e '.capture_streams > 0 and (.locked | not)' "$TUNA_COMPOSITOR_STATE" >/dev/null; do [ "$SECONDS" -lt "$end" ] || expired 'an active unlocked capture stream before the lock (30 s)'; sleep .1; done
gdbus call --session --dest org.gnome.ScreenSaver --object-path /org/gnome/ScreenSaver --method org.gnome.ScreenSaver.SetActive true >/out/lock.log
end=$((SECONDS + 5))
touch /out/lock-grant.revoke
wait "$client"
until jq -e '.locked and .capture_streams == 0' "$TUNA_COMPOSITOR_STATE" >/dev/null; do [ "$SECONDS" -lt "$end" ] || expired 'the lock to revoke every capture stream (5 s)'; sleep .1; done
python3 /repo/scripts/lib/tuna-remote-security-client.py > /out/locked-denial.log
python3 /repo/scripts/lib/tuna-portal-remote-client.py /out/locked.png locked >/out/locked.log 2>&1 & client=$!; pids="$pids $client"
sleep 2
if [ ! -f /out/locked.response.json ]; then python3 /repo/scripts/lib/tuna-portal-consent.py /out/a11y-locked.json grant remote; fi
wait "$client"
[ ! -f /out/locked.png ]
pw-dump > /out/locked-pipewire.json
jq -e 'all(.[]; .type != "PipeWire:Interface:Node" or (.info.props["node.name"] // "" | contains("tuna-screen-cast") | not))' /out/locked-pipewire.json >/dev/null
# One `<ID> pass <description>` line per gate, as scripts/tuna-ledger reads.
printf '%s\n' \
    'R-REMOTE-PORTAL pass GNOME51 genuine RemoteDesktop portal: Cancel, keyboard/pointer grant, linked PipeWire frame, foreign input denial, legacy GTK delivery, libei GTK delivery/FD revocation, Close/client/backend disconnect, active lock revocation and locked denial; touch unsupported' \
    "R-GRD-ENABLE pass genuine gnome-remote-desktop 51 Desktop Sharing daemon with RDP on in its own settings: a FreeRDP client is refused while the user's rdp enable is off, streams once it is on, and is dropped ${grd_dropped_ms} ms after it is switched off; in the genuine grd binary, Handover is refused with every sharing switch on, Headless follows only rdp.headless and User also vnc" \
    'R-REMOTE-INDICATOR pass an admitted grd remote control session shows the Stop Screen Sharing panel button, absent before and after it' \
    > /out/assertions.txt
