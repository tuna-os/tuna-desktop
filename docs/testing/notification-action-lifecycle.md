# Notification action lifecycle proof

The ignored GTK unit test
`notify::gtk_action_lifecycle_tests::actual_gtk_captured_actions_obey_resident_lifecycle`
constructs real `NotifyUi` widgets with an in-memory `NotificationCenter`. It
retains the actual action buttons and emits their `clicked` signals after Take,
banner close, programmatic dismissal, replacement, actual timeout `tick`, history
close, and Clear. Local counters must remain zero after retirement, while Take
runs once and a fresh resident still works. External notices keep their
per-action, single-use history behavior after banner dismissal.

Run the source-matched GTK test executable in an owned Xvfb and private D-Bus
session after admitting its Cargo JSON build identity. For example, with the
already admitted GTK test binary:

```sh
env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET dbus-run-session -- xvfb-run -a \
  env TUNA_REQUIRE_NOTIFY_UI=1 "$GTK_TEST_BINARY" \
  notify::gtk_action_lifecycle_tests::actual_gtk_captured_actions_obey_resident_lifecycle \
  --exact --ignored --test-threads=1 --nocapture
```

Admission must retain the source commit, Cargo JSON test-artifact executable and
SHA-256, invocation, output, and exit status. Missing display or the explicit test
flag fails the test. `scripts/tests/test_notify_callback_contract.py` only checks
the source contract; it does not execute GTK or qualify the lifecycle.

Scope: real GTK signal handlers and the native notification store on Xvfb. This
is not pointer delivery, installed layer-shell/compositor, D-Bus sender-signal,
production wellbeing timer, or hardware acceptance. Production construction keeps
the persisted notification center and layer-shell banner window. Only the test
fixture selects an ordinary GTK window and memory-only store.

Clear removes notification history, so captured old buttons are rejected by the
center and cannot fire locally. Clear currently retains unused local callback
entries until explicit retirement; this test does not claim those entries are
removed. Other terminal paths explicitly retire the local resident.
