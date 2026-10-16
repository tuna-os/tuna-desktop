# Published Marlin Tuna Desktop nightly

The `marlin-tuna-nightly` workflow qualifies one immutable digest of the
published TunaOS Marlin Tuna Desktop flavor. The nightly resolves the newest
`ghcr.io/tuna-os/marlin:tuna` once and pins every later step to that digest;
a manual dispatch qualifies exactly the `digest` input. It installs a CI
fixture layer on that image and does not rebuild or replace Tuna Desktop.
The fixture requires every shipped package version to remain present
unchanged, and checks hashes of the six Tuna Desktop binaries, the session
entry, the shipped Cage/gtkgreet wrapper, greetd configuration, CSS and
session list before and after setup.

The fixture provisions the public `tuna-test` account and adds QEMU guest
agent probes, a count-only GTK input client, Disks and System Monitor for
the existing application tour, plus serial capture. A test-user profile
exports the introspection path and directs session output to the journal.
It preserves the shipped greeter wrapper, styling and session list. An
initial/autologin session is rejected.

Each of five pristine snapshot boots waits for the actual non-root
gtkgreet process and checks its session picker (`V-GREETER-LIST`): gtkgreet
lists `/etc/greetd/environments` verbatim and preselects the first line, so
that line must read `Tuna Desktop`, not a command (tunaOS#3110). The boot
then types the fixture username and password through QMP and requires a
real non-root logind session with `Service=greetd` and the actual Tuna
Desktop compositor (`V-GREETD-LOGIN`). Portal checks require Settings, the
GNOME backend, and Screenshot and ScreenCast routing. The last boot runs
the existing semantic tour and [lifecycle checks](vm-lifecycle.md),
including lock/unlock, strict first-resume locked state, sleep
authentication fail-closed, real PAM refusal, VT repaint and logout. Every
boot runs even after an earlier one fails, so one run reports every check.
No pause substitute or fabricated greeter success is accepted.

| Area | Checks |
|---|---|
| Install and boot | `bootc install to-disk` of the digest, `V-DRM`, `V-SHELL`, `V-PANEL`, `V-NOPANIC`, `V-REPEAT` |
| Greeter | `V-GREETER-LIST`, `V-GREETD-LOGIN`, `V-DISPLAY-MANAGER` |
| Session and portals | `V-CALENDAR`, `V-PORTAL`, `V-PORTAL-GNOME`, `V-PORTAL-SCREENSHOT`, `V-PORTAL-SCREENCAST` |
| Lock and sleep | `V-LOCK`, `V-AUTH-FAIL-CLOSED`, `V-SLEEP-AUTH-FAIL-CLOSED`, `V-SLEEP-FRESH-AUTH`, `V-LIFECYCLE` |

The VM profile is the existing q35/OVMF KVM guest: four CPUs, 6 GiB RAM,
virtio VGA at 1280×800 and the existing 20 GiB raw disk. The daily workflow
requires KVM rather than silently falling back to emulation. It also runs
on pull requests that change the workflow, fixture or harness.

The artifact records the digest under test, fixture image identity, shipped
package version and hashes, harness commit, run ID, frames, tour clip,
serial journal and redacted introspection. Passwords, process arguments
and input key values are excluded from probe artifacts.
`qualification.json` and the job summary list every check with its result
for that digest. A failure also writes `failure.json` with the run URL,
digest and a bounded journal excerpt containing only lifecycle/health
markers; it is retained with the other evidence instead of sending
messages or filing issues.

The workflow must produce an actual green published-flavor run before
issue #68 is accepted. These checks do not qualify physical hardware,
output hotplug or a 24-hour soak.

Source references: [shipped TunaOS greeter wrapper](https://github.com/tuna-os/tunaOS/blob/main/build_scripts/desktop/greetd-gtkgreet.sh),
[greetd profile/session execution](https://github.com/kennylevinsen/greetd/blob/master/greetd/src/session/worker.rs),
and [gtkgreet entry focus and activation](https://github.com/kennylevinsen/gtkgreet/blob/master/gtkgreet/window.c).
