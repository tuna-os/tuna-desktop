# ADR 0008: v1 beta track and v1 release scope

- Status: Decided
- Decision date: 2026-10-15 (items 3 and 4: 2026-10-10 and 2026-10-13; item 6: 2026-11-16)
- Owner: project (the owner delegated these decisions to the coordinating agent on 2026-10-15)
- Related: epic #577, v1 definition (`docs/v1.md`), [ADR 0007](0007-gnome51-marlin-baseline.md), #664 prerelease versioning

## Context

The v1 bar in `docs/v1.md` covers every GNOME 51 capability verified, responsiveness better than GNOME 51, and a qualified shipped image. It is weeks of work away. No installable release has ever been tagged. The roadmap's change control requires an ADR to move scope between the developer preview, 1.0 and later.

## Decision

1. **Beta track.** Ship `v1.0.0-beta.N` prereleases from `main` before v1, using the existing release runbook (`docs/release.md`, prerelease section). A beta needs a green `main` CI at the tagged commit, including nested stress, plus a filled visual review and release notes listing every parity-ledger row that is not `pass` as a known limitation. A beta never claims a v1 gate. The first beta is tagged from `main` as soon as CI is reliably green; it does not wait for integration PR #654.
2. **All TunaOS variants.** Tuna Desktop v1 ships as a flavor of every TunaOS variant (14 today), not only Marlin. This supersedes the "Marlin image flavor only" constraint of the v1 definition. Marlin stays the GNOME 51 comparison target from ADR 0007, so performance and parity are measured on Marlin only. Every other variant qualifies on install, real-greeter login, session, portals and lock/unlock from its own image digest. A variant that cannot qualify is published as unsupported with a recorded reason.
3. **GPU and total-power comparisons are waived for v1** (owner decision, 2026-10-13). v1 requires faster matched latency than GNOME 51 and guest CPU and memory no worse. GPU and total-power measurement is follow-up work, and no claim of being better on GPU or power is made until it is measured.
4. **Scope boundaries** (owner decisions, 2026-10-10): noise removal is v2. A webcam overlay with microphone recording for the screen recorder is a proposed extension and is not in v1.
5. **Rows that can only be proven on physical devices** (touchscreens and orientation sensors P-DS-04, graphics tablets P-IN-05, colour management/HDR P-DS-03, native DRM lease and explicit sync P-SY-09, fingerprint and smartcard P-LK-02, modems in P-ST-10). The behavior still has to be implemented for v1. A row may pass on evidence from emulated devices (QEMU/uinput virtual tablet, touch, sensor, fprintd/smartcard test doubles) when its ledger notes name the emulation. Physical-device qualification of those rows moves to the post-v1 hardware support matrix that ADR 0007 already leaves open.
6. **Tuna Desktop's own login screen** (owner decision, 2026-11-16). v1 replaces GDM's greeter, `gnome-shell --mode=gdm`, with a Tuna greeter: the Tuna compositor in a greeter mode and a GTK4/libadwaita greeter client that use GDM's greeter D-Bus API. GDM's daemon stays and still owns PAM and its conversations, seats, VT handoff, user switching and session start. The greeter must match GDM 51's login screen in these areas:
   - the user list and password login;
   - fingerprint, smartcard and multi-step authentication through GDM's conversations;
   - Orca, the on-screen keyboard, the other accessibility settings, input sources and translations before login;
   - user switching;
   - GDM's policy keys (banner, hidden user list, disabled restart buttons, allowed authentication methods).

   The rollout replaces the GNOME Shell greeter outright on every variant; images keep no GNOME Shell greeter as a fallback. This supersedes the roadmap's post-1.0 "GDM replacement" backlog item and the earlier choice of stock GDM for v1. Tracked in epic #1164 and ledger row P-LK-03.

## Alternatives

- **Wait for full v1 before any release.** Rejected: nothing would be installable for weeks, and the packaging, image and rollout pipeline would go untested until the end.
- **Version the betas as 0.x.** Rejected: the work targets v1, and SemVer prereleases (`1.0.0-beta.N`) order correctly before `1.0.0` in dpkg, pacman and RPM.
- **Defer the hardware-only rows entirely.** Rejected: it would remove user-visible capabilities from v1. Only the physical proof moves.
- **Keep stock GDM's GNOME Shell greeter for v1.** Rejected by the owner on 2026-11-16: the login screen is the first thing a user sees, and it should be Tuna Desktop's.
- **Write a full display manager that replaces GDM's daemon.** Rejected: PAM, seat, VT and session handoff are security-critical. GDM already does them, and its greeter API lets another greeter reuse them.
- **Keep the GNOME Shell greeter installed as a fallback.** Rejected by the owner: the rollout replaces it. Recovery when the greeter fails relies on GDM's own greeter restart and on text-console login. The qualification gates must show the Tuna greeter starting on every boot of every variant.

## Consequences

- `docs/v1.md` and the computed victory signal keep the full v1 bar, and the betas do not change it.
- The package factory recipe and the image flavors are pinned to beta tags. The all-variant rollout starts from the first beta.
- The rows in item 5 need emulated-device proofs. Their ledger notes must say "emulated" until physical evidence exists.
- Item 6 adds release gates. The greeter must pass the VM lane's login gates (V-GREETER-LIST, V-GDM-LOGIN and the rest) and new gates for each area named in item 6, on every variant's image digest. It must also show greeter start time and memory no worse than the GNOME Shell greeter on Marlin.
- A greeter failure means no graphical login, so greeter crash and restart behaviour is part of the recovery gate.
