# Published GDM preflight prototype

This is an **unwired, review-stage static helper**. It has not been run against
the complete published Marlin filesystem. Passing its synthetic tests does not
establish real-image compatibility, password authentication, a visible desktop,
or release qualification.

## Explicit contracts

`published-gdm` requires the shipped GDM service, Tuna Wayland session entry,
installed ALPM ownership, effective PAM include/module closure, and protected
payload/configuration hashes. It rejects automatic/timed login, unreviewed GDM
service overrides, additional GDM aliases, competing enabled managers, and
ambiguous policy syntax. Residual `/etc/greetd/environments` does not select a
backend.
The installed, GDM-owned `/usr/share/gdm/gdm.schemas` is hashed and must provide
unambiguous boolean-false defaults for both login-enable keys; omitted keys in
`custom.conf` do not silently assume those defaults.

`current-greetd` is a separate **pure session contract only**. It retains the
current lane's greetd PAM service and VT 1 assumption. No existing current-build
or published-image harness behavior changes in this patch.

`validate_session()` consumes one caller-observed logind/process tuple. For GDM
it takes the positive VT from that tuple instead of assuming VT 1. The caller
must actually join the compositor PID to its logind session, retain the returned
session/UID/VT/PID/start-ticks identity, and compare it with later observations.
If observations can cross boots, compare a separately collected boot ID too.
Validating each sample independently does not prove continuity. Neither this
function nor static inventory proves password login, PAM success, picker
selection, graphics, logout, or relogin.

## Invocation boundary

The entry point is `scripts/tuna-published-dm-inventory`. It needs Python 3 and
the adjacent `lib/published_dm_contract.py`. Its only subprocesses are absolute
`/usr/bin/pacman -Q` and `-Qqo -- /absolute/path` calls, with a 15-second timeout.
It reads the current `/`; there is deliberately no alternate-root CLI whose
filesystem could disagree with the package database being queried.

The future orchestrator must run this trusted helper in a pristine, independently
verified **read-only** container view of the requested immutable image, before
fixture installation or boot. Do not invoke an untrusted image entrypoint or
mount helper files over shipped payload/configuration. This patch does not
implement or authorize that orchestration.

Required arguments are:

```
--profile published-gdm
--image ghcr.io/tuna-os/marlin@sha256:<full index digest>
--platform-digest sha256:<full selected platform manifest digest>
```

JSON is written to stdout; a failed static check returns exit status 1 while
preserving other observations. `runtime_qualified` is always false.
`effective_launch_verified` is also always false, and `unverified_checks`
explicitly lists image binding, effective session/executable search paths,
password authentication and desktop runtime. `static_inventory_ready` means
only that this bounded inventory's checks passed, not that those open checks did.
`subject.binding` explicitly says caller-declared. The helper cannot discover
or authenticate its own image digest. Its caller must retain the registry
index/platform/config digest chain, actual container invocation and helper
version, then join those receipts to the inventory. A schema-valid fabricated
report is still fabricated: `validate_report()` checks completeness and internal
consistency, not authenticity.

Partial registry-layer prefixes, cached package members, source intent and SBOM
records are not accepted in place of a complete mounted-filesystem inventory.
Even a fully hash-verified individual layer does not prove the merged image
after later-layer replacements and whiteouts.

## Immutability comparison

`compare_immutable(before, after)` requires complete passing reports with the
same immutable subject. Every original package name/version must remain, and
protected bytes, ownership, modes, links, directory membership and effective
PAM closure must match. Extra packages and unrelated executables/services may
be added. Changes inside protected configuration/session/PAM trees fail.

The second inventory must also be a quiescent read-only pre-boot view. This
prototype intentionally rejects generated `/run/gdm/custom.conf` and unreviewed
unit overrides; it is not a collector for a running desktop. Do not compare a
pristine image with a live mutable boot and interpret failure as an image bug.

## Untested assumptions and deliberately narrow support

- The exact installed GDM/package/PAM ownership and merged file layout have not
  been verified by this helper. Earlier individual-layer findings are only
  evidence for planning the checks, not a passing receipt.
