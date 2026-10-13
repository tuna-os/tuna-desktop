"""Read-only guest aggregate counters; preserves existing UID/PSS metrics.

/proc/stat semantics: https://docs.kernel.org/filesystems/proc.html
Guest columns are already in user/nice (kernel/sched/cputime.c).
I/O wait can regress; it is never silently clamped to zero. CPU rows must
match online IDs exactly; kernels exposing extra configured/offline rows are
conservatively refused instead of inventing active CPU capacity.
"""
import math
import re
import statistics
import hashlib
import json
import shlex
from pathlib import Path

FIELDS = ('user', 'nice', 'system', 'idle', 'iowait', 'irq', 'softirq', 'steal', 'guest', 'guest_nice')
BUSY = ('user', 'nice', 'system', 'irq', 'softirq')
LIMIT = 2 * 1024 * 1024


def online_ids(text):
    if not isinstance(text, str) or len(text) > 32768:raise ValueError('online CPU list bound')
    result = set()
    for part in text.strip().split(','):
        match = re.fullmatch(r'(\d+)(?:-(\d+))?', part)
        if not match:raise ValueError('invalid online CPU list')
        first = int(match[1]);last = int(match[2] or match[1])
        if not 0 <= first <= last < 4096:raise ValueError('online CPU range refused')
        values = set(range(first, last + 1))
        if result & values:raise ValueError('duplicate online CPU id')
        result.update(values)
    return sorted(result)


def stat_counters(text, online):
    if not isinstance(text, str) or len(text) > LIMIT:raise ValueError('proc stat bound')
    rows = {}
    for line in text.splitlines():
        values = line.split()
        if not values or not re.fullmatch(r'cpu(?:\d+)?', values[0]):continue
        if values[0] in rows or len(values) != len(FIELDS) + 1:raise ValueError('CPU field schema refused')
        if any(not re.fullmatch(r'\d+', v) or int(v) >= 2**64 for v in values[1:]):raise ValueError('invalid CPU counter')
        row = dict(zip(FIELDS, map(int, values[1:])))
        if row['guest'] > row['user'] or row['guest_nice'] > row['nice']:raise ValueError('guest CPU subset invalid')
        rows[values[0]] = row
    if 'cpu' not in rows:raise ValueError('aggregate CPU line missing')
    ids = sorted(int(name[3:]) for name in rows if name != 'cpu')
    if ids != online:raise ValueError('proc stat/online CPU topology differs')
    return rows


def memory_capacity(text):
    if not isinstance(text, str) or len(text) > LIMIT:raise ValueError('meminfo bound')
    result = {}
    for line in text.splitlines():
        if line.startswith(('MemTotal:', 'MemAvailable:')):
            match = re.fullmatch(r'(MemTotal|MemAvailable):\s+(\d+) kB', line)
            if not match or match[1] in result:raise ValueError('invalid memory capacity field')
            result[match[1]] = int(match[2])
    if set(result) != {'MemTotal', 'MemAvailable'} or not 0 < result['MemTotal'] < 2**64 or not 0 <= result['MemAvailable'] <= result['MemTotal']:
        raise ValueError('memory capacity estimate missing or invalid')
    return {'total_kib': result['MemTotal'], 'available_estimate_kib': result['MemAvailable'],
            'used_estimate_kib': result['MemTotal'] - result['MemAvailable'],
            'scope': 'Kernel MemAvailable capacity estimate; neither process PSS nor unique allocated bytes.'}


def parse_sample(value):
    if not isinstance(value, dict):raise ValueError('guest sample missing')
    boot = value.get('boot_id')
    if not isinstance(boot, str) or not re.fullmatch(r'[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', boot) or value.get('boot_after') != boot:
        raise ValueError('guest sample boot identity differs')
    for field in ('begin_monotonic_ns', 'cpu_monotonic_ns', 'end_monotonic_ns', 'hz'):
        if type(value.get(field)) is not int or not 0 < value[field] < 2**64:raise ValueError('invalid guest clock/HZ')
    if not value['begin_monotonic_ns'] <= value['cpu_monotonic_ns'] <= value['end_monotonic_ns']:raise ValueError('guest collection clock order')
    ids = online_ids(value.get('online_before'));after = online_ids(value.get('online_after'))
    if ids != after:raise ValueError('CPU topology changed during collection')
    return {**value, 'core_ids': ids, 'counters': stat_counters(value.get('proc_stat'), ids),
            'memory_capacity': memory_capacity(value.get('meminfo'))}


def delta(before, after):
    if before['boot_id'] != after['boot_id'] or before['core_ids'] != after['core_ids'] or before['hz'] != after['hz']:
        raise ValueError('guest boot/topology/HZ drift')
    elapsed = (after['cpu_monotonic_ns'] - before['cpu_monotonic_ns']) / 1e9
    if elapsed <= 0 or before['end_monotonic_ns'] >= after['begin_monotonic_ns']:raise ValueError('nonpositive/overlapping sample interval')
    differences = {}
    for cpu in before['counters']:
        differences[cpu] = {f: after['counters'][cpu][f] - before['counters'][cpu][f] for f in FIELDS}
        if any(differences[cpu][f] < 0 for f in FIELDS if f != 'iowait'):raise ValueError('CPU counter reset/regression')
    diff = differences['cpu'];busy = sum(diff[f] for f in BUSY)
    denominator = before['hz'] * elapsed
    capacity = denominator * len(before['core_ids'])
    io_regressed = any(row['iowait'] < 0 for row in differences.values())
    total = sum(diff[f] for f in FIELDS[:8])  # guest/guest_nice already included.
    if total <= 0:raise ValueError('aggregate CPU interval did not advance')
    unavailable = {'status': 'unavailable', 'reason': 'kernel iowait counter regressed; not clamped'}
    return {'begin_cpu_monotonic_ns': before['cpu_monotonic_ns'], 'end_cpu_monotonic_ns': after['cpu_monotonic_ns'],
            'elapsed_s': elapsed, 'core_ids': before['core_ids'], 'hz': before['hz'], 'counter_deltas': differences,
            'busy_one_core_percent': 100 * busy / denominator,
            'busy_guest_capacity_percent': 100 * busy / capacity,
            'steal_guest_capacity_percent': 100 * diff['steal'] / capacity,
            'iowait': unavailable if io_regressed else {'status': 'available', 'guest_capacity_percent': 100 * diff['iowait'] / capacity},
            'busy_accounted_time_fraction': unavailable if io_regressed else {'status': 'available', 'percent': 100 * busy / total}}


