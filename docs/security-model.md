# Security model and threat model

This is the entry point for security review of Tuna Desktop. It collects
the trust boundaries, the properties the design enforces and the gaps that
remain. Component deep dives are linked from each section. Report
vulnerabilities through [SECURITY.md](../SECURITY.md), not public issues.

**Status.** Tuna Desktop is a developer preview. The architecture's first
delivery tier (nested developer preview) is explicitly "not a secure
daily-driver session" ([architecture](architecture.md#delivery-tiers)). The
[ADR register](adr/README.md#decision-register) still lists the same-UID
threat model, trusted lock/portal identity and the production lock/auth and
portal/capture grant lifecycle as open 1.0 release blockers. Everything
below describes what the code on `main` does today. Planned behaviour is
labelled **target**.

## What is protected

- **Screen contents.** Pixels of other applications, through capture,
  screenshots, screen casts and remote desktop.
- **Input.** Keystrokes and pointer events belong to the focused client.
  Nothing may inject input for another client without consent.
- **The lock.** While the session is locked, no application surface is
  shown or receives input, and only the session user's own credentials
  unlock it.
- **Window metadata.** Window titles and application identities are
  private. They are published only to the portal's picker.
- **Shell authority.** Commands sent over the shell control socket can
  focus or close windows, drive the overview and engage the lock. Only the
  supervised shell may hold a session on that socket.

## Adversaries in scope

1. **Ordinary Wayland clients**, sandboxed or not. They may send arbitrary
   protocol requests, claim any app ID, title or layer-shell namespace, and
   connect and disconnect at will. They must not capture the screen, type
   into other clients, see other windows' contents or get past the lock.
2. **Session-bus callers.** Any same-user process on the session bus may
   call the compositor's and shell's D-Bus interfaces, and may claim
   well-known names that nobody owns yet. Capture, remote input and window
   introspection must not be granted on the strength of a claimed name.
3. **Processes of other users.** They must not reach the control socket or
   any per-session state.
4. **A person at a locked screen.** They can type and click on the lock
   surface. Lock failure must never expose application surfaces or send
   input to them.
5. **Hostile extension scripts** (see [extensions](extensions.md)). They must
   not reach the filesystem, network, processes, input or window contents.
6. **A crashing, hung or malformed shell.** Shell faults must not take
   applications down or block the compositor. A malformed control stream
   must not crash or stall the compositor.

## Out of scope

- **Same-UID processes that can debug or rewrite the shell or compositor.**
  ADR 0005 records that a process able to `ptrace` the shell "still wins;
  that is the platform's same-uid boundary, not something a socket can fix"
  ([ADR 0005](adr/0005-control-socket-peer-authentication.md#consequences)).
  The same applies to a same-user process that edits the user's
  configuration, autostart entries or extension directory.
- **root, the kernel, GPU drivers and firmware.** Executable-identity
  checks trust root-owned, non-writable installed binaries (see below), so
  a root compromise defeats them by design.
- **Platform services**: GDM, logind, PAM and its modules, polkit and its
  setuid helper, NetworkManager, BlueZ, fprintd, PipeWire,
  xdg-desktop-portal and the GNOME portal backend. Tuna Desktop
  authenticates who it talks to but does not defend against these
  components misbehaving.
- **Physical attacks beyond the lock screen**: DMA, cold boot and similar.
- **Network attackers.** The compositor and shell expose no network
  listener. Network-facing remote desktop belongs to the portal/daemon
  stack; see [Remote desktop](remote-desktop.md).
- **GNOME Shell extensions and private Mutter APIs.** They are not loaded or
  supported ([architecture](architecture.md#product-decision)).

## Components and trust boundaries

| Component | Trusted for | How the compositor recognises it | Code |
|---|---|---|---|
| Kernel, logind, libseat | Device access, session activity | Platform | `crates/compositor/src/drm.rs`, `logind_session.rs` |
| GDM | Login authentication; starts `tuna-session` from `share/wayland-sessions/tuna.desktop` | Platform. Since #866 GDM is the only supported login path. Tuna Desktop ships no greeter. | `share/wayland-sessions/tuna.desktop` |
| `tuna-compositor` | Everything below. Owns focus, stacking, input routing, the lock flag, capture authorization and privileged protocol exposure ([architecture](architecture.md#process-and-authority-boundaries)) | The trusted computing base of the session | `crates/compositor` |
| Private PAM helper | Running the `tuna-lock` PAM transaction | Re-executes the compositor's own held image. It never takes an executable path from UI, environment or configuration ([private PAM helper](security/private-pam-helper.md)). | `crates/compositor/src/pam_helper.rs` |
| Supervised shell (`tuna-shell-gtk` by default, legacy `tuna-shell-host`) | UI intent, lock surfaces, the shell's own capture UI | PID of the compositor's own shell child (`Authority::publish`, `runtime.rs:4881-4890`). Between restarts no process is trusted. | `crates/shell-gtk`, `crates/shell-host` |
| Compositor's IBus bridge | Typing for other clients (virtual keyboard) | A Wayland connection the compositor inserts itself with the `ime_bridge` flag (`ime.rs:360`, `lib.rs:321-327`) | `crates/compositor/src/ime.rs` |
| GNOME portal backend (`xdg-desktop-portal-gnome`) | Consent broker for ScreenCast, Screenshot and RemoteDesktop | Caller's bus-authenticated PID resolves to a root-owned, non-group/world-writable installed executable, and the caller currently owns `org.freedesktop.impl.portal.desktop.gnome` (`capture_security.rs:176-277`) | `crates/compositor/src/capture_security.rs` |
| GNOME providers (Nautilus file chooser, global-shortcuts provider) | Opening an ordinary display connection, and `mutter_x11_interop` | Same installed-executable check for the requested service type only (`capture_security.rs:9-40`). Confers no capture or input authority ([service channel](portal-service-channel.md)). | `crates/compositor/src/mutter.rs:198-293`, `x11_interop.rs` |
| Applications | Nothing beyond their own surfaces | Ordinary clients. Discovered Flatpak/Snap identity is metadata only ([service channel](portal-service-channel.md#optional-gnome-sandbox-identities-410)). | `crates/compositor/src/sandbox_identity.rs` |
| Extension scripts (current) | A bar cell, a dock badge and notes in the legacy shell | In-process Rhai inside `tuna-shell-host` | `crates/shell-host/src/extensions.rs` |
| Extensions (**target**) | Declared capabilities only | Separate processes behind a capability broker. Not implemented. | [extensions](extensions.md#target-architecture) |

### Shell control socket

The compositor and shell talk over a dedicated Unix socket with a
versioned, length-prefixed postcard protocol
([ADR 0002](adr/0002-shell-control-ipc-envelope.md),
`crates/shell-control-schema`). [ADR 0005](adr/0005-control-socket-peer-authentication.md)
sets three layers of protection. First, the socket exists only in a private
`XDG_RUNTIME_DIR` with mode `0600`. Second, `SO_PEERCRED` is read before
any state is sent, and other UIDs are refused. Third, once supervising, the
compositor admits only the supervised child's PID, with at most eight
pending peers (`crates/compositor/src/control.rs:1602-1660`,
`1785-1803`, `2297`). Decoding rejects frames over 1 MiB before postcard
runs, and also rejects trailing bytes, over-long titles, oversized
collections and a foreign major version
(`crates/shell-control-schema/src/lib.rs:1965-2015`). Evidence:
`crates/compositor/tests/peer_auth.rs`, `handshake.rs`, `control.rs`.

The shell is trusted for intent, not authority. It cannot unlock by
itself. A client's `unlock_and_destroy` never unlocks the session: the
compositor unlocks only after PAM verifies the password and the
compositor clears its own flag (`crates/compositor/src/session_lock.rs:1-11`).

### Lock and authentication

Lock state lives in the compositor. Only the supervised shell's lock
request is approved, and only the approved client's lock surfaces are shown
(`session_lock.rs:58-110`). Engaging the lock does four things: it cancels
pending accelerators, stops held remote-desktop input, publishes the
locked state to the capture authority and closes the overview
(`runtime.rs:1661-1687`). Unlock runs the session user's own PAM
transaction against the `tuna-lock` service (`share/pam.d/tuna-lock`) in a
private helper. Attempts are bound to the control connection, request,
attempt serial and lock epoch. See:

- [Lock-screen authentication conversations](security/lock-conversations.md)
- [Password result ownership](security/password-owner-epoch.md)
- [PAM conversation relay](security/pam-conversation-relay.md)
- [Private PAM helper](security/private-pam-helper.md)
- [Fingerprint prompt guards](security/fingerprint-prompt-guards.md)
- [Authentication consumer foundation](security/auth-consumer-foundation.md)

### Capture, screenshots and remote desktop

Only the supervised shell or the authenticated installed portal backend
may create ScreenCast sessions, take screenshots or create RemoteDesktop
sessions (`Authority::authenticate_caller`,
`capture_security.rs:220-245`). Claiming the portal's bus name alone is not
enough. Sessions belong to their unique creator, are single-use, and are
bounded at 256 sessions and 64 streams per session. Revocation is
permanent. A 100 ms scan on the D-Bus thread revokes a grant when its
owner disconnects, the trusted portal name changes hands, the shell PID
changes or the session locks (`mutter.rs:1339-1392`). Capture paths also
refuse locked frames. Full detail and the qualification evidence:
[capture admission and revocation](capture-security.md),
[remote desktop](remote-desktop.md),
[screenshot consent](portal-screenshot-consent.md).

### Privileged Wayland globals

- `zwp_virtual_keyboard_manager_v1` is visible only to the compositor's IBus
  bridge (`protocols.rs:171-181`).
- `mutter_x11_interop` is visible only to typed service-channel
  connections (`x11_interop.rs:65-67`).
- `ext_session_lock_manager_v1` is advertised to every client, but a lock
  is granted only to the supervised shell's PID (`session_lock.rs:63-82`).
- Input-method v2 is open to any client, as on GNOME (`protocols.rs:165-166`).

### Session-bus interfaces

Each interface checks its caller itself. The main policies:

| Interface | Served by | Caller policy |
|---|---|---|
| `org.gnome.Mutter.ScreenCast`, `.RemoteDesktop`, `org.gnome.Shell.Screenshot`, `org.tuna.Screenshot` | compositor | Shell PID or authenticated installed portal. Denied while locked. |
| `org.gnome.Mutter.ServiceChannel` | compositor | Same-user caller. Typed connections require the installed provider for that type. |
| `org.gnome.Shell.Introspect` | compositor | Portal only. `TUNA_INTROSPECT_UNRESTRICTED=1` (test/development "unsafe mode") allows metadata only while unlocked (`introspect.rs:1-23`). |
| `org.gnome.Shell` (OSD, search, app grid) | GTK shell | Caller must own `org.gnome.Settings`, `org.gnome.SettingsDaemon.MediaKeys` or the GNOME portal name, as GNOME's `DBusSenderChecker`. `TUNA_SHELL_DBUS_UNRESTRICTED=1` disables this for harnesses (`shell-gtk/src/shell_dbus.rs:25-29`, `372-376`). |
| `org.gnome.Shell.NetworkAgent` (system bus) | GTK shell | Only NetworkManager's current unique owner ([network agent](network-agent.md)). |
| `org.freedesktop.impl.portal.Access` | shell (`org.freedesktop.impl.portal.desktop.tuna`) | Always answers "cancelled", so the requester is refused (`shell-host/src/lockdown.rs:66-83`). |

Neither `UNRESTRICTED` variable is set by the packaged session
(`share/wayland-sessions/tuna.desktop`, `crates/compositor/src/bin/tuna-session.rs`).

## Current state compared with the target

| Area | Today on `main` | Target |
|---|---|---|
| Extensions | Rhai scripts run in-process in the legacy `tuna-shell-host` only. The default GTK shell loads none. Containment is a language sandbox with an operation budget. | Separate, untrusted processes behind a capability broker, with no privileged Wayland socket, native plugin ABI, raw input, clipboard, capture, filesystem or network by default ([architecture](architecture.md#process-and-authority-boundaries), [extensions](extensions.md)) |
| Shell identity | PID of the compositor's child, checked on the control socket and for capture | Inherited pre-connected socketpair or another production credential, deferred to spek 004 ([ADR 0005](adr/0005-control-socket-peer-authentication.md#alternatives-considered)) |
| Shell supervision | Compositor-owned child with a finite restart budget and capped backoff ([ADR 0003](adr/0003-nested-supervision-and-recovery.md), `supervise.rs`) | Production supervision and identity are a 004 gate |
| Activation tokens | Compositor-minted, one-use, 30 s, seat and app-ID bound ([ADR 0002](adr/0002-shell-control-ipc-envelope.md)) | Binding to `xdg_activation` external tokens deferred to 004 |
| Portal grants | Consent by the GNOME backend; compositor-enforced, revocable grants (above) | Complete grant lifecycle reviewed; open release blocker in the ADR register |

## Known limitations

- **Same-UID boundary.** PID-based shell identity and executable-identity
  checks stop name spoofing. They do not stop a same-user process that can
  `ptrace` or inject into the shell or portal.
- **Name-based allowlists.** The `org.gnome.Shell` allowlist trusts
  whoever owns an allowlisted well-known name, as GNOME does. A same-user
  process that claims `org.gnome.Settings` first passes it. Those methods
  only show OSDs and open search or the app grid.
- **Layer shell is open.** `zwlr_layer_shell_v1` is created without a client
  filter (`crates/compositor/src/lib.rs:999`). The compositor identifies
  shell surfaces by their client-chosen namespace, which names a role, not
  an owner (`layer.rs:492-510`). The R14 "unauthorized layer" cases are not
  qualified.
- **Extension sandbox is in-process.** The Rhai engine sets an operation
  budget but no string, array or memory limits (`extensions.rs:103-146`), so
  a script can exhaust the legacy shell host's memory. A Rhai engine bug is
  a shell host bug. The compositor is unaffected: the shell is a separate,
  supervised process.
- **Revocation latency** is the 100 ms scan plus one compositor tick. This is
  not a hard real-time bound under CPU starvation
  ([capture security](capture-security.md)).
- **Hardware.** Qualification covers the Marlin VM, vkms and CI. The
  physical GPU/support matrix is open
  ([architecture](architecture.md#decisions-still-open)).
- **Sandboxed app identity** is best-effort metadata, unqualified for
  shipped Flatpak/Snap processes, and X11 windows have none
  ([service channel](portal-service-channel.md#optional-gnome-sandbox-identities-410)).

## Release gates

These must pass before the security claims above can be called release
quality:

- The architecture's cross-cutting gates: apps survive shell and extension
  crashes, lock failure never exposes surfaces or redirects input, and
  capture and remote input use portal consent with compositor-validated
  authorization ([architecture](architecture.md#cross-cutting-release-gates)).
- Roadmap decision gate 4: the threat model assigns trusted identities and
  lock/auth/portal/capture authority end to end, and specialist review
  leaves no unresolved critical issue. Gate 5: before 1.0, no critical
  lock/capture/input/clipboard/IME regression. Gate 6: before public
  extensions, broker containment criteria pass
  ([roadmap](roadmap.md#global-decision-gates)).
- Requirements R6 (session/lock), R11 (extension boundary) and R14
  (capture/privileged surfaces) with their security cases
  ([requirements](requirements.md)).
- The open ADR register rows for same-UID identity and the production
  lock/auth and portal/capture lifecycle
  ([ADR register](adr/README.md#decision-register)).

The `portal-security` and `remote-security` CI lanes run the packaged
compositor and GTK shell against the installed GNOME portal stack
(`scripts/tuna-portal-security`, [testing](testing.md)). Their evidence
manifests are [capture-evidence.json](capture-evidence.json) and
[capture-security-evidence.json](capture-security-evidence.json).

## Related documents

- [Program architecture](architecture.md)
- [Portal and D-Bus service integration](portal-service-integration.md)
- [Extensions](extensions.md)
- [Privacy device enforcement](privacy-enforcement.md)
- [ADR 0002](adr/0002-shell-control-ipc-envelope.md),
  [ADR 0003](adr/0003-nested-supervision-and-recovery.md),
  [ADR 0005](adr/0005-control-socket-peer-authentication.md)
