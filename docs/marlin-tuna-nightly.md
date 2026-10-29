# Published Marlin Tuna Desktop nightly

The `marlin-tuna-nightly` workflow qualifies one immutable digest of the
published TunaOS Marlin Tuna Desktop flavor. The nightly resolves the newest
`ghcr.io/tuna-os/marlin:tuna` once and pins every later step to that digest;
a manual dispatch qualifies exactly the `digest` input. It installs a CI
fixture layer on that image and does not rebuild or replace Tuna Desktop.
The fixture requires every shipped package version to remain present
unchanged, and checks hashes of the six Tuna Desktop binaries and of every
shipped login-path file before and after setup: the session entries, the
`gdm-password`, `gdm-launch-environment` and `tuna-lock` PAM services,
GDM's `custom.conf` and unit, and the TunaOS greeter theme overlay. The
build also requires `display-manager.service` to be `gdm.service` and
GDM's configuration to enable neither automatic nor timed login.

Since tunaOS#3438 (2026-10-20) the published flavor logs in through stock
GDM: it ships no greetd, gtkgreet or `/etc/greetd` files. The fixture
provisions the public `tuna-test` account and adds QEMU guest agent probes,
a count-only GTK input client, Disks and System Monitor for the existing
application tour, plus serial capture. Its AccountsService record only
lists the account in GDM's user list; it names no session, so the shipped
image alone decides which session starts. GDM 51.1 starts a Wayland session
without a login shell and imports the systemd user environment, so the
introspection path comes from the account's `environment.d`; session output
reaches the journal through GDM's own `gdm-wayland-session` stream. An
initial/autologin session is rejected.

Each of five pristine snapshot boots waits for GDM's actual non-root
gnome-shell greeter in its active `gdm-launch-environment` session and
checks the sessions GDM offers (`V-GREETER-LIST`), computed as GDM 51.1
`libgdm/gdm-sessions.c` does: entries with `NoDisplay`, `Hidden` or a
missing `TryExec` program are dropped and one entry per name is kept. The
only offered session must be `tuna` named `Tuna Desktop`; gnome-shell
51.0's login dialog then shows no session menu. A fresh account has no
saved session, so GDM starts its fallback session instead: `[daemon]
FallbackSession`, default `gnome` (GDM 51.1 `daemon/gdm-session.c`). That
lookup skips only `Hidden` entries, so a `NoDisplay` GNOME entry still
starts. `V-GDM-FALLBACK` requires the fallback to be `tuna`. The boot then
selects the account in GDM's user list, types the fixture password through
QMP and requires a real non-root logind session with `Service=gdm-password`
and the actual Tuna Desktop compositor (`V-GDM-LOGIN`). If the session runs
another desktop, the check names it and does not type into that session.
Selection tries Enter on
the focused user, then Tab and Enter, then the typed user name, as
tunaOS's own GDM login gate does. Portal checks require Settings, the
GNOME backend, and Screenshot and ScreenCast routing. The last boot runs
the existing semantic tour and [lifecycle checks](vm-lifecycle.md),
including lock/unlock, strict first-resume locked state, sleep
authentication fail-closed, real PAM refusal, VT repaint and logout back to
the GDM greeter. Every boot runs even after an earlier one fails, so one run
reports every check. No pause substitute or fabricated greeter success is
accepted.

| Area | Checks |
|---|---|
| Install and boot | `bootc install to-disk` of the digest, `V-DRM`, `V-SHELL`, `V-PANEL`, `V-NOPANIC`, `V-REPEAT` |
| Greeter | `V-GREETER-LIST`, `V-GDM-FALLBACK`, `V-GDM-LOGIN`, `V-DISPLAY-MANAGER` |
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

Source references: [TunaOS GDM switch for Tuna flavors](https://github.com/tuna-os/tunaOS/blob/main/experiences/tuna/README.md),
[TunaOS session hiding](https://github.com/tuna-os/tunaOS/blob/main/build_scripts/desktop/tuna-hide-other-sessions.sh),
[GDM 51.1 session list](https://gitlab.gnome.org/GNOME/gdm/-/blob/51.1/libgdm/gdm-sessions.c),
[GDM 51.1 Wayland session launch](https://gitlab.gnome.org/GNOME/gdm/-/blob/51.1/daemon/gdm-wayland-session.c),
[GDM 51.1 session output stream](https://gitlab.gnome.org/GNOME/gdm/-/blob/51.1/daemon/gdm-session-worker.c),
and [gnome-shell 51.0 login dialog](https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/gdm/loginDialog.js).
