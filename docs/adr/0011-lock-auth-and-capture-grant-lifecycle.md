# ADR 0011: Lock/auth and portal/capture grant lifecycle

- Status: Proposed (#1162); owner decision pending
- Date: 2026-11-16
- Owner: project (decision pending)
- Dependent speks: 004 (secure session)
- Related: [ADR 0010](0010-same-uid-trusted-identity.md) (who is trusted),
  [ADR 0005](0005-control-socket-peer-authentication.md),
  [security model](../security-model.md),
  [capture security](../capture-security.md), issue #1162

## Context

[ADR 0010](0010-same-uid-trusted-identity.md) records which processes the
compositor trusts under one UID. This ADR records what those processes may
do over time: how the lock engages and ends, and how a capture or remote
grant is created, used and revoked.

The threat model is the same. Every process runs as the session user. A
person at a locked screen can type and click on the lock surface. Any
same-UID process can call the session bus and connect to the Wayland
socket. The properties to keep are:

- while locked, no application surface is shown or receives input, and no
  capture or remote input runs;
- only the session user's credentials end the lock;
- capture and remote input need consent, belong to the consenting caller,
  and end when that caller, its identity or the unlocked session ends.

The [ADR register](README.md#decision-register) lists the production
lock/auth and portal/capture grant lifecycle as an open 1.0 release
blocker. This ADR describes the code on `main` and proposes it as the 1.0
lifecycle.

## Decision (proposed)

The compositor owns the lock flag and every grant. The shell and the portal
ask; the compositor decides. Every failure path keeps the session locked
and every grant revoked. Paths are relative to the repository root.

### Lock lifecycle

**Engage.** Every trigger calls `Runtime::engage_lock`
(`crates/compositor/src/runtime.rs`):

- the shell's control `Lock` command (`crates/compositor/src/control.rs`;
  adopted in `Runtime::tick`);
- an `ext_session_lock_v1` lock approved for the shell (ADR 0010);
- logind's `Lock` signal, refused under the disable-lock-screen lockdown
  (`Runtime::lock_for_logind`);
- the idle timeout (`crates/compositor/src/lock.rs`, `SessionLock`;
  `Runtime::tick`);
- logind's `PrepareForSleep`, both before sleep and again on wake
  (`crates/compositor/src/sleep.rs`, `Monitor`; `Runtime::on_sleep_update`);
- `org.gnome.ScreenSaver.Lock` or `SetActive(true)` on the shell, which asks
  the compositor to lock. `SetActive(false)` never unlocks
  (`crates/shell-gtk/src/screensaver.rs`).

`engage_lock` advances the authentication epoch, sets the lock flag in the
idle machine and the control hub, cancels held and pending accelerators,
stops remote input and remote clipboard, revokes every InputCapture
session, publishes the locked state to the capture authority, closes the
overview, dismisses popup grabs, clears switcher thumbnails and shows the
overlay with an empty window list.

**While locked.**

- Rendering takes `content_visible(locked)`: no window, layer or wallpaper
  is drawn, only a dimmed background and the approved lock surface
  (`Runtime::render`). Until that surface exists the screen stays blank.
  Evidence: `lock.rs` (`content_hidden_only_while_locked`);
  `crates/compositor/tests/session_lock_protocol.rs`
  (`only_the_approved_lock_screen_is_presented_while_locked`);
  `crates/compositor/tests/lock.rs` (`lock_surface_carries_no_titles`).
- Input goes only to the approved lock surface on the primary output
  (`WindowManager::lock_input`, `crates/compositor/src/windows.rs`). With no
  lock surface it is discarded. Media keys still reach the shell, as in
  GNOME. Evidence: `crates/compositor/tests/session_lock_runtime.rs`
  (`actual_runtime_grants_only_the_shell_lock_and_input_reaches_only_its_curtain`);
  VM gate `V-NORMAL-INPUT-FAIL-CLOSED`.

**PAM conversation.** The shell relays the user's answers over the control
socket. The compositor runs the conversation in the private PAM helper
(ADR 0010) against the `tuna-lock` service (`share/pam.d/tuna-lock`,
which includes `login`) for `getpwuid(getuid())`.

- Every attempt is bound to its origin (control connection and request),
  attempt serial and lock epoch (`crates/compositor/src/runtime/auth.rs`;
  `crates/compositor/src/auth_owner.rs`, `Slot`). The origin stays live only
  while the control session belongs to the current shell child, pinned by
  `SO_PEERPIDFD`, and its supervisor generation.
- One attempt runs at a time (`Slot::begin_method`). A password check has a
  10 s deadline (`crates/compositor/src/pam.rs`, `DEADLINE`;
  `crates/compositor/src/pam_helper.rs`, `MAX_BUDGET`). A conversation has a
  30 s backend phase and a 900 s total (`runtime/auth.rs`, `POLICY`).
- Evidence: `auth_owner.rs` unit tests
  (`stale_mismatched_duplicate_and_concurrent_results_preserve_ownership`
  and others); `crates/compositor/tests/auth_owner_runtime.rs`
  (`actual_runtime_epoch_invalidation_and_public_sync_unlock`, run with
  `TUNA_REQUIRE_PAM_WRAPPER=1` in the CI `check` job);
  `scripts/tuna-lock-auth-proof` in the `gtk-shell` lane. Detail:
  [lock conversations](../security/lock-conversations.md),
  [password result ownership](../security/password-owner-epoch.md),
  [PAM conversation relay](../security/pam-conversation-relay.md).

**Fingerprint through PAM (F5; fixed in #1173).** Fingerprint unlock runs
`pam_fprintd` in the private PAM helper on its own PAM service,
`tuna-lock-fingerprint` (`share/pam.d/tuna-lock-fingerprint`: `pam_fprintd`
plus `login`'s account stack), as GNOME 51 runs `gdm-fingerprint`. The
helper runs `pam_authenticate` then `pam_acct_mgmt`, so the account stack,
`pam_faillock` and `pam_access` decide a fingerprint unlock as they decide a
password one. The compositor asks fprintd only whether a reader and an
enrolled print exist and which scan type the reader has
(`crates/compositor/src/fprintd.rs`). It enforces
`enable-fingerprint-authentication` itself and admits three failed
fingerprint conversations per lock (`runtime/auth.rs`,
`Runtime::begin_fingerprint`). Before #1173 a fprintd `Matched` result went
straight to `Runtime::apply_verified_unlock` and no account stack ran.
Detail: [lock conversations](../security/lock-conversations.md#fingerprint-conversation).

**Unlock.** A verified credential reaches `Runtime::apply_verified_unlock`,
which unlocks only if the session is still locked and `Slot::grant` wins a
compare-and-swap on the attempt's epoch. Then `Runtime::release_lock`
clears the flag and hands the keyboard back through
`WindowManager::unlock_input`, which quarantines keys held across the lock.

- A lock client's `unlock_and_destroy` never unlocks. From an unapproved
  lock it is a protocol error; from the approved one it only clears protocol
  state and the session stays locked (`crates/compositor/src/session_lock.rs`;
  `Runtime::tick`).
- Evidence: `session_lock_protocol.rs`
  (`destroying_a_granted_lock_without_unlock_is_an_error_and_never_unlocks`,
  `unapproved_release_is_a_protocol_error_without_clearing_the_owner`,
  `relocking_after_unlock_needs_a_fresh_approval`);
  `crates/compositor/tests/windows.rs`
  (`unlock_restores_keyboard_without_raising_and_quarantines_lock_origin_keys`);
  `crates/compositor/tests/pam_unlock.rs`.
- logind's `Unlock` signal also ends the lock, without a credential, as
  GNOME 51's screen shield does (`Runtime::unlock_for_logind`). See risk L1.

**Crash.**

- *Shell or lock UI.* The lock stays engaged and the screen stays blank.
  The control gate closes until a new child exists. The auth owner moves to
  the new child's generation, which drops old control sessions and pending
  handshakes (`ControlHub::set_auth_owner`). The replacement shell must ask
  for a new lock approval. Evidence: `crates/compositor/tests/lock.rs`
  (`reconnect_while_locked_stays_locked`); VM gate `V-SHELL-FAIL-CLOSED`.
- *PAM helper.* A crash, timeout or unconfirmed retirement is
  `Verification::Unretired`: the attempt is denied and the slot is
  quarantined (`Runtime::finish_unlock`). Evidence: `pam_helper.rs`
  (`private_pam_live_owned_process_is_quarantined_until_confirmed_retirement`).
- *Compositor.* `tuna-session` replaces itself with the compositor, so
  nothing restarts it. [INFERENCE] The session ends and its clients lose
  their display; no surface is shown unlocked. Untested.

**VT switch.** Switching VTs is allowed while locked, as in GNOME 51. The
flag is untouched by libseat's pause and activate events
(`Runtime::on_session_event`). Evidence: VM gates `V-VT` and
`V-VT-FAIL-CLOSED`.

**Sleep.** On `PrepareForSleep(true)` the logind signal thread advances the
auth epoch at once, so a PAM success from before sleep cannot unlock
(`sleep.rs`, `Monitor::from_connection`). The compositor locks then, and
locks again on wake before the DRM resume (`Runtime::on_sleep_update`).
Only logind's bus-name owner is believed (`sleep.rs`,
`only_the_bus_name_owner_can_prepare_and_wake_and_monitor_releases_worker`).
Evidence: VM gates `V-SUSPEND`, `V-SLEEP-AUTH-FAIL-CLOSED`,
`V-SLEEP-FRESH-AUTH`. On `main` the compositor takes no logind sleep delay
inhibitor; #1170 adds one (risk L2).

**Fail-closed VM gates.** Defined in `scripts/tuna-vm-lane`
(`run_lifecycle`), run by the `marlin-vm` lane with the lifecycle profile,
described in [VM lifecycle](../vm-lifecycle.md):

| Gate | What it proves on the Marlin VM |
|---|---|
| `V-NORMAL-INPUT-FAIL-CLOSED` | A real GTK client gets no QMP keys while locked |
| `V-CAPTURE-INPUT-FAIL-CLOSED` | Reachable untrusted `grim` and `wtype` are refused capture and virtual keyboard while locked |
| `V-VT-FAIL-CLOSED` | VT away and back while locked keeps the lock mask and the same app processes |
| `V-AUTH-FAIL-CLOSED` | With `tuna-lock` swapped to `pam_deny`, the correct password is tried and refused; still locked |
| `V-SHELL-FAIL-CLOSED` | Portal stopped and shell killed: the replacement shell stays locked and masked, apps survive, no input leaks |

### Capture and remote grant lifecycle

**Consent.** The compositor shows no picker. Consent comes from the GNOME
portal backend's dialogs, from the shell's own capture UI, or, for
gnome-remote-desktop, from its configured credentials. The compositor only
authenticates the caller (ADR 0010). Screenshot consent is the portal's
Access dialog ([screenshot consent](../portal-screenshot-consent.md)).
InputCapture admits the portal only. No indicator or Stop control shows an
active grant today; one is being built (F2, risk L13).

**Grant.**

- `CreateSession` calls `Authority::admit` (unlocked, authenticate,
  unlocked again) or `admit_remote` for RemoteDesktop and its linked
  ScreenCast (`crates/compositor/src/capture_security.rs`;
  `crates/compositor/src/mutter.rs`).
- A session belongs to the unique bus name that created it. Every later
  call checks `owns_session` (`capture_security.rs`; `mutter.rs`,
  `session_owner`; `mutter/remote_desktop.rs`, `RemoteSession::admitted`;
  `mutter/input_capture.rs`, `CaptureSession::admitted`).
  `PipeWireStreamAdded` is sent only to the owner.
- `Start` is single-use for sessions and streams. A stream added after
  Start waits for its own `Stream.Start`, as in Mutter (`mutter.rs`,
  `Session::add_stream`, `start_streams`, `Stream::begin`).
- A RemoteDesktop grant links at most one ScreenCast session, from its own
  owner (`mutter.rs`, `linked_grant`).
- `ConnectToEIS` works once per RemoteDesktop grant, for its owner, while
  unlocked. The compositor keeps one end of a socketpair; input is accepted
  only after Start (`mutter/remote_desktop.rs`; `mutter/remote_eis.rs`).
  InputCapture allows one live EIS client per session.

**Limits.**

| Resource | Limit | Where |
|---|---|---|
| ScreenCast sessions | 256, shared by all owners | `mutter.rs` |
| Streams per ScreenCast session | 64 | `Session::add_stream` |
| RemoteDesktop grants | 64, shared | `mutter/remote_desktop.rs` |
| InputCapture sessions; barriers each | 64; 1024 | `mutter/input_capture.rs` |
| Queued remote input events | 256 | `mutter/remote_desktop.rs`, `queue_input` |
| Remote clipboard | 64 MIME types, 16 pending transfers, 256 MiB and 15 s per transfer | `mutter/remote_clipboard.rs` |

**Revocation.** Revocation is permanent.

- While any grant exists, a scan on the D-Bus thread runs every 100 ms
  (`mutter.rs`, `SCAN_PERIOD`). It revokes a grant when
  `Authority::owner_alive` fails: the session is locked, the owner's unique
  name is gone, the shell no longer declares that name over its pinned
  control session (ADR 0010), the portal name changed hands or its
  executable changed, or grd lost its name. An owner check that does not
  answer within 5 s counts as gone (`owners_alive`, `OWNER_CHECK_TIMEOUT`).
  Evidence: `mutter.rs` (`owner_checks_are_bounded_and_fail_closed_on_a_wedged_bus`).
- Lock is faster than the scan for the risky parts. `screencast_tick`
  closes every PipeWire stream before any locked frame is produced.
  `engage_lock` revokes InputCapture and stops remote keys, buttons and
  clipboard at once. `remote_input` drops input while locked.
  `capture`, `capture_window` and the screenshot flash refuse locked frames,
  and Screenshot answers `false` while locked (`runtime.rs`;
  `crates/compositor/src/screenshot.rs`). Evidence:
  `mutter/input_capture.rs` (`lock_and_pointer_constraints_never_capture`).
- Revoked EIS workers send `disconnected` and close the compositor's end
  (`mutter/remote_eis.rs`; `mutter/capture_eis.rs`).
- CI `portal-security` and `remote-security` check creator disconnect,
  backend disconnect, Close and active lock: the node leaves `pw-dump`,
  `capture_streams` returns to 0 and held keys are released
  (`tests/portal-reference/run.sh`, `tests/remote-desktop-reference/run.sh`).

**Session end.** `Session.Stop` revokes at once and removes the objects.
RemoteDesktop `Stop` releases held keys and buttons, revokes the clipboard
and cuts transfers in flight (`mutter.rs`; `mutter/remote_desktop.rs`;
`Runtime` remote stop handling). Evidence:
`mutter/remote_bus_tests.rs`
(`session_clipboard_and_keysyms_are_owned_bounded_and_revocable`,
`a_plain_session_streams_after_start_and_ends_with_its_stream`).

**Re-use.** A stopped or revoked session cannot start again; `Start` after
`Stop` is `AccessDenied`. The compositor stores no restore token. Portal
persistence (`persist_mode`, restore tokens) lives entirely in
xdg-desktop-portal-gnome, which creates a new, freshly admitted session.
The compositor cannot tell a restored grant from a new one.

**Lockdown.** `disable-save-to-disk` refuses the portal's screenshots
(`screenshot.rs`, `save_to_disk_lockdown_refuses_only_the_portal`); the shell
keeps its own captures on the clipboard and refuses its own screencasts.
As in GNOME it does not gate portal ScreenCast or RemoteDesktop. Camera and
microphone privacy is enforced in the shell
([privacy enforcement](../privacy-enforcement.md)).

## Alternatives considered

- **Lock only on explicit request, not on wake.** Rejected: the VT can stay
  active through S3, and the shell may have died; the compositor locks on
  wake regardless.
- **Block VT switching while locked.** Not adopted: GNOME 51 allows it, and
  the other VT shows only its own session. `V-VT-FAIL-CLOSED` proves the
  lock survives.
- **Ignore logind `Unlock`.** Not adopted: `loginctl unlock-session` and
  GDM rely on it, and GNOME honours it. Revisit with risk L1.
- **Revoke on lock only through the scan.** Rejected for streams, input
  capture and held remote input: the lock path ends them directly.

## Consequences

- The lock, its UI and its authentication are three processes with one
  owner. A shell crash, helper crash, PAM denial, VT switch or sleep leaves
  the session locked.
- A grant lives no longer than its creator, the creator's trusted identity
  and the unlocked session, give or take one scan.
- Persistence policy for portal grants is the portal's, not the compositor's.

## Residual risks

[ADR 0010](0010-same-uid-trusted-identity.md#review-findings-and-dispositions)
maps every finding of the #1162 security review, and every gap found while
writing these ADRs, to a disposition. "Fixed in #N" names the pull request
that closes a risk below; "in progress" means a fix is still being built.

- **L1. Unlock without a password (F9, G5; accepted, GNOME parity).** Any
  same-UID process can run
  `loginctl unlock-session`; logind lets the session owner do so without
  polkit, and the compositor unlocks (`Runtime::unlock_for_logind`). VM gate
  `V-LOCKED-HINT` exercises this path. GNOME 51 behaves the same way
  (`screenShield.js` `deactivate`).
- **L2. No sleep delay inhibitor (F6, G3; fixed in #1170).** `sleep.rs` subscribes to
  `PrepareForSleep` but holds no logind `delay` inhibitor, so logind does
  not wait for a locked frame before suspending. [INFERENCE] The last
  scanned-out frame can be the unlocked desktop and may show on resume
  until the first repaint. `V-SUSPEND` only suspends an already locked
  session (`scripts/tuna-vm-lane`, `suspend_and_check`). GNOME 51 takes a
  delay inhibitor (`loginManager.js`, `screenShield.js` `_syncInhibitor`).
- **L3. Stuck lock.** If the shell's restart budget runs out while locked,
  no lock surface returns and input is discarded. A quarantined PAM attempt
  keeps the slot busy. Before #1171 a flood of foreign lock requests could
  also leave the lock without a prompt (F3; ADR 0010 R5). All fail closed;
  logind `Unlock` or ending the session is the way out.
- **L4. No compositor rate limit.** The compositor allows one attempt at a
  time and nothing more. Throttling is whatever `login` includes. The helper
  runs as the user, not root, so modules that need root, such as
  `pam_faillock` tallies, may not apply. [INFERENCE, untested.] GNOME runs
  `gdm-password` in GDM's root worker.
- **L5. Revocation latency.** One 100 ms scan plus a compositor tick, and up
  to 5 s for an owner on a wedged bus. Remote grants that hold no keys wait
  for the scan after lock; their input is refused meanwhile.
- **L6. PipeWire is not access-controlled by the compositor.** A cast node is
  named `tuna-screen-cast` (`crates/compositor/src/screencast.rs`). Any
  unsandboxed same-UID PipeWire client can link to an active node.
  [INFERENCE.] Mutter's nodes are equally open to unsandboxed clients.
- **L7. Untested EIS revocation (G9; fixed in #1169).** No test checks that a delivered EIS
  client sees a disconnect after lock or owner loss. The code closes the
  compositor's end. Fixed: `remote_eis.rs`
  (`a_delivered_eis_client_is_disconnected_when_its_grant_is_revoked`) and
  R-EIS-REVOKE in `tests/remote-desktop-reference/run.sh`, where a real
  libei client the GNOME 51 portal delivered hears the disconnect once the
  grant's backend owner is lost.
- **L8. Portal reach beyond need (G6; fixed in #1169).** `org.tuna.Screenshot.ScreenshotWindows`
  uses `Authority::admit` (`screenshot.rs`, `WindowsService::screenshot_windows`),
  so the authenticated portal, not only the shell, can save every window to
  any absolute directory. `disable-save-to-disk` does not cover it.
  Screenshots written to disk outlive their request by design. Fixed: only
  the shell passes; G-SCREENSHOT-WINDOWS-DENY (`scripts/tuna-gtk-shell-proof`)
  refuses code inside the genuine portal backend that owns its name.
- **L9. Shared limits.** Session limits are global, so one trusted caller
  can use up the pool.
- **L10. Evidence that is not per pull request.** `marlin-vm`,
  `portal-security` and `remote-security` are rotated lanes; a change to
  `crates/compositor` alone does not select them (`scripts/ci-select-lanes`),
  and `ci-gate` accepts a skipped lane. They always run on `main` and
  nightly (G7; accepted). The P-LK-01 ledger row cited the compositor's
  `unlock.rs` integration test (`correct_password_dismisses_lock_intact`),
  which drove `unlock::unlock_session`, a path the runtime does not use;
  production unlock is `Runtime::apply_verified_unlock` (G8; fixed in #1169).
  Fixed: that test and `unlock_session` are gone, and the row cites
  `crates/compositor/tests/auth_owner_runtime.rs`, which unlocks the actual
  nested runtime only through `apply_verified_unlock`.
- **L11. A drag icon above the lock (F7; fixed in #1170).** `engage_lock` does
  not cancel a client drag, and the drag icon is drawn on top of the scene
  without a lock check (`runtime.rs`, `dnd_icon_elements`). An application
  that starts a drag and then locks the session can cover the lock screen
  with its icon until the button is released. Keys still reach the lock
  surface.
- **L12. An unmapped idle inhibitor stops the idle lock (F8; fixed in #1170).**
  `State::idle_inhibited` (`protocols.rs`) counts any live surface with an
  inhibitor, mapped or not, and the idle-inhibit global is open to
  sandboxed clients. A background client can keep the session from ever
  locking on idle.
- **L13. Silent grants (F2; in progress).** No indicator or Stop control
  shows an active ScreenCast, RemoteDesktop or InputCapture grant. Only the
  shell's own recorder shows one (`crates/shell-gtk/src/screencast.rs`). A
  consented share, a restored share or a gnome-remote-desktop session runs
  with nothing on screen until it ends or the session locks. GNOME 51 shows
  a remote-access indicator with Stop.
- **L14. Fingerprint bypasses PAM (F5; fixed in #1173).** See
  [Fingerprint through PAM](#lock-lifecycle) above.

## Evidence

- Tests named above, run by the CI `check` job.
- VM gates above and `V-SUSPEND`, `V-SLEEP-*`, `V-LOCKED-HINT` in
  `marlin-vm` ([VM lifecycle](../vm-lifecycle.md)).
- `portal-security` and `remote-security` lanes
  ([capture security](../capture-security.md),
  [remote desktop](../remote-desktop.md),
  [input capture](../input-capture.md)).
