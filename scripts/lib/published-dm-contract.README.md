# Published GDM preflight prototype

This is a **review-stage static helper and workflow preflight**. It has not been run against
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
current lane's greetd PAM service and VT 1 assumption. Existing current-build
and runtime login harness behavior remains unchanged. The published workflow
adds only a static pre-fixture gate; its old greetd fixture guard still applies.

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

The host orchestrator runs this trusted helper in a pristine, independently
verified **read-only** container view of the requested immutable image, before
fixture installation or boot. Do not invoke an untrusted image entrypoint or
mount helper files over shipped payload/configuration. This patch does not
change the image or relax the existing fixture/runtime gates.

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
- Whole-package compilation, actual image execution during development,
  boot/login tests, five-boot qualification and publication are outside the
  local validation performed for this patch.

## Workflow caller and verified image binding

`sudo -n /usr/bin/python3 -I -S -B scripts/tuna-published-preflight
--image <immutable-reference> --out published-vm-artifacts` is called immediately after the existing immutable pull/inspect check and before
`Containerfile.published` is built. No fixture installation precedes this report.
The workflow's triggers, permissions, time limits, upload path and old greetd
guard are unchanged. Both pure suites run beside the existing VM unit tests.
The unprivileged workflow creates the existing artifact directory first. Only
this fixed checked-out supervisor crosses the existing CI sudo boundary; its
fixed Podman/Skopeo children execute directly with the same UID. Podman is
explicitly local (`--remote=false`); a read-only host check requires Linux, EUID 0
and the already-effective CAP_SYS_ADMIN bit to avoid its rootless re-exec route.
This does not grant capabilities. No sudo grants,
host security settings, persistent access, or container privileges are added.

The host runner uses anonymous `skopeo inspect --raw` metadata reads to hash the
exact requested manifest/index bytes, select exactly one Linux/amd64 manifest,
and verify its raw bytes against the index descriptor's digest and size. It uses
`skopeo inspect --config --raw` to hash the referenced config bytes, then joins
that config digest, architecture/OS and rootfs diff IDs to `podman image inspect`
for the already-pulled image. It does not treat Podman's `.Digest` alone as a
platform/config binding. Index and single-manifest references are supported;
ambiguous platform selections fail closed.

The exact verified local image ID is passed to `podman create --pull=never`.
The explicit process is `/usr/bin/python3 -I -S -B -c <trusted prelude>`; the
prelude verifies that the dedicated helper mount path does not resolve through
an image symlink, then runs the mounted inventory script with explicit profile,
image and platform digest arguments. Python site startup and image/host-proxy
environment inheritance are disabled. Only six fixed environment entries are
allowed. No image entrypoint, shell, init system or healthcheck is invoked.

Create options include `--network=none --read-only --read-only-tmpfs=false
--image-volume=ignore --cap-drop=all --security-opt=no-new-privileges
--user=0:0 --unsetenv-all --http-proxy=false --no-healthcheck --systemd=false
--log-driver=none --pid=private --ipc=private --uts=private --pids-limit=64
--timeout=150`. The only user mount is a read-only directory containing copies
of the two trusted helper files. Staging directories are 0555 and files 0444 so
the no-capabilities container can read them regardless of host-runner UID.
No repository, host credentials, sockets, devices or output directory is mounted.

Before `podman start --attach`, actual container inspection must match the
verified image ID, exact interpreter/arguments/environment, read-only root and
helper mount, no capabilities, no-new-privileges, private namespaces and disabled
automatic execution settings. Post-exit inspection checks the same container
and command, non-running/exited status, exact exit code and no OOM/error. The
receipt retains an allowlisted observed invocation and helper/runner hashes.
The helper's own caller-declared binding and unverified launch/runtime flags are
preserved; the outer `image_binding_verified` and `inventory_subject_join_verified`
fields describe the independently checked host joins, not desktop qualification.

The requested image, installed interpreter/package manager and host
Podman/Skopeo are trusted execution inputs. This is not cryptographic attestation
of a hostile host or a malicious image interpreter. No actual container invocation
was performed during local development; compatibility is based on official
Podman/Skopeo API documentation/source and fake-command tests.

### Failure, bounds and ownership

All normal commands share one 300-second monotonic command budget; metadata
commands also have 30-second limits. The inventory attach has at most 180 seconds
of that budget, with Podman's 150-second **started-container** timeout as a
separate backstop. It does not settle uncertain creation or prove cleanup.
All cleanup commands share a separate 20-second reserve. Up to two
seconds of each command's own remaining budget is reserved for process-group
retirement, rather than adding an unbudgeted wait after every command.

Each command starts a dedicated session/process group. Linux `waitid(WNOWAIT)`
observes completion without reaping the leader, retaining its PGID identity even
if the leader exits while a child holds a pipe. On success or failure, the
supervisor signals only that anchored group, checks `/proc` for executable
members, and reaps the leader only after group retirement is established.
Unverified group retirement blocks every further command and records `command_groups_settled=false` and
`container_cleanup_terminal=false`. Create/start are never retried.

