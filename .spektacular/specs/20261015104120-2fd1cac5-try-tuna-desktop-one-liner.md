---
created_date: "2026-10-15"
document_status: final
closed_date: "2026-10-15"
---

# Feature: 20261015104120-2fd1cac5-try-tuna-desktop-one-liner

## Overview

Trying Tuna Desktop today means installing a TunaOS image. A single copy-paste command on Linux or macOS downloads Corral, boots a published Tuna image in QEMU and opens the desktop in the browser over noVNC, so anyone can evaluate Tuna in minutes without touching their machine.


## Requirements

- [ ] **One command**
  Users run one documented command on Linux (x86_64) or macOS (Apple Silicon and Intel) and end with Tuna Desktop in a browser tab.
- [ ] **Published image only**
  The command boots an immutable, digest-pinned published Tuna image, never an untagged build.
- [ ] **Local-only access**
  The noVNC endpoint is reachable only from the local machine and rejects foreign browser Origins.
- [ ] **Clean stop**
  Users can stop the VM with one command or Ctrl-C, and no QEMU process or disk lock remains.
- [ ] **Clear prerequisites**
  Missing QEMU/virtualization support is reported with the fix, not a stack trace.


## Constraints

- Must not require root beyond what QEMU acceleration needs (KVM group on Linux / Hypervisor.framework on macOS).
- The download must be verified (checksum or signature) before execution, aborting on mismatch.


## Acceptance Criteria

- [ ] **Linux run**
  On a fresh Linux host with QEMU, the command opens a browser session showing the Tuna greeter or session.
- [ ] **macOS run**
  Same on macOS.
- [ ] **Origin check**
  A WebSocket upgrade from a foreign Origin is refused.
- [ ] **Stop leaves nothing**
  After stop, no QEMU process, lock or listening port remains.


## Technical Approach

- Build on Corral's QEMU backend and existing noVNC/websocket support; the script is a thin bootstrapper.
- Image reference comes from the release manifest of the current tagged release.


## Success Metrics

- No success metrics defined beyond the acceptance criteria.


## Non-Goals

- Persistent installs.
- GPU acceleration.
- Windows hosts.
