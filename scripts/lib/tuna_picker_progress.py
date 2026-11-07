"""Observe the actual caller after one modal-input probe, without replaying it."""
import time


def observe_modal_parent(read_parent, read_scene, expected, focused_id, deadline, retain):
    # The caller has already injected the sole real click. Progress while
    # xdotool was running must not satisfy this post-injection boundary.
    baseline = read_parent()
    started = time.monotonic()
    probe_end = min(deadline, started + 2)
    receipt = {
        "before_input": expected,
        "baseline_after_input": baseline,
        "started_monotonic_ns": time.monotonic_ns(),
        "required_advances": 10,
        "inner_bound_seconds": 2,
        "observations": [],
        "result": "failed",
    }
    try:
        while time.monotonic() < probe_end:
            after = read_parent()
            scene = read_scene()
            receipt["observations"].append({
                "elapsed_seconds": time.monotonic() - started,
                "parent": after,
                "focused": scene["focused"],
            })
            if (after["clicks"] != expected["clicks"]
                    or after["keys"] != expected["keys"]
                    or scene["focused"] != focused_id):
                raise RuntimeError("Actual modal parent click leaked input or stole dialog focus")
            if after["sequence"] >= baseline["sequence"] + 10:
                receipt["result"] = "passed"
                return after, scene
            time.sleep(.05)
        raise RuntimeError("Actual parent did not process fresh events after the modal probe")
    finally:
        receipt["elapsed_seconds"] = time.monotonic() - started
        retain(receipt)