These are cooperative subprocess budgets, not a strict 320-second end-to-end
promise: host filesystem/kernel calls and artifact serialization have no hard
wall-clock bound, and abrupt supervisor/job termination cannot promise a cleanup
receipt. The existing disposable runner/job time limit is unchanged. The
supervisor trusts the official local Podman CLI; it does not contain arbitrary
host commands that daemonize or deliberately escape their session. In particular,
conmon can use another process group, so CLI-group retirement does not establish
container retirement; the owned-container cleanup checks below remain required.

Cleanup is limited to a **confirmed successful create** with an exact container
ID and an inspected association matching the unique name and invocation label,
immutable image ID, exact Python command, and unique staged helper source and
destination. A label or name alone is insufficient. The owned ID is removed and
a subsequent `podman container exists <verified-id>` must confirm absence before
`container_cleanup_terminal=true` is recorded. This field concerns the verified
container, not a general audit or cleanup of all host storage.

Any uncertain create, including a timeout, lost output, or invalid ID, remains
failed/unqualified with `create_confirmed=false` and
`container_cleanup_terminal=false`. It is never recovered by name, started or
retried. An immediate absence observation would not settle it: Podman storage
setup can allocate an unregistered writable layer before registering a container.
There is no generic storage pruning, and residual state is left to the disposable
CI runner's lifecycle. A confirmed ID whose ownership inspection fails likewise
is not removed without the required association. Command-group retirement and
container/resource cleanup are separate claims; neither the 150-second started-
container timeout nor a successful group kill proves residual storage retirement.
This limitation follows the [Podman 5.7 creation ordering](https://github.com/containers/podman/blob/v5.7.0/libpod/runtime_ctr.go)
and [containers/storage creation ordering](https://github.com/containers/podman/blob/v5.7.0/vendor/go.podman.io/storage/store.go).

Registry/inspect stdout is capped at 2 MiB, helper stdout at 8 MiB, and each stderr
stream at 64 KiB while reading. Raw registry config, raw inspect output, raw
stderr and arbitrary exception text are never artifacts or workflow log output.
Stderr is fully redacted: the receipt retains only fixed stage/classification,
captured byte count and digest, explicitly labeled complete-stream or retained
prefix. Inventory artifacts contain allowlisted paths, package identities,
hashes and structural metadata; free-form errors are replaced by a fixed marker.
No file contents, inherited environment values, host helper-source path or auth
data is retained. The final JSON has an additional bounded-overhead check.

`published-vm-artifacts/published-image-preflight.json` is written on success or
failure; the existing always-run uploader retains it. The root supervisor accepts
only that direct output-directory argument and opens the existing directory with
`O_DIRECTORY|O_NOFOLLOW`, refusing group/other-write access. A new exclusive
no-follow temporary file is atomically renamed relative to the retained directory
FD, so pre-existing symlinks/hardlinks cannot redirect a root write or truncate
another file. The receipt is 0644 for the existing uploader; no recursive
root-owned output-directory creation occurs. If the output directory itself is
unsafe/unavailable, the runner refuses with a fixed message rather than following
it, and cannot promise a report there. A static failure prevents
fixture creation. A static pass leaves `effective_launch_verified=false` and
`runtime_qualified=false`, and still reaches the unchanged greetd fixture guard.

### Supported API evidence and runtime limitation

The official [Ubuntu 26.04 runner snapshot `20261018.168`](https://github.com/actions/runner-images/blob/ubuntu26/20261018.168/images/ubuntu/Ubuntu2604-Readme.md)
reports Podman 5.7.0 and Skopeo 1.21.0-dev. Ubuntu's package metadata identifies
[Podman `5.7.0+ds2-3build1`](https://packages.ubuntu.com/en/resolute/amd64/podman)
and [Skopeo `1.21.0~pre1-2build1`](https://packages.ubuntu.com/en/source/resolute/skopeo).
The exact distribution [Skopeo inspect manpage](https://manpages.ubuntu.com/manpages/resolute/man1/skopeo-inspect.1.html)
explicitly supports `--config --raw`; raw config bytes, rather than reformatted
JSON, are necessary for the config digest check. Relevant Podman 5.7 sources are
the [inspect schema](https://github.com/containers/podman/blob/v5.7.0/libpod/define/container_inspect.go),
[inspect implementation](https://github.com/containers/podman/blob/v5.7.0/libpod/container_inspect.go),
and [conmon timeout/process-group implementation](https://github.com/containers/podman/blob/v5.7.0/libpod/oci_conmon_common.go).

The hosted-runner label and apt packages are not version-pinned by this existing
workflow. These official version/schema checks are compatibility evidence, not
a real invocation result or a guarantee about future updates. Missing/changed
inspect fields, unsupported flags, inability to read Linux process state, or
failure to retire a command fail closed. No image or Podman/Skopeo command was
executed during this local implementation.

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
PYTHONDONTWRITEBYTECODE=1 python3 scripts/lib/published-preflight-tests.py
```

Tests use temporary synthetic trees and fake read-only package-query responses.
They never execute fake payloads, install packages, invoke PAM, start a display
manager, authenticate, or alter a real image. UID/GID are modeled in returned
fixture records without chown. The fixtures are test data only; they are not
injected into a qualification image.
