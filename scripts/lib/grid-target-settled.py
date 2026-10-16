#!/usr/bin/env python3
"""Observe an owned grid target until fresh bounds stop moving; inject no input."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import time


def target_bounds(nodes, name, pid, origin, viewport):
    if not nodes or nodes[0].get('process_id') != pid:
        return None
    matches = [n for n in nodes if n.get('showing') and n.get('name') == name
               and n.get('role') in ('push button', 'button')]
    if len(matches) != 1:
        return None
    bounds = matches[0].get('bounds')
    if not isinstance(bounds, list) or len(bounds) != 4 or any(type(v) is not int for v in bounds):
        return None
    x, y, width, height = bounds
    x += origin[0]
    y += origin[1]
    if width <= 0 or height <= 0 or x < 0 or y < 0 or x + width > viewport[0] or y + height > viewport[1]:
        return None
    return tuple(bounds)


def wait_for_target(capture, *, timeout, clock=time.monotonic, sleep=time.sleep, record=lambda _: None):
    deadline = clock() + timeout
    previous, since = None, None
    while clock() < deadline:
        remaining = deadline - clock()
        bounds = capture(remaining)
        now = clock()
        record({'elapsed': timeout - (deadline - now), 'bounds': bounds})
        if now >= deadline:
            break
        if bounds is None:
            previous, since = None, None
        elif bounds != previous:
            previous, since = bounds, now
        elif now - since >= 0.2:
            return bounds
        remaining = deadline - clock()
        if remaining > 0:
            sleep(min(0.2, remaining))
    raise TimeoutError('grid target did not settle before the absolute deadline')


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--dump', required=True)
    p.add_argument('--out', type=Path, required=True)
    p.add_argument('--receipt', type=Path, required=True)
    p.add_argument('--name', required=True)
    p.add_argument('--pid', type=int, required=True)
    p.add_argument('--origin', type=int, nargs=2, required=True)
    p.add_argument('--viewport', type=int, nargs=2, required=True)
    p.add_argument('--timeout', type=float, default=10)
    a = p.parse_args()
    if a.pid <= 0 or not 0 < a.timeout <= 10 or any(v <= 0 for v in a.viewport):
        p.error('invalid PID, viewport or bounded deadline')
    receipt = {'qualified': False, 'pid': a.pid, 'name': a.name, 'origin': a.origin,
               'viewport': a.viewport, 'deadline_seconds': a.timeout, 'samples': []}

    def record(sample):
        receipt['samples'].append(sample)
        a.receipt.write_text(json.dumps(receipt, indent=2))

    record({'phase': 'awaiting-fresh-owned-bounds'})

    def capture(remaining):
        # No old successful dump can qualify a failed fresh attempt. The dump
        # has no child processes; subprocess timeout kills and reaps it even
        # when an AT-SPI call blocks past its own registration timeout.
        a.out.unlink(missing_ok=True)
        try:
            result = subprocess.run([sys.executable, a.dump, 'tuna-shell-gtk', str(a.out),
                                     str(remaining), '--pid', str(a.pid)],
                                    timeout=remaining, stdout=subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL, check=False)
            if result.returncode != 0:
                return None
            return target_bounds(json.loads(a.out.read_text()), a.name, a.pid, a.origin, a.viewport)
        except (subprocess.TimeoutExpired, OSError, ValueError, TypeError, AttributeError):
            return None

    try:
        bounds = wait_for_target(capture, timeout=a.timeout, record=record)
    except TimeoutError as e:
        print(str(e), file=sys.stderr)
        return 1
    receipt.update(qualified=True, bounds=bounds,
                   final_dump_sha256=hashlib.sha256(a.out.read_bytes()).hexdigest())
    a.receipt.write_text(json.dumps(receipt, indent=2))
    print(f'{bounds[0] + a.origin[0] + 4} {bounds[1] + a.origin[1] + bounds[3] // 2}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
