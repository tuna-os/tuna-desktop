# Internal PAM conversation relay

This API verifies authentication. It cannot unlock a session. Its consumer must
hold the common auth Slot. It must check the original connection, request,
serial and lock epoch before it displays a challenge or applies a result.
The lock screen's runtime consumer, shell wire (protocol 0.44) and GTK prompt
are described in [lock-conversations.md](lock-conversations.md).

## API

`Backend::begin_conversation` takes the fixed session user, PAM service, original
`Binding`, explicit `Policy` and exact `Cancellation`. It shares the same owner
thread, backend admission and held executable as the password path. It does not
start a second owner or select an executable from user input.

The returned ticket exposes a bounded challenge queue and a result poll. Each
challenge has its binding, sequence, indexed messages and one response handle.
The message styles are Visible, Secret, Info and Error. Each typed response must
be fresh for its challenge. Info and Error need no text. The relay validates the
whole answer batch before it sends it to the existing Supervisor.

Submission consumes the handle. It rejects another origin, epoch or
sequence. It cancels on abandonment. Owned AnswerText buffers keep the existing
wipe behavior on rejection, receiver loss and drop. This does not prove a wipe of
GTK text, borrowed callers or every decoded schema buffer.

The caller must call `note_user_input` only for real admitted input. Ticks,
redraws, focus changes and backend messages are not input. The relay reserves
space for an answer while it coalesces input notices. Both the exact cancel fd
and an owned socket can wake the stable owner while it waits for a response.

## Independent deadlines

There is no default interactive policy. The method caller must supply a backend
phase and a total ceiling. This source limits the phase to 120 seconds and the
total to between 120 seconds and one hour. These limits need review against each
installed method before a runtime consumer can admit that method.

The inactivity watch is 120 seconds. Only an event from admitted input or a fresh
response with typed text can reset it. An Info/Error acknowledgement cannot reset it. The
inactivity watch does not define the total authentication budget. GNOME uses an
idle watch for its 120-second unlock timeout:
[GNOME Shell 51 unlock dialog](https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/ui/unlockDialog.js).

Each backend read or answer write has a separate phase deadline, clipped to the
idle watch and explicit total ceiling. A result that retires after its phase
cannot authenticate. The child receives the explicit total ceiling; it does
not inherit the old budget of ten seconds for a password for an interactive conversation.
Legacy constructors keep their ten-second limit.

The initial Start write still has a separate bootstrap ceiling of ten seconds,
clipped to the Supervisor-local total deadline, which starts later than the
original Relay/Ticket admission Instant. The supplied backend phase and original
admission deadline do not bound that bootstrap write. Authentication acceptance
still checks the original shared admission deadline after setup. The OS exec
handshake also has no portable hard deadline. A later consumer must admit these
residual startup limits before it enables a method.

Each try at physical cleanup retains the existing 500 ms budget. Failure to
retire leaves the Supervisor on the same creator thread in quarantine. Repeated
tries at cleanup do not imply a finite bound on OS termination. Cancellation,
UI destruction and a returned denial never free backend admission. Only actual
Retired can free it; an owner with poison cannot accept a new job.

## Compatibility and tests

The single-password PAM adapter still sends at most one secret. The unlock
conversation adapter also sends at most one ordinary Secret answer. It
cancels on another Secret or any Visible question. It cannot reuse a password
as a PIN or token. The interactive relay uses styles and fresh answers; it does
not use English text to guess whether a challenge is a password or PIN.

The tests cover limits on policies, independent deadlines and distinct
answers. Other controls cover Info/Error, wrong bindings and indices, idle
expiry, abandoned handles, bounded queues and the actual cancel fd.

The existing required test for the PAM helper adds controlled relay cases.
It uses the admitted compositor executable for two rounds with different
answers. Controls include a wrong second answer, wrong origin and sequence,
cancellation, shutdown and a backend hang. They also check the stable creator and actual
helper reap before Retired. The tests observe the owned thread in its blocked
poll before they signal cancellation or shutdown. Its original 45-second watchdog remains in place.

These tests have not run locally. CI must execute them. Six existing source contracts pass in Python. They check source only. They do
not qualify runtime acceptance. The
controlled PAM stack does not qualify installed services, real logind sleep,
fingerprint hardware, smartcards, web auth, credential storage or full issue #366.

## Consumer

`crates/compositor/src/runtime/auth.rs` binds each ticket to the common Slot
(`MethodKind::PamConversation`) and retires or quarantines that Slot from the
actual backend outcome. It cancels on origin loss, lock/suspend epoch changes
and shell cancellation, and rechecks the binding before every prompt and the
grant. Protocol 0.44 carries distinct begin/query/answer/cancel messages over
the existing NoReplay writer; see [lock-conversations.md](lock-conversations.md).
