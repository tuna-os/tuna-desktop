# Running tests locally

How to run, on your own machine, the checks the `ci` workflow
(`.github/workflows/ci.yml`) runs. For what each
verification layer must prove, see [the test strategy](test-strategy.md).
Install the packages listed under [Prerequisites](development.md#prerequisites)
first; each proof below names anything extra it needs.

Most lanes run on self-hosted runners for this repository's own branches
and on GitHub-hosted runners for forks; the steps and assertions are the
same on both. See [CI runners](ci-runners.md) for what runs where and the
switch back to GitHub-hosted runners.

## Which lanes a pull request runs

Every push to `main`, the nightly schedule and `workflow_dispatch` run
every proof lane. Both `select-lanes` and `check` always run. The lightweight
`select-lanes` job runs `scripts/ci-select-lanes`, diffs the PR against its
base, and picks the proof lanes the change can affect:

- `ci.yml`, `.github/actions/ci-tool/`, `scripts/ci-*`, `Cargo.toml`,
  `Cargo.lock`, `third-party/` or `scripts/tuna-nested`: every lane.
- most documentation, other workflows, `fuzz/` and the script self-tests: no
  proof lanes. The selector-owned documentation exceptions are
  `docs/nested-session.md` (`arch-package`), `docs/walkthrough*`
  (`gtk-shell`) and `docs/parity-ledger.md` (`parity-ledger`), each plus the
  lanes whose artifacts it needs. `scripts/ci-select-lanes` (`OWNED` and
  `NO_LANE`) is the authoritative, ordered mapping.
- a lane's own proof script, reference fixtures or packaging: that lane
  (plus the lanes whose artifacts it needs).
- anything else (crates, `share/`, other scripts): `gtk-shell` plus a
  round-robin slice of the other end-to-end lanes, rotated by run number.
  The slice size is the repository variable `CI_ROUND_ROBIN_SIZE`
  (default 2); each push to a PR covers a different slice and `main` runs
  them all after merge.

The step summary of `select-lanes` names the chosen lanes and why.
Independent lanes start after selection, without waiting for workspace
tests. GTK, grid comparison and fingerprint admission still require the
successful current-run `check`; the ledger also consumes its test list,
and image publication waits for it. Package and screenshot consumers still
wait for their producing proof jobs.

`ci-gate` requires both infrastructure jobs to succeed and fails when any
selected job fails or is cancelled. Only unselected proof lanes may be
skipped. To run every lane on a branch, dispatch `ci` on it.

## Dependency caches

Rust caches remain job-specific; no cache hit skips Cargo, executable
admission or a proof. Only non-PR runs on `refs/heads/main` may save them.
PRs and branch dispatches restore main's compatible caches without
creating competing branch copies. New keys need a successful main job
to seed them; the first run after a key change can be cold.

The `ci` and measured-coverage workflows also cache isolated installations
of cargo-audit 0.22.2, cargo-llvm-cov 0.9.1 and the pinned gtk4-layer-shell
source revision through `.github/actions/ci-tool`. Exact-key restores have
no fallback: tool version, installer/action content, Rust compiler and host,
native distro/architecture and the installed package inventory must match.
Cold paths build the exact locked Cargo tools or revision-checked GTK source;
warm paths activate those installations without recompiling. Only non-PR
main runs publish tool caches, using the same trust boundary as Rust targets.
The audit and instrumented tests still execute and all coverage reports keep
their existing paths. GTK caches contain only its staged `/usr/local` install,
not system packages, Rust targets or proof artifacts.
The staged files stay runner-owned so the cache action can archive and restore
them without sudo. Activation copies GTK into `/usr/local` as root without
preserving the staged ownership; it never changes ownership inside the cache.

Tool consumers sparsely check out `.ci-workflow-tools` at the immutable
`github.workflow_sha`, separate from the candidate source checkout. The
action invokes its paired installer through `GITHUB_ACTION_PATH` and hashes
those actual workflow-owned files, not similarly named candidate files.
Older PR heads need not contain the cache action or staged installer;
exact-head jobs still test the PR head, not the tooling checkout.
Before source scans, the action appends only `/.ci-workflow-tools/` to the
candidate repository's local Git excludes, preserving existing rules. This
also works on older heads with no matching `.gitignore` entry; genuinely
untracked candidate source remains visible to Git-based source checks.

The same writer policy and native-platform separation apply to measured
coverage, fuzzing, nested stress and visual capture. Workflows without a
main-push trigger seed on their existing scheduled or manual main runs;
their branch runs remain restore-only. The two fuzz targets retain their
shared nightly dependency cache, separate from other jobs and profiles.

Ubuntu keys include the native distro ID/version in addition to the
pinned cache action's compiler, architecture, environment and dependency
identity. `check` includes the standalone `third-party/reis` and
`third-party/smithay` targets. Grid comparison has separate cached
old/fixed targets under `target/ci-grid`, keyed to its reviewed old source;
fingerprint uses `target/ci-fprint`. Setting `CARGO_TARGET_DIR` opts these
two proof scripts into persistent build targets; without it, local runs
keep isolated targets inside their fresh artifact directory. Receipts,
admissions and verdicts are always freshly produced outside the cache.

The Arch job shares an explicit Cargo home between cache restoration and
the unprivileged package builder. Its key includes the installed native
package inventory, and its target is never shared with Ubuntu. Restoration
precedes the existing builder ownership change; package build, installation
and version checks still run.
Both paths are derived inside the container and published through
`GITHUB_ENV`, rather than using the host-side `github.workspace` expression.
The target stays under `GITHUB_WORKSPACE`; Cargo home lives under
`RUNNER_TEMP`, outside the workspace. The pinned cache action excludes
dependency manifests inside the workspace by default, so putting Cargo
home there would prune registry crates and their compiled dependencies.
After restoration, both the workspace and Cargo home become builder-owned.

When diagnosing misses, inspect the cache action's primary/restore keys,
hit status, archive size and transfer times before increasing storage.
Job/profile/native-environment caches are deliberately separate; a single
shared key can retain an incomplete first-writer snapshot. OCI/ORAS or
resumed VM sessions do not replace fresh source/package/runtime proofs.

## Local iteration with `just`

Iterate locally before pushing; a local loop is much faster than a CI
round trip. The [`justfile`](../justfile) wraps the commands below with the
settings local builds need (`just` lists the recipes):

| Recipe | Runs |
|---|---|
| `just check` | `fmt-check`, `clippy` and `test`, like the `check` job's core |
| `just test [cargo args]` | `cargo test --workspace`, or e.g. `just test -p tuna-compositor --lib orca::` |
| `just clippy` / `just fmt` / `just build [args]` | the matching cargo command |
| `just proof <name> [args]` | `scripts/tuna-<name> --artifacts <tmp>`, e.g. `just proof gtk-shell-proof` |
| `just ci-artifacts <run> [name]` | downloads a CI run's artifacts and prints the directory |
| `just clean` / `just gc` | deletes this worktree's target / frees disk (below) |
| `just disk-guard [--watch [s]]` | frees disk now, or keeps checking every `s` seconds (below) |

Every build uses its own checkout's `target/` (an inherited
`CARGO_TARGET_DIR` is ignored; set `TUNA_TARGET_DIR` to choose one),
two jobs, no incremental state, and waits on one machine-wide lock
(`/tmp/tuna-local-build.lock`), so builds from parallel worktrees queue
instead of exhausting memory.

