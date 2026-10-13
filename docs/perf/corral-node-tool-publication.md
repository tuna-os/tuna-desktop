# Candidate benchmark tooling publication

This tooling is separate from a Tuna Desktop release. The original tool build,
registry availability, tiny node method, full disk import, and guest comparison
are five distinct qualifications. Source preparation qualifies none of the last
four. GPU and total-power comparisons are follow-ups under the owner's v1 waiver;
no unsupported advantage is claimed. Guest CPU/memory and the other release gates
remain unchanged.

## Original build and successor publisher

Use the existing registered `corral-visual-tools.yml` manual workflow. Its
`node_tool_registry_action` defaults to `disabled`. Only `publish` grants
`packages: write`; `verify-only` has read-only repository access. Both explicitly
grant `actions: read` for run/job/artifact APIs and require the focused tests and
an original-repository manual event. Neither rebuilds the image. Before custody
acquisition and again immediately
before registry writes, the command binds the actual GitHub repository/event/run,
attempt, branch/workflow ref and mode-specific current job to a fresh in-progress
API run and attempt jobs. The focused prerequisite must have succeeded. The
executing checkout must be clean at the advertised HEAD and its remote commit's
exact tree. CLI, helper and workflow Git blob bytes/hashes must match that source
through the GitHub API; self-reported local hashes alone do not admit publication.
Artifacts are required outside the checkout.

Supply the original successful build run ID, original small closure artifact ID,
original OCI artifact ID, and full built source SHA. The publisher workflow SHA
is separately recorded. The selected build must also be an original successful
manual run of that registered workflow, not a PR or a differently named workflow.

The command checks the two original API ZIP digests and sizes, safe bounded ZIP
members/CRC, the original OCI archive digest, manifest/config bytes and digests,
all compressed layer digests, decompressed rootfs diff IDs, and the eight original
built source hashes against the source at that exact commit. The original build
closure retains `registry_published: false`. Availability is recorded separately.

`publish` uses a temporary private destination authfile and token stdin for
`skopeo login`. The registry target is exclusively
`ghcr.io/tuna-os/benchmark-node-tools:<built-source>`. `skopeo copy
--preserve-digests` must preserve the original single-platform manifest digest;
otherwise the operation fails. No image labels, recompression, or config changes
are made. The receipt records the actual destination digest and tool version.

Afterwards, or in `verify-only`, the command uses an empty explicit authfile and
no-credentials options to fetch the exact manifest and copy **all** config/layer
blobs anonymously. A successful authenticated push with denied anonymous access
retains `authenticated_copy_succeeded: true`, `completed: false`, and
`registry_published: false` (availability unqualified). It does not silently
change package visibility or install Kubernetes image-pull credentials.
`verify-only` proves existing anonymous availability; its receipt explicitly
records that it did not perform an authenticated copy.

The small `corral-node-tool-publication` artifact preserves `publication.json`,
the original small build ZIP bytes, and both original build artifact API records.
The large original OCI remains retained in the original build run. The importer
requires the small publication ZIP/API record plus both explicit source SHAs:
`--node-tool-source-sha` and `--node-tool-publisher-sha`. It independently verifies
both original successful registered runs, the current-attempt publisher/focused
job IDs and success, its remotely verified Git tree/blobs, the original nested build ZIP, artifact
metadata, manifest/config/all-blob receipt, and the local publisher source hashes.
The importer refuses a missing/failed/stale/different-source publication chain.
This does not replace its independently required actual tiny node-stage receipt.
The frozen exporter and existing CDI RAW/QCOW admission remain unchanged.

External commands share an absolute 25-minute execution budget and captured
output bounds, with at most five seconds reserved separately for each retirement.
Linux supervision requires one thread and no preexisting children. A temporary
subreaper owns adopted descendants. The session leader is held unreaped with
WNOWAIT until its process group is killed even if the leader already exited;
adopted descendants are pidfd-killed and reaped. Lingering descendants cause
refusal even after successful cleanup. Unconfirmed retirement fails closed and
retains subreaper ownership only while the publisher process remains alive; it
cannot become a completed publication receipt. Raising an error followed by CLI
exit does not prove physical retirement or preserve that ownership. Outer job or
VM cleanup is not qualified by this source proof.
The destination authfile is created with mode 0600 before login and checked again
after login. The OCI manifest/config descriptor sizes must equal the actual raw
bytes and archive member sizes before any copy. Temporary registry
credentials and downloaded OCI/blob copies are removed in `finally`. No raw
registry stderr, command environment, or credential data enters the small receipt.
This cannot guarantee bounded filesystem operations during a kernel I/O stall;
the CI job has a separate 30-minute limit. No actual publisher or node method has
been exercised by the source tests.

## Practical public-package setup

GitHub documents that newly published packages default to private and that linking
a repository inherits **access permissions, not visibility**. An organization
owner or package administrator can open the organization's Packages page, select
`benchmark-node-tools`, then **Package settings**, and choose **Public** if the
organization policy permits it. Making a package public can be irreversible.
An administrator can also grant `tuna-desktop` Actions access in package settings
without changing the qualified image bytes. Do not add a repository-linking OCI
label to this already qualified image: that would change its config digest.
[GitHub package access and visibility documentation](https://docs.github.com/en/packages/learn-github-packages/configuring-a-packages-access-control-and-visibility),
[Container Registry documentation](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry).

The documented [Packages REST API](https://docs.github.com/en/rest/packages/packages)
does not offer a package visibility-update operation. This source therefore
contains no invented REST PATCH or automatic visibility change. If a first push
creates a private package, retain its failure receipt, apply the separately
reviewed administrator action, then use `verify-only` with the same original
build inputs. Alternatively, an already public dedicated package may be used
only within the approved fixed namespace and with suitable Actions write access.
Actual anonymous manifest **and blob** verification remains mandatory whichever
setup route is used.

Skopeo documents that [`--preserve-digests`](https://github.com/containers/skopeo/blob/main/docs/skopeo-copy.1.md)
fails when preservation is impossible. Registry permission, anonymous visibility,
and an actual node pull without service-account credentials still require runtime
proof. A tiny node proof does not admit a full image or prove guest performance.
