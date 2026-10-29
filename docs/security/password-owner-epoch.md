# Password result ownership

Each admitted control connection has a unique internal ID. The counter does not
wrap. Authentication replies address that connection and its request ID. Extra
requests receive a generic denial. They cannot replace a pending password.

The runtime records the connection, request, attempt serial and lock epoch. A
callback must match that whole key before it can retire an attempt. The runtime
joins its finished outer worker before it clears the slot. It checks the original
connection is still admitted and live before it grants unlock. Peer half-close
and EOF checks do not consume queued control bytes.

Manual and idle lock entry advance the epoch once per lock cycle. Successful
unlock advances it too. Trusted sleep signals invalidate it immediately on the
signal thread. Epoch and attempt counters do not wrap. A stale result cannot
clear another attempt or unlock a later lock cycle.

All verified unlock paths use the same transition. It clears input aid ownership,
the idle and control flags, shortcut inhibition and the recovery overlay. The
existing password field and silent failure response remain intact.

## Qualification boundary

The slot tracks the **outer password wrapper**. The existing PAM deadline can
return while its inner worker still runs. This change does not prove PAM worker
retirement or strict backend single-flight. The private PAM helper and its stable
owner are a separate adoption step.

Unit tests use real Unix sockets for connection routing, excess requests,
half-close with unread data, compaction, reconnect, admission changes and counter
overflow. Slot tests cover stale and duplicate results, suspend invalidation and
lock epochs. A required CI fixture runs the actual compositor with an owned
supervised control client and a delayed controlled PAM module (loaded through
libpam_wrapper into the private PAM helper). It checks busy denials, origin loss
waking and retiring the held helper without releasing the module, fresh success
from the held self image, manual lock and idle lock. Its receipt binds source,
binaries and the supervised client's process identity. Rust and runtime results
remain pending until the exact candidate passes CI.

The fixture keeps a 30-second total parent deadline. The module publishes each
numbered attempt's helper PID only after the complete PID is written and then
waits for that attempt's release marker, bounded by the private helper budget
and the outer watchdog. Each cross-process JSON receipt (`success.json`, `epoch-injected.json` and
`sync-result.json`) is written to a sibling temporary file and renamed into
place, so existence signals a complete document. These synchronization rules
do not relax any ownership, lock-epoch or private PAM retirement assertions.

This fixture covers native control IPC under winit and Xvfb. It does not qualify
the GTK lock screen, installed DRM, physical devices or multiple authentication
challenges. Existing GTK unlock and Marlin sleep controls remain required.

## Controlled runtime epoch proof

A second required CI case uses a separate admitted Rust example. It calls the
same private runtime loop on its main thread through a narrow authentication
probe. The normal compositor supplies a no-op probe. It gains no environment,
CLI, configuration or control-wire injection command.

The probe re-executes its own held image as the private PAM helper, like the
compositor binary. While a delayed controlled PAM attempt owns the outer slot,
another connection is denied and the probe verifies that public
`Runtime::try_unlock` rejects another attempt. It advances the same epoch helper
used by the sleep monitor. The stale attempt must deny while its original
connection stays live, and its helper must retire without the module ever being
released to succeed. A fresh connection can then authenticate. After manual
relock, the probe calls the real synchronous public unlock path and checks its
success epoch and lock flags.

This is controlled runtime epoch injection. It is not actual logind notification
or S3, nor an observation of a backend sending a duplicate result. Existing
Marlin sleep gates still qualify that native path. Pure slot tests separately
check mismatched and duplicate callbacks while a newer attempt owns the slot.
The example and test executable require exact Cargo package, target kind,
profile, source path and hash admission before and after execution. Runtime
qualification remains pending until those required exact-head cases pass CI.