The polkit unit tests can be run with
`timeout 15m just test -p tuna-shell-gtk polkit`. Their fake executable
helpers are written and made executable by a dedicated child process,
which exits before the helper is launched. Do not write those executables
in the multithreaded test process: a concurrent fork can inherit the
writable file descriptor and make execution fail with `ETXTBSY`, even
after the parent closes it or renames the file. A parallel fixture test
also checks immediate execution, literal script bytes and exit status;
the existing session tests still assert socket fallback and password
handling.

Cleanup is automatic where it is safe:

- `just proof` runs each proof with a fresh `HOME` and XDG directories
  inside its artifact directory, as on a CI runner, so desktop state in
  your own home can't leak in. One proof runs at a time machine-wide
  (`/tmp/tuna-local-proof.lock`). Proofs that need another proof's
  prebuilt binaries (`--bin-dir`) are refused with the name of their
  caller. When the proof ends, pass or fail, any process it started is
  stopped, including daemons that detached from it (`ibus-daemon
  --daemonize`, `dbus-daemon --fork`): each carries a `TUNA_PROOF_RUN`
  marker in its environment. A passing proof deletes its artifacts; a
  failing one keeps them and prints the path;
- `just gc` deletes the targets of worktrees whose pull request is merged
  or closed, removes those worktrees when they have no uncommitted
  changes, and expires `just ci-artifacts` downloads after six hours. It
  never touches the main checkout or the worktree it runs from;
