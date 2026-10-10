# Corral / KubeVirt benchmark preparation

`scripts/tuna-corral-perf` reads the explicitly selected Kubernetes context and
node. It checks KVM/tun/vhost-net resources, scheduling taints and all reported
filesystem pools, reserving at least 15% plus 1 GiB. A positive byte estimate must
include imported raw extents, compressed input, CDI scratch/preallocation,
container layers and guest writes. A passing disk check does not qualify CPU,
memory, input latency, GPU or total power, and is not a full scheduler admission.
Configured kubelet thresholds still require verification.

`lib/corral_perf.py` supplies exact VM/VMI ownership checks and a bounded RFB3.8
transport with actual key events and raw pixels. Incomplete rectangles cannot
become a completed framebuffer. RFB output alone cannot qualify native presented
overview completion. Use the existing native receipt and baseline gates.

## Import and measurement contract

Build the coherent Arch GNOME51 stock/instrumented benchmark disk externally.
Retain qcow2 SHA256, virtual size, allocated/map extents, kernel/initramfs and
critical GNOME hashes, immutable reference image digest, package/source revisions
and exact Tuna executable hashes. GitHub artifact ZIPs require authenticated
local download and extraction; they are not unauthenticated CDI HTTP sources.
A Filesystem PVC must exceed virtual disk size plus CDI filesystem overhead;
an 18 GiB virtual disk in a 20 GiB claim avoids the literal 20/20 assumption.
Neither compressed size nor virtual size bounds peak physical usage alone.

Corral local import uses virtctl image-upload. Its current local-import command
lacks node/UEFI options, so a derived owned VM must be configured before starting.
The on-cluster bootc builder uses 40 GiB scratch; do not use it on a constrained
node. Corral `--cloud-init` takes literal YAML, not a pathname. Keep private keys
and unredacted cloud-init out of artifacts. Use an owned SSH known_hosts file,
API tunnel, strict host verification, and bind VM/VMI UID before/after operations.

The paired measurement runner is not yet ported: transport plumbing and admission
are preparation only. It must retain the original twenty-action pairing, stock
and instrumented provenance, guest helper acquisition failure receipts, native
presentation completion gates, same image/kernel/resources and repeated alternating
sessions. Do not relabel local QEMU hardware metadata as KubeVirt. GPU device and
host total-power counters are currently unavailable on the selected AWS node;
these cannot count as a performance win.

## Paired transport runner

`scripts/tuna-vm-perf --corral-vm NAME` uses actual API-tunneled RFB key events,
full requested raw framebuffer coverage, and fixed SSH guest phases. Phase children
have their own guest `timeout` supervisor as well as host acquisition limits.
Sampler journal records must match the exact boot ID and `tuna-perf.service`.
VM/VMI/storage source hashes, node UID, resources, boot ID and actual selected
compositor executable identity are retained and checked. The host is the observer,
not the AWS hypervisor. Existing native completion and GNOME reference validators
remain in force. RFB first response is still only a broad visible-response bound.

`scripts/tuna-corral-perf-pairs` selects GNOME/Tuna three times in alternating
order, reboots before each acquisition, requires six distinct fresh boot IDs and
twenty actual input observations per case. Session selection writes only mutable
GDM/AccountsService/tmpfiles overrides; it does not modify the protected image.
It requires `--export-dir` from an authenticated original CI download, original
reference/package/selection receipts, and an original successful upload receipt.
The upload receipt schema1 contains `completed:true`, `context`, `namespace`,
`vm_name`, `vm_uid`, `pvc_name`, `pvc_uid`, `datavolume_name`, `datavolume_uid`,
`disk_sha256` and `manifest_sha256`. These must describe the actual original
owned upload action. Current successful CDI/PVC/VM state is checked too; it does
not retroactively prove old disk bytes. The source-owned upload procedure still
needs implementation and actual export/import qualification.

The six boots use one persistent fixture disk; they are not six new disk installs.
Do not call these fresh disks or assume user state never persists across sessions.
SSH key/sshd provisioning must be part of the external test fixture and validated
by actual boot. Fedora containerDisk preflight alone proves no Marlin SSH support.
Budget the full20Gi PVC plus transfer, scratch and temporary allocation for the
chosen import procedure, rather than admitting the optimistic mapped7Gi estimate.

## Original artifact importer

`scripts/tuna-corral-benchmark-import` requires explicit original workflow run,
artifact ID and source SHA. GitHub API run metadata must identify a successful
manual dispatch from the original repository/workflow. Original archive digest,
finite ZIP members/expansion, disk bytes, manifest source/reference, package run
and exporter evidence files are checked before cluster writes. A new randomly
named halted VM and owned CDI DataVolume are created; existing resources are never
adopted or overwritten. Upload uses an exact-context authenticated Kubernetes
port-forward to CDI, an owned localhost listener and actual `virtctl image-upload`.
The original upload receipt is written only after real successful CDI completion.
Failure cleanup checks exact original resource UIDs and requests dependent deletion.
The optional `--start` runs Corral only after upload completion. Actual guest boot,
SSH, installed critical bytes and native receipts still require qualification.

The current importer reserves the full20Gi target plus20Gi scratch, transfer,
1Gi runtime layers and512Mi guest writes. It cannot proceed on a node with only
32Gi conservative headroom. Do not lower this from a mapped-size estimate.

The owned `tuna-corral-raw-upload-probe` diagnostic creates a small raw upload on
an explicitly selected node, records uploader/scratch observations and cleans up.
CDI1.66.1's raw data source selects direct target transfer, while its controller
still creates a scratch PVC. The actual first64Mi/1Gi diagnostic timed out with
zero target/scratch data and remains a failure; it does not justify a smaller
production import reservation. No shared CDI deployment or network was changed.
