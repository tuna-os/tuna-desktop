#!/usr/bin/env python3
"""Bounded complete-token Xvfb admission; no GUI or authentication qualification."""
import json
import os
from pathlib import Path
import sys
import time


def process_alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def wait_display(path, pid, *, clock=time.monotonic, pause=time.sleep,
                 alive=process_alive, read=None, budget=5.0):
    deadline = clock() + budget
    observed = b""
    def read_file():
        with Path(path).open("rb") as stream:
            return stream.read(65)
    reader = read or read_file
    while clock() < deadline:
        if not alive(pid):
            return {"admitted": False, "reason": "xvfb_exited", "pid": pid,
                    "display_hex": observed.hex()}
        observed = reader()
        after = clock()
        if after >= deadline:
            break
        if len(observed) > 64:
            return {"admitted": False, "reason": "oversized_token", "pid": pid,
                    "display_hex": observed[:64].hex()}
        if observed.endswith(b"\n"):
            token = observed[:-1]
            if not token or not token.isdigit():
                return {"admitted": False, "reason": "invalid_token", "pid": pid,
                        "display_hex": observed.hex()}
            if not alive(pid) or clock() >= deadline:
                break
            return {"admitted": True, "reason": "complete_token", "pid": pid,
                    "display_hex": observed.hex(), "display": token.decode("ascii")}
        pause(min(0.1, max(0.0, deadline - after)))
    return {"admitted": False, "reason": "deadline", "pid": pid,
            "display_hex": observed[:64].hex()}


def main():
    path, raw_pid, output = sys.argv[1:]
    pid = int(raw_pid)
    if pid <= 0:
        raise ValueError("positive owned Xvfb PID required")
    try:
        receipt = wait_display(path, pid)
    except Exception as error:
        receipt = {"admitted": False, "reason": "observer_error",
                   "error_type": type(error).__name__, "pid": pid}
    Path(output).write_text(json.dumps(receipt, sort_keys=True) + "\n")
    if receipt["admitted"]:
        print(receipt["display"])
        return 0
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
