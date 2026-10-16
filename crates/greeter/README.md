# tuna-greeter

The Tuna Desktop login greeter for greetd: a prompt state machine, installed
session enumeration, a greetd JSON-IPC client, and a GTK4/libadwaita login
window rendered from the model. No password-specific flow exists anywhere
in the crate; it follows whatever conversation greetd drives.

## Binary

`tuna-greeter` (`src/main.rs`), built only with the `gtk-ui` feature
(on by default).

## Public API (`src/lib.rs`)

- `model`: `GreeterModel`, the prompt state machine that turns greetd's
  auth conversation into UI state.
- `session`: parse installed `.desktop` session files
  (`enumerate_system`).
- `client`: blocking greetd conversation driver over a Unix socket.
- `ui`: the login window (feature `gtk-ui`).

Feature `gtk-ui` (default) pulls in GTK4 and libadwaita. Consumers that
need only the model and client build with `default-features = false` and
need no GTK system libraries.

## Dependents

`tuna-compositor` uses `model` and `client` for its greetd unlock path,
with `gtk-ui` off. Tests in `tests/` drive a fake greetd.

## Docs

[Greeter VM acceptance](../../docs/greeter-vm-acceptance.md),
[installing Tuna Desktop](../../docs/install.md),
[architecture](../../docs/architecture.md).

## Live login controller

The binary owns `login::LoginWindow` until application shutdown. The GTK username,
selected session, Sign in/Enter and Cancel/Escape controls drive the same
`controller::Controller` used by the prompt tests. Socket exchanges execute on
Gio workers; GTK receives completion wakeups rather than polling. Closing the
window interrupts the current socket exchange. Only a successful **StartSession
reply** exits the greeter for greetd handoff. Cancel replies are consumed before
another attempt, and rejected starts/transport failures remain generic and
retryable. The production image still uses gtkgreet until installed authentication
and session launch are qualified.

Responses have a 256 KiB frame limit and a 30 second total read deadline per
exchange (writes: five seconds). Informational prompt loops are capped at 64
responses per attempt. Cancel while an exchange is pending queues cancellation
behind that response; a silent daemon times out instead of blocking GTK. These
bounds may need adjustment for an installed long-running authentication provider;
they do not establish fingerprint/smartcard/web-login parity.

CI runs `scripts/tuna-greeter-proof "$RUNNER_TEMP/native-greeter"`. It freezes the
Cargo-reported GTK test executable and source identities outside the checkout,
then uses Xvfb, actual GTK widgets and a Unix-socket fake greetd with the real IPC
codec. Captures and failure receipts cover masked/visible prompts, info/error
acknowledgement, multi-step authentication, failure, idle/pending cancellation,
retry, transport reconnection and rejected/successful start replies. This fixture
proves controller/UI integration; it does not execute PAM, log into an installed
TunaOS image or authorize replacing its production greeter. The IPC behavior is
specified by [greetd-ipc(7)](https://man.archlinux.org/man/greetd-ipc.7).

Existing PR #487 extracts the shared client/model modules without UI wiring;
reconcile its imports when integrating that extraction rather than duplicating
its shared-schema package.