def summarize(raw, expected_boot):
    if not isinstance(raw, list) or len(raw) != 31:raise ValueError('thirty complete guest intervals required')
    parsed = [parse_sample(v) for v in raw]
    if any(v['boot_id'] != expected_boot for v in parsed):raise ValueError('selected boot differs from guest samples')
    intervals = [delta(a, b) for a, b in zip(parsed, parsed[1:])]
    duration = (parsed[-1]['cpu_monotonic_ns']-parsed[0]['cpu_monotonic_ns'])/1e9
    if not 29.5 <= duration <= 33:raise ValueError('fixed thirty-second collection window differs')
    def distribution(key):
        values = sorted(v[key] for v in intervals)
        if any(not math.isfinite(v) for v in values):raise ValueError('nonfinite CPU metric')
        return {'samples': len(values), 'min': values[0], 'median': statistics.median(values), 'max': values[-1]}
    return {'schema': 1, 'boot_id': expected_boot, 'core_ids': parsed[0]['core_ids'], 'hz': parsed[0]['hz'],
            'intervals': intervals, 'busy_one_core_percent': distribution('busy_one_core_percent'),
            'busy_guest_capacity_percent': distribution('busy_guest_capacity_percent'),
            'steal_guest_capacity_percent': distribution('steal_guest_capacity_percent'),
            'iowait_unavailable_intervals': sum(v['iowait']['status'] != 'available' for v in intervals),
            'memory_capacity_samples': [v['memory_capacity'] for v in parsed],
            'scope': 'Read-only guest idle window. All guest UIDs/kernel/collector/SSH overhead included; no whole-host CPU or action-completion coverage.',
            'existing_uid_cpu_and_pss_unchanged': True, 'hardware_gpu_qualified': False, 'total_power_qualified': False}


COLLECT = r'''import json,pathlib,os,sys,time
args=json.loads(sys.argv[1]);seconds=args['seconds']
if type(seconds) is not int or seconds!=30:raise ValueError('fixed thirty-second idle window required')
def read(path,limit=2097152):
 with open(path) as stream:text=stream.read(limit+1)
 if len(text)>limit:raise ValueError('kernel file bound')
 return text
rows=[];start=time.monotonic();deadline=start+seconds+3
for index in range(seconds+1):
 if time.monotonic()>deadline:raise TimeoutError('guest collection deadline')
 begin=time.monotonic_ns();boot=read('/proc/sys/kernel/random/boot_id').strip()
 online=read('/sys/devices/system/cpu/online');text=read('/proc/stat');at=time.monotonic_ns()
 stat='\n'.join(line for line in text.splitlines() if line.split() and (line.split()[0].startswith('cpu') or line.startswith('btime ')))
 mem=read('/proc/meminfo');online_after=read('/sys/devices/system/cpu/online');boot_after=read('/proc/sys/kernel/random/boot_id').strip();end=time.monotonic_ns()
 if time.monotonic()>deadline:raise TimeoutError('guest collection deadline')
 rows.append({'begin_monotonic_ns':begin,'cpu_monotonic_ns':at,'end_monotonic_ns':end,'boot_id':boot,'boot_after':boot_after,'online_before':online,'online_after':online_after,'proc_stat':stat,'meminfo':mem,'hz':os.sysconf('SC_CLK_TCK')})
 if index<seconds:time.sleep(max(0,start+index+1-time.monotonic()))
print(json.dumps(rows))
'''


def collect_idle(session, out):
    """Host-only hook for the existing paired runner's guest idle markers.

    Caller must admit original image/key/reference/selected process first, then
    apply its equal 30 second settle. Replace its 30 second idle sleep with this same collector
    for BOTH desktops, bracketed by the existing guest clock phase markers.
    No references, input or expensive package hashing are performed in-window.
    """
    command = '/usr/bin/timeout --signal=TERM --kill-after=2s 35s python3 -c ' + shlex.quote(COLLECT) + ' ' + shlex.quote(json.dumps({'seconds': 30}))
    payload = session.remote(command, timeout=42)
    path = Path(out) / 'guest-aggregate-raw.json'
    path.write_bytes(payload)  # Retain even invalid/missing-counter output.
    raw = json.loads(payload)
    summary = summarize(raw, session.boot_id)
    summary['raw_sha256'] = hashlib.sha256(payload).hexdigest()
    summary['collector_source_sha256'] = hashlib.sha256(COLLECT.encode()).hexdigest()
    summary['guest_window_begin_monotonic_ns'] = raw[0]['begin_monotonic_ns']
    summary['guest_window_end_monotonic_ns'] = raw[-1]['end_monotonic_ns']
    return summary