- `just disk-guard` (`scripts/tuna-disk-guard`), when less than 40 GiB
  is free on the repository's filesystem (`TUNA_MIN_FREE_GB`), deletes
  the `target/` of each of this repository's worktrees that no running
  process references (by working directory, executable, arguments or
  environment) and that nothing touched for 45 minutes
  (`TUNA_IDLE_MINUTES`), plus `/tmp/tuna-proof-*` and `just ci-artifacts`
  entries older than 90 minutes. It logs each removal and leaves the main
  checkout's target alone unless given `--include-main`. `--watch
  [seconds]` repeats it (default every 600 s); on a shared host run it
  that way from a clean checkout of `main`;
- any build recipe runs `just disk-guard` first (it only removes anything
  below `TUNA_MIN_FREE_GB`, 40 GiB); automatic cleanup never calls `gc`,
  whose merged-PR cleanup is not process-aware.

VM lanes need KVM, so they run in CI's `marlin-vm` jobs or on a Corral
VM, not through these recipes.

## Before opening a PR

Run the workspace checks locally before opening a PR (`just check`):

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Clippy runs with `-D warnings`, so any warning fails CI.

`cargo test --workspace` includes the 100-run shell fault harness
(`hundred_fault_runs_keep_apps_alive_and_resync` in
`crates/compositor/tests/recovery.rs`) and needs no display or GPU.

The real Wayland-wire pointer tests in `crates/compositor/tests/move_resize.rs`
cover a window mapping beneath a stationary pointer (#425). The scene repick
must send `wl_pointer.enter` before any press, the first click must arrive
without replay, and closing the window must re-enter the window beneath.
The existing press-time refresh remains a separate regression; releases
keep Smithay's implicit grab ownership.

The lock-screen PAM test (`crates/compositor/tests/pam_unlock.rs`) needs
`pam_wrapper` (`libpam-wrapper` on Ubuntu). Without it the test prints
`pam_unlock: pam_wrapper not installed, skipped` and passes. CI sets
`TUNA_REQUIRE_PAM_WRAPPER=1`, which turns that skip into a failure; set it
locally to be sure the test really ran:

```sh
TUNA_REQUIRE_PAM_WRAPPER=1 cargo test --workspace
```

`scripts/tuna-auth-owner-ci-proof <artifact-directory>` runs the actual
native ownership and private PAM retirement cases. Its PID readiness
markers are published only after complete PID contents are written.
Artifacts retain supervised test-client panics and early compositor exit
status; a compiled test or cache hit is not a substitute for these cases.

The focused private-helper proof is
`TUNA_REQUIRE_PAM_WRAPPER=1 cargo test -p tuna-compositor --test pam_helper`.
An authenticated reply is provisional until the helper exits, its process
group has no live members, and its leader is reaped. The bounded `/proc`
scan tolerates only process-disappearance errors (`ENOENT`, or `ESRCH`
when a process is reaped between opening and reading its `stat` file);
other scan failures keep retirement quarantined. Its 500 ms cleanup budget
is separate from the conversation deadline and is not extended on a race.

Run one crate with `cargo test -p <package>` (package names are in each
crate's `README.md`, for example `cargo test -p tuna-compositor`).

Native resource observation regressions live in
`crates/wallpaper-svg/src/resource.rs`. Select `resource::tests::` in the native
test invocation when checking these cases separately.

The concurrent membership regression adds `new.conf` after the first directory
inventory and must reject that observation without sleeps or timestamp advances:
directory size, mtime and ctime can all remain equal across a member addition.
The observer compares two bounded, sorted raw-name inventories through the same
opened directory, then retains the descriptor/pathname identity guards. The
rescan-bound regression also requires an over-limit second inventory to fail.
These are nonrecursive observations, not atomic filesystem snapshots or proof
against mutations after the final guard; native file hashing and symlink
following retain their separate coverage.

Desktop-entry launch coverage lives in `tuna-shell-host`:
`cargo test --locked -p tuna-shell-host --lib apps::`.
It checks quoted multiword and empty arguments, field-code expansion after
argument grouping, and literal `$HOME` handling. Desktop `Exec` commands are
parsed as an argument vector, never evaluated by a shell.

`scripts/lib/tuna-perf-repeatability-tests.py` uses POSIX-shell logging-only
stubs for `sudo`, `truncate`, and `rm`, avoiding 96 unnecessary Python startups
per full paired scenario; its arity-prefixed NUL argv journal preserves empty
arguments, newlines, and non-UTF-8 bytes without shell re-parsing.
The Python VM stub still exercises real `Popen`/`Handoff` termination and
reaping rather than creating guests.
This fixture optimization does not replace the twelve real guest acquisitions
or change production measurement windows.

## Nested proofs

Each proof job builds the workspace, runs the nested compositor on a
private Xvfb (unless `DISPLAY` already works), drives it with real X input,
and writes frames, logs, and usually an `assertions.txt` into the
artifacts directory. A nonzero exit is a failure; the frames are for human
review. CI shellchecks each script before running it.

The native grid-drag cancellation proof holds the pointer button until both
the compositor reports cancellation and a fresh, complete, owner-bound AT-SPI
tree restores the normal page arrow without a drag preview. Widget removal
can invalidate a traversal while GTK processes cancellation; that snapshot
is rejected, and the existing five-second polling budget must still obtain
a valid tree. Persistent transport errors, wrong owners and incomplete trees
cannot satisfy the gate.

| CI job | Command | Extra packages | What it asserts |
|---|---|---|---|
| `journey` | `scripts/tuna-journey --artifacts journey-artifacts` | `xvfb xdotool x11-apps scrot jq dbus`, Mesa EGL (`libegl1 libgl1-mesa-dri`) | Overview, search, launch from search, Escape, workspace switch, Alt+Tab; writes `assertions.txt` |
| `journey` | `scripts/tuna-release --allow-dirty --out package-artifacts && scripts/check-release-package package-artifacts/*.deb` | as above | The Debian package builds and passes its checks |
| `app-content` | `scripts/tuna-app-content --artifacts app-content-artifacts` | `python3-venv`; the script installs Playwright and Chromium itself | Chromium maps as a Wayland client, tiles with Super+Left, closes with Alt+F4 |
| `scroll-proof` | `scripts/tuna-scroll-proof --artifacts scroll-proof-artifacts` | `python3-venv` | Scroll-mode strip: Super+Shift+T, Super+R, wheel scrolling, gap geometry |
| `gtk-shell` | `TUNA_REQUIRE_PAM_WRAPPER=1 scripts/tuna-gtk-shell-proof --artifacts gtk-shell-artifacts` | gtk4-layer-shell (`scripts/ci-install-gtk4-layer-shell`), `xwayland pipewire wireplumber at-spi2-core python3-pyatspi fonts-cantarell ffmpeg libpam-wrapper` and the GStreamer PipeWire plugins; see the job for the full list | GTK shell panel, calendar, quick settings, overview, notifications, lock and unlock, AT-SPI trees; writes `assertions.txt` |
| `gtk-shell` | `scripts/tuna-scale-proof --artifacts gtk-shell-artifacts/scale` | as above | The same session at 150 percent scale; writes `assertions.txt` |
| `gtk-shell` | `scripts/tuna-slow-portal-proof --artifacts gtk-shell-artifacts/slow-portal` | as above | On a dbus-daemon bus whose portal activation hangs, the shell owns `org.gnome.Shell` within 15 s (40 s on GTK before 4.17, whose GtkApplication always waits on the inhibit portal) and makes no GDK portal call; writes `assertions.txt` |
| `drm-smoke` | `scripts/tuna-drm-smoke --artifacts drm-smoke-artifacts` | `seatd jq ffmpeg kmod`; run `scripts/ci-install-vkms` after `apt-get update` to ensure vkms for the running kernel | Hardware backend on the vkms virtual KMS device; needs `sudo` for `modprobe` and seatd |

`app-content` and `scroll-proof` download Playwright and Chromium into a
temporary venv on every run, so they need network access.

**Start `tuna-gtk-shell-proof` from a fresh artifacts directory.** The
script points `XDG_STATE_HOME` at `gtk-shell-artifacts/state`, and the
shell keeps its state, notification history included, under
`$XDG_STATE_HOME/tuna-shell`. A reused directory carries the previous
run's history into the next one, so the notification stages no longer
start from an empty list. Remove it first:

```sh
rm -rf gtk-shell-artifacts
TUNA_REQUIRE_PAM_WRAPPER=1 scripts/tuna-gtk-shell-proof --artifacts gtk-shell-artifacts
```

`scripts/tuna-drm-smoke` loads a kernel module and starts seatd as root.
Run it in a VM or a disposable machine rather than your desktop session.
The Ubuntu setup helper first checks whether vkms is already installed for
`uname -r`, then checks the matching `linux-modules-extra` and `linux-modules`
package candidates if needed. For example, Ubuntu's
[7.0.0-1017 Azure modules package](https://packages.ubuntu.com/noble-updates/amd64/linux-modules-7.0.0-1017-azure/filelist)
includes vkms without a separate extra-modules package. No usable candidate
or a still-missing module fails setup, and package installation errors are
fatal. Module loading and the real vkms device and presentation assertions
remain mandatory in the smoke test.

`D-POINTER-SETTINGS` retains its uinput mouse, trackball, pointing stick and
touchpad across a native compositor restart, then reads every device's
libinput configuration again without motion or another hotplug. In
[CI run 41175449190](https://github.com/tuna-os/tuna-desktop/actions/runs/41175449190/job/124476667615),
defaults, live changes and replug passed with libinput 1.25, but the restarted
session published an empty device list. Seat assignment queues startup device
additions before the polling fd necessarily becomes readable; native setup
now consumes those additions through the same configuration path as hotplug,
without dispatching input or consuming a pending non-addition event. See
[libinput's event queue API](https://wayland.freedesktop.org/libinput/doc/latest/api/group__base.html).
The pointer launcher also closes the inherited emulator FIFO writer (FD 7),
so a running restarted compositor cannot prevent emulator EOF during cleanup.
`scripts/tests/test_ci_install_vkms.py` contains launcher descriptor and
coldplug/restart contract regressions; these do not qualify native device
behavior. [CI run 41193585314](https://github.com/tuna-os/tuna-desktop/actions/runs/41193585314/job/124520435500)
passed the unchanged arrival, live, replug, restart and reset gates with
libinput 1.25. The subsequent hot-corner proof requires
`gsettings-desktop-schemas` for `org.gnome.desktop.interface`; the DRM lane
installs that package explicitly alongside the Hebrew locale and GTK translations.

## Marlin VM lane

The `marlin-vm` job (#68) boots the real TunaOS Marlin image with Tuna Desktop
installed and checks that the display manager logs into the Tuna Desktop
hardware session. A CI-only guest service probes the calendar and portal
on the test user bus; the runner reads the serial log and takes QMP
screendumps from outside.

1. It builds `packaging/marlin/Containerfile` (Marlin GNOME 51 plus the
   Tuna Desktop Arch package from `arch-package`) with rootful podman. On top of
   that it builds the CI-only layer `packaging/marlin/vm-lane/Containerfile`.
   This layer adds a `tuna-test` user and turns on GDM automatic login
   into "Tuna Desktop (preview)". It also sends the kernel console and the
   journal to `ttyS0`. The shipped image does not get these changes.
2. It runs `bootc install to-disk --via-loopback --generic-image` from
   that image to write a 20 GB raw disk.
3. `scripts/tuna-vm-lane --disk disk.raw --out vm-lane-artifacts --boots 5` boots
   the disk in QEMU/KVM with OVMF firmware and virtio-vga at 1280x800.
   It writes these assertions to `assertions.txt`:

| ID | What it asserts |
|---|---|
| `V-DRM` | The serial journal has `tuna-compositor: drm: output ...`, so the compositor lit an output on the DRM/KMS backend |
| `V-SHELL` | `tuna-shell-gtk: keybindings:` appears and shell supervision never logs `BudgetExhausted` |
| `V-PANEL` | The session opens on the overview, where the panel is transparent. After Escape, a screendump shows a pure black strip at y=5 across at least 90 percent of the width (the top panel) over a desktop that is not one flat color |
| `V-NOPANIC` | No `panicked` anywhere in the serial log |

The lane retains plymouth and makes five pristine snapshot boots. Tuna Desktop
explicitly registers its ready session and display with GDM instead of
relying on GDM's delayed fallback. Every boot must retain DRM ownership
and show the panel. Earlier tests disabled plymouth after a greeter stole
DRM master about ten seconds after autologin; the repeated gate now tests
that startup path directly.

Additional gates are `V-GDM` (explicit registration, no session pause),
`V-CALENDAR` (CalendarServer responds), `V-PORTAL` (portal Settings responds),
`V-PORTAL-SCREENSHOT` and `V-PORTAL-SCREENCAST` (the portal frontend exports
both interfaces, so `tuna-portals.conf` routes them to a working backend),
`V-LOCK` (the final tour observes unlocked → locked → unlocked), and
`V-REPEAT` (all five boots pass). Earlier boots keep their own artifacts
under `repeat-NN/`; `repeat-manifest.json` records every boot, while the
root `assertions.txt` aggregates results for the parity ledger.

The installed strip gate validates both PNG and PPM pixels against the same
owned window geometry. A transient obstruction or an unpresented frame may
retry for at most 10 seconds, within the existing 120-second strip deadline.
The first rejected image pair is retained as `*-not-ready.{png,ppm}`; the
receipt records capture attempts. Window identity/geometry drift, malformed
images and QMP failures remain terminal. A retry never qualifies pixels from
the rejected frame or claims presentation latency.

The artifact (`marlin-vm`) holds `serial.log`, `tuna-lines.log` (every
`tuna-*` line), the boot frames, `V-OVERVIEW.png` (the login overview),
`V-SESSION.png` (the desktop), and `manifest.json`.
The manifest records the base image digest, the test image ID, the Tuna Desktop
package, and the QEMU version. If the run fails or times out (15
minutes), the script prints the relevant serial lines before exiting.

When the shell has printed but has not logged `keybindings:` within
20 seconds, a bounded startup watchdog captures
`shell-startup-stack-1.json` in that boot's artifact directory. It captures
again at 60 seconds if startup is still stalled. If the shell prints
nothing, the first capture is 40 seconds after the DRM output appears.
The CI-only guest layer installs gdb; the dump includes every session
shell thread's kernel wait site/stack and user backtrace, without frame
arguments, locals, or debuginfod downloads. Guest transport/debugger errors
are retained in the JSON instead of hiding the startup failure. This
diagnostic does not restart the shell or relax any readiness gate.

After the assertions pass, `--tour` records a feature tour. The script
sends keys and pointer clicks through QMP to open the overview and search,
the app grid, quick settings, and the calendar. It then opens Disks,
System Monitor, and Files, switches between them with Alt+Tab, and turns
scroll mode on and off (Super+Shift+T, Super+R, Super+Left/Right). Last,
it locks the screen with Super+L and unlocks it with the `tuna-test`
password through PAM. Only the last repeated boot records the tour. The
script takes a screendump twice a second and plays the result back at
double speed. Before each section it inserts a title card, a committed
PNG in `scripts/lib/vm-tour-cards/` (`render.py` there redraws them). The
`tuna-tour` artifact, kept for 30 days, holds `tuna-tour.webm`,
`preview.webp`, and one `T-<section>.png` still per section. The tour
asserts nothing; only a missing video fails the job.

You need `/dev/kvm`, about 30 GB of free disk, and the podman and
`bootc install` commands from the job to run this locally. Do not use your
desktop session for it.

To drive the published image through GDM in a KubeVirt VM with keyboard
and pointer input and save a console frame for every step, see
[Published-image journey through Corral](corral-journey.md).

## Animation clock and motion policy

Compositor animations (the overview transition, the scroll-strip springs and
the tile preview) read time from one animation clock
(`crates/compositor/src/animation_clock.rs`). Two environment variables
make their timing reproducible on slow CI renderers such as llvmpipe:

| Variable | Read by | Effect |
|---|---|---|
| `TUNA_ANIMATION_CLOCK` | compositor, only when `TUNA_COMPOSITOR_STATE` is also set | Absolute path of a clock file. While the file is absent, animations run on real time. When it first holds a whole number of milliseconds `N`, animation time freezes at that moment plus `N`. Rewriting it with `M` moves time to that moment plus `M`. Removing it resumes real time. Time never runs backwards across these switches. |
| `GNOME_SHELL_SLOWDOWN_FACTOR` | shell (sent to the compositor in `InputSettings`) | GNOME Shell's own slow-down factor: a positive number multiplies every animation duration, as GNOME's `adjustAnimationTime` does. Anything else keeps 1. Read at shell start. |

Write the clock file atomically (write a temporary file, then `mv`), as
`anim_clock` in `scripts/tuna-gtk-shell-proof` does. The compositor state
file reports `animation_clock_manual` and, while the clock is frozen, `animation_clock_ms`, plus the
effective `motion_policy` (`full`, `fade-only` or `off`),
`animation_slowdown` and `animations_enabled` (GNOME's
`enable-animations`, which Reduced Motion leaves on). For example, the
overview transition takes 250 ms each way, so after Super:

```sh
echo 0 > clock.tmp && mv clock.tmp "$TUNA_ANIMATION_CLOCK"    # freeze
xdotool key Super_L                                            # progress 0
echo 125 > clock.tmp && mv clock.tmp "$TUNA_ANIMATION_CLOCK"  # progress 0.5
rm "$TUNA_ANIMATION_CLOCK"                                    # real time again
```

Proof `G-ANIMATION-CLOCK` samples the overview this way at 125 and 250 ms
in both directions. `G-ANIMATIONS-OFF` and `G-INTROSPECT-MOTION` check the
live motion policy: animations off snaps everything, Reduced Motion
(fade-only) snaps overview and strip motion but keeps the idle-shield fade.

The shell's Alt+Tab fades read the same clock (`crates/shell-gtk/src/anim_clock.rs`),
and the compositor state lists `switcher_thumbnail_alphas`, the opacity each
switcher window image is drawn at. `G-SWITCHER-FADE` freezes the clock and
samples them at 0, 50 and 100 ms of the thumbnails' fade in and 50 ms into
the fade out (Alt+Escape, which cancels without moving focus), where GNOME's
ease-out quad gives 0, 191/255 and 1, then 64/255. The grid-cancel comparison
slice skips the sampling: its old binaries predate the published alphas.

## Frame cost

The compositor state (`TUNA_COMPOSITOR_STATE`) carries `frame_cost`, one
entry per output describing its last drawn frame: `rendered` and `culled`
element counts, `damage_rects` and `damage_area` against `output_area`,
and the window ids the frame shows (`drawn_windows`) or wholly hides
behind opaque surfaces (`culled_windows`). Both cover the whole output, so
a frame that repaints only the panel or the cursor still lists the windows
it leaves on screen as drawn. `throttled_windows` lists the windows
whose frame callbacks run at the hidden rate (about 1 Hz): off the scene
or wholly culled, unless the overview, the switcher or a cast shows them.
Proof `G-OCCLUSION-CULL` fullscreens an opaque window over another and
requires the lower one to be culled and throttled, then drawn at full
rate again once the upper window's translucent shadow returns.

## Settings writes in proofs

`scripts/tuna-gtk-shell-proof` keeps GSettings in a private keyfile
(`GSETTINGS_BACKEND=keyfile`). Each writer rewrites the whole file from
the copy it loaded, so two writers at once can lose a key. The
compositor writes `numlock-state` through the `gsettings` CLI after
every Num Lock change. Until its `keyboard.numlock_saving` state field
is `false` again, any setting the proof writes can be undone. Wait for
it after a Num Lock change and before the next `gsettings set`. The
`keyboard.keymap_generation` field goes up each time a keymap is
applied. Wait for it after changing the input sources or `xkb-options`
and before typing.

## Parity ledger check

The `parity-ledger` job runs `scripts/tuna-ledger` against the test list
from `check` and the `assertions.txt` files from the proofs. Reproduce it
after running the proofs above:

```sh
cargo test --workspace -q -- --list --format terse > test-list.txt
scripts/tuna-ledger check --tests test-list.txt \
  --assertions journey-artifacts/assertions.txt \
  --assertions gtk-shell-artifacts/assertions.txt \
  --assertions gtk-shell-artifacts/scale/assertions.txt \
  --assertions drm-smoke-artifacts/assertions.txt \
  --assertions vm-lane-artifacts/assertions.txt
```

`scripts/tuna-ledger summary` prints status counts only.

The rule, from [the parity ledger](parity-ledger.md): a row is `pass` only
when a test or recorded review compares Tuna Desktop against GNOME 51 baseline
evidence, and `untested` is not a pass. The check fails when a row whose
status starts with `pass` cites no test, when a cited `cargo:` test does
not exist in the test list, when a cited `journey:` or `proof:` assertion
is not recorded as passing, or when the table is malformed or repeats an
ID. If your change renames or removes a test, update every ledger row that
cites it in the same PR.

## Release screenshots

The `docs-shots` job runs `scripts/tuna-docs-shots collect` over the
`journey`, `app-content` and `scroll-proof` artifacts and uploads the
curated frames as the `docs-shots` artifact. Nothing in the repository is
refreshed from it. The docs show the GTK shell through the
[walkthrough](walkthrough.md), which the GTK shell proof captures.

## Pixel parity capture (not in CI)

`scripts/tuna-parity-capture` captures Tuna Desktop in the states
`scripts/tuna-gnome-reference` captures GNOME 51 in, for comparison with
`scripts/lib/tuna-parity-compare.py`. CI does not run it. The workflow and
the states are in [pixel parity with GNOME 51](gnome-parity.md).

## When a run fails

Read `nested.log` in the artifacts directory first (`compositor.log` for
`tuna-drm-smoke`); see
[nested session troubleshooting](nested-session.md#troubleshooting). Do not
retry a deterministic test until it passes: the timing rule in the
[development guide](development.md#writing-a-test) applies.

## Bounded nested stress

`nested stress` runs four concurrent Wayland GTK clients. Each round maps
and destroys 400 windows; three warmup rounds settle renderer caches.
After every measured round, Escape and Super must still change the
compositor's overview state, and every destroyed window must disappear.
The lane then sends 1,000 notifications and closes each over D-Bus.

The per-change smoke uses ten measured rounds; nightly uses 300. The
report retains PSS/RSS, file descriptors and descendant process counts
for the compositor and supervised shell after each round. Final PSS must
stay within warmup PSS × 1.25 + 64 MiB, and fd count within warmup + 32.
These diagnostic bounds catch unbounded growth in this workload; they
are separate from the GNOME performance targets in #73. A panic, lost
input response, orphaned window or hung worker fails the lane. Logs,
metrics, final state, assertions and the exact source revision are kept.

Run it on a development host after building the compositor and GTK shell:

```sh
scripts/tuna-stress --rounds 10 --out /tmp/tuna-stress-fresh
```

Use a fresh empty artifact directory for each run. The harness starts its
own Xvfb, session bus and private user state. It covers client churn and
notification flood portions of #203; malformed Wayland requests, service
faults, hotplug, locked-input adversaries and a measured 24-hour VM soak
remain open.

## Control decoder fuzzing

The `control decoder fuzz` workflow exercises both arbitrary length-prefixed
frames and mutated postcard bodies with a generated valid length prefix.
Successful decodes must re-encode into another valid frame. Seed inputs
include a Hello, a truncated varint, and an oversized declared frame.

Pull requests touching the decoder or fuzz harness run each target for
60 seconds; the nightly lane runs each for 600 seconds. The address
sanitizer, five-second per-input timeout and 2 GiB RSS limit turn crashes,
hangs and runaway allocation into failures. Logs, the resulting corpus,
and crash artifacts are retained for 14 days. A found crash needs a small
regression test in the protocol crate before its fix is merged.

To reproduce locally, install the pinned tool and toolchain, then run:

```sh
rustup toolchain install nightly-2026-08-28
cargo install cargo-fuzz --version 0.13.2 --locked
cargo +nightly-2026-08-28 fuzz run control_body -- -max_total_time=60 -max_len=1048580 -timeout=5 -rss_limit_mb=2048
```

This covers the decoder portion of #203. Wayland misuse, client churn,
service failures and a measured long soak remain separate stress gates.
