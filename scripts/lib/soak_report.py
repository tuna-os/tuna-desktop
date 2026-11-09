#!/usr/bin/python3
"""Validate a retained tuna-vm-soak directory; summarize memory and frame pacing over time."""
import json
from pathlib import Path
import statistics
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from endurance_presentation import Reader  # noqa: E402

QUALIFICATION_S = 86400
MAX_ROWS = 60
MIB = 1024


def slope_per_hour(points):
    """Least-squares slope of (elapsed_s, value) points, per hour."""
    if len(points) < 2:
        return None
    xs, ys = zip(*points)
    mean_x, mean_y = statistics.fmean(xs), statistics.fmean(ys)
    spread = sum((x - mean_x) ** 2 for x in xs)
    if spread == 0:
        return None
    return sum((x - mean_x) * (y - mean_y) for x, y in zip(xs, ys)) / spread * 3600


def rows(samples, limit=MAX_ROWS):
    """Every sample, or evenly spaced ones that keep the first and last."""
    if len(samples) <= limit:
        return list(samples)
    picks = sorted({round(i * (len(samples) - 1) / (limit - 1)) for i in range(limit)})
    return [samples[i] for i in picks]


def memory(samples, baseline):
    keys = ('pss_kib', 'rss_kib', 'fds', 'processes')
    if baseline is None or not samples:
        return None
    after = [s for s in samples if s['elapsed_s'] >= baseline['elapsed_s']]
    final = samples[-1]
    # The observer's growth limits (tuna-vm-soak), recomputed for the report.
    limits = {'pss_kib': baseline['pss_kib'] * 1.5 + 16384,
              'fds': baseline['fds'] * 1.5 + 32,
              'processes': baseline['processes'] + 12}
    return {'baseline_elapsed_s': baseline['elapsed_s'],
            'baseline': {key: baseline[key] for key in keys},
            'final': {key: final[key] for key in keys},
            'peak_after_warmup': {key: max(s[key] for s in after) for key in keys},
            'limits': limits,
            'pss_slope_kib_per_h': slope_per_hour([(s['elapsed_s'], s['pss_kib']) for s in after]),
            'samples_after_warmup': len(after)}


def summarize(directory):
    directory = Path(directory)
    problems = []
    provenance = json.loads((directory / 'provenance.json').read_text())
    seconds = provenance['requested_duration_s']
    try:
        report = json.loads((directory / 'report.json').read_text())
    except (OSError, ValueError) as error:
        report = {'complete': False, 'failure': f'no observer report: {error}'}
    samples = []
    path = directory / 'samples.jsonl'
    if path.exists():
        samples = [json.loads(line) for line in path.read_text().splitlines()]
    if not report.get('complete'):
        problems.append(f"observer did not complete: {report.get('failure', 'no failure recorded')}")
    elif report.get('elapsed_s', 0) < seconds:
        problems.append('observer completed before the requested duration')
    if report.get('samples') is not None and report['samples'] != len(samples):
        problems.append(f"report counts {report['samples']} samples; samples.jsonl has {len(samples)}")
    if provenance.get('scope') != report.get('scope'):
        problems.append('report and provenance disagree on scope')
    presentation = None
    if 'presentation_workload' in provenance:
        # Re-validate the whole retained capture independently of the observer.
        try:
            with (directory / 'presentation.jsonl').open('rb') as stream:
                presentation = Reader(stream, seconds).finish()
        except (OSError, ValueError) as error:
            problems.append(f'presentation capture does not qualify: {error}')
    return {'scope': provenance.get('scope'), 'requested_duration_s': seconds,
            'warmup_s': provenance['warmup_s'], 'interval_s': provenance['interval_s'],
            'source_revision': provenance['source_revision'], 'boot_id': provenance['boot_id'],
            'complete': not problems, 'qualifies_24h': not problems and seconds == QUALIFICATION_S,
            'problems': problems, 'samples': len(samples),
            'elapsed_s': report.get('elapsed_s'),
            'memory': memory(samples, report.get('baseline')),
            'presentation': presentation,
            'over_time': [{'elapsed_s': s['elapsed_s'], 'pss_kib': s['pss_kib'], 'rss_kib': s['rss_kib'],
                           'fds': s['fds'], 'processes': s['processes'],
                           'interval_ms': s.get('presentation', {}).get('interval_ms'),
                           'frames': s.get('presentation', {}).get('frames')}
                          for s in samples]}


