# Synthetic node-stage probe — preparation only

No cluster operation is authorized by this source draft. The source-only plan is
based on reviewed publisher `66c766d3` and its actual successful registered run
38387006150. The anonymously accessible tool is:

`ghcr.io/tuna-os/benchmark-node-tools@sha256:e6c52c9653c1406acc685063d571d28222251bb46eefbda34692a9b7ad197893`

Its source build remains f6da7084 / run38379761439; original OCI, manifest,
config/layers and source identities stay immutable. The existing actual publication
ZIP is only 12,974 bytes. No large artifact download, local QEMU/container build,
VM operation, namespace/RBAC/secret creation or image-pull credential is needed.
Publisher CLI/helper/workflow bytes are unchanged, so the real publication loader
can continue checking their original hashes. The new tests use the existing
`test_corral_*.py` focused glob; no workflow change is proposed.

## Proposed bounded invocation after review

Use a clean exact reviewed observer commit; do not execute this uncommitted draft.

```sh
python3 scripts/tuna-corral-node-stage-probe \
  --execute --observer-source <reviewed-full-probe-commit> \
  --context admin@aws-migration --namespace corral-vms --node ip-10-20-1-12 \
  --tool-directory /tmp/tuna-v1-node-tools-publisher-publication-review \
  --built-source f6da7084d9b31250eb19ecb527a22b169456e4e1 \
  --publisher-source 66c766d3e9e9bab39b69f694817ac475c9430ac4 \
  --out /tmp/<new-unique-owned-probe-output>
```

No resources can be created until the original build/publication APIs, ZIP,
current successful attempt jobs and original source/OCI closure pass the unchanged
loader. This runs in a bounded owned host child; GitHub credentials never enter a
pod or exec payload. The observer checkout/head/tree must be clean and match the
explicit approved source before operations and after the completed proof.

## Fixture and real data path

1. Bind actual kube-system namespace UID (cluster), corral-vms namespace UID,
   node UID and exact local-path backend data. Refuse missing/unknown node readiness,
   pressure, taints or filesystem data. Recheck these identities/capacity throughout.
2. Create only two unique nonce-labelled **1 GiB requested** local-path PVCs and a
   unique node-pinned pod. Recover an ambiguous API creation by the nonce and record
   UID before cleanup. No name-based deletion is used; deletion has server UID
   preconditions. No unrelated VM/disk/pod is adopted.
3. Verify actual ready image ID, UID 107 execution, unchanged QEMU SHA/21 libraries,
   actual private Python executable/ELF mappings, resources and mounted owned PVCs.
   No host namespaces/hostPath/service-account token/imagePullSecrets are admitted.
4. On the owned pod, generate a **synthetic** sparse RAW fixture with two fixed
   nonzero regions and an 18 GiB logical size, then encode it with the actual QEMU
   into a QCOW capped at 1 MiB and ZIP capped at 2 MiB. This is test-data generation,
   not an installed image build. The host independently computes the known RAW SHA
   in 1 MiB blocks without creating or downloading any RAW file. It must equal the
   already retained original tool-smoke known-pattern SHA.
5. Download at most 2 MiB of that ZIP to the owned host output and bind exact
   length/SHA. Stream those actual host file bytes back over authenticated exec
   stdin. Exercise the unchanged worker's wrong-digest rejection at a separate
   owned path, then its real receive/CRC/extract and decoder/readback path.
6. The unchanged worker verifies standalone QCOW format, complete depth-zero
   mapping coverage and original QCOW SHA; it converts to sparse RAW and checks
   logical size plus complete content SHA. Refuse synthetic output allocation
   above 16 MiB. Re-observe pod/PVC/PV/node identities on both sides of each exec;
   exec remains name-based, with no claimed atomic UID precondition.
7. Delete the producer pod, verify its API absence, then create a new UID pod with
   both volumes read-only. It independently hashes the complete RAW and verifies
   the known content SHA and logical size. No 18 GiB host download occurs.
