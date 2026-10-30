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
didn’t work. Please try again.") before its denial. A configured greetd
daemon has no relay: the password service reports `Unavailable` and the
shell keeps the single-password `Unlock` path; fingerprint is refused too, so
no direct fprintd match bypasses greetd. `Unavailable` is only sent for such
permanent refusals (greetd, no backend or user, a quarantined Slot, no reader
or prints); a begin refused because a previous attempt is still retiring, and
every cancellation the compositor starts itself (lock-epoch change such as
suspend, origin loss), end with a plain `Denied` that the shell may retry.
The shell never replays an answer it held for such a conversation (typed
before PAM asked): the restarted conversation asks for fresh input, as the
single-password `Unlock` is denied and never resubmitted. The cancelled
helper's retirement writes the same `lock authentication owned retirement`
receipt as the single-password worker, and the `auth_owner` snapshot names
the conversation's Slot attempt, so the VM lanes bind it the same way.

## Fingerprint conversation

GNOME runs `gdm-fingerprint` (`pam_fprintd`). The compositor runs the same
loop directly over fprintd's D-Bus API on its own Slot
(`MethodKind::Fingerprint`, `unlock::run_fingerprint`): it is offered only
when a reader is present and the user enrolled a print, and makes up to
three scans of at most 30 s. Each finished non-matching scan reports
"Failed to match fingerprint"; retry statuses report pam_fprintd's message;
after three failures GDM's max-tries message ends the conversation and the
service becomes unavailable, as GDM 51 reports `PAM_MAXTRIES`
(`report_and_stop_conversation`, `TOO_MANY_RETRIES`): the shell keeps the
password prompt as it is and begins fingerprint again only after the next
reset or a return to the clock (gnome-shell 51 `authServices.js` `_canRetry`
always retries on the unlock screen). A
timed-out scan reports "Verification timed out" and the service becomes
unavailable, as `PAM_AUTHINFO_UNAVAIL` does in GDM. `FingerprintScan`
carries the reader's `scan-type` for GNOME's "(or place finger on reader)"
or "(or swipe finger across reader)" hint.

`Fprintd::verify_interactive` adds cancellation to the existing bounded
attempt: the parent polls the cancel flag every 50 ms and retires through
the deadline's fd-shutdown path, so fprintd drops the claim of the vanished
client and the claim reaper keeps release-or-quarantine. The fingerprint
Slot retires only on the joined result of its own worker thread; a later
scan waits up to 2 s for the previous claim to be released.

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
- `crates/compositor/src/unlock.rs` and `fprintd.rs` unit tests: the
  pam_fprintd loop, retry wording, interactive progress, cancel and claim
  release against the private-bus fprintd stub.
- `scripts/tuna-lock-auth-proof`: the nested GTK lock screen against a
  pam_wrapper stack (pam_matrix password, then a one-time code) and an
  emulated fprintd on a private system bus.
- `scripts/tuna-vm-lane` V-SLEEP-AUTH-FAIL-CLOSED and V-AUTH-FAIL-CLOSED
  (marlin-vm, KVM, CI only): a real delayed PAM helper of an open
  conversation is cancelled and physically retired by real S3 without
  unlocking, and a real `pam_deny` stack refuses a conversation.

Not covered: real readers, smartcards and PIN pads (`pam_pkcs11` or
`pam_sss` smartcard), web login (GDM's `eidp` extension) and installed PAM
stacks on the published image.
