#!/bin/sh
# Private D-COLORD startup boundary. Sourced by the proof and boundary tests.
# fd 3 is the receipt; fd 4 is exclusively the daemon's stdout/stderr.
colord_status() {
    printf '%s_status=%s' "$1" "$2"
    if [ "$2" -gt 128 ] && [ "$2" -le 192 ]; then
        printf ' signal_candidate=%s (%s; shell 128+signal convention, not proof of signal)' \
            "$(($2 - 128))" "$(kill -l "$(($2 - 128))" 2>/dev/null || echo unknown)"
    fi
    printf '\n'
}

# Linux PID plus start ticks: cleanup must not signal a subsequently reused PID.
colord_identity() (
    IFS= read -r colord_proc_stat <"/proc/$1/stat" 2>/dev/null || exit 1
    colord_proc_fields=${colord_proc_stat##*) }
    # The comm field may contain spaces; fields after its final ')' are numeric
    # or single tokens. starttime is field 22 (20th token after comm).
    # shellcheck disable=SC2086
    set -- "$1" $colord_proc_fields
    colord_identity_pid=$1
    shift 20
    printf '%s:%s\n' "$colord_identity_pid" "$1"
)

colord_identity_alive() {
    [ -n "$1" ] && [ "$(colord_identity "${1%%:*}" 2>/dev/null || true)" = "$1" ]
}

colord_signal_owned() {
    if colord_identity_alive "$1"; then
        timeout 2s sudo -n kill "-$2" "${1%%:*}" 2>/dev/null || true
    fi
}

colord_exec() {
    printf 'phase=daemon-exec pid=%s uid=%s gid=%s bus=%s binary=%s\n' \
        "$$" "$(id -u)" "$(id -g)" "$1" "$2" >&3
    exec env DBUS_SYSTEM_BUS_ADDRESS="$1" "$2" -v >&4 2>&1
}

colord_namespace() {
    printf 'supervisor_identity=%s\n' "$(colord_identity "$$")" >&3
    printf 'phase=mount pid=%s\n' "$$" >&3
    mount -t tmpfs tmpfs /var/lib/colord || return "$?"
    # Observe the actual mounted root; do not change its ownership or mode.
    stat -c 'store=%n uid=%u gid=%g mode=%a' /var/lib/colord >&3 || true
    printf 'phase=drop-uid pid=%s intended_uid=%s intended_gid=%s\n' "$$" "$2" "$3" >&3
    # shellcheck disable=SC2016
    setpriv --reuid="$2" --regid="$3" --clear-groups sh -c \
        '. "$1"; colord_exec "$2" "$3"' sh "$1" "$4" "$5" &
    colord_child=$!
    # Install forwarding before publishing the owned child for cleanup.
    trap 'kill -TERM "$colord_child" 2>/dev/null || true' TERM INT
    printf 'child_pid=%s supervisor_pid=%s\n' "$colord_child" "$$" >&3
    printf 'child_identity=%s\n' "$(colord_identity "$colord_child" 2>/dev/null || true)" >&3
    # sudo forwards termination to this supervisor. Never leave the owned
    # daemon behind, and never address other host colord processes.
    colord_child_status=0
    wait "$colord_child" || colord_child_status=$?
    # A trapped signal can interrupt wait before the child is reaped.
    if kill -0 "$colord_child" 2>/dev/null; then
        wait "$colord_child" || colord_child_status=$?
    fi
    colord_status child "$colord_child_status" >&3
    return "$colord_child_status"
}

# COLORD_BIN is supplied by the harness.
# shellcheck disable=SC2153
colord_start() {
    COLORD_RECEIPT="$ARTIFACTS/colord-startup.txt"
    COLORD_STARTED=$(date +%s)
    COLORD_REAPED=0
    COLORD_WAIT_STATUS=""
    : >"$ARTIFACTS/colord.log"
    {
        printf 'started_epoch=%s intended_uid=%s intended_gid=%s bus=%s binary=%s\n' \
            "$COLORD_STARTED" "$(id -u)" "$(id -g)" "$SYSTEM_BUS" "$COLORD_BIN"
        stat -Lc 'binary=%n uid=%u gid=%g mode=%a size=%s' "$COLORD_BIN" || true
        sha256sum "$COLORD_BIN" || true
        printf 'phase=namespace-launch\n'
    } >"$COLORD_RECEIPT" 2>&1
    # sudo closes extra inherited fds. Open the precreated, runner-owned files
    # inside the privileged boundary; no sudo policy change is required.
    : >"$ARTIFACTS/colord-launch.log"
    # shellcheck disable=SC2016,SC2024
    sudo unshare --mount --propagation private sh -c \
        'exec 3>>"$2" 4>>"$3"; . "$1"; colord_namespace "$1" "$4" "$5" "$6" "$7"' \
        sh "$ROOT/scripts/lib/colord-startup.sh" "$COLORD_RECEIPT" "$ARTIFACTS/colord.log" \
        "$(id -u)" "$(id -g)" "$SYSTEM_BUS" "$COLORD_BIN" \
        >"$ARTIFACTS/colord-launch.log" 2>&1 &
    COLORD_PID=$!
    printf 'wrapper_pid=%s\n' "$COLORD_PID" >>"$COLORD_RECEIPT"
}

# Call only after kill -0 reports death. Reap the actual shell child once;
# status 127 from a second wait must never overwrite the original receipt.
colord_reap() {
    if [ "$COLORD_REAPED" -eq 0 ]; then
        COLORD_WAIT_STATUS=0
        wait "$COLORD_PID" || COLORD_WAIT_STATUS=$?
        COLORD_REAPED=1
        colord_status wrapper "$COLORD_WAIT_STATUS" >>"$COLORD_RECEIPT"
        COLORD_PID=""
    fi
}

colord_failure_receipt() {
    {
        printf 'failure_epoch=%s\n' "$(date +%s)"
        if [ -n "$COLORD_PID" ]; then
            ps -p "$COLORD_PID" -o pid,ppid,stat,wchan:40,args || true
        fi
        printf '\nKernel journal (raw, bounded; availability is not a diagnosis):\n'
        colord_journal_status=0
        timeout 2s sudo -n journalctl --no-pager -k -n 200 --since "@$COLORD_STARTED" || colord_journal_status=$?
        printf 'journal_status=%s (nonzero means unavailable/permission/error/timeout; see raw output)\n' "$colord_journal_status"
        printf '\nKernel ring buffer (raw last 65536 bytes; may predate this launch):\n'
        # Preserve the reader status inside the byte-bounded stream.
        # shellcheck disable=SC2016
        timeout 3s sh -c '
            rc=0
            timeout 2s sudo -n dmesg --ctime || rc=$?
            printf "\ndmesg_status=%s (nonzero means unavailable/permission/error/timeout)\n" "$rc"
        ' 2>&1 | tail -c 65536
    } >>"$COLORD_RECEIPT" 2>&1
}

colord_stop() {
    [ -n "${COLORD_RECEIPT:-}" ] || return 0
    colord_owned_supervisor=$(sed -n 's/^supervisor_identity=//p' "$COLORD_RECEIPT")
    colord_owned_child=$(sed -n 's/^child_identity=//p' "$COLORD_RECEIPT")
    # Also clean an owned daemon if its sudo wrapper died first. Fingerprints
    # survive reaping the wrapper but never authorize signaling a reused PID.
    colord_signal_owned "$colord_owned_child" TERM
    colord_signal_owned "$colord_owned_supervisor" TERM
    if [ -n "$COLORD_PID" ]; then
        timeout 2s sudo -n kill -TERM "$COLORD_PID" 2>/dev/null || true
    fi
    colord_stop_end=$(($(date +%s) + 3))
    while colord_identity_alive "$colord_owned_child" ||
        colord_identity_alive "$colord_owned_supervisor" ||
        { [ -n "$COLORD_PID" ] && kill -0 "$COLORD_PID" 2>/dev/null; }; do
        if [ "$(date +%s)" -ge "$colord_stop_end" ]; then
            colord_signal_owned "$colord_owned_child" KILL
            colord_signal_owned "$colord_owned_supervisor" KILL
            if [ -n "$COLORD_PID" ]; then
                timeout 2s sudo -n kill -KILL "$COLORD_PID" 2>/dev/null || true
            fi
            break
        fi
        sleep 0.1
    done
    if [ -n "$COLORD_PID" ]; then
        # Bound even an uninterruptible wrapper: only wait once it is dead.
        if ! kill -0 "$COLORD_PID" 2>/dev/null; then colord_reap; fi
    fi
}
