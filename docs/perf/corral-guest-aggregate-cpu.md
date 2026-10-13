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

The guest returns 31 raw reads, with monotonic nanosecond timestamps bracketing
collection, online CPU IDs, USER_HZ, CPU counters and MemTotal/MemAvailable. The
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

The existing Corral paired runner in PR #599 selects three alternating pairs with
fresh boots and the same immutable image/domain/node. Its resource runner already
performs an equal 30 second settle, then brackets a 30 second idle interval with guest
clock markers before the 20 action workload.

The new host helper exposes `corral_guest_cpu.collect_idle(session,out)`. For
both desktop cases, after original admission/selected-process checks and the
existing equal settle, the runner should keep `guest_phases['idle_start']`, call
this hook **in place of the 30 second idle sleep**, then keep `guest_phases['idle_end']`.
Reference hashing and selected-process checks belong outside this window. It
should retain the returned summary beside its existing UID/PSS metrics, check
identical online core IDs/HZ across the six paired cases, and reject failed or
missing aggregate receipts. The image's existing sampler continues supplying
UID/PSS data over that same guest-clock interval.

This leaf supplies and tests the hook and standalone observer. It does not claim
that PR #599's paired runner already calls it. The standalone CLI's additional
settle must not be invoked inside the existing runner's measured idle interval.
No image update is required: the bounded kernel-reading Python source is sent
through the already authenticated SSH transport.
