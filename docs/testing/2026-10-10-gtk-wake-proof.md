# GTK event wake regression

The source change removes the permanent 16ms full reconciliation callback.
Control IN/HUP/ERR sources drain synchronously, buffered continuations are
coalesced, accepted output retains exact partial offsets and registers OUT only
while pending, and local cache/policy/actual geometry events request work.
The switcher's genuine half-second delayed popup uses a cancellable deadline.
This does not establish whole-session CPU, GNOME parity, native GPU or power.

## Frozen native check

`dbus-run-session -- /usr/bin/python3 scripts/tuna-gtk-wake-proof --bin-dir "$bundle" --artifacts "$artifacts" --display :108 --trace-idle`

The bundle must contain matched compositor, GTK shell and `tuna-modal-dim-client`
executables and a manifest with clean source revision and their SHA256 hashes.
The runner verifies frozen and live executable identities and records proof
source identity separately. Use a private unused display. Dependencies are
Xvfb, xdotool, scrot, strace, PyGObject, Pillow, D-Bus, AT-SPI and the same native
runtime libraries as the existing nested GTK proofs. Missing host settings keys
are explicit fixture extensions, not installed Settings panel qualification.

The runner checks:

- Real Alt-Tab with only Alt held reveals four actual opaque red/blue client
  clones after the popup deadline; the first response has no clones.
- Accepted frame count/geometry alone never qualifies visually complete content.
  Fresh private framebuffer pixels and a later genuine render are required.
- Real host resize1820/960/1280, with no further key or manual clock advance,
  presents six/three/four actual clones. This exercises GTK allocation and paint
  wakeups and publication of current visible geometry.
- Down plus five Right events followed by Alt release eventually acknowledge
  real window1 focus with no thumbnail frames or held keys. Empty frames alone
  can precede activation acknowledgment and therefore cannot finish the gate.
- The owned GTK control fd is identified from its exact private connect path.
  Initial empty and post-switcher-close five-second syscall phases reject more
  than ten read attempts each; this detects the old repeated empty polling.
  It is a scoped trace threshold, not a zero-wakeup or CPU claim.
- Own accessibility pidfds are stopped and private runtime removed. Failed gates
  retain evidence and cannot write a successful assertions receipt.

## Negative control retained locally

Frozen baseline921 (`921b3e5d08edd47644fa49b0d63bcae465e3bd10`) reproduced275
control read attempts (261 complete EAGAIN) during5.0002s initial empty and255
attempts (227 complete EAGAIN) during5.0013s post-close. Unfinished calls were
reported separately14/28. The actual popup pixels, resized clones and eventual
focus acknowledgment passed. `--expect-periodic-poll` explicitly tests this old
behavior as a negative control; it must not be used to qualify the new source.

This was a shared host, llvmpipe/Cairo nested session under strace. Tracing,
software rendering and screenshot observer delays affect rates and elapsed
observations. The old popup count arrived before actual visible clones; the
runner retains original input start and later observed completion separately.
No physical input latency or resource comparison is inferred from these runs.

## Remaining gates

Fresh source-owned full tests/Clippy and matched candidate native execution must
pass before this change is qualified. CI is preferred for full subsequent
builds. The existing local queued immutable b883 build remains independent of
the weak signal ownership followup. Native policy/cache reconciling has focused
source tests; complete installed Settings, multiple output, all input/device
paths, physical paired GNOME CPU/GPU/power qualification remain separate.
