# Local build and test recipes. `just` lists them; docs/testing.md explains
# what each CI job proves. Everything runs against this checkout (or
# worktree) and its own `target/`, so parallel worktrees never share one.

set shell := ["bash", "-euo", "pipefail", "-c"]
set positional-arguments := false

export PKG_CONFIG := env("PKG_CONFIG", "/usr/bin/pkg-config")
export CARGO_INCREMENTAL := env("CARGO_INCREMENTAL", "0")
export CARGO_BUILD_JOBS := env("CARGO_BUILD_JOBS", "2")
# Always this worktree's own target (an inherited CARGO_TARGET_DIR could point
# at another worktree's); override deliberately with TUNA_TARGET_DIR.
export CARGO_TARGET_DIR := env("TUNA_TARGET_DIR", justfile_directory() / "target")

# Cargo builds from every worktree on the machine queue on one lock, so
# concurrent builds don't exhaust memory. `-o` closes the lock before the
# command runs, so a daemon it leaves behind can't keep holding the lock.
lock := "flock -o " + env("TUNA_BUILD_LOCK", "/tmp/tuna-local-build.lock")
proof_lock := "flock -o " + env("TUNA_PROOF_LOCK", "/tmp/tuna-local-proof.lock")
# Below this much free disk (GiB), builds run `gc` first.
min_free_gb := env("TUNA_MIN_FREE_GB", "20")
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
clippy: _disk
    {{lock}} cargo clippy --workspace --all-targets -- -D warnings

# Build the workspace (extra arguments go to cargo, e.g. `just build --release`).
build *args: _disk
    {{lock}} cargo build --locked {{args}}

# Test the workspace, or pass cargo arguments: `just test -p tuna-compositor --lib orca::`.
test *args: _disk
    {{lock}} cargo test {{ if args == "" { "--workspace" } else { args } }}

# Run a nested proof (`just proof gtk-shell-proof`); artifacts are kept only if it fails.
proof name *args: _disk
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
    export HOME="$art/home" XDG_CONFIG_HOME="$art/home/.config" XDG_CACHE_HOME="$art/home/.cache" \
        XDG_DATA_HOME="$art/home/.local/share" XDG_STATE_HOME="$art/home/.local/state"
    mkdir -p "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
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
    # whatever still carries it when the proof ends is stopped.
    export TUNA_PROOF_RUN="$art"
    leftovers() {
        local p
        for p in /proc/[0-9]*; do
            if grep -qzx "TUNA_PROOF_RUN=$art" "$p/environ" 2>/dev/null; then echo "${p#/proc/}"; fi
        done
    }
    reap() {
        local pids
        pids=$(leftovers)
        [ -n "$pids" ] || return 0
        echo "proof {{name}}: stopping leftover processes: $(echo $pids)" >&2
        kill $pids 2>/dev/null || true
        sleep 2
        pids=$(leftovers)
        [ -z "$pids" ] || kill -9 $pids 2>/dev/null || true
    }
    trap reap EXIT
    # One proof at a time machine-wide (Xvfb, CPU); builds keep their own lock.
    if {{proof_lock}} "$script" "$@" {{args}}; then
        rm -rf "$art"
        echo "proof {{name}} passed"
    else
        status=$?
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
    rm -rf "$CARGO_TARGET_DIR"

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

# Run `gc` when free disk falls below `min_free_gb`.
_disk:
    #!/usr/bin/env bash
    set -euo pipefail
    free=$(df --output=avail -BG "{{justfile_directory()}}" | tail -1 | tr -dc '0-9')
    if [ "$free" -lt {{min_free_gb}} ]; then
        echo "only ${free}G free (< {{min_free_gb}}G); running gc" >&2
        just --justfile "{{justfile()}}" gc
    fi
