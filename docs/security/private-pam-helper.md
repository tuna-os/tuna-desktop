# Private PAM helper foundation

This authentication-only source increment for issue #366 connects `PamClient`
and Runtime password checks to a private helper. GTK, fingerprint and the shell
control protocol are unchanged. Actual CI execution of this increment is pending.

The generic `Supervisor::spawn(executable, ...)` is a trusted caller/test seam;
private IPC cannot authenticate an arbitrary executable. The CI fixture admits
its Cargo compositor inode and byte hash. The live factory re-executes the held
current compositor image through a private file descriptor. It never accepts an
executable path from UI, environment or configuration.

The fixed internal compositor entry point runs before desktop or legacy-directory
initialization. A supervisor owns its child, private pipes, absolute IPC budget,
pidfd and process group. The creating thread must remain alive until retirement:
Linux parent-death signalling follows that thread. IPC is bounded after launch;
`Command::spawn`'s OS exec handshake does not have a portable hard deadline here.
The helper must therefore be launched by a stable supervisor off the UI loop.
Start/challenge/answer/cancel/result frames have strict
fields and bounds. Each PAM message has its own index and echo-on, echo-off,
information or error type; responses are never substituted with another answer.
Request identity, nonce, UID, generation and challenge sequence must match.
Concurrent module callbacks are rejected through a mutex with `try_lock`; they
cannot alias the mutable exchange or introduce an unbounded lock wait.

The helper requires its real/effective UID and requested user to agree, checks
the final PAM user, runs authentication and account checks, and completes PAM
teardown. It neither changes privilege nor opens/starts a login session.

Authentication remains provisional until the helper exits successfully and its
reserved process group has no live members. Cancellation and failed exchanges
kill owned processes. An unconfirmed retirement leaves an owned quarantined
supervisor; future runtime integration must retain it and block another backend.
Dropping a quarantined object performs shutdown cleanup, not a retirement proof.
Kernel uninterruptible states, detached PAM module processes and external daemon
state are outside a universal process-group retirement guarantee. Actual installed
stacks require separate service/device cleanup qualification.

The supervisor reads EOF on the helper's pipes as the helper being gone and kills
its process group at once. The helper therefore never closes its conversation
pipes itself; only the kernel closes them, after the process has finished
exiting. An early close let that kill land during exit work: in instrumented
builds it truncated the helper's coverage profile and failed the measured
coverage merge.

Answers use private stdin, not argv, environment, logs or temporary files.
Owned answer/transport buffers are wiped and diagnostics redact values. Core dumps
are disabled in the helper. PAM modules own successful callback response buffers;
the application cannot promise how an arbitrary module handles them or its logs.

## Qualification

CI tests launch `CARGO_BIN_EXE_tuna-compositor` under an isolated required
`pam_wrapper` service directory with a controlled C module. The module requests
multiple different indexed answers, then a separate PIN challenge. Tests cover
account denial, changed PAM user, cancellation, stale responses, oversized frames,
partial-frame dribble, authentication/teardown hangs, crash, and a live process
group descendant, plus creator-thread death while its parent process survives.
Unit tests cover strict codec and quarantine behavior.

The fixture module needs `libpam0g-dev` and a C compiler in CI only. The production
helper still loads the existing libpam dependency dynamically; no additional
installed executable, privilege or service change is introduced. No local Cargo
build or helper execution qualifies this source draft.

GNOME 51 uses GDM's reauthentication services and exposes distinct challenge and
web-login handling in [authPrompt.js](https://raw.githubusercontent.com/GNOME/gnome-shell/51.0/js/gdm/authPrompt.js)
and [userVerifier.js](https://raw.githubusercontent.com/GNOME/gnome-shell/51.0/js/gdm/userVerifier.js).
This private PAM foundation does not qualify that stack, GDM session launch,
keystore integration, smartcard hardware, web login or all of issue #366.
Common-slot ownership and live helper routing are present in the new source
increment below. Accessible challenge UI and installed-method proofs remain
subsequent required increments.

## Live password adapter source increment

The live factory holds the current executable through `/proc/self/exe`.
It has no caller-selected executable path. One stable creator thread owns each
helper through retirement or quarantine. A timeout sends denial. It does not
release backend admission or the Runtime attempt slot. Lost receivers and owner
faults fail closed. A poisoned owner cannot admit a replacement credential.

Runtime retains its outer wrapper while helper retirement is pending. Its event
channel wakes the loop for denial and completion. Quarantine does not request
continuous frame polling. Public synchronous PAM calls deny after the same budget
and retain the slot until actual retirement. Late results cannot grant access.

The password adapter answers one Secret challenge per transaction. A second
Secret, or a Visible challenge after a Secret, is unsupported and denied. It never
reuses the same password for a separate PIN. Full accessible challenge UI and
MFA remain required for issue #366.

New CI source contains controlled stable-owner, busy, cancellation, account-denial
and repeated-secret tests. A separate actual-main Runtime case uses the admitted
binary, replaces only its owned copied path, and checks helper inode identity,
old-origin denial and retirement before a fresh grant. These new cases have not
run yet. They do not qualify installed PAM stacks, detached module processes,
physical authentication devices, logind suspend or all of issue #366.
