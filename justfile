# Local build and test recipes. `just` lists them; docs/testing.md explains
# what each CI job proves. Everything runs against this checkout (or
# worktree) and its own `target/`, so parallel worktrees never share one.

set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments := false

export PKG_CONFIG := env("PKG_CONFIG", "/usr/bin/pkg-config")
# sccache can't cache incremental builds.
export CARGO_INCREMENTAL := env("CARGO_INCREMENTAL", "0")
# Half the host's CPUs per build: two builds (below) fill it.
export CARGO_BUILD_JOBS := env("CARGO_BUILD_JOBS", `n=$(nproc); echo $((n > 1 ? n / 2 : 1))`)
# Always this worktree's own target: an inherited CARGO_TARGET_DIR (which
# could point at another worktree's) is dropped, and none is exported,
# because sccache keys on every CARGO_* variable: a per-worktree value would
# stop worktrees sharing its cache. Choose another target with TUNA_TARGET_DIR.
unexport CARGO_TARGET_DIR
target_dir := env("TUNA_TARGET_DIR", justfile_directory() / "target")
target_arg := "--target-dir " + quote(target_dir)
# When sccache is installed, every worktree's builds share one compiler
# cache, so a fresh worktree reuses the dependencies another one compiled.
export RUSTC_WRAPPER := env("RUSTC_WRAPPER", `command -v sccache || true`)
export SCCACHE_DIR := env("SCCACHE_DIR", home_directory() / ".cache/sccache")
export SCCACHE_CACHE_SIZE := env("SCCACHE_CACHE_SIZE", "40G")

semaphore := justfile_directory() / "scripts/lib/tuna-semaphore"
# Cargo builds from every worktree on the machine share `build_slots` slots,
# so concurrent builds don't exhaust memory: a cold `cargo test --workspace`
# build at 6 jobs peaks at 6.4 GiB (page cache included), so one slot per
# 8 GiB of RAM, at most two. The slot is released even if a daemon the
# build leaves behind lives on.
build_slot_base := env("TUNA_BUILD_SLOT_BASE", "/tmp/tuna-local-build.slot")
build_slots := env("TUNA_BUILD_SLOTS", `m=$(awk '/^MemTotal:/ { print int($2 / 1048576 / 8) }' /proc/meminfo); echo $((m < 1 ? 1 : m > 2 ? 2 : m))`)
lock := semaphore + " " + build_slot_base + " " + build_slots
# A build, test or clippy run still going after this many minutes is
# stopped with its whole process group, freeing its slot.
build_timeout_min := env("TUNA_BUILD_TIMEOUT", "60")
timed := justfile_directory() / "scripts/lib/tuna-timeout"
# Nested proofs each get their own Xvfb, Wayland socket, buses and runtime
# directory, but under concurrent load the gtk-shell proof's fixed waits
# still flake (3-way runs: 1/3 and 2/3 passed), so one runs at a time unless
# TUNA_PROOF_SLOTS opts in. Proofs that touch machine-wide state (vkms,
# seatd, uinput, sudo) always run one at a time.
proof_slot_base := env("TUNA_PROOF_SLOT_BASE", "/tmp/tuna-local-proof.slot")
proof_slots := env("TUNA_PROOF_SLOTS", "1")
proof_lock := env("TUNA_PROOF_LOCK", "/tmp/tuna-local-proof.lock")
# A proof still running after TUNA_PROOF_TIMEOUT minutes is stopped; unset,
# that is the timeout-minutes of the CI job that runs the proof, at least 45.
proof_timeout_min := env("TUNA_PROOF_TIMEOUT", "")
# Below this much free disk (GiB), builds run `disk-guard` first.
min_free_gb := env("TUNA_MIN_FREE_GB", "40")
# Downloaded CI artifacts live here and expire after `artifact_ttl_min`.
ci_artifacts := env("TUNA_CI_ARTIFACTS", "/tmp/tuna-ci-artifacts")
artifact_ttl_min := env("TUNA_CI_ARTIFACT_TTL_MIN", "360")

default:
    @just --list --unsorted

# Format, lint and test the workspace like CI's `check` job.
check: fmt-check clippy test

# Fail if any file needs `cargo fmt`.
fmt-check:
    cargo fmt --all -- --check

# Apply `cargo fmt`.
fmt:
    cargo fmt --all

# Clippy over every target with warnings as errors, as CI runs it.
clippy: _disk _sccache
    {{lock}} {{timed}} {{build_timeout_min}} "just clippy (TUNA_BUILD_TIMEOUT)" cargo clippy {{target_arg}} --workspace --all-targets -- -D warnings

# Build the workspace (extra arguments go to cargo, e.g. `just build --release`).
build *args: _disk _sccache
    {{lock}} {{timed}} {{build_timeout_min}} "just build (TUNA_BUILD_TIMEOUT)" cargo build {{target_arg}} --locked {{args}}

