# Node staging for the matched guest benchmark

This is an intermediate disk preparation method. It creates no VM and refuses
`--start`. Existing CDI RAW/QCOW admission and paired import validation remain
unchanged. An actual qualified tiny node-stage method receipt is required before
this method can prepare a full image; the existing CDI receipt cannot substitute.

## Tool closure

The read-only existing Fedora launcher audit observed qemu-img 10.1.0 at
`/usr/bin/qemu-img`, SHA256
`250b2b4ee552fdd96e1078525d12bb0b1f2557f33f153c282bfaea1ece5b3fad`, UID 107.
The immutable launcher image is pinned in the Containerfile. It had neither
Python nor dnf/microdnf/yum, so it cannot run the strict ZIP/helper contract.

The dedicated manual CI workflow uses a separately pinned official EL9 installer
stage to resolve Python 3.12 RPMs, verify signatures, and retain package NEVRAs and
SHA256s. Python 3.12 is available since RHEL 9.4 according to
[Red Hat's Python documentation](https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/9/html/installing_and_using_dynamic_programming_languages/assembly_installing-and-using-python_installing-and-using-dynamic-programming-languages).
It preserves the original qemu executable SHA before and after installation,
records the actual ELF library file hashes, and exercises actual synthetic
mixed-extents QCOW2-to-RAW conversion plus ZIP roundtrip and negative controls
as UID 107 without capabilities and with a read-only image root.

No tool image has been built or published by this source change. Repository
availability and RPM compatibility remain actual CI checks. The optional manual
publish input targets only `ghcr.io/tuna-os/benchmark-node-tools`; it is not a
TunaOS image/package release. The importer requires an original successful
manual CI run, original small artifact ZIP digest and exact closure source. A
non-published closure fails admission. The OCI original bytes and layer/config
records are retained by CI; compressed and uncompressed layer sizes are measured,
not guessed. RPM inventory is a bounded SBOM scope, not a security certification.

## Storage and transport

The plan reserves the full 20 GiB final PVC, rounded staging size covering the exact
original archive plus the existing 3 GiB safe expansion ceiling and 0.5 GiB metadata,
actual tool pull/extraction closure allowance, and 0.5 GiB guest writes. The entire tool reservation remains in force after image readiness; OCI logical
layer sizes never credit measured node filesystem allocation. Only observed
owned PVC blocks reduce the remaining disk reservation. The existing
node 15% filesystem plus 1 GiB operational reserve remains authoritative. Node UID,
Ready/DiskPressure, taints, filesystem metrics and exact local-path config are
checked. Both PV affinities and claim UIDs must select the requested node.

The authenticated host `gh api` process streams only artifact bytes to owned
`kubectl exec` stdin. No GitHub credential, service-account token, hostPath or new
RBAC is placed in the pod. The full original ZIP SHA and byte length are checked
before extraction. Member/expansion/CRC, evidence hashes, package/source/reference,
SSH/ESP manifest, standalone QCOW2/map and original RAW hash contracts are retained.

A separate read-only pod independently hashes the final disk before disk completion.
Temporary pods and staging PVC use nonce recovery and UID-preconditioned deletion,
with finite absence observation. Failed cleanup retains ownership/error receipts;
it does not fabricate success. Deadline expiry can request pod deletion but cannot
promise an unkillable kernel task has already retired. Whole-operation and cleanup
budgets are distinct. No partial disk is boot-admitted.

## Remaining implementation and qualification

- Actual exact-source tool CI, candidate registry publication and measured closure.
- Separately authorized tiny staging probe, including identity, backing/truncation,
  dribble/timeout, capacity, replacement and independent readback/cleanup negatives.
- Actual original whole-green export and fresh capacity admission, then full-image
  staging/readback evidence. Old 29.49 GiB node headroom is not current admission.
- VM attachment, exact receipt-aware paired importer integration, authenticated
  boot/source identity and six matched guest CPU/memory/latency cases. This first
  leaf intentionally cannot satisfy the existing CDI paired admission function.

GPU and total-power comparison are nonblocking v1 follow-ups under the owner's
2026-10-13 decision. Unsupported counters remain explicit and never imply an
advantage. Guest CPU/memory, matched latency and all other release requirements
are unchanged. No benchmark measurement or release qualification follows from
source tests or the tool smoke.
