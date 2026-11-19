# Lock-screen authentication conversations

The lock screen authenticates the way GNOME Shell 51's unlock prompt does
through GDM's UserVerifier: a password conversation and a fingerprint
conversation run side by side, every PAM prompt and message reaches the
prompt in order, and either service can unlock. The compositor plays GDM's
part (`crates/compositor/src/runtime/auth.rs`); the GTK shell plays
`authPrompt.js` (`crates/shell-gtk/src/lock.rs`).

GNOME 51 sources: [authPrompt.js](https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/gdm/authPrompt.js),
[authServicesLegacy.js](https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/gdm/authServicesLegacy.js),
[authServices.js](https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/gdm/authServices.js),
[unlockDialog.js](https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/ui/unlockDialog.js),
GDM 51 [gdm-session-worker.c](https://gitlab.gnome.org/GNOME/gdm/-/blob/51.0/daemon/gdm-session-worker.c)
and fprintd's [pam_fprintd.c](https://gitlab.freedesktop.org/libfprint/fprintd/-/blob/master/pam/pam_fprintd.c)
with [fingerprint-strings.h](https://gitlab.freedesktop.org/libfprint/fprintd/-/blob/master/pam/fingerprint-strings.h).

## Wire (shell protocol 0.44)

`BeginVerification { service }` starts one conversation for the session's
own user; the user is never taken from the shell. `AuthEvent { request,
service, event }` carries its steps: `Info`, `Problem`, `Query { sequence,
question, secret }`, `FingerprintScan { swipe }` and `Unavailable`.
`AnswerQuery { service, sequence, answer }` answers the outstanding query
through the same never-replayed, wiped writer as `Unlock`.
`CancelVerification` stops the connection's conversations. Each begin gets
exactly one `CommandResult`, sent only after its conversation retired:
`Applied` when it unlocked the session, `Denied` otherwise. A second begin
for a service whose result is still owed is denied at once. Protocol 0.43
is reserved for the on-screen keyboard (#839).

## Password conversation

The private PAM helper runs the `tuna-lock` service through
`Backend::begin_conversation` under the common unlock Slot
(`MethodKind::PamConversation`), so it excludes the legacy `Unlock` path and
any unretired helper. Each PAM batch is relayed one question at a time with
a fresh wire sequence; Info and Error messages are forwarded and answered
empty; the batch is submitted only when every question has its own answer.
A stale or foreign answer is wiped and dropped. Nothing decides from prompt
text: a PIN or code after the password is another question, never the
password again.

The admitted policy is 30 s per backend phase and 15 minutes in total. The
relay's 120 s inactivity watch matches GNOME's unlock-dialog idle timeout;
real seat key and button input to the locked session counts as activity,
as GNOME's idle monitor does. A failed conversation that was not cancelled
reports GDM's `gdm-password` message ("Sorry, password authentication
didn’t work. Please try again.") before its denial. `Unavailable` is only
sent for permanent refusals (no backend or user, a quarantined Slot, no reader
or prints); after a password `Unavailable` the shell keeps the single-password
`Unlock` path. A begin refused because a previous attempt is still retiring, and
every cancellation the compositor starts itself (lock-epoch change such as
suspend, origin loss), end with a plain `Denied` that the shell may retry.
The shell never replays an answer it held for such a conversation (typed
before PAM asked): the restarted conversation asks for fresh input, as the
single-password `Unlock` is denied and never resubmitted. The cancelled
helper's retirement writes the same `lock authentication owned retirement`
receipt as the single-password worker, and the `auth_owner` snapshot names
the conversation's Slot attempt, so the VM lanes bind it the same way.

## Fingerprint conversation

GNOME runs `gdm-fingerprint`: `pam_fprintd` plus that service's account
stack. The compositor does the same (#1162 F5). The fingerprint conversation
is a PAM transaction on its own service, `tuna-lock-fingerprint`
(`share/pam.d/tuna-lock-fingerprint`: `auth required pam_fprintd.so`,
`account include login`, installed beside `tuna-lock`), in its own private
helper (`Backend::live_fingerprint`, a second stable owner so it runs beside
the password helper) on its own Slot (`MethodKind::Fingerprint`). The
helper runs `pam_authenticate` then `pam_acct_mgmt`, so an expired or
locked account, `pam_access` or faillock refuses a finger exactly as it
refuses a password; a match alone never unlocks. The attempt retires only
on its helper's retirement, like the password conversation's.

Before PAM runs, a probe off the loop reads
`org.gnome.login-screen enable-fingerprint-authentication` afresh (the
compositor enforces it; the shell's choice is not trusted; without GDM's
schema the key's default, on, applies) and asks fprintd whether a reader is
present, the user enrolled a print, and the reader's `scan-type`. It never
claims the reader. Without any of them the service is unavailable.

pam_fprintd's conversation drives the UI as in GNOME 51's
`authServicesLegacy.js`: an info message ("Place your finger on the
fingerprint reader", "Verification timed out") is only the cue for
`FingerprintScan`, GNOME's own "(or place finger on reader)" or "(or swipe
finger across reader)" hint by scan type; an error message ("Failed to match
fingerprint", retry statuses) is a `Problem`. A question has no place in this
conversation and cancels it. pam_fprintd makes up to `max-tries` (3) scans of
at most `timeout` (30 s); the policy gives a backend phase 60 s, and the
relay's 120 s inactivity watch bounds the whole conversation, since nothing
in it is user input.

The helper reports GDM's failure classes (`pam_helper::Refusal`), and the
compositor answers as GDM 51's session worker does: `PAM_MAXTRIES` is GDM's
max-tries message, then the service becomes unavailable
(`TOO_MANY_RETRIES`; the shell keeps the password prompt as it is and
begins fingerprint again only after the next reset or a return to the
clock, gnome-shell 51 `authServices.js` `_canRetry`); `PAM_AUTHINFO_UNAVAIL`
(a timed-out scan, no prints) is service-unavailable; `PAM_ACCT_EXPIRED` and
`PAM_AUTHTOK_EXPIRED` are "Your account was given a time limit that’s now
passed."; any other failure is "Sorry, fingerprint authentication didn’t
work. Please try again." Both of the last two end with a plain denial, so
the shell resets the prompt. GDM itself skips `pam_acct_mgmt` for
reauthentication; Tuna runs it for both services, as `tuna-lock` always did.

GNOME bounds nothing across fingerprint conversations on the unlock screen.
The compositor does: a lock epoch admits three failed fingerprint
conversations (one that relayed a rejected scan, or that PAM refused), so a
shell restarting the service can get at most three times `max-tries` scans
per lock; after that fingerprint is unavailable until the next lock, and the
password is unaffected. A fingerprint helper that does not retire leaves the
service refused for the life of the compositor.

## Grant

A success becomes a pending winner; the other conversation is cancelled.
The grant happens only when the common Slot has no live attempt (a
cancelled password helper must retire first), the origin connection and
request are still owed and live, the lock epoch is unchanged, and the epoch
compare-and-swap in `Slot::grant` succeeds. Every prompt is also rechecked
against the epoch and origin before it is sent; an epoch change (manual,
idle or sleep lock), origin loss or unlock cancels the conversation.

## Evidence

- `crates/compositor/tests/auth_owner_runtime.rs`
  `actual_main_runtime_pam_conversation_relay`: the actual nested runtime,
  private helper and controlled multi-prompt module (visible question,
  distinct secret, info, error, separate PIN); stale sequence, wrong PIN,
  cancellation and a second conversation stay locked; fresh answers unlock.
  Run by `scripts/tuna-auth-owner-ci-proof`.
- `crates/compositor/tests/auth_owner_runtime.rs`
  `actual_main_runtime_fingerprint_through_pam`: the actual runtime, the
  fingerprint helper and a pam_fprintd-like controlled module behind a
  private-bus fprintd probe; a match on an expired account is refused with
  GDM's account message, max tries and a timed-out scan end the service as
  GDM does, the fourth failed conversation is refused before PAM runs, the
  password still unlocks, and a match on a new lock unlocks.
  `actual_main_runtime_fingerprint_setting_enforced`: with
  `enable-fingerprint-authentication` off the compositor refuses the service
  itself and PAM never runs.
- `crates/compositor/tests/pam_helper.rs`: the actual helper reports
  `Refusal::{Expired, MaxTries, Unavailable, Failed}` for the fingerprint
  shapes, and an expired account never authenticates after a match.
- `crates/compositor/src/fprintd.rs` unit tests: the probe (reader,
  enrolled prints, scan type) against the private-bus fprintd stub.
- `scripts/tuna-lock-auth-proof`: the nested GTK lock screen against a
  pam_wrapper `tuna-lock` stack (pam_matrix password, then a one-time code),
  a `tuna-lock-fingerprint` stack (`scripts/lib/tuna-pam-fprint.c`,
  pam_fprintd's loop, then an account stack the proof expires for
  G-LOCK-AUTH-FPRINT-EXPIRED) and an emulated fprintd on a private system
  bus. `scripts/tests/test_tuna_lock_auth_fixtures.py` proves the fixtures.
- `scripts/tuna-vm-lane` V-SLEEP-AUTH-FAIL-CLOSED and V-AUTH-FAIL-CLOSED
  (marlin-vm, KVM, CI only): a real delayed PAM helper of an open
  conversation is cancelled and physically retired by real S3 without
  unlocking, and a real `pam_deny` stack refuses a conversation.

Not covered: real readers and the installed `pam_fprintd`, smartcards and
PIN pads (`pam_pkcs11` or `pam_sss` smartcard), web login (GDM's `eidp`
extension) and installed PAM stacks on the published image.
