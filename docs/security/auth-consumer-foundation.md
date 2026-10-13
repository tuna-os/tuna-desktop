# Authentication consumer foundation

## Scope

This change adds the runtime interfaces for a PAM conversation consumer. It does
not start an interactive conversation in Runtime or add a shell message or GTK
prompt. The existing password route remains the only runtime consumer here.
Wire and UI changes must follow this foundation.

## Owned notifications

The ticket has a source of type `calloop::ping::PingSource`. The caller can take it once.
The backend publishes a challenge or result before it signals that source. The
reply publisher closes its sender before it signals a disconnect. A wake tells
the caller to read the queue. It does not prove authentication or retirement.

The ticket exposes the current shared idle and total deadline. The caller must
schedule that deadline in its loop. Accepted user input changes the shared idle
clock and signals the source. An expired deadline remains due until the caller
denies the request. A canceled or completed ticket has no input deadline. Its
owner must retain a canceled ticket until a result proves retirement, or keep
its slot in quarantine.

## Method ownership

The common `Slot` records the exact `Attempt` and method. All method tags use one
slot. An active method blocks a grant and admission of another method. The
legacy wrapper release applies only to Password. Release of a PAM method needs a result that allows retirement,
the exact `Attempt`, and the exact method.

The Fingerprint tag has no retirement consumer in this change. Neither a PAM result nor a match from hardware can release that tag. A later fingerprint consumer must
provide its own owned retirement path. Parallel methods must retain all their
children through retirement. This change does not add fallback.

## Shell ownership

The shell supervisor assigns a generation at each spawn admission. Failed
spawns consume a generation. Budget resets do not reset it. Overflow closes
authentication authority. The runtime publishes the held child PID and
generation before it polls control and after supervision.

A changed child identity closes old control connections. A new connection needs
Hello and a snapshot. Its authentication origin must match the credentials from the kernel, current child, connection, and request.

A lease binds that origin to the serial of the `Attempt`, lock epoch, method, and child
generation. A command denial can end reply correlation but does not release the
lease or the common `Slot`. Cancellation and EOF do not prove retirement.
Replacement of a child does not prove retirement. A lease from a prior child does not release a newer lease.

## Evidence and remaining work

The source includes real Unix socket, calloop, and child process regressions.
The required PAM fixture uses the actual compositor helper and an owned event
source. Its first positive case uses an explicit controlled policy: 30 seconds
per backend phase, 900 seconds total, and 120 seconds input inactivity. The other phases of the fixture retain their existing limits. This profile is not a
production default or installed service admission.

Python controls and checks from `rustfmt` are source evidence. Rust tests
and the PAM fixture need execution in CI. They do not prove GTK prompts or input to the lock surface in a native session.
They do not prove the installed services for PAM, hardware, login, a switch of users,
suspend, or behavior of the key store.

The next consumer must bind each wire response to its original connection,
request, `Attempt`, epoch, batch sequence, and message indices. Each answer needs
fresh input. It must retain NoReplay transport and owned secret buffers. Only input from an admitted user event may reset inactivity.

The final unlock still needs
the live origin, exact common Slot, method retirement, current lock epoch, and
atomic grant check. 
A GTK surface closes only on an authoritative unlocked snapshot. No schema version changes are made here.