# Test the workspace, or pass cargo arguments: `just test -p tuna-compositor --lib orca::`.
test *args: _disk _sccache
    {{lock}} {{timed}} {{build_timeout_min}} "just test (TUNA_BUILD_TIMEOUT)" cargo test {{target_arg}} {{ if args == "" { "--workspace" } else { args } }}

# Run a nested proof (`just proof gtk-shell-proof`); artifacts are kept only if it fails.
proof name *args: _disk _sccache
    #!/usr/bin/env bash
    set -euo pipefail
    script="scripts/tuna-{{name}}"
    [ -x "$script" ] || { echo "no such proof: $script" >&2; ls scripts | sed -n 's/^tuna-\(.*proof\)$/  \1/p' >&2; exit 2; }
    # Proofs that need another proof's prebuilt binaries run through that parent.
    if grep -qE -- "--bin-dir'?,? *(required=True|.*required)" "$script" || grep -qE -- '--bin-dir.*:\?' "$script"; then
        echo "$script needs prebuilt binaries (--bin-dir); run it through its caller:" >&2
        grep -lE "tuna-{{name}}\b" scripts/tuna-* .github/workflows/*.yml 2>/dev/null | grep -v "^$script$" | sed 's/^/  /' >&2
        exit 2
    fi
    # A CI runner's umask: under 002 the session's directories come out
    # group-writable and the compositor's ownership checks refuse them.
    umask 022
    art=$(mktemp -d "${TMPDIR:-/tmp}/tuna-proof-{{name}}.XXXXXX")
    # A fresh home like a CI runner's, so desktop state in yours can't leak in
    # (or block the session's settings upgrade); Cargo and rustup stay yours.
    export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
    # The proof's own builds use this worktree's target unless TUNA_TARGET_DIR names one.
    [ -z "${TUNA_TARGET_DIR:-}" ] || export CARGO_TARGET_DIR="$TUNA_TARGET_DIR"
    export HOME="$art/home" XDG_CONFIG_HOME="$art/home/.config" XDG_CACHE_HOME="$art/home/.cache" \
        XDG_DATA_HOME="$art/home/.local/share" XDG_STATE_HOME="$art/home/.local/state"
    mkdir -p "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
    # No display, socket or bus of yours either: every proof starts its own
    # Xvfb, compositor socket and buses in a private runtime directory (a
    # short path: Unix socket paths cap at 108 bytes), so proofs from several
    # worktrees can run at once.
    runtime=$(mktemp -d /tmp/tuna-rt.XXXXXX)
    export XDG_RUNTIME_DIR="$runtime"
    unset DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET DBUS_SESSION_BUS_ADDRESS AT_SPI_BUS_ADDRESS
    # Scripts that call `pkg-config` by name get the one PKG_CONFIG names (a
    # Homebrew one earlier in PATH lacks the system's wayland-protocols),
    # with Cargo's own bin directory still first.
    export PATH="$CARGO_HOME/bin:$(dirname "$PKG_CONFIG"):$PATH"
    # The proof's own cargo builds wait for a build slot like `just build`.
    shim=$(mktemp -d /tmp/tuna-proof-bin.XXXXXX)
    ln -s "{{justfile_directory()}}/scripts/lib/tuna-cargo-slot" "$shim/cargo"
    export TUNA_REAL_CARGO="$(command -v cargo)" TUNA_BUILD_SLOT_BASE="{{build_slot_base}}" TUNA_BUILD_SLOTS="{{build_slots}}"
    export PATH="$shim:$PATH"
    # A positional artifact contract is stated in the script's first lines.
    if head -n 30 "$script" | grep -qE '\$\{1:\?|sys\.argv\[1\]'; then
        set -- "$art"
    elif grep -q -- '--artifacts' "$script"; then
        set -- --artifacts "$art"
    else
        set --                         # the proof keeps no artifacts
    fi
    # Every process the proof starts inherits this marker, including daemons
    # that detach from it (ibus-daemon --daemonize, dbus-daemon --fork);
    # whatever still carries it when the proof ends is stopped, except the
    # machine's shared sccache server should a build in the proof start it.
    export TUNA_PROOF_RUN="$art"
    leftovers() {
        local p
        for p in /proc/[0-9]*; do
            if grep -qzx "TUNA_PROOF_RUN=$art" "$p/environ" 2>/dev/null &&
                [ "$(cat "$p/comm" 2>/dev/null)" != sccache ]; then
                echo "${p#/proc/}"
            fi
        done
    }
    reap() {
        local pids
        pids=$(leftovers)
        if [ -n "$pids" ]; then
            echo "proof {{name}}: stopping leftover processes: $(echo $pids)" >&2
            kill $pids 2>/dev/null || true
            sleep 2
            pids=$(leftovers)
            [ -z "$pids" ] || kill -9 $pids 2>/dev/null || true
        fi
        rm -rf "$runtime" "$shim"
    }
    trap reap EXIT
    # A nested proof (its own Xvfb) takes one of the machine's proof slots;
    # one that touches machine-wide state (vkms, seatd, uinput, sudo, apt),
    # or isn't nested, runs alone. Builds keep their own slots.
    if grep -qE 'uinput|seatd|/dev/dri|vkms|/dev/input|sudo|apt-get|playwright install' "$script" ||
        ! grep -qi xvfb "$script"; then
        slot=("{{semaphore}}" "{{proof_lock}}" 1)
    else
        slot=("{{semaphore}}" "{{proof_slot_base}}" "{{proof_slots}}")
    fi
    # A hung proof is stopped after its timeout (TERM to its process group,
    # then KILL 30 s later) and everything it started is reaped, freeing its
    # slot. The default is its CI job's timeout-minutes, at least 45.
    timeout_min="{{proof_timeout_min}}"
    if [ -z "$timeout_min" ]; then
        timeout_min=$(awk -v s="scripts/tuna-{{name}}" '
            /^  [A-Za-z0-9_-]+:[ \t]*$/ { job = $1 }
            /^    timeout-minutes:/ { limit[job] = $2 }
            { i = index($0, s); c = substr($0, i + length(s), 1) }
            i && c !~ /[A-Za-z0-9_-]/ { used[job] = 1 }
            END { m = 45; for (j in used) if (limit[j] > m) m = limit[j]; print m }
        ' .github/workflows/*.yml)
    fi
    status=0
    "${slot[@]}" "{{timed}}" "$timeout_min" "proof {{name}} (TUNA_PROOF_TIMEOUT)" "$script" "$@" {{args}} || status=$?
    if [ "$status" = 0 ]; then
        rm -rf "$art"
        echo "proof {{name}} passed"
    else
        echo "proof {{name}} FAILED (exit $status); artifacts kept in $art" >&2
        exit "$status"
    fi

# Download a CI run's artifacts (optionally one by name) for inspection; they expire on `gc`.
ci-artifacts run name="":
    #!/usr/bin/env bash
    set -euo pipefail
    dest="{{ci_artifacts}}/{{run}}{{ if name == "" { "" } else { "-" + name } }}"
    mkdir -p "$dest"
    gh run download {{run}} -D "$dest" {{ if name == "" { "" } else { "-n " + name } }}
    echo "$dest"

# Delete this worktree's build target.
clean:
    rm -rf {{quote(target_dir)}}

# Free disk: drop targets and clean worktrees of merged/closed PRs, and expired CI artifacts.
gc:
    #!/usr/bin/env bash
    set -euo pipefail
    # Never the main checkout or the worktree gc runs from.
    here=$(git rev-parse --show-toplevel)
    main=$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")
    git worktree list --porcelain | awk '/^worktree /{w=$2} /^branch /{print w, $2}' |
    while read -r wt ref; do
        [ "$wt" = "$main" ] || [ "$wt" = "$here" ] && continue
        branch=${ref#refs/heads/}
        state=$(gh pr list --head "$branch" --state all --json state --jq '.[0].state // ""' 2>/dev/null || true)
        case "$state" in MERGED|CLOSED) ;; *) continue ;; esac
        if [ -d "$wt/target" ]; then
            rm -rf "$wt/target"
            echo "freed $wt/target ($branch $state)"
        fi
        if [ -z "$(git -C "$wt" status --porcelain 2>/dev/null)" ]; then
            git worktree remove "$wt" && echo "removed worktree $wt ($branch $state)"
        fi
    done
    if [ -d "{{ci_artifacts}}" ]; then
        find "{{ci_artifacts}}" -mindepth 1 -maxdepth 1 -mmin +{{artifact_ttl_min}} -print -exec rm -rf {} +
    fi
    df -h "$here" | tail -1

# Free disk now: delete idle worktree targets and stale proof/CI scratch when
# below `min_free_gb` (`--watch [seconds]` keeps checking, `--include-main`).
disk-guard *args:
    TUNA_MIN_FREE_GB={{min_free_gb}} TUNA_CI_ARTIFACTS="{{ci_artifacts}}" "{{justfile_directory()}}/scripts/tuna-disk-guard" {{args}}

# Run the process-aware disk guard before builds; it checks the free-space threshold.
_disk:
    @just --justfile "{{justfile()}}" disk-guard

# Start the shared sccache server, if it isn't running, with no idle timeout
# and outside any proof's environment, so it outlives every build and proof.
_sccache:
    #!/usr/bin/env bash
    [ "$(basename -- "${RUSTC_WRAPPER:-}")" = sccache ] || exit 0
    env -u TUNA_PROOF_RUN SCCACHE_IDLE_TIMEOUT=0 "$RUSTC_WRAPPER" --start-server >/dev/null 2>&1 || true
