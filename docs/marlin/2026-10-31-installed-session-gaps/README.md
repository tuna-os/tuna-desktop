# Installed Marlin LockedHint and libnotify qualification

2026-10-31, owned Corral VM `tuna-q-isg-mar1`, `karnataka-k3s`, node
`karnataka`, namespace `corral-vms`, 4 vCPU / 6 GiB / 40 GiB, absolute USB
tablet, bochs-drm `Virtual-1` 1280×800. Scope: **logind LockedHint and
notify-send/GetServerInformation only**. No portal qualification is claimed.

## Result and source/publication boundary

Both failures reproduce on the **unmodified published** Marlin image and both
pass in a real GDM-launched Tuna session on the **existing exact-main CI-built
bootc preview image**. No product code was changed or duplicated: main already
contains both fixes. This does **not** qualify a newer release, a different main
revision, or a newly published `marlin:tuna` image.

| Gap | Published baseline | Exact-main installed candidate | Cause |
|---|---|---|---|
| logind LockedHint | Super+L: ScreenSaver `GetActive` `(true,)`, but seat0 `LockedHint=no`; correct password unlock: `(false,)`, `no` | Super+L: `(true,)`, `yes`; correct password unlock: `(false,)`, `no` | Published beta2 source predates `90417283c6f69945acb1ac401361d3204b728d7c` (#849). Main reports compositor lock transitions to the current logind session. |
| notify-send UnknownMethod | Introspection omits `GetServerInformation`; direct call and real `notify-send` both exit 1 with `org.freedesktop.DBus.Error.UnknownMethod` | Introspection exports four string outputs; call returns `('tuna-shell', 'Tuna', '0.1.0', '1.2')`; real `notify-send` exits 0 and its banner is visible | Published beta2 source predates `1bc9d96bfd1fff8e171fa13624303782bfc8f6f4` (#850). Main adds the mandatory notification server-information method used by libnotify. |

The release tag `v1.0.0-beta.2` resolves to
`1e6ce28f4d5dfd7918ac7340bd93454a9fcc211c` (2026-10-29 15:00:47 -05:00).
The notification fix merged at 16:24:13 -05:00 and the logind fix at
21:03:22 -05:00 that day, **after the tag**. The package factory's beta2-2
recipe still downloads that exact release tag, not current main. Its retained
[source recipe](published-package-recipe.yaml) records source archive SHA-256
`1e8abf38f4959a171007c3c61865057b76deaeb92aa7cbb79bcade830ec999b3`.
The installed baseline `pacman -Qi` independently names the same tag URL.
Incrementing the recipe's package release to 2 does not include later upstream
source fixes. The remaining delivery boundary is publishing source containing
these fixes and rebuilding/qualifying the released flavor; this evidence does
not assert that boundary has been crossed.

## Subjects and package/binary identity

### Published baseline

- Requested and booted image:
  `ghcr.io/tuna-os/marlin@sha256:d0e65bce0ac55cb6c58bcf3739e30d6372fe4be978e6ea7c0c1310b3962bdfa5`.
- Native package: `tuna-desktop 1.0.0beta.2-2`, build date
  2026-10-30 23:30:12 UTC; GDM `51.1.1-1`, GNOME Shell `1:51.1-1`,
  libnotify `0.8.9-1`. `bootc status` is retained in the manifest.
- GDM seat0 session 5: `Service=gdm-password`, `Type=wayland`,
  `Class=user`, `State=active`. Running compositor PID 2138 and shell PID 2222
  `/proc/PID/exe` hashes match the installed payload below.
- The original package archive was **not retained; its archive SHA-256 is
  unknown**. The immutable image digest, native installed package query,
  source archive pin, and installed/running executable hashes bind this
  baseline without substituting a different beta2 package.

| Executable | Published SHA-256 | CI candidate SHA-256 |
|---|---|---|
| `tuna-session` | `c0cbf31288061099f4ceb7a475ab3dc3bcc6905252a5fc5db2af3b641594fdb4` | `d9a612ef48108826b5efcad82505c38c46b0c35503c8705b4be7a0218c4eeb2d` |
| `tuna-compositor` | `de2c70dfd556945e1b812b8caf5f61cd9f5e27a58d85f5183940f92357ad99c5` | `975c5b677d2196e586d78074f1ea289206fe3a67bbfdb25b9349015079bb6520` |
| `tuna-shell-gtk` | `c517b237d455e20f72940f85e555d02e5f3f85a7098cfc5defa1a12d1c58ba46` | `4e995fd2c0e598c0546c211c5de0fefdf9b60d388edf1e1b1c3ce5f0832866e1` |

### Existing CI image, not a sideload

- Source: `7a623e10b856ca663343271de6528f87e82dc75b`.
- Existing [CI run 41227349354](https://github.com/tuna-os/tuna-desktop/actions/runs/41227349354),
  artifact `tuna-arch`, ID `13054270086`.
  The overall workflow concluded **failure**; artifact availability and the
  installed checks here are not a claim that the whole workflow passed.
- Package: `tuna-desktop-1.0.0beta.2-1-x86_64.pkg.tar.zst`, SHA-256
  `8e123e50d29d73d007c377401c0002893efa400572ff7e9443655c297fd05de3`.
  Zip SHA-256: `f2bdae45b28e4810a3c5278aba31fafc003f2871c710dca3a3b6ad38732d43d4`.
  Native package metadata reports build date 2026-10-31 18:45:49 UTC.
  CI's `arch-package` job uses `TUNA_LOCAL_SRC=$GITHUB_WORKSPACE`, not the
  release tag; equal beta version labels therefore do not imply equal source.
- Existing CI tag `marlin-tuna-preview:7a623e10b856ca663343271de6528f87e82dc75b`
  resolved to, and Corral installed:
  `ghcr.io/tuna-os/tuna-desktop/marlin-tuna-preview@sha256:fd982116d87a978e795f462e412d7acab0d3d7d3af035285002979033a8e92a8`.
  [Registry inspection](candidate-image.json), [Corral install receipt](candidate-build.log),
  and guest `bootc status` are retained. No pipeline was triggered for this work.
- `corral bootc switch` rebuilt the owned disk from this digest, with the public
  `tuna-test` fixture account. **No `bootc usroverlay`, `pacman -U` sideload,
  replacement running binary, or nested compositor was used to qualify it.**
- The real GDM chooser's selected Tuna radio option is in
  [frame 16](16-gdm-tuna-selected.png). Final Tuna session 16 is seat0,
  `gdm-password`, Wayland, active. Compositor PID 4907 has
  `XDG_SESSION_ID=16`, `XDG_SESSION_DESKTOP=tuna`, `GDMSESSION=tuna`,
  `XDG_CURRENT_DESKTOP=Tuna:GNOME`. Bus owner of
  `org.freedesktop.Notifications` is `tuna-shell-gtk` PID 5008 in
  `session-16.scope`. Their `/proc/PID/exe` hashes match both installed files
  and the independently downloaded CI package's extracted files.
- `GetServerInformation`'s `0.1.0` is the existing implementation's
  `CARGO_PKG_VERSION`; it is not used as a source-revision or release identity.

## Real console evidence

Input uses the existing `scripts/tuna-corral-journey` Guest/Console helpers,
Playwright and Corral's own noVNC canvas. Console keys, pointer selection,
Super+L, password entry and Enter are real VNC input. D-Bus probes are run
on the fixture user's actual session bus; none sets LockedHint directly.
[manifest.json](manifest.json) retains commands, exit codes, stdout/stderr,
UTC observation times and frame SHA-256 values.

| Evidence | Frame |
|---|---|
| Published GDM and actual Tuna login | [01](01-published-gdm.png), [02](02-published-session.png) |
| Published Super+L and successful password unlock | [03](03-published-super-l-lock.png), [04](04-published-unlock.png) |
| Candidate actual Tuna hardware session | [17](17-tuna-candidate-session.png) |
| Successful libnotify notification, visible banner | [18](18-tuna-notify-send.png), [19](19-tuna-notification-list.png) |
| Candidate Super+L, password prompt, unlocked desktop | [20](20-tuna-super-l-lock.png), [21](21-tuna-unlock-password.png), [22](22-tuna-unlocked.png) |

The immediate published lock frame 03 is an opaque blue/power-save frame,
not a rendered authentication prompt. Its concurrent ScreenSaver=true and
LockedHint=no probes establish the reported mismatch; frame 04 and
ScreenSaver=false record successful unlock. Candidate frame 21 explicitly
retains the real Tuna password prompt before correct-password unlock.
Frame 19 retains a visible notification banner despite its historical
`notification-list` filename; no claim that a notification-list menu opened
is needed for this qualification.

### GNOME 51 comparison, including actual reference session

The initial candidate-image GDM selection landed in GNOME, not Tuna.
Those observations are retained honestly as **GNOME reference**, not candidate
passes: frames 08–14 and manifest events named `candidate notification
introspection`, `candidate GetServerInformation`, `candidate notify-send`
are GNOME Shell 51.1. The first attempted `/proc//exe` probe failed because
there was no Tuna process. A failed initial GDM password submission is also
retained. The corrected actual Tuna selection and separate probes begin at
frame 16 and manifest events prefixed `Tuna candidate`.

On the same CI image, the GNOME Shell 51.1 reference returns
`('gnome-shell', 'GNOME', '51.1', '1.2')`; its real `notify-send` exits 0.
Its bus owner is the GNOME `gjs` notifications service. Real Super+L gives
LockedHint=yes / ScreenSaver=true; correct password unlock gives no / false
([13](13-gnome51-reference-lock.png), [14](14-gnome51-reference-unlocked.png)).
This runtime reference is **51.1**, not an assertion that GNOME 51.0 was run.

The requested GNOME **51.0 source** comparison uses the available
`/tmp/gnome-shell-51.0`; reference-file hashes and upstream links are retained
in `manifest.json`:

- [loginManager.js lines 141–163](https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/misc/loginManager.js#L141):
  resolve the current session from XDG_SESSION_ID, with logind fallback.
  Tuna's `crates/compositor/src/logind_session.rs::current_session_path`
  resolves XDG_SESSION_ID or GetSessionByPID; it does not address an arbitrary
  greeter or SSH session.
- [screenShield.js lines 151–180](https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/ui/screenShield.js#L151):
  connect current-session Lock/Unlock signals and call SetLockedHint when
  locked state changes. Tuna's DRM compositor owns the lock, so main's
  `Session::report_locked` sends its real transition through an off-loop
  logind worker. The installed Super+L/password run proves yes→no behavior;
  logind-triggered Lock/Unlock was not separately exercised here.
- [notificationDaemon.js lines 295–301](https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/ui/notificationDaemon.js#L295):
  GetServerInformation returns server/vendor/version/spec-version, spec 1.2.
  [The notifications D-Bus service forwards that tuple](https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/dbusServices/notifications/notificationDaemon.js#L140).
  Main's `crates/shell-host/src/intake.rs::get_server_information` implements
  the same required four-string contract; the installed shell owns and serves
  it and the real libnotify client now succeeds.

## Existing regression coverage; what was and was not run

No new causal source bug remained in either assigned gap, so no duplicate
implementation or replacement regression test was added. Existing named
coverage in the exact source revision:

- `logind_session::monitor::tests::logind_lock_and_unlock_reach_the_compositor_and_the_hint_follows_the_lock`
  (`crates/compositor/src/logind_session.rs`).
- `auth_owner::tests::logind_unlock_ends_the_lock_and_strands_the_attempt_in_flight`
  (`crates/compositor/src/auth_owner.rs`).
- `intake::tests::live_bus::stub_notify_shows_a_banner_and_close_notification_dismisses_it`
  (`crates/shell-host/src/intake.rs`, including the
  GetServerInformation/spec-1.2 wire assertion).
- Existing proof gates `V-LOCKED-HINT` (`scripts/tuna-vm-lane`) and
  `G-NOTIFY-SEND` (`scripts/tuna-gtk-shell-proof`).

**These test functions and proof scripts were not run by this qualification.**
No local build, unit suite, lint, formatter, nested-shell test, or full journey
was run or used to claim installed success. In particular the existing
published-mode `V-LOCKED-HINT` special case is not evidence that this published
payload has correct LockedHint. The real commands and real console frames
above supply the installed evidence independently. Image rebuild/start and
live installed checks were run only as required by this assignment.

## Ownership and cleanup

The initial transferred creation receipt was copied unchanged to
[published-build.log](published-build.log); its accidental root-worktree copy
was left to its owning parent to remove. Only this worktree and the assigned
`tuna-q-isg-mar1` resources were owned here.

[`corral delete --force --wait`](cleanup-delete.json) exited 0, reporting VM,
VMI and bootc disk PVC removed. A subsequent explicit
`kubectl --context=karnataka-k3s --request-timeout=30s -n corral-vms get vm,vmi,pvc,secret,pod,service -o json`
inventory filtered by the owned name/labels returned **[]**. The retained
[cleanup-inventory.json](cleanup-inventory.json) covers builder VM/pod,
Secrets, VMI, PVC and service leftovers as well as the final VM. No resource
on another node/cluster or owned by another task was deleted.