8. UID-precondition-delete every recorded own pod/PVC and wait for all original
   PVs to disappear under their verified Delete policy. Reject a replaced PV
   without deleting the foreign replacement. Record final node/filesystem data.
   Receipt completion requires successful transport/decoder/readback and cleanup.

All API/exec calls share the 900-second operation deadline; ambiguous recovery and
final cleanup have a separately reserved 180-second outer budget. Reviewed Linux
whole-command supervision additionally has at most five seconds for retirement;
subreaper ownership lasts only while the process lives. Kernel stalls, physical
container retirement after an API deletion, and node filesystem reclamation are
not guaranteed by an object-absence receipt. Cleanup failure leaves completed
false and fails the process. Private host output is mode 0700; no credentials or
raw environment are recorded.

## Conditional storage and memory admission

Local source worktree is sparse (~4.2 MiB). Runtime host transfer is capped at
2 MiB; no raw conversion or large OCI/export is downloaded locally. Current
workspace pressure (~430 MiB free when assigned) must be freshly observed before
execution, not replaced by an old receipt. Source/tests may run now; execution
requires the explicit separate review.

Node admission reserves **both full 1 GiB PVC requests**, the complete measured
compressed plus uncompressed tool closure and 512 MiB tool metadata allowance,
and 512 MiB guest writes. No OCI logical bytes are credited as an existing on-node
allocation. This is about 3.97 GiB for the published closure, subject to a fresh
exact calculation. Existing 15% + 1 GiB filesystem reserves apply to node, image
and container pools; every pool must be observable. Actual node available memory
must exceed the 512 MiB container limit plus 1 GiB reserve. Requests/limits are
100m/1 CPU and 256/512 MiB memory. Actual PVC/PV specs, node allocatable and raw
stats, allocated file blocks and memory observations are retained, not inferred.

When available, actual container cgroup-v2 memory.peak/memory.max are recorded
before/after phases, with their numeric 512 MiB limit checked. This is the whole
owned container's peak since start, including fixture encoding, not isolated
decoder RSS. Unsupported counters retain an explicit reason, never invented zero.
Sparse synthetic success does not qualify a dense 15 GiB payload under that limit.

## Scope and refusal of full admission

The receipt method is deliberately `node-stage-tiny-v1`. It records tiny success
separately, with full-node-method, full-20-GiB-reservation, dense-decoder-memory,
full-image-import and performance qualification false. The unchanged production
`method_proof` must reject this distinct receipt even when source bytes, cleanup,
context/node/image and all other identity fields match. Reconciliation with that
larger admission contract needs separate review, not relabeling tiny evidence.

No installed Marlin/GNOME/Tuna identity, VM attach/boot, CPU/PSS/latency, actual
session functionality or release is proven. GPU/total-power comparisons remain
owner-waived nonblocking follow-ups; unsupported advantages are never claimed.


## Corrected source admission and cleanup

The successor binds a private, flattened, single-context kubeconfig projection
in a separate 0700 temporary directory with a 0600 config. External credential
plugins and unflattened credential paths are refused. The projection is removed
on exit and its secret contents are excluded from receipts. Every Kubernetes
command, including exec, inventory and deletion, uses that projection and the
reviewed whole-command supervisor. Cleanup checks cluster and namespace UIDs
before each atomic UID-preconditioned deletion, within its absolute deadline.

Before deleting claims, cleanup inventories their current bound PVs and all PV
claim references to recorded claim UIDs. It retains original PV UIDs even when a
pod fails before becoming ready. Missing or unavailable transitive inventory
cannot establish cleanup success. Final API object absence is not proof of
physical filesystem reclamation. An exec by pod name has no UID precondition;
identity observations before and after each operation detect drift and refuse
completion.

The wrong-digest control requires complete original ZIP length and actual SHA
before testing a mismatched expected SHA. Truncation and unrelated errors do not
count. Pod admission rejects init and ephemeral containers, privilege and
security policy deviations, swapped claim mappings and mount subpaths. These
are source tests until a separately approved actual cluster probe runs.
