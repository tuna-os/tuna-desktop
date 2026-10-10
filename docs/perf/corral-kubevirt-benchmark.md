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