- GDM executable/helper paths, the Tuna entry values and the service directive
  allowlist are explicit Arch/GDM expectations. Package updates may require a
  reviewed contract change. Generic `service.d` drop-ins, additional GDM
  aliases, symlinked unit-search/enablement directories, or new unit directives
  fail closed, even if a particular use could be legitimate.
- Known alternate session roots under `/usr/local/share` and
  `/usr/share/gdm/greeter` are inventoried; an alternate `tuna.desktop` is
  rejected. This is conservative coverage, not verification of the exact
  installed GDM build's session search order or effective `XDG_DATA_DIRS`.
  Resolving that open check is required before effective-launch qualification.
- PAM includes/substacks and conventional modules under `/usr/lib/security`
  are supported. Missing modules are allowed only on a `-`-prefixed `optional`
  line; `-required` is still required. This records policy and ownership; it
  does not interpret arguments or prove the policy's authentication semantics.
- The keyfile reader is a strict linewise subset, not a complete GLib parser.
  Indentation never hides a subsequent key as an INI continuation. Unknown or
  ambiguous syntax may require review rather than relaxation.
- `/etc/locale.conf` may be unowned by ALPM but must be a trusted regular file
  containing only locale-variable assignments; its bytes remain immutable.
- Required payloads/configuration are root-owned and not group/world writable;
  required executables must have all three execute bits, independent of the
  eventual nonroot user's primary/supplementary groups.
  Symlink/ancestor checks assume the caller supplies a quiescent read-only
  filesystem. They are not a defense against hostile concurrent mount changes.
- Hashes plus installed ALPM ownership establish observed identity, not package
  signature authenticity. The trusted image/provenance chain remains separate.
- The shipped entry uses the basenames `Exec=tuna-session` and
  `TryExec=tuna-session`. This prototype verifies those exact bytes and the
  owned `/usr/bin/tuna-session`, but **does not bind GDM's effective PATH to that
  executable**. A higher-priority shadow binary is not ruled out by this
  inventory. Before runtime wiring can qualify a launch, the coherent harness
  must verify the actual launcher executable/environment and its relationship
  to the observed compositor, or add a reviewed effective-launch static check.
  Checking only a guessed finite list of PATH directories would not close this
  gap. `static_inventory_ready` must not be used as proof of that launch binding.
- These checks are not a general audit of every possible systemd dependency,
  authentication mechanism, environment variable, or desktop launch path.
- Whole-package compilation, actual image execution, boot/login tests, five-boot
  qualification, workflow wiring and publication are outside this patch.

## Evidence used to choose these prototype checks

The investigation targeted the immutable index
`sha256:db5bef1da75117080e937273c1c3e5852673dc7a20306481cbcc50eeeba78c4d`
and its amd64 manifest
`sha256:71711ea130b9c6d8c29b77626860c6c9a93a94cbb2cf65caa6da666cea3922fd`.
The publisher's [run 39471041989](https://github.com/tuna-os/tunaOS/actions/runs/39471041989)
and config identify source revision `4d5657bf97a0b9328b34d7c353c2dd01ac9663be`.
These references are investigation context, not embedded allowlisting or a
passing inventory.

The cached official `gdm-51.1-1` package inspected in that investigation has
SHA-256 `8104ec99fc8d12343aec852acd14a77d8abe7c800bc60b4964fe4bda22df307e`.
Its service unit, helper paths, vendor PAM layout and login-default schema
informed the checks. A cached package is not proof of installed ownership or
merged-image bytes. The Tuna session entry came from a fully hash-verified
individual Tuna payload layer, which likewise is not final-filesystem proof.

Known alternative session-directory checks were motivated by upstream GDM
source and cached-binary strings. They are intentionally conservative: upstream
main's complete search order was **not** established for this exact installed
GDM build. Effective search order and environment remain explicitly unresolved.

## Local tests

```
PYTHONDONTWRITEBYTECODE=1 python3 scripts/lib/published-dm-contract-tests.py
```

Tests use temporary synthetic trees and fake read-only package-query responses.
They never execute fake payloads, install packages, invoke PAM, start a display
manager, authenticate, or alter a real image. UID/GID are modeled in returned
fixture records without chown. The fixtures are test data only; they are not
injected into a qualification image.
