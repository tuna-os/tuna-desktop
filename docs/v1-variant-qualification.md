# TunaOS variant qualification for Tuna Desktop v1

**No all-variant v1 qualification is claimed.** Package publication, an enabled
image build, and an image boot gate are not real-GDM/session/portal/lock proof.
The [v1 checklist](v1.md) and [ADR 0008](adr/0008-v1-beta-track-and-release-scope.md)
remain unchanged. Marlin remains the GNOME 51 comparison target; the other
variants still need their own immutable-image runtime evidence.

## Current source inventory

The primary inventory is [TunaOS `tunaos.toml` at
`124cc32623ce084580a2ef888350be07bd83508d`](https://github.com/tuna-os/tunaOS/blob/124cc32623ce084580a2ef888350be07bd83508d/tunaos.toml).
It declares **14 variants**, eleven admitted amd64 Tuna image cells and three
source-declared unsupported rows. No arm64 Tuna cell is enabled. All eleven
Tuna cells have `build_iso=false`, `build_qcow2=false`, and `luks_e2e=false`;
this matrix does not invent ISO/LUKS acceptance cells.

The native package inventory is [tunaos-packages
`c0998b94bd7b4fbc67360c239e439de6debf539b`](https://github.com/tuna-os/tunaos-packages/tree/c0998b94bd7b4fbc67360c239e439de6debf539b).
Its Tuna recipe/spec target **beta.2 release 2**, source desktop tag
`v1.0.0-beta.2`, commit `1e6ce28f4d5dfd7918ac7340bd93454a9fcc211c`.
That is not current desktop main. Recipe and vendor archive digests, actual
Gentoo payload evidence, and publication attempts are recorded in
[package-evidence.json](variants/2026-10-31/package-evidence.json).

| Variant | Base | Package target / format | Support and integration state |
|---|---|---|---|
| yellowfin | AlmaLinux Kitten 10 | `el10` / RPM | admitted; published image, not v1-qualified |
| albacore | AlmaLinux 10 | `el10` / RPM | admitted; published image, not v1-qualified |
| skipjack | CentOS Stream 10 | `el10` / RPM | admitted; published image, not v1-qualified |
| wahoo | Fedora ELN / EL11 preview | `eln` / RPM | admitted, experimental; missing Tuna tag repaired through existing signed workflow; runtime still required |
| bonito | Fedora 44 | `fedora` / RPM | admitted; published image, not v1-qualified |
| bonito-rawhide | Fedora Rawhide | `rawhide` / RPM | admitted; published as `bonito:tuna-rawhide`, not a separate registry repository |
| sailfin | openSUSE Tumbleweed | rollout label `opensuse`, recipe target `opensuse-tumbleweed` / RPM | admitted; published image, not v1-qualified |
| grouper | Ubuntu 26.04 Resolute | `ubuntu` / DEB | admitted; source retains a portal session-start blocker note; needs actual digest-specific verdict |
| marlin | Arch Linux | `arch` / pacman | admitted; real published-image journey has failures, below |
| flounder | Debian 13 Trixie | `trixie` / DEB | admitted; source retains a portal session-start blocker note; needs actual digest-specific verdict |
| flounder-sid | Debian Sid | `debian` / DEB | admitted; published as `flounder:tuna-sid`; same portal-note qualification limit |
| hummingbird | Fedora Hummingbird | native Hummingbird build chain / RPM | **not admitted on main; not an accepted package-unavailable waiver**; native package implementation exists; current served beta1 and power-provider closure block strict admission |
| guppy | Gentoo | `gentoo` / gpkg, amd64 | **not admitted on main; not an accepted package-unavailable waiver**; signed beta2-r1 publication and served-install verification now pass; image admission/runtime remain open |
| gurnard | Ubuntu 24.04 Noble | `noble` / no implemented Tuna target | source-backed unsupported: no native target, Rust >=1.92 required, current Noble-updates search exposes only rustc-1.91 in the 1.9 series; no vendored-toolchain substitution |

### Unsupported reasons are evidence, not a scope shortcut

TunaOS main still says Hummingbird/Guppy await Tuna package publication. Those
reasons are stale: both have native implementations, and Gentoo's new revision
was actually published during this qualification. They must not be counted as
satisfied unsupported exceptions merely because the old strings remain in TOML.

Existing admission branches were repaired rather than creating competing PRs:

- [TunaOS #3883](https://github.com/tuna-os/tunaOS/pull/3883): current-main
  integration, `brltty[python]` closure for Orca, and exact published
  `=gui-wm/tuna-desktop-1.0.0_beta2-r1::tunaos` pin. Signed package
  [run 41213265948](https://github.com/tuna-os/tunaos-packages/actions/runs/41213265948)
  passed build, publication and pristine served-binhost installation checks.
  Direct payload inspection finds the six current binaries without
  `tuna-greeter`. This is not a boot/login qualification.
- [TunaOS #3885](https://github.com/tuna-os/tunaOS/pull/3885): current-main
  integration and removal of unavailable redundant `liberation-fonts`, retaining
  Noto/DejaVu, strict GDM-only checks and the power-provider requirement.
  Observed native beta1 RPM includes the obsolete greeter and lacks
  `tuna-wallpaper-svg`. The native TuneD 2.28 RPM additionally requires
  `(rpm-ostree if bootc)` with no enabled `rpm-ostree` provider. Dropping the
  provider gate or inventing a `Provides` would hide the actual failure; neither
  was done. Targeted beta2 publication does not rebuild TuneD.

Both branches retain their manual workflow-path merge fence and promotion hold.
Neither was merged or bypassed. Noble's missing target is tracked in
[packages #834](https://github.com/tuna-os/tunaos-packages/issues/834).
The current public [Noble-updates package search](https://packages.ubuntu.com/search?keywords=rustc-1.9&searchon=names&suite=noble-updates&section=all)
returns `rustc-1.91` version `1.91.1+dfsg~24.04-0ubuntu0.24.04.3`;
[the Noble rustc-1.92 search](https://packages.ubuntu.com/search?keywords=rustc-1.92&searchon=names&suite=noble&section=all)
returns no results. These observations do not claim an exhaustive archive audit.

## Published immutable subjects

The complete [registry receipt](variants/2026-10-31/registry-matrix.json) records
source revision, exact resolver command/output, image creation time and labels.
An image's `org.opencontainers.image.version` is an image-source label, not the
installed desktop package version. Native guest package queries and binary
hashes belong in each runtime manifest.

All references below use `ghcr.io/tuna-os/`; digest prefixes are **not**
truncated. The registry receipt predates the successful Wahoo publication, which
has its own [publication receipt](variants/2026-10-31/wahoo-publication.json).

| Variant | Repository and immutable digest |
|---|---|
| yellowfin | `yellowfin@sha256:355f66da7929059fc046527da6177e4958d97dab9546d62ca3d5ac53bc4daa4f` |
| albacore | `albacore@sha256:b68a195dbc6e079bbab6b330086e6420d860f5364d1f2232e5f377bc6cdf2a13` |
| skipjack | `skipjack@sha256:30811ab4a8c8dbb4ee3cb1b77ef99fd312ec4835d05a7b40a3ed1aa410b7e5c3` |
| wahoo | `wahoo@sha256:865ec5320f25ae368316f07b4d3b49b5e0600308e5bc50b973ace90a5b53a46d` |
| bonito | `bonito@sha256:e9a58bb671a71b3eaad4efe7c80de35eb62847bf721bc08ef37168d028273c65` |
| bonito-rawhide | `bonito@sha256:dd2bc06ea05ea73fc06c7f208ac3ea2c7107cc4125024362a7d4a9ddfe18c2b3` |
| sailfin | `sailfin@sha256:cbfba99f07a74a099de7366d5de2a2552d768a677cba9000476dacf0d7486605` |
| grouper | `grouper@sha256:8cebaff1fbfe7d8f5076b0d09c7988cce804bbe193b12af733180f2eee3ef6fa` |
| marlin | `marlin@sha256:d0e65bce0ac55cb6c58bcf3739e30d6372fe4be978e6ea7c0c1310b3962bdfa5` |
| flounder | `flounder@sha256:03a4224a791370669f5bf96228097397298fa5e39b566cd3c3d1c1fd11481e2e` |
| flounder-sid | `flounder@sha256:81570c4609ad406ffa4ce5ac453c64f43708e74285ffc00aba7fc4006883cbc8` |
| hummingbird, guppy, gurnard | no admitted Tuna image subject in the source inventory; no image pass inferred from native packages |

### Existing Marlin runtime verdict is not a v1 pass

The retained [2026-10-31 real-GDM journey](marlin/2026-10-31-corral-journey/README.md)
for the Marlin digest above installed `tuna-desktop 1.0.0beta.2-2` and recorded
**24 pass, 3 fail**. Install, real-GDM login, session, refused wrong password,
correct unlock and logout were exercised. Raw failures remain unchanged:
absent VM backlight, `notify-send` missing `GetServerInformation`, and
`LockedHint=no` while locked. Visual findings include the incorrect Dark Style
tile, banner covering the date menu, and translucent power popover. These are
published beta2 observations, not claims about later current source.

## Actual Yellowfin runtime, 2026-10-31

The [complete immutable-image manifest](variants/2026-10-31/yellowfin/manifest.json)
ran 14:53:24–15:29:24 UTC through Corral `76b6dd52d35d`, with 4 vCPUs,
6 GiB RAM, a 40 GiB disk, bochs-drm 1280×800 and real GDM on `karnataka`.
The booted index matches `355f66…`; the booted amd64 manifest is
`sha256:de704153960289d73a58e509d2f00d3713c8d5ee6f7ac23278b21e53cb85f120`.
**The actual installed desktop is `1.0.0~beta.1-1.el10`, not the beta2
release2 recipe now on main.** GDM is 47.0, GNOME Shell 49.5 and Settings
47.7. No current-source qualification is inferred from a recent image date.

Raw result: **20 pass, 7 fail, 1 not exercised**; all 28 noVNC canvas frames,
hashes and guest checks are retained. The deleted VM/VMI/PVC and empty
post-delete inventory are recorded in the manifest.

| Required gate | Actual Yellowfin result |
|---|---|
| Install and booted-image identity | pass for the exact published digest, bootc disk install |
| Real GDM login | pass; GDM greeter on seat0, password login through `gdm-password` |
| Tuna session | pass; compositor and GTK shell run, no user GNOME Shell |
| Portals | frontend Settings.Read, Screenshot version and ScreenCast source types respond; **functional consent/capture/revocation not qualified** |
| Lock/unlock | lock gate fails: ScreenSaver active true but `LockedHint=no`; wrong password refusal and correct unlock pass |
| Visual review | reviewed GDM, session, failed Settings-launch and lock frames; not a complete visual pass |

Direct frame review:
[GDM](variants/2026-10-31/yellowfin/01-greeter-user-list.png),
[Tuna session](variants/2026-10-31/yellowfin/03-session-is-tuna.png),
[Settings attempt](variants/2026-10-31/yellowfin/10-launch-settings.png),
[lock curtain](variants/2026-10-31/yellowfin/23-lock.png).
The Settings attempt visibly contains repeated trailing `s` characters in the
search field, not a Settings window. Consequently its timeout and downstream
style/wallpaper checks are retained as failed journey observations, **not
independent proof that Settings itself cannot start**. Other raw failures are
Night Light false/no Color owner, `notify-send` missing `GetServerInformation`,
and the lock hint. Sliders are not exercised because this VM has no relevant
hardware device. An automated “settings-appearance” pass only records a key/
pointer action; the reviewed frame does not prove that UI appeared.

The subsequent Albacore attempt was interrupted by the outer 3600-second
command deadline while Corral API reads were timing out. The
[interrupted matrix](variants/2026-10-31/interrupted-runtime-matrix.json)
is intentionally not a completed matrix or a passing Albacore run. Host
Tailscale/SSH and authenticated API `/livez` responded; Corral's default
5-second read timeout tripped its 15-second circuit breaker. This is access/
orchestration evidence, not a guest crash or an unsupported-variant reason.
The [access and cleanup receipt](variants/2026-10-31/interruption-cleanup.json)
records successful deletion of only the interrupted Albacore VM/VMI/PVC and an
empty post-delete inventory after using the documented
`CORRAL_KUBECTL_TIMEOUT=30s` management budget. No cluster or desktop acceptance
threshold was changed. Remaining journeys resumed sequentially with that
transport setting; the original failure receipt is unchanged.

## Final observed disposition

The [resumed discovery/interruption receipt](variants/2026-10-31/resumed-interrupted-matrix.json)
resolved all admitted publications, including a later Skipjack digest
`sha256:36889e650e35893e4d3779deba2ddce7ce375f65a36271f86b77ca90d0c8e924`.
The resumed Albacore attempt did not produce a runtime/readiness manifest
before interruption. No later variant VM was created by that sequence.
The [final disposition](variants/2026-10-31/final-disposition.json) records
deletion of that owned Albacore VM/PVC and an empty `tuna-q-variant-` inventory.
Neither `punjab` nor the independent soak VM was touched.

| Variant(s) | Install / GDM / session | Portals | Lock/unlock | Qualification |
|---|---|---|---|---|
| yellowfin old `355f66…` (beta1) | pass in both retained runs | availability only; expanded run Screenshot Response 2 and ScreenCast Start timeout because generic Enter did not activate the visible Allow/Share controls | `LockedHint=no` fails; refusal/unlock pass | fail |
| yellowfin refreshed `64e7a29…` | trusted refresh run [41221855489](https://github.com/tuna-os/tunaOS/actions/runs/41221855489) succeeded; guest reports beta2-2.el10; retained core run 10 pass/3 fail | retained capture failures were **locked-session denials**, not product capture defects; later untouched-image unlocked Screenshot and ScreenCast both deliver real frames | `LockedHint=no`; refusal/unlock/logout pass | not qualified; timed unlocked flow and lock-triggered withdrawal still require corrected matrix receipt |
| marlin | existing exact beta2-2 journey passes these gates | not qualified by that daily-driver receipt | lock hint fails; refused password/unlock pass | fail, not a v1 pass |
| albacore `b68a195…` | pass; actual beta1; subsequent retained core run 12 pass/1 fail | real Screenshot PNG and ScreenCast frame/caller isolation pass | `LockedHint=no`; withdrawal unproven because requester expired before host armed the unchanged 30-second handshake | not qualified; capture failures from the earlier Enter run are superseded, not product defects |
| skipjack, wahoo, bonito, sailfin | expanded run: install/GDM/session pass | earlier receipts lack unlocked/timely-consent preconditions; not functional qualification | `LockedHint=no` | fail on retained receipts; corrected preconditions pending |
| bonito-rawhide | builder Secret label exceeded Kubernetes' 63-byte limit before the VM existed | unproven | unproven | harness failure fixed by VM-name bound; rerun pending |
| grouper | expanded run interrupted during build; orphan builder VM/Secret/PVC deleted, inventory empty | unproven | unproven | not qualified |
| flounder, flounder-sid | publication resolved; not yet run | unproven | unproven | not qualified |
| hummingbird, guppy | admission branches remain open; no admitted immutable-image runtime proof | unproven | unproven | not qualified; stale package-unavailable reasons rejected |
| gurnard | no native Tuna target | unproven | unproven | source-backed unsupported prerequisite, not a runtime pass |

The expanded-run receipts are not retroactively converted into failures of
the published desktop. The visible GNOME dialogs prove the harness pressed
Enter while Allow/Share remained visible. They do prove portal qualification
remained incomplete. The consent path now uses the existing AT-SPI helper with
Corral's external capture; it does not spoof app IDs or replace backends.

The refreshed Yellowfin run at 19:19–19:50 UTC retained a black Screenshot
prompt frame and a ScreenCast prompt showing the lock password screen. It did
not assert `ScreenSaver.GetActive=false` at request/consent time. Its Screenshot
Response 0 with empty URI and ScreenCast Response 2 must **not** be attributed
to broken capture. On the same untouched `64e7a29…` image, the independent
InstalledSessionGaps receipt at `/tmp/isg/proof` records actual unlocked
`GetActive=false` before/after timely Allow/Share: a real 1280×800 Screenshot
file and a decoded ScreenCast frame from node 58. Explicit Session.Close
removed that node in 0.013 seconds; that is not lock-triggered withdrawal.
The deliberate locked negatives reproduce the earlier responses, with no new
Screenshot image and no ScreenCast stream. Raw failed receipts remain unchanged.

The [subsequent interrupted sequence](variants/2026-10-31/corrected-interrupted/matrix.json)
completed Albacore and Skipjack before its coordinator stopped during Wahoo's
builder phase. Albacore actually captured both portal images. Its lock observer
expired with `runner did not revoke the active grant`: remote evidence collection
used the client's unchanged 30-second `.active`→`.revoke` handshake before the
host armed the lock. That receipt does not establish a product stream-withdrawal
failure. Skipjack's capture attempts lacked the unlocked preconditions and remain
unqualified. [Recovery cleanup](variants/2026-10-31/recovery-interruption.json)
records deletion of only the interrupted Wahoo builder VM/Secret/disk and empty
owned-prefix inventory. Coordinator loss is not an unsupported-variant reason.

Exact admission/publication [gate snapshot](variants/2026-10-31/admission-gates.json):

- Guppy PR #3883 head `b1150929473ead1316427b8eae48f76832cf65d1`.
  Earlier run `41215956243` selected no Guppy job because the migrated selector
  omitted it; **that run is not Guppy evidence**. In
  [run 41219478905](https://github.com/tuna-os/tunaOS/actions/runs/41219478905),
  Guppy job `124590901178` succeeded, including image build and PR boot verify;
  boot artifact `13054522656` records digest
  `sha256:49b93f9e32579f03cb1f61978b080277ad8f2dd79d1dc463b59b0d96d8a1f662`.
  That PR image is not a published immutable subject.
- **Exact Guppy publication blocker:** the maintainer's
  [tunaOS #3202 comment](https://github.com/tuna-os/tunaOS/issues/3202#issuecomment-6547926694)
  says: “Merge stays manual, because the change touches workflow paths and has
  the sentinel flag. The issue body's release gates still apply: no publication
  before the tuna-desktop v1 epic (tuna-desktop#577) approves it.” No candidate
  or signed registry image was published. Without that approval, Guppy GDM/session/
  portal/lock qualification against a published digest is blocked.
- Hummingbird PR #3885 head `0de95641bfdd350e1f148b7f0b1eb600acf58913`.
  [Exact-head run 41213419115](https://github.com/tuna-os/tunaOS/actions/runs/41213419115)
  failed the native TuneD transaction before desktop/boot gates.
- Native Hummingbird publisher [run 41213268502](https://github.com/tuna-os/tunaos-packages/actions/runs/41213268502)
  built and signed beta2 in staging, then failed the pre-sync installability gate
  on a new `libcanberra` blocker. The served index remains beta1. Maintainer-owned
  [packages #959](https://github.com/tuna-os/tunaos-packages/pull/959) adds
  native `rpm-ostree` and `libcanberra` roots. Its first native build was cancelled
  by a later external commit, so no native closure result is claimed.

Focused harness regressions: 8 tests pass, including prefix-collision cleanup.
They are not runtime qualification.

## Reusable actual-image path

`python3 scripts/tuna-variant-qualification --out NEW_DIRECTORY` snapshots
current primary configuration and resolves every admitted Tuna publication.
`--run` additionally installs and exercises each reachable image **sequentially**
through `tuna-corral-journey`; `--variant NAME` limits runtime work, never hides
other inventory rows. The only node accepted by this lane is `karnataka`.
Only newly owned `tuna-q-variant-*` VMs/disks are created/deleted. Existing
`punjab` and the independent soak VM are never named.

The default runtime scope is the required real-GDM/session/portal/
lock/unlock/logout core, using the journey's explicit `--qualification-only`
mode. `--full-journey` also runs its app, Settings and notification tour.
Neither mode upgrades portal availability into functional portal qualification.
The matrix stops rather than creating another VM if owned cleanup is unproven.

```sh
~/.cache/tuna-journey-venv/bin/python scripts/tuna-variant-qualification \
  --out /tmp/tuna-variants-NEW --run --variant grouper --variant flounder
```

Requirements and console behavior follow [the Corral journey runbook](corral-journey.md):
Corral bootc plugin/KubeVirt access, `gh`, `skopeo`, Python 3.11+, Pillow and
Playwright Chromium. The script uses native pacman/RPM/dpkg package queries,
retains booted digest/package/binary provenance, drives the VM's own noVNC
canvas, retains frame hashes, and deletes its VM/disks. Failure manifests also
attempt a Corral framebuffer capture before deletion. No candidate compositor,
shell or session payload is copied into the published-image test.

The portal availability step checks real frontend Settings, Screenshot and
ScreenCast interface replies in the logged-in published session. Those probes
alone remain **availability-only**. The functional Screenshot step reuses
`scripts/lib/tuna-portal-screenshot-client.py`: a persistent native Gio
requester that rejects a screenshot returned before a picker decision,
awaits the actual Response, and validates real varied 1280×800 image pixels.
The host retains that PNG and verifies its guest/host hashes. No app ID is
spoofed and no backend or candidate desktop code is installed.

The ScreenCast step reuses `tuna-portal-capture-client.py` and
`tuna-pipewire-dump.py`. It creates/selects/starts the real frontend session,
opens its actual PipeWire FD, consumes a frame with native GStreamer, checks
caller isolation, and holds its grant until the actual Super+L lock step.
The unchanged two-second withdrawal observer is armed **before** the lock key,
so remote transport latency cannot create a false pass. Actual response,
requester logs, varied frame, hashes and the withdrawal snapshot are retained.
Missing native Python/GObject/GStreamer/`pw-dump` tools remain explicit failures/prerequisites,
not reasons to call the image unsupported or to install a replacement backend.

At a pending decision, the existing AT-SPI helper activates the actual visible
GNOME Allow/Share control; Corral retains the console frame externally instead
of using X11-only `scrot`. A grant test requires an unlocked session at request,
consent and response; locked-session denial is a separate negative result, never
a capture-product failure. Console frames still require visual review.

New matrix runs freeze the host journey, consent helper and native requester/parser
sources into the output's `harness/` directory before starting any VM, recording
their hashes. Subsequent worktree edits cannot mutate those long-running tests.
Only test requester code is placed in the fixture user's runtime directory;
the installed desktop/provider payload stays untouched.

Earlier receipts identify their actual runner hash and do not gain new proof
retroactively: Yellowfin's retained run predates the functional requesters.
Full visual consent decisions remain required, never inferred from a successful
frame. Lock-triggered withdrawal must be observed, not substituted with explicit
Session.Close. Unreviewed frames, interface availability or incomplete portal
flows do not become a v1 pass. Registry errors remain errors, not waivers.
