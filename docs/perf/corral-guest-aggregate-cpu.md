# Guest aggregate CPU observer

`scripts/tuna-corral-guest-cpu` observes an already selected stock GNOME 51 or Tuna
session on the original admitted Corral shared fixture. It changes no image,
service, package, VM configuration or desktop selection. It waits 30 seconds and
collects a separate 30 second idle window. No comparison has been executed by this
source change.

Original candidate/package intent, authenticated export/import/readback, public
client key and measured SSH host identity are required. Actual running executable
bytes are read through `/proc/PID/exe` before and after collection; stock GNOME 51
also requires the original principal/modules/reference receipt. Boot, VM, node,
domain, source bytes and image reference must remain unchanged. Output belongs
outside the source checkout, and a changed collector/transport source refuses
execution. A failed receipt has `completed:false` even if raw data was retained.

## Metrics and raw evidence

The guest returns 31 raw reads, with monotonic and CLOCK_BOOTTIME nanosecond
timestamps bracketing collection, online CPU IDs, USER_HZ, CPU counters and
MemTotal/MemAvailable. Each boot-time read is bracketed by monotonic reads. Their
offset intervals must intersect throughout the collection; a suspend or clock
relationship drift visible to these reads is refused. This is sampled clock
evidence, not continuous observation between reads. The
observer refuses missing/malformed counters, boot/HZ/topology drift, overlapping
or nonpositive time intervals and counter resets. A guest timeout of 35 seconds,
host budget of 42 seconds and kernel file limits bound the collector.

The parser requires emitted CPU rows to match
`/sys/devices/system/cpu/online` exactly. Linux's current
[stat producer](https://github.com/torvalds/linux/blob/master/fs/proc/stat.c)
sums the aggregate over possible CPUs and emits individual rows for online
CPUs. If a guest kernel emits additional configured or offline CPU rows, or
online topology differs across these separate reads, this collector refuses
the sample. It does not silently drop rows or count offline CPUs as capacity.
This conservative refusal leaves that guest unqualified.


- Busy ticks: user+nice+system+irq+softirq.
- One-core percent: busy_ticks / USER_HZ / elapsed_seconds * 100.
- Guest capacity percent: one-core percent / number_of_online_cores.
- Accounted-time fraction: busy_ticks / sum_of_first_eight_CPU_deltas * 100.
  The guest/guest_nice columns are already included in user/nice and are never
  added again. Kernel accounting explicitly updates both sets in
  [account_guest_time](https://github.com/torvalds/linux/blob/master/kernel/sched/cputime.c).
- Steal and I/O wait are separate from busy. A regressing aggregate or per-core
  I/O-wait counter makes I/O wait and the accounted-time fraction unavailable
  with a reason; neither becomes zero. Kernel documentation describes I/O-wait
  accounting limitations in [the proc filesystem](https://docs.kernel.org/filesystems/proc.html).
- MemAvailable is the kernel's capacity estimate. MemTotal minus MemAvailable is
  also an estimate; neither is process PSS. Existing UID CPU, RSS and PSS metrics
  remain unchanged.

Aggregate guest CPU includes all UIDs, kernel work, short-lived processes and
collector/SSH overhead. It is not physical host CPU. Guest core-normalized values
and one-core values are reported together, with raw counter deltas and timestamps.
Unsupported GPU and total power remain unqualified. These idle measurements do
not prove action completion, smoothness, input latency or outperformance.

## Same phase for each paired desktop

`scripts/tuna-corral-perf-pairs --guest-aggregate-cpu` selects three alternating
pairs with fresh boots and the same admitted immutable image, VM, domain and
node. The opt-in path requires `--profile stock`, root SSH, `--candidate-sha` and
`--package-run`, plus the existing original export/import, package payload,
selection, image reference, private key and owned known-hosts paths. The original
exporter and its SSH/ESP contracts are unchanged. The importer retains the
reviewed candidate/package intent check before creating any resources.

The wrapper invokes `scripts/tuna-vm-perf --guest-aggregate-cpu` for all six cases.
Both desktops retain the existing equal 30 second settle and guest BOOTTIME idle
start/end markers. The collector replaces the idle sleep; the image's existing
UID sampler continues supplying CPU/RSS/PSS over that same phase. Every complete
raw collection window must lie inside the markers. Reference hashing and actual
running-process checks occur outside this measured window.

Each case preserves raw bytes, their SHA-256 and the collector source hash. The
paired validator re-derives the report from those bytes and requires all six
cases in the original alternating order, unique boots and identical observer
source, online core IDs and USER_HZ. Missing, changed, out-of-phase or invalid
receipts refuse completion. The existing 20 real action observations and native
completion validators remain separate; aggregate idle CPU does not qualify
those actions.

The standalone CLI supplies an additional equal settle and independent idle
window. It is not invoked inside the paired runner's idle phase. No image update
is required: bounded kernel-reading Python is sent through authenticated SSH.
The workflow runs source tests and CLI admission checks only. Neither this leaf
nor those checks have executed a VM comparison or qualified hardware GPU/power.
