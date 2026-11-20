# shellcheck shell=sh
# tuna-state-snapshot.sh — copy a live state file the compositor republishes.
#
#   snapshot_state SRC DST
#       copy SRC to DST through one open file descriptor; fail, as cp
#       does, when SRC cannot be read.
#
# The compositor publishes TUNA_COMPOSITOR_STATE by renaming a fresh file
# over the path. cp stats the source before opening it and refuses with
# "replaced while being copied" when a rename lands in between. A single
# redirect opens the path once, so a concurrent rename yields one whole
# version, old or new. DST is renamed into place, so it is never partial.

snapshot_state() {
    if cat -- "$1" >"$2.tmp"; then
        mv -f -- "$2.tmp" "$2"
    else
        rm -f -- "$2.tmp"
        return 1
    fi
}
