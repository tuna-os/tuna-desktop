# Separate full RAW.gzip transport candidate

This source candidate adds `--format raw-gzip` to the CI disk exporter and
`export_disk_format: raw-gzip` to its existing manually dispatched workflow.
The default remains QCOW2 with the reviewed 28 GiB dense policy. Neither format
qualifies a release or a benchmark by itself.

The gzip branch wraps every byte of the original 18 GiB RAW after package,
reference, kernel, ESP and image checks. It does not shrink partitions or remove
GNOME/Tuna payloads. The owned compression process independently decompresses
one member and matches the exact original length and SHA256. A gzip trailer's
32-bit length is never used as the full image length. Concatenated members,
truncation and trailing payload are refused. Actual compressed length and SHA
are recorded; the 5 GiB maximum is a refusal bound, not a predicted size.

The manifest names `raw-gzip-upload` / `full-paired-raw-gzip-20gi-v1`, `disk.raw.gz`
and requested target `20Gi`. An old 28 GiB QCOW manifest or RAW probe cannot
stand in for this contract. Download retains original successful API run and
ZIP digest, member safety/CRC/expansion and source/package/reference evidence.
The regular gzip file is what virtctl opens, sizes and seeks; no FIFO is used.
Local admission still conservatively reserves original ZIP plus the existing
6 GiB extraction bound and 0.5 GiB allowance. No local RAW conversion occurs.

## Source-only tiny probe

`scripts/tuna-corral-gzip-upload-probe --context CONTEXT --namespace corral-vms
--node NODE --out NEW_OUTSIDE_CHECKOUT_DIRECTORY` is prepared source only.
No probe has been dispatched or executed. It constructs a distinct gzip of a
64 MiB synthetic RAW, records source/helper hashes, uses an owned 1 GiB target,
observes the exact CDI server, nonpreallocation, target/scratch PV ownership and
scratch use, reads back the full 64 MiB prefix, and verifies original UID cleanup.
The public entry owns the complete Linux process tree using the reviewed held
leader/subreaper/pidfd supervisor, a private one-context kubeconfig projection,
plugin refusal and namespace UID checks. It retains the original error if
cleanup also fails. Original process-tree retirement must be confirmed and the
projection removed before its final ownership receipt is written.

The new probe must actually pass and its original bundle digest must be reviewed
before it can authorize full upload. It is not the old RAW64Mi proof, a full
18 GiB readback, 20 GiB reservation proof, dense decoder RSS bound, VM boot,
performance measurement or installed-session acceptance.

## Full import conditions

The proposed node estimate is full target 20 GiB + scratch metadata 1 GiB +
runtime layers 1 GiB + guest allowance 0.5 GiB = 24,159,191,040 bytes. The recorded
31,410,515,558-byte headroom is historical only. Fresh identity, readiness,
pressure, applicable filesystem pools and capacity guards remain required.
The directory-only storage backend and exact server/client bytes must agree
with the separately reviewed gzip probe. No compressed node spool is inferred
from its small size; the probe's actual scratch observations must establish it.

Before upload an owned read-only observer mounts the bound target and measures
available space. The effective CDIConfig filesystem overhead is acquired and
checked with CDI's actual ceil/divide/1 MiB rounding formula. Usable target must
fit all 18 GiB plus 0.5 GiB; a nominal PVC alone cannot pass. The full image must
then independently match its original 18 GiB SHA by readback before any optional
VM start. CDI expansion means the entire grown target need not have that SHA.
Unsupported or unavailable measurements refuse; no invented zero is allowed.

Spawn, kernel I/O, scheduling and post-process storage reclamation do not have
hard real-time guarantees. Failure cannot manufacture a completion receipt.
Any unknown retirement or original PV reclamation remains unqualified. The
source controls do not establish actual CDI gzip memory/time behavior.

This transport leaf does not supersede draft PR648's separate guest aggregate
CPU/PSS and paired observer. The eventual reviewed union must retain that lane's
raw CLOCK_BOOTTIME/monotonic windows, source/core/HZ/boot/session identities and
six case pairing, then qualify an exact current desktop/package image. No guest
comparison, all72/all14 closure, production pin, publication or GPU/power
advantage claim follows from this source candidate.
