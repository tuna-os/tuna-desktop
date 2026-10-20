#!/usr/bin/env bash
set -euo pipefail
if [ "$(id -u)" -eq 0 ]; then
    # A real isolated system bus, without fabricated service implementations.
    mkdir -p /run/dbus
    dbus-daemon --system --fork --nopidfile
    chown -R --no-dereference tuna-proof:tuna-proof /out
    status=0
    env -u DBUS_SESSION_BUS_ADDRESS -u TUNA_BACKGROUNDS_BUS runuser -u tuna-proof -- "$0" || status=$?
    # Never traverse artifacts while a writer may still be running.
    if ! jq -e '.qualified_retirement == true' /out/process-retirement.json >/dev/null; then
        [ "$status" -eq 0 ] || exit "$status"
        exit 1
    fi
    finalization=0
    python3 /repo/scripts/lib/tuna-portal-artifact-finalize.py /out --proof-status "$status" || finalization=$?
    [ "$status" -eq 0 ] || exit "$status"
    exit "$finalization"
fi
if [ -z "${TUNA_BACKGROUNDS_BUS:-}" ]; then TUNA_BACKGROUNDS_BUS=1 exec dbus-run-session -- "$0"; fi
[ "$(id -u)" -eq 1000 ]
export DISPLAY=:99 XDG_RUNTIME_DIR=/out/runtime XDG_CONFIG_HOME=/out/config XDG_DATA_HOME=/out/data XDG_STATE_HOME=/out/state
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME GSETTINGS_BACKEND=keyfile LIBGL_ALWAYS_SOFTWARE=1 GTK_A11Y=atspi LC_ALL=C.UTF-8
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"
id > /out/session-identity.txt
rpm -q gnome-control-center mutter-common gsettings-desktop-schemas gtk4 libadwaita at-spi2-core > /out/runtime-versions.txt
[ "$(rpm -q --qf '%{VERSION}' gnome-control-center | cut -d. -f1)" = 51 ]
rpm -V gnome-control-center > /out/settings-package-verify.txt
[ "$(rpm -q --qf '%{VERSION}' mutter-common | cut -d. -f1)" = 51 ]
rpm -qf /usr/share/glib-2.0/schemas/org.gnome.mutter.gschema.xml > /out/mutter-schema-owner.txt
rpm -V mutter-common > /out/mutter-package-verify.txt
python3 - <<'SCHEMA' > /out/mutter-schema-preflight.txt
from gi.repository import Gio
source = Gio.SettingsSchemaSource.get_default()
schema = source.lookup('org.gnome.mutter', True)
if schema is None:
    raise RuntimeError('actual packaged org.gnome.mutter schema is missing')
print(schema.get_id())
print('keys:', ','.join(sorted(schema.list_keys())))
SCHEMA
cleanup() {
    local status=$? retirement=0 pid
    local -a children
    trap - EXIT
    # Snapshot current direct jobs, excluding the already-reaped first session.
    mapfile -t children < <(jobs -p)
    python3 /repo/scripts/lib/tuna-portal-artifact-retire.py "$$" "${children[@]}" > /out/process-retirement.json || retirement=$?
    if [ "$retirement" -eq 0 ]; then
        if [ -n "${compositor_pid:-}" ]; then
            wait "$compositor_pid" || retirement=$?
        fi
        for pid in "${children[@]}"; do wait "$pid" || true; done
        if [ "$retirement" -ne 0 ]; then
            printf '{"qualified_retirement":false,"error":"compositor did not exit normally","compositor_status":%d}\n' "$retirement" > /out/process-retirement.json
        fi
    fi
    [ "$status" -eq 0 ] || exit "$status"
    exit "$retirement"
}
trap cleanup EXIT
Xvfb :99 -screen 0 1280x800x24 -ac -nolisten tcp >/out/xvfb.log 2>&1 &
sleep 1
export WAYLAND_DISPLAY=backgrounds-proof GDK_BACKEND=wayland
export TUNA_COMPOSITOR_STATE=/out/compositor-state.json
dbus-update-activation-environment DISPLAY WAYLAND_DISPLAY GDK_BACKEND GTK_A11Y GSETTINGS_BACKEND XDG_SESSION_TYPE XDG_CURRENT_DESKTOP XDG_RUNTIME_DIR XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME LC_ALL
env -u WAYLAND_DISPLAY /candidate/usr/bin/tuna-compositor --backend winit --socket backgrounds-proof --width 1280 --height 800 --shell-bin /candidate/usr/bin/tuna-shell-gtk >/out/compositor.log 2>&1 & compositor_pid=$!
end=$((SECONDS + 30))
until [ -S "$XDG_RUNTIME_DIR/backgrounds-proof" ] && [ -s "$TUNA_COMPOSITOR_STATE" ]; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
python3 /repo/scripts/lib/tuna-stock-background-proof.py initial > /out/journey-initial.log 2>&1
python3 /repo/scripts/lib/tuna-portal-artifact-retire.py "$$" "$compositor_pid" > /out/initial-process-retirement.json
wait "$compositor_pid"
# Once both actual owners are gone, remove their private stale socket/lock.
end=$((SECONDS + 15))
until gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.NameHasOwner org.gnome.Shell | grep -q false; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
rm -f "$XDG_RUNTIME_DIR/backgrounds-proof" "$XDG_RUNTIME_DIR/backgrounds-proof.lock" "$TUNA_COMPOSITOR_STATE"
env -u WAYLAND_DISPLAY /candidate/usr/bin/tuna-compositor --backend winit --socket backgrounds-proof --width 1280 --height 800 --shell-bin /candidate/usr/bin/tuna-shell-gtk >/out/compositor-restarted.log 2>&1 & compositor_pid=$!
end=$((SECONDS + 30))
until [ -S "$XDG_RUNTIME_DIR/backgrounds-proof" ] && [ -s "$TUNA_COMPOSITOR_STATE" ]; do [ "$SECONDS" -lt "$end" ] || exit 1; sleep .1; done
python3 /repo/scripts/lib/tuna-stock-background-proof.py restarted > /out/journey-restarted.log 2>&1
printf '%s\n' 'GNOME51 stock Background chooser: metadata and actual placement/gradient pixels, ordinary app identity, genuine compositor/GTK-shell session restart and persisted selected background' > /out/assertions.txt