def mib(kib):
    return f'{kib / MIB:.1f}'


def ms(distribution, key):
    value = (distribution or {}).get(key)
    return '–' if value is None else f'{value:.2f}'


def markdown(summary):
    lines = [f"### Tuna VM soak: {summary['scope']}", '']
    if summary['complete']:
        lines.append('Result: **complete**' + ('' if summary['qualifies_24h']
                                                 else ' (bounded run; not 24-hour evidence)'))
    else:
        lines.append('Result: **FAILED**: ' + '; '.join(summary['problems']))
    elapsed = summary['elapsed_s']
    lines += ['', f"Source revision `{summary['source_revision']}`, guest boot `{summary['boot_id']}`; "
              f"requested {summary['requested_duration_s']} s, observed "
              f"{'–' if elapsed is None else f'{elapsed:.0f}'} s, warmup {summary['warmup_s']} s, "
              f"interval {summary['interval_s']} s, {summary['samples']} samples.", '']
    memory_summary = summary['memory']
    if memory_summary:
        base, final, peak, limits = (memory_summary[k] for k in ('baseline', 'final', 'peak_after_warmup', 'limits'))
        lines += [f"| Session UID | Baseline (t={memory_summary['baseline_elapsed_s']:.0f} s) | Final | "
                  'Peak after warmup | Limit |', '|---|---|---|---|---|',
                  f"| PSS MiB | {mib(base['pss_kib'])} | {mib(final['pss_kib'])} | {mib(peak['pss_kib'])} | "
                  f"{mib(limits['pss_kib'])} |",
                  f"| RSS MiB | {mib(base['rss_kib'])} | {mib(final['rss_kib'])} | {mib(peak['rss_kib'])} | – |",
                  f"| fds | {base['fds']} | {final['fds']} | {peak['fds']} | {limits['fds']:.0f} |",
                  f"| processes | {base['processes']} | {final['processes']} | {peak['processes']} | "
                  f"{limits['processes']} |", '']
        slope = memory_summary['pss_slope_kib_per_h']
        if slope is not None:
            lines += [f"PSS trend after warmup: {slope / MIB:+.2f} MiB/h (least squares over "
                      f"{memory_summary['samples_after_warmup']} samples).", '']
    else:
        lines += ['No post-warmup baseline was reached.', '']
    presentation = summary['presentation']
    if presentation:
        interval = presentation['interval_ms']
        lines += [f"Native 320×180 client frame pacing: {presentation['total_frames']} presented frames in "
                  f"{presentation['batches']} batches; whole-run interval p50 {ms(interval, 'p50')} ms, "
                  f"p95 {ms(interval, 'p95')} ms, p99 {ms(interval, 'p99')} ms, max {ms(interval, 'max')} ms; "
                  f"unknown refresh on {presentation['unknown_refresh']} frames.", '']
    lines += ['| t (min) | PSS MiB | RSS MiB | fds | procs | frames | p50 ms | p95 ms | p99 ms |',
              '|---|---|---|---|---|---|---|---|---|']
    for row in rows(summary['over_time']):
        frames = row['frames']
        lines.append(f"| {row['elapsed_s'] / 60:.1f} | {mib(row['pss_kib'])} | {mib(row['rss_kib'])} | "
                     f"{row['fds']} | {row['processes']} | {'–' if frames is None else frames} | "
                     f"{ms(row['interval_ms'], 'p50')} | {ms(row['interval_ms'], 'p95')} | "
                     f"{ms(row['interval_ms'], 'p99')} |")
    if len(summary['over_time']) > MAX_ROWS:
        lines.append(f"\n{MAX_ROWS} of {len(summary['over_time'])} samples shown; samples.jsonl has all.")
    return '\n'.join(lines) + '\n'


def write(directory):
    """Write summary.json and summary.md beside the retained soak files."""
    directory = Path(directory)
    summary = summarize(directory)
    (directory / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    text = markdown(summary)
    (directory / 'summary.md').write_text(text)
    return summary, text


if __name__ == '__main__':
    if len(sys.argv) != 2:
        sys.exit('usage: soak_report.py SOAK_DIRECTORY')
    result, text = write(sys.argv[1])
    print(text, end='')
    sys.exit(0 if result['complete'] else 1)
