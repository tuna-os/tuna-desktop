# Marlin VM lifecycle proof

The `marlin-vm` and `marlin-vm-orca51` CI jobs install the ordinary Marlin
preview image built from source, then a CI-only fixture layer
(`packaging/marlin/vm-lane/Containerfile`). The layer gives the image the
published `marlin:tuna` login path: stock GDM with tunaOS's tuna-flavor GDM
steps (`tunaos.toml` `[desktops.tuna]` `after_install`: greeter theme
overlay, `FallbackSession=tuna` with version-appropriate global account
templates, every other session hidden), vendored byte-for-byte in
`packaging/marlin/vm-lane/tunaos/` from tunaOS d98ed1e09b61635a2a9b115ce559944e42f97d74, on the package's
own `Tuna Desktop` session entry. The fixture account has no saved session.
No autologin or initial session exists: the lane drives GDM's user list and
types the fixture password exactly as the published-flavor nightly does
(`V-GREETER-LIST`, `V-GDM-FALLBACK`, `V-GDM-LOGIN`;
`docs/marlin-tuna-nightly.md`), from the same `scripts/tuna-vm-lane` code.
Logout must return to that GDM greeter. This changes neither the shipped
preview image nor the number of CI VM profiles. GDM's session worker opens a
journal stream for `gdm-wayland-session`, which the fixture forwards to the
serial port. The fixture's session environment comes from `environment.d`,
which gdm-wayland-session imports from the user manager; the per-login Orca
test secret comes from a user environment generator.

## Record-free fresh accounts (tunaOS#4092)

`tuna-test` above has a session-less AccountsService record, so it proves
GDM's `FallbackSession` path only. `scripts/tuna-vm-lane --fresh-accounts`
is a separate boot of the same disk (QEMU `-snapshot`) that passes the
`tuna.vm.fresh-accounts` systemd credential (SMBIOS type 11). Only then does
`tuna-vm-fresh-accounts.service` run `useradd -m` before GDM starts, as an
installer would: `tunastd` (standard), `tunaadmin` (`wheel`), and the
control `tunaseeded`, whose record is pre-seeded. `tunastd` and `tunaadmin`
get no per-user AccountsService record, no per-user Session preselection
and no autologin. This is distinct from the global AccountsService
templates: on an installed GDM 51+ with `FallbackSession` support, their
`[User]` group explicitly sets `Session=`. GDM can launch the configured
`tuna` fallback without propagating that name to the worker's persistent
settings; empty templates alone have not qualified record creation.
Older GDM retains global `Session=tuna`: it can prefer launchable GNOME
even when that session has `NoDisplay=true`. An absent or unparseable GDM
version must not opt into the empty-template policy. Neither policy edits
existing per-user records or replaces a valid saved session choice.

| Gate | Actual operation and evidence |
| --- | --- |
| V-FRESH-RECORD-FREE-<user> | Before any login: account exists with a home, no `/var/lib/AccountsService/users/<user>`, no GDM automatic/timed login, `wheel` only for the admin |
| V-FRESH-CONTROL | The same probe classifies the pre-seeded `tunaseeded` as NOT record-free |
| V-FRESH-LOGIN-<user>-{first,second,reboot} | GDM "Not listed?" name and password typed through QMP; exactly one `gdm-password` VT session runs `tuna-compositor` and no `gnome-shell`/`gnome-session` |
| V-FRESH-RECORD-CREATED-<user> | After the first login AccountsService wrote the record; any saved `Session` is `tuna` |
| V-FRESH-SUDO-<user> | `sudo -S -k true` as the account with its password: admin succeeds, standard is refused by sudo policy |
| V-FRESH-LOGOUT-<user>-<round> | logind ends the session and the greeter is ready again |
| V-FRESH-REBOOT / V-FRESH-RECORD-KEPT-<user> | Observe a different `boot_id` and ready greeter, then physical records for both accounts; both must log in and log out again |

