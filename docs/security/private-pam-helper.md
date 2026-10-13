# Private PAM helper foundation

This helper is an authentication-only foundation for issue #366. It is not
connected to `PamClient`, live unlock, GTK, fingerprint verification or greetd.
The existing password path and shell control protocol remain unchanged.

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
This private PAM foundation does not qualify that stack, native-greeter session
launch, keystore integration, smartcard hardware, web login or all of issue #366.
Common-slot ownership/correlation must land with live helper adoption; accessible
challenge UI and installed-method proofs are subsequent bounded increments.