The controller reads a nonempty old boot ID through the read-only `greeter`
action, then calls `GuestAgent.request_reboot()` once.
It sends QGA [`guest-shutdown` with `mode=reboot`](https://www.qemu.org/docs/master/interop/qemu-ga-ref.html#command-QGA-qapi-schema.guest-shutdown).
QGA returns no response on success and does not guarantee shutdown.
The method retires its transport after it tries to send; send errors propagate.
A return is not an acknowledgement or proof of reboot.

The controller reconnects at once, with no extra sleep.
It retains the 180-second limit for a new boot and ready greeter.
A boot ID that did not change cannot pass, nor can a greeter that is not ready.
Physical record retention and both post-reboot logins and logouts remain mandatory.
The 60-second login limit, two authentication tries and 90-second trace stay unchanged.

Run 41215443626 failed both native VM jobs with the AccountsService record
still missing after a real record-free `tunastd` manual GDM login on
GDM 51.1.1-1 / AccountsService 26.27.3-1. First login, standard-user sudo
denial and logout passed; admin, second-login and reboot coverage aborted.
The inherited global `Session=tuna` suggested a same-value setter might
explain the missing write. Run 41237623922 disproved that as the remaining
cause: even empty effective settings left the record absent. An empty
global template is not itself proof that persistence is fixed: only the
unchanged native gates above can qualify the fix. No setup script may
create the fresh accounts' records or invoke a cache operation to do so.

Receipts are in `fresh-accounts-receipts.json`; frames per attempt and
session. Both native VM jobs must complete the standard and administrator
first login, real record creation, sudo policy, logout, manual second
login, reboot, record retention and post-reboot login gates. Only Arch/GDM
51 is covered by this native lane; helper-execution tests also cover
legacy GDM template policy, but do not constitute a pre-51 native proof.

Run 41226473836 (desktop c3713c34) left the record absent after a real
orca51 `tunastd` first login, and the helper's chosen policy unobserved.
Two read-only diagnostics now record what happened; neither is a gate.
The image build prints `tuna-vm-lifecycle gdm-policy`: the `gdm` the helper
resolves and its realpath, bounded `--version` output/status and parse, the
`daemon/FallbackSession` literal-probe status, the derived `empty`/`legacy`
policy, `pacman -Q gdm libgdm accountsservice` and the binary's owner.
Each `fresh-state` receipt adds an `accounts` block, collected in order:
physical record presence, the global `standard`/`administrator` templates
(candidate presence in `/run`, `/etc`, `/usr/local/share`, `/usr/share`
precedence, the winner, and every `[User]` `Session`, `SessionType`,
`XSession` and `SystemAccount` occurrence: `[]` is missing, `[""]` empty),
`Properties.Get` without D-Bus activation of exactly `Session`,
`SessionType`, `Saved` and `LocalAccount` on
`/org/freedesktop/Accounts/User<uid>`, then record presence again.
An unexported object, absent bus or wrong type is an explicit error
(remote D-Bus error name or exception type only), never an empty Session or
false `Saved`. Collection points: preflight, first session, after each
first-round logout (before a missing-record abort), after reboot, and once
when a login fails. Nothing calls `FindUserByName`, `CacheUser` or a
setter, retries, or dumps other template keys, error messages or stderr.
A record seen only after logout, `Saved=true` or an effective
`Session=tuna` never substitutes for `V-FRESH-RECORD-CREATED-<user>` at
the first session.

### Observed GDM 51 persistence failure and bounded trace

Run [41237623922](https://github.com/tuna-os/tuna-desktop/actions/runs/41237623922),
head `f7208bba`, failed `V-FRESH-RECORD-CREATED-tunastd` in **both**
native profiles. The receipts show UID 1001, a real `gdm-password` VT 2
Wayland login running `tuna-compositor` and `tuna-shell-gtk`, successful
standard-user sudo denial and logout, but no physical record during the
session or after logout. The winning `/etc/accountsservice/user-templates/`
standard and administrator templates each contain `Session=`; effective
`Session` and `SessionType` are empty, `Saved=false`, `LocalAccount=true`.
Packages are `gdm 51.1.1-1` and `accountsservice 26.27.3-1`. Administrator,
second-login and reboot acceptance remains unexercised, not passed.
The retained logs of jobs 124646121324 and 124646121372 contain none of the
four allowlisted worker markers below and no session-setter call headers.
This is an evidence gap, **not proof that saving was skipped**.

For review before the next native campaign, add just `--fresh-gdm-trace`
to its existing `scripts/tuna-vm-lane --fresh-accounts ...` command.
The opt-in helper `tuna-vm-gdm-trace` runs only around the first ordinary
`tunastd` login attempt, stops before any existing selection retry, and is
stopped in `finally` on login/input/guest-agent failure. The observer has
an independent 90-second lifetime, a 1 MiB header-input bound and 128-event
bound; these add no login timeout, wait, retry, session or acceptance gate.
Transient-service startup allows at most two seconds for observer readiness.
Existing-marker queries additionally stop at 64 KiB or three seconds.
The trace receipts are `tunastd-first-trace-{start,stop}` in the existing
`fresh-accounts-receipts.json`; collection failure is explicit and non-gating.

Privacy contract:

- `dbus-monitor --system --profile`, **not** normal monitor/binary mode,
  prints tab-separated message headers and no argument or error bodies.
  Match rules select only `SetSession`, `SetSessionType`, `SetXSession`
  calls on `/org/freedesktop/Accounts/User1001`. Return/error headers have
  no path, so the observer consumes their metadata and retains only replies
  correlated by call sender/serial and reply destination/reply-serial.
  Unrelated headers are discarded in memory; no raw bus log is written.
- Existing `gdm-password` worker journal entries are reduced to four fixed
  labels: saving account details for `tunastd`, session name set to `tuna`,
  save failed, and `SESSION_OPENED`. PID, time, method and reply metadata
  are sufficient; usernames other than the fixture, journal text, setter
  argument values, error names/bodies, PAM conversations and environment
  values are not exported by this observer.
- **Never enable GDM debug or send SIGUSR1.** The audited upstream
  [worker source](https://github.com/GNOME/gdm/blob/51.1.1/daemon/gdm-session-worker.c#L757)
  logs complete PAM query payloads at line 757, selected choice responses
  at line 576 and PAM environment entries at line 1537. Filtering this
  observer's output cannot make a broader journal/serial export safe.
  Missing pre-existing markers are reported as
  `unavailable-or-debug-disabled`; no conclusion about the save stage
  follows from their absence.
- No probe invokes `FindUserByName`, `CacheUser` or an AccountsService
  setter, writes a per-user cache record, changes GDM configuration, or
  offers a fake second desktop session.

A matching setter error header proves an error reply, not its cause.
A successful reply without a record points at persistence, not at missing
session propagation. No setter observed with unavailable worker markers
remains incomplete evidence. The physical-record gate is unchanged.

### Root-fix candidate and actual Arch package boundary

The upstream [GDM 51.1.1 normal-session open path](https://github.com/GNOME/gdm/blob/51.1.1/daemon/gdm-session.c#L2982)
sends display mode and `XDG_SESSION_TYPE`, then `Open`; it does not send
`SetSessionName`. Explicit selection does send
`SetSessionName(get_session_name(self))`. With one offered Tuna session,
[GNOME Shell 51.0 hides the session menu](https://github.com/GNOME/gnome-shell/blob/51.0/js/gdm/loginDialog.js#L543).
The [settings save routine](https://github.com/GNOME/gdm/blob/51.1.1/daemon/gdm-session-settings.c#L369)
can succeed with null session/type/language and, for a local account,
without a setter or `CacheUser`. Registration is not this save trigger.
**Inference:** fallback selection may never reach the worker's settings.

Run [41245509890](https://github.com/tuna-os/tuna-desktop/actions/runs/41245509890)
had healthy observers in both native profiles.
Neither saw a call to set the User1001 session.
Worker markers: unavailable; debug off.
Their absence proves nothing about the save stage.
Neither run proves the cause.

The native image gets GDM from Arch through `pacman`, not this repository's
`packaging/arch/PKGBUILD` (which builds only Tuna Desktop).
The canonical tunaOS `tunaos.toml` desktop package list requests plain
`gdm`, without a Tuna GDM version/patch override. This lane starts from
`ghcr.io/tuna-os/marlin:gnome`; both the preview Containerfile and VM
fixture layer run `pacman -Syu`, so a future replacement must survive the
final package transaction rather than being silently overwritten.

The [Arch recipe at revision 36e76b2b](https://gitlab.archlinux.org/archlinux/packaging/packages/gdm/-/blob/36e76b2b4674dc196e611782c4f8f6d5e476e231/PKGBUILD)
splits `gdm` and `libgdm` from one build.
It uses the upstream tag and an Xsession patch.
Its `prepare()` uses `git apply -3`.
The build uses `arch-meson`, `meson compile` and `meson test`.

[Package source PR963](https://github.com/tuna-os/tunaos-packages/pull/963)
adds a private candidate at
`7b52bdf34be22bc60fef62b157be64b589b13f6a`.
It builds GDM and libgdm **51.1.1-1.1** together.
It retains the PAM configuration, conffile declarations and split ownership.
Both sources and patches have fixed hashes.
`prepare()` applies the patches to the release archive.

The candidate queues `gdm_dbus_worker_call_set_session_name()` before `Open`.
It uses `get_session_name(self)` on the same worker connection.
It does not change the resolver or program sessions.
Explicit, saved and fallback choices keep their precedence.
It does not force `tuna` or change registration.
No root probe writes AccountsService state.

The proposed insertion, inside the existing non-null-conversation block
immediately before `gdm_dbus_worker_call_open()`, is:

```c
if (!self->is_program_session)
        gdm_dbus_worker_call_set_session_name (conversation->worker_proxy,
                                               get_session_name (self),
                                               conversation->worker_cancellable,
                                               NULL, NULL);
```

The approved local factory build produced both unsigned packages.
Meson: one pass, zero failures.
The staged verifier installed both packages.
It checked exact versions, owners and ELF closure.
Both containers enforced 8 GiB memory, zero swap and CPU quota 200%.
We removed both containers.

The local archive hashes are:

| Package | SHA256 |
| --- | --- |
| gdm 51.1.1-1.1 x86_64 | `5fa15579210678f6168ea5102b79050503b768fd93476910855281f4b3d60d92` |
| libgdm 51.1.1-1.1 x86_64 | `ca4587fe886490c1e1925c9419bf5714fe9327cb9367685fb8d306b9a2243797` |

These checks do not prove login or physical record creation.
The C smoke used actual functions with stubs.
It checked explicit, saved, fallback and program choices.
It does not prove GDM, PAM, D-Bus or AccountsService runtime behavior.
We deleted its temporary source and binary.

### Private native GDM fixture

The CI job builds the pinned package source once.
It uses the existing factory build and staged verifier.
Both native profiles use the same run's artifact.
The artifact has package hashes and the source identity.
Those hashes need not equal the local hashes above.
The manifest also binds the package members to their hashes.

The final `gdm51` overlay runs after every `pacman -Syu` and Orca step.
It checks both archive hashes and package metadata.
It installs both packages in one transaction.
It checks their versions, owners and installed bytes.
It checks the existing shipped-payload manifest without changes.
No package upgrade follows.

CI retains this evidence in `vm-lane-artifacts/gdm51/`.
The final image ID and kernel provenance describe the image after the overlay.
The fresh journey receives `TUNA_VM_GDM_NATIVE_CANDIDATE` and
`TUNA_VM_GDM_NATIVE_SOURCE_SHA`.
The first names the bounded manifest; the second pins the expected source.
The journey checks the manifest and guest package identity before input.
The existing read-only `gdm-policy` action runs once for this check.

Neither the manifest nor the package versions can satisfy the physical-record guard.
All standard/admin, first/second login, sudo, logout and reboot gates stay intact.
Candidate admission alone does not exercise either native journey.
No source, function or package check counts as runtime acceptance.
The source PR stays unmerged.
This fixture does not sign packages, publish a repository or change production pins.

### Actual candidate campaign 41255271531

[Run 41255271531](https://github.com/tuna-os/tuna-desktop/actions/runs/41255271531)
ran for desktop PR head `31bf6bb30ee9949918b161a3a6035e906c2efb79`.
CI uses a merge checkout, not the PR head directly.
It built package source `7b52bdf34be22bc60fef62b157be64b589b13f6a` once.
The factory build and staged verifier passed.
Both containers reported 8 GiB memory, zero swap and CPU quota 200%.
Cleanup completed.

These are the CI hashes of the package archives, not the local hashes above:

| Package | SHA256 |
| --- | --- |
| gdm 51.1.1-1.1 x86_64 | `8c67e6be0e707f157cd901d319dd2af522f9666c352af42958d4a8bbaca62f66` |
| libgdm 51.1.1-1.1 x86_64 | `a559fae8e6d3166294c6d11ff76b39258ed0ca2c5e62f4e9ccdd5bc5d24f9af4` |

Both profiles installed that exact pair after all upgrades.
Their metadata, owners and installed bytes matched its manifest.
Both checks left the shipped-payload manifest unchanged.
Both profiles passed the ordinary hardware-session lane.

| Profile | Final immutable image |
| --- | --- |
| distribution | `sha256:92de3d18015d68e81e4fc769154ac5f130bfa719f3ed70043a0fb8ca6cd90491` |
| upstream Orca 51 | `sha256:6a00f62d298ad8e3f79f5b9bfde0247f65976cc474a8d0c931f60c8f2a6ec3f4` |

Both profiles began with no physical record for UID1001.
Both passed its first real `gdm-password` login.
Both saw a physical record with `Session=tuna` during that session.
That record also survived logout.
The Orca profile passed the same first-record guard for UID1002.
These results cover those logins, not the complete journey.

The Orca observer saw a `SetSession` call and its successful reply.
It retained only headers.
The distribution trace covered its first try.
Its standard login succeeded on the second try, after the trace stopped.
Missing headers do not prove a missing setter.
Worker markers: unavailable; debug off.

Sudo refused the standard user in both profiles.
The admin did not log in on the distribution profile after two tries.
The admin login and record checks passed on the Orca profile.
Sudo refused that wheel administrator by policy, with exit status 1.
The evidence does not establish the inherited contents of sudoers.
Second-login and reboot acceptance did not run.

Both fresh journeys remain **failed**, not qualified.
Neither the persistence evidence nor the package provenance satisfies the missing gates.
No production publication, image pin or merge follows from this run.

### Actual candidate campaign 41262331704

[Run 41262331704](https://github.com/tuna-os/tuna-desktop/actions/runs/41262331704)
ran for PR #985 head `598ca553abc24f4b10bb335d22f99a5ff4e17a81`.
CI used merge commit `a3d6334efd4176ff7082a6b4d1ddc5d9ad1fe1d3`,
with parents `eed9aa95658f5d75496954b10142d50c807ee88f` and
`598ca553abc24f4b10bb335d22f99a5ff4e17a81`.
Package source stayed at `7b52bdf34be22bc60fef62b157be64b589b13f6a`.
The archives in CI had the same hashes as those in the previous run:

| Package | SHA256 |
| --- | --- |
| gdm 51.1.1-1.1 x86_64 | `8c67e6be0e707f157cd901d319dd2af522f9666c352af42958d4a8bbaca62f66` |
| libgdm 51.1.1-1.1 x86_64 | `a559fae8e6d3166294c6d11ff76b39258ed0ca2c5e62f4e9ccdd5bc5d24f9af4` |

For both profiles, real first logins passed for the standard user and administrator.
Physical records held `Session=tuna`; read-only observations reported `Saved=true`.
Both records survived the first logout.
Standard sudo denial, administrator authorization, both manual second logins
and all pre-reboot logouts passed.

Then the host reported literal `timed out` before `V-FRESH-REBOOT`.
No actual new boot ID, post-boot retention or post-reboot login/logout received credit.
Both fresh journeys failed.

The old CLI used `systemctl reboot --no-block` before it wrote JSON.
Its host still awaited the child exit and output through the agent being torn down.
That sequence can cause a teardown/reply race; the run does not identify the
exact timed-out I/O or prove that the guest changed boots.
The QGA request removes only that dependency; all gates stay intact.

The receipt for each profile contains nine cases for the strip.
Both reported `TimeoutError` and `cleanup_confirmed=true`.
This failure of a strip deadline is independent of policy binding.
This run is not a PASS for the full suite.
This run does not qualify reboot.
No production publication, image pin or merge follows from it.

### Bounded strip capture failure metadata

The retained metadata places the strip failure inside the capture for case 10.
It does not establish whether the local or total deadline caused the failure.
The new diagnostic does not change either deadline or qualify a failed capture.

A failed capture adds one `capture_failure` object to the existing receipt.
It contains the source-owned case `label` and `role`, plus the `attempts` count.
Its fixed fields are:

- `capture_begin_monotonic_ns` and `failure_monotonic_ns`;
- `local_deadline_monotonic_ns`, `capture_deadline_monotonic_ns`
  and `total_deadline_monotonic_ns`;
- `stage`: `png-acquire`, `ppm-acquire`, `ppm-pixels`, `png-pixels`,
  `retry-refresh` or `post-frame-read`;
- `last_not_ready_reason`: `fixture-body-not-visible`,
  `fixture-color-mismatch`, `other-not-ready` or null;
- `ppm_rgb_verified` and `png_rgb_verified`: separate booleans for the latest try.
  Null means that the capture did not reach the color check.

The object contains no frames, inventory, exception text or bus payload.
The capture re-raises its original exception; cleanup stays unchanged.
The local 10-second and total 120-second limits stay unchanged.
Both pixel checks and the identity check after the frame remain mandatory.

The existing code does not guard the final write of `receipt.json`.
An `OSError` from that write can mask an earlier failure.
This diagnostic does not change that existing limit.

### Private image and input corrections

The old controller used one or two forward Tabs.
[Each user has a focusable button](https://github.com/GNOME/gnome-shell/blob/51.0/js/gdm/loginDialog.js#L67-L137).
Hover can also move focus.
The number of Tabs depends on the list and current focus.
The retained run does not reveal where the UI had focus.

The corrected input parks the pointer outside the list.
Both Escapes share the original two-second budget.
Five Up presses reach the first item without vertical wrap.
Two backward Tabs then reach `Not listed?` through the bottom accessibility button.
The fixture has at most four human accounts and those two buttons.
The old per-Tab sleeps are gone.

[GNOME's focus handler](https://github.com/GNOME/gnome-shell/blob/51.0/src/st/st-focus-manager.c#L102-L140)
lets backward Tabs wrap, but not Up by default.
The controller still allows at most two login tries.
Its 60-second login deadline and 90-second trace bound stay unchanged.
Real UI qualification still needs the complete native journeys.

The [pinned installer](https://github.com/tuna-os/bootc-installer/blob/9f582fe0ecc19561be1115dfef64a59e91ea5072/bootc_installer/defaults/user.py#L16-L17)
gives administrators the wheel group.
Its [user setup](https://github.com/tuna-os/fisherman/blob/d349c31e47b1c05ef82d67811d5f4f9e77a62e2a/internal/post/user.go)
does not grant sudo authorization.
The result shows a gap in authorization for the administrator.
It does not establish the inherited sudoers bytes.

The shared image uses `tuna-vm-admin-policy` for its global wheel role.
Its rule is `%wheel ALL=(ALL:ALL) PASSWD: ALL`.
It names no test account or probe command.
The installed file has owner `0:0` and mode `0440`.
The account helper and sudo probe remain unchanged.
Standard accounts outside wheel must still fail by policy.

An explicit include precedes inherited rules.
The `.conf` suffix prevents a second include through `@includedir`.
This order keeps exceptions for specific commands, such as bootc upgrade.
The image uses real `visudo` to check syntax and effective inclusion.

The `cvtsudoers` tree proves the role is first and unique.
It rejects alternate or indirect duplicate includes.
Setup keeps one explicit include.
Tests parse the actual role with sudo's tools, without PAM or privileges.

The image appends hashes for the policy and root sudoers file.
It does not regenerate the existing GDM payload entries.
Later Orca and GDM layers must preserve that complete receipt.
These source and parser checks do not qualify the runtime gates.


The Debian package journey must retain the original beta package's
`share/pam.d/tuna-lock` conffile bytes, including its comment. Run
41151873648's `journey` failed on dpkg's conffile prompt before the
upgrade could qualify. PR #930 restores those bytes;
the fresh-account change must include that fix, not force a conffile
answer or change PAM policy to bypass the upgrade failure.
The Python lane.boot API keeps its original four-value return and original
VM device profile by default. Lifecycle callers explicitly request
`guest_agent=True` to add the QGA transport and receive its socket as a fifth
value; existing performance callers keep working without a guest agent.

QEMU's guest agent executes a fixed probe with a root UID, while the
compositor, shell and application clients must belong to the non-root
session owner. Artifacts record window IDs, app IDs, rectangles, process
PIDs/start ticks and redacted session metadata. They contain no process
arguments, passwords or application titles.

The last of the existing five pristine boots runs these checks after the
feature tour:

| Gate | Actual operation and evidence |
| --- | --- |
| V-LIFECYCLE-OWNER | Root probe, non-root `gdm-password` login session, three application surfaces/processes |
| V-NORMAL-INPUT / V-NORMAL-INPUT-FAIL-CLOSED | A real GTK client receives ordinary QMP keys while unlocked, then receives none while locked; artifacts contain only counts and PID, never key values |
| V-MOUSE-HANDEDNESS | Nine physical QMP clicks pass through native libinput to the same non-root GTK client while the real GNOME mouse setting changes false→true→false; left/right swap, middle stays unchanged, exact before/after counters retained in lifecycle/mouse-handedness.json |
| V-POINTER-LIVE | GNOME mouse speed/natural-scroll, acceleration profile (`flat`, then `adaptive`), middle-click emulation (on, then off) and scroll-wheel emulation (button 2 → `on-button-down` with BTN_MIDDLE and button lock, then off) reach live libinput device state and restore, asserted on the compositor's per-device libinput readback with per-option availability (each option needs at least one mouse offering it; scroll-wheel emulation is asserted on mice that offer button scrolling and report BTN_MIDDLE in `pointer_buttons`, since libinput rejects a scroll button the device lacks, e.g. the guest's VMMouse); a published guest that predates profile or middle-click/scroll-button reporting skips only those checks, a current build that omits them fails, and a guest mouse schema without GNOME 51's scroll-wheel emulation keys skips only that check; snapshots retained in lifecycle/pointer-live.json (#343) |
| V-POINTER-HOTPLUG | A QMP-hotplugged USB mouse arrives with the current settings already applied (DRM DeviceAdded path); unplug drops it while survivors keep theirs; bus discovery, arrival and survivor snapshots retained in lifecycle/pointer-hotplug.json (#343) |
| V-VT | VT away/back, DRM pause/activate events, unchanged process identities and app rectangles, matching body plus a fresh visible count update from the real GTK client |
| V-SUSPEND | Real logind suspend reaches QEMU `suspended`; the first guest snapshot after `system_wakeup` must already be locked, with all application identities intact; the real varied lock mask must repaint within five seconds, matching the pre-suspend mask; QEMU inactive-output placeholders fail |
| V-SLEEP-AUTH-FAIL-CLOSED / V-SLEEP-FRESH-AUTH | The single real suspend interrupts a correct password attempt delayed by actual pam_exec; the real PAM success from the old generation is refused, then fresh correct authentication succeeds |
| V-SLEEP-CLIENT-REPAINT | The same GTK client receives a fresh key and visibly repaints after wake/unlock |
| V-VT-FAIL-CLOSED | VT away/back while locked keeps the mask and original application processes |
| V-AUTH-FAIL-CLOSED | Real PAM service temporarily uses `pam_deny`; fresh submitted/refused milestones prove the correct test password was attempted and denied; original service restored afterward |
| V-CAPTURE-INPUT-FAIL-CLOSED | Reachable real untrusted grim/wtype clients are denied capture/virtual-keyboard interfaces while locked; a successful Wayland connection is required and timeouts do not count as refusal; this does not assert that these interfaces are available while unlocked |
| V-SHELL-FAIL-CLOSED | Stop the portal and kill the lock UI; original app/compositor identities survive and the supervised replacement stays locked |
| V-GSD-PLUGINS | Each of the thirteen gnome-settings-daemon plugins the session target wants, MediaKeys included, is the packaged `gsd-*` binary, running as the session user as its unit's main PID and owning its bus name; `org.gnome.SettingsDaemon.Color` belongs to the compositor; A11ySettings has no owner; the session targets are active and the compositor holds `gnome-session-leader-fifo` (lifecycle/gsd-plugins.json, #355, #589) |
| V-GSD-POWER-IDLE | With `idle-delay` set to 20 s and no input, dbus-monitor records gsd-power's own `SetDimming(true)` before its `PowerSaveMode=3`; the compositor snapshot reports locked and `power_save_mode` 3; Return wakes it to 0 and gsd-power undims; the original delay is restored (lifecycle/gsd-power-idle.json, #355) |
| V-LOCKED-HINT | logind's `LockedHint` for the login session reads `no`; the session user's `loginctl lock-session` locks the compositor and the hint turns `yes`; the real password unlocks and it reads `no`; Super+L locks with `yes`; `loginctl unlock-session` unlocks with `no`, as GNOME 51's `screenShield.js` does (lifecycle/locked-hint.json, #846). The published flavor predates the integration and defers to the current-build lane |
| V-POWER-BUTTON | gsd-media-keys (its unit's main PID, the session user) holds logind's `block` inhibitor on `handle-power-key:handle-suspend-key:handle-hibernate-key` with Why "GNOME handling keypresses". On a fixture desktop chassis, after each gsd restart a new gsd-media-keys must own its name, hold that inhibitor, have `XF86PowerOff` granted by `GrabAccelerators` and the shell must log that it yielded the bindings. QMP `system_powerdown` (ACPI power button) with `power-button-action` `interactive`: `AcceleratorActivated` reaches gsd, gsd calls `org.gnome.SessionManager.Shutdown`, the compositor calls `EndSessionDialog.Open(1, …)` and the `tuna-shell-end-session` layer shows within 15 s; 5 s after the press QEMU is `running` with the same compositor process and logind session and no logind power request; Escape (always sent, far inside the 60 s countdown) removes the layer, the shell emits `Canceled` and logs its D-Bus Power Off dialog. `nothing`: gsd receives the key, and for at least 8 s no Shutdown, dialog call, layer or logind request follows. On the image's `vm` chassis with a fixture `shutdown` block inhibitor held by root: the key reaches gsd and gsd's own process calls logind `PowerOffWithFlags(0)`; gsd sends it without a reply callback (NO_REPLY_EXPECTED), so logind's refusal has no bus reply, and 5 s later logind has emitted no `PrepareForShutdown`/`PrepareForSleep`, `PreparingForShutdown` is still false with the fixture inhibitor held, no other logind request or non-error reply followed, no dialog opened and QEMU, compositor and session are unchanged. Setting, chassis and `/etc/machine-info` are restored (lifecycle/power-button.json, #353, #589) |
| V-LOGOUT | The product's Log Out: `org.gnome.SessionManager.Logout(1)` on the session bus, which ends the compositor (the session leader) through its orderly shutdown, as gnome-session's exit ends a GNOME session; a published guest that predates that manager logs out as its shell did, through logind `Session.Terminate`. Within 30 s GDM's non-root gnome-shell greeter in its active gdm-launch-environment session repaints the greeter frame captured before login and no original compositor survives. logind's Terminate signals GDM's session worker together with the compositor; the worker's switch to the login VT could then be cancelled by logind restoring the session VT, leaving GDM without a greeter on the session VT (published nightly runs 40883859027 and 40940329310 attempt 1) |

The original tour's `V-LOCK` still requires exactly unlocked → locked →
unlocked. Its observations end before lifecycle operations add their own
lock transitions. A QEMU pause is never a substitute for guest suspend.
The repaint comparison excludes the panel clock and tolerates up to 20%
changed body pixels for applications such as System Monitor; it also
requires exact window and process identity preservation.

V-POWER-BUTTON runs on the unlocked desktop after V-SHELL-FAIL-CLOSED and
before V-LOGOUT. gnome-settings-daemon 51 reads hostnamed's `Chassis` once
at startup and, on `vm`, powers off for every `power-button-action` but
`nothing`. QEMU guests report `vm`, so the fixture (root, through QGA)
records `/etc/machine-info` and `Chassis`, calls hostnamed `SetChassis`
with `desktop` and restarts `org.gnome.SettingsDaemon.MediaKeys.target`
as the session user. The service refuses manual start and stop, but a
restart of its `PartOf` target propagates; if refused, the fixture kills the
main PID and `Restart=on-failure` brings it back. Afterwards the fixture
restores the recorded chassis and file bytes and restarts gsd again, so the
rest of the lane sees the image default. The `vm` case stays safe because
gsd 51 sends `PowerOffWithFlags` without `SD_LOGIND_SKIP_INHIBITORS`, and
since systemd 257 logind refuses that request outright under a `block`
inhibitor. The fixture refuses to press the button on an older logind.
`power-button-action` `suspend` is not exercised. It would add a second
real S3 cycle, and the user-authorized single-S3 contract below forbids
that. `hibernate`, the lock-screen mode, and an unrestrained VM power-off
are not exercised either.

The compositor also listens to the trusted logind system-bus sleep signal:
preparation invalidates pending authentication generations and engages its own lock; wake restores KMS connector/plane
state and retires lost flip buffers without marking them presented. After
the blocking device reset, bounded nonblocking reads drain obsolete kernel
completions before any new frame is submitted. A valid modeset completion
can carry the preceding vblank's timestamp, so timestamps alone do not
identify abandoned frames. A failed reset or drain keeps scanout blocked
until recovery succeeds. Wake
recovery does not depend on a VT change; an off-seat wake waits for seat
activation. This does not establish a delay inhibitor or physical masked
frame acknowledgement before sleep.

These new gates require their own actual CI results before acceptance.
Output hotplug and loss of every compositor-adjacent service remain
unproven, as do physical hardware and 24-hour soak. This document does not
claim those parts of issue #62 are complete.

Protocol references: [QEMU guest agent](https://www.qemu.org/docs/master/interop/qemu-ga-ref.html)
and [QEMU wake-up](https://www.qemu.org/docs/master/interop/qemu-qmp-ref.html#command-system-wakeup).

The shared GPU profile `virtio-vga-pcie-pm-preserved-v1` keeps the same
virtio-vga, resolution, CPU, RAM and workloads. It places that GPU behind
one PCIe root port and advertises PM no-soft-reset. All native GNOME
capture and Tuna Desktop/performance boot callers use the same GPU arguments;
manifest/report fields record them. Fresh GNOME comparison evidence must
use this profile; old-profile latency or cadence does not qualify it.
The actual guest PCI capability chain must include PCIe, PM and the
no-soft-reset bit, and each real resume must produce fresh host resource
creation, backing attachment and nonzero scanout commands as well as the
painted lock mask. A configured flag alone never passes the gate. The
shipping AWS Bochs GPU is a separate, unmeasured suspend profile.

The diagnosed fixture runs kernel 7.2.8-arch1-2 and QEMU 10.2.1. In
[Linux 7.2.8 virtio GPU](https://raw.githubusercontent.com/gregkh/linux/v7.2.8/drivers/gpu/drm/virtio/virtgpu_drv.c)
the driver has no freeze/restore callbacks. The
[PCI PM path](https://raw.githubusercontent.com/gregkh/linux/v7.2.8/drivers/virtio/virtio_pci_common.c)
skips reset when the device advertises no-soft-reset; otherwise
[virtio restore](https://raw.githubusercontent.com/gregkh/linux/v7.2.8/drivers/virtio/virtio.c)
resets the device. [QEMU 10.2 virtio PCI](https://raw.githubusercontent.com/qemu/qemu/v10.2.0/hw/virtio/virtio-pci.c)
provides that capability only behind a PCIe port and preserves the device
in D3hot. A [GPU reset](https://raw.githubusercontent.com/qemu/qemu/v10.2.0/hw/display/virtio-gpu.c)
destroys its resources and clears the display. The prior actual trial
37211055978/111464650400 reached real S3, stayed locked with apps intact
and processed guest pageflips, but showed inactive host output and no
fresh resource uploads. It remains a failed qualification. The supported
PM profile requires its own strict actual run; no kernel/package override,
extra VM matrix cell or physical hardware claim is introduced. QEMU 10.2
is required, so the existing performance runner uses ubuntu-26.04.

The performance lane keeps the user-authorized single-S3 contract: the session
is prelocked and a real PAM attempt is pending before sleep. Its fail-closed
wake and stale-success/fresh-authentication checks share that cycle. Varied
mask repaint is retained as an observation; fresh host GPU uploads, client
input/repaint and unchanged application identity remain mandatory. The prior
native lifecycle qualification (#265) separately demonstrated locking from
an unlocked session and two real suspend cycles on the preserved-resource VM
profile; that evidence does not certify a later candidate automatically.

### Date & Time restoration and published capability failures

Published-flavor run 41151873652 observed the new Auckland panel time, then
restored timedated to UTC without observing the panel's UTC repaint.
`G-DATETIME-CLOCK-SET` immediately read the Auckland label after the manual
clock jump (39,610 seconds from UTC), so that assertion did not have a
restored-panel baseline.

The timezone round trip now retains `panel_revert` dumps and
`panel_revert_verified` in `lifecycle/datetime-evidence.json`, waiting within
the existing ten-second repaint deadline. The clock timezone gate requires
both outbound and return repaint. A failed restoration or dump remains
visible in the probe error, fails the clock timezone gate, and prevents the
manual-time probe from changing the clock against an unverified baseline.
The polkit timezone probe runs after the manual-clock probe, so its own
timezone round trip cannot invalidate that baseline.

Both published panel-change assertions stay strict. A released image that
cannot perform them is reported as unqualified/unsupported with its actual
failure evidence, never converted to a pass or credited with another
lane's result. No additional wait extends the capability deadline.

## Locked password speech

Run [41151873648, job 124396764753](https://github.com/tuna-os/tuna-desktop/actions/runs/41151873648/job/124396764753)
passed eight speech gates, then failed `G-ORCA-SPEECH-LOCK`: the retained
[`orca-speech` artifact 13025680053](https://github.com/tuna-os/tuna-desktop/actions/runs/41151873648/artifacts/13025680053)
contains `Lock Screen`, `Password`, `password text.` followed by an empty
`lock-type` speech section. Its sanitized state records an active password
conversation, `locked: true`, and an owned, watched KeyboardMonitor. The
AT-SPI registry was already active. This is not evidence of bus startup
failure.

The missing route was committed masked input to speech, not raw-key
delivery. The compositor intentionally suppresses raw KeyboardMonitor
events while locked. In the installed [Orca 50.3 insertion classifier](https://raw.githubusercontent.com/GNOME/orca/50.3/src/orca/ax_utilities_event.py),
`_get_insertion_text_key_reason` relies on a last printable key/space to
classify password insertion as typing echo. Without that cue,
`_get_insertion_fallback_reason` can return `UNKNOWN` or an unpresentable
auto-insertion reason; [the default insertion handler](https://raw.githubusercontent.com/GNOME/orca/50.3/src/orca/scripts/default.py)
does not speak those insertions. The retained artifact does not contain
Orca's internal event-reason trace, so no particular fallback branch is
claimed as an observed runtime result.

The native Orca consumer needs a package-level correction for the supported
50.3 and 51 versions so committed masked password insertions are presented
without raw locked-session keys. That correction and its package/runtime
qualification are separate and pending; this change does not claim the
masked-input speech failure is fixed. No shell-generated mask announcement
or replacement for Orca's native text-event handling is included.

`scripts/tuna-orca-speech-proof --artifacts orca-speech-artifacts` remains
the end-to-end native nested proof: it physically types into the lock
prompt, requires exactly one spoken mask per typed character, rejects
plaintext speech, verifies that the locked raw-key event count does not
advance, then unlocks through PAM. No injected Orca customization or
relaxation of the bullet assertion is used. These changes still require a
new native proof result; the failed run is not retroactively qualified.

The native proof isolates `XDG_STATE_HOME` and `XDG_CACHE_HOME` under its
artifact root as well as HOME/config/data. A local native attempt stopped
before the speech gates because inherited state selected
`~/.local/state/roost-shell` for upgrade, where the ownership/ancestor <!-- tuna-rename: keep -->
guard refused migration. `user_upgrade::pairs_from` and the shell's
`xdg::state_home_from` prefer an absolute `XDG_STATE_HOME` over HOME;
changing HOME alone does not isolate that route. The proof now exports
private state/cache roots explicitly. It does not touch the real user
paths or bypass the migration guard.

The first local attempt used host Orca 46.1, which could not satisfy the
KeyboardMonitor contract. A later isolated Ubuntu 26.04 run with native
Orca `50.3-0ubuntu0.1+tuna1` passed all 11 speech assertions in 141 seconds.
It typed nine characters, observed nine spoken bullets without plaintext,
unlocked through PAM, and confirmed that the toggle stopped Orca.
[The retained report](https://github.com/tuna-os/tuna-desktop/pull/931#issuecomment-6545553530)
records source `69e9a04fd20aa2bc38427c79aec043cb3174da3b` and the actual
package and binary identities. This proves the nested speech path, not
the separate GDM fresh-account journey or ISO installation.
