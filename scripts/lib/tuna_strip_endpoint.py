"""Strict canonical strip endpoints; timing samples never wait past their delay."""
import math
import hashlib
import time


def endpoint(state, width, *, baseline=None, focused=None):
    if type(width) is not int or width <= 0:
        raise ValueError("positive genuine width required")
    if baseline is not None and (type(baseline) is not int or baseline < 0):
        raise ValueError("nonnegative genuine frame baseline required")
    if focused is not None and (type(focused) is not int or focused <= 0):
        raise ValueError("positive genuine expected client ID required")
    if not isinstance(state, dict) or state.get("session_mode") != "scroll":
        raise ValueError("scroll state required")
    if type(state.get("strip_animating")) is not bool:
        raise ValueError("canonical strip animation state absent")
    frames = state.get("rendered_frames")
    if type(frames) is not int or frames < 0:
        raise ValueError("actual rendered frame count absent")
    for key in ("strip_view", "strip_offset"):
        if type(state.get(key)) not in (int, float) or not math.isfinite(state[key]):
            raise ValueError("finite strip positions required")
    windows = state.get("windows")
    if not isinstance(windows, list) or any(not isinstance(w, dict) for w in windows):
        raise ValueError("window state malformed")
    if type(state.get("focused")) is not int or state["focused"] <= 0:
        raise ValueError("positive genuine focused client ID required")
    ids = [w.get("id") for w in windows]
    if any(type(value) is not int or value <= 0 for value in ids) or len(set(ids)) != len(ids):
        raise ValueError("unique genuine window IDs required")
    own = [w for w in windows if w.get("id") == state["focused"]]
    if len(own) != 1:
        raise ValueError("unique focused window absent")
    rect = own[0].get("rect")
    if not isinstance(rect, list) or len(rect) != 4 or any(type(v) is not int for v in rect):
        raise ValueError("focused window geometry malformed")
    if (state["strip_animating"] or rect[1:] != [48, width, 736]
            or (baseline is not None and frames <= baseline)
            or (focused is not None and state["focused"] != focused)):
        return False
    if abs(state["strip_view"] - state["strip_offset"]) >= 0.1:
        raise ValueError("canonical settled strip has residual outside original threshold")
    return True


def wait_endpoint(read, width, *, deadline, baseline=None, focused=None,
                  now=time.monotonic, pause=time.sleep):
    while now() < deadline:
        state = read()
        if now() >= deadline:
            raise TimeoutError("strip state reader completed after the original bound")
        if endpoint(state, width, baseline=baseline, focused=focused):
            return state
        pause(min(0.01, max(0, deadline - now())))
    raise TimeoutError("strip endpoint did not render within the original bound")


def capture_bracket(before, after, width):
    """Validate immediate state reads bracketing the image grab, not encoding."""
    if not endpoint(before, width) or not endpoint(after, width, focused=before["focused"]):
        raise ValueError("endpoint image grab was not canonically settled throughout")
    def own(state):
        return next(w for w in state["windows"] if w["id"] == state["focused"])
    if (after["rendered_frames"] < before["rendered_frames"]
            or own(before)["rect"] != own(after)["rect"]
            or before["strip_offset"] != after["strip_offset"]
            or before["strip_view"] != after["strip_view"]):
        raise ValueError("endpoint geometry/frame changed across image grab")


def capture_receipt(receipt, observed, width, image_bytes, stage):
    if not isinstance(receipt, dict) or receipt.get("width") != width or receipt.get("stage") != stage:
        raise ValueError("endpoint capture identity mismatch")
    capture_bracket(receipt.get("before"), receipt.get("after"), width)
    if receipt["after"] != observed or receipt.get("png_sha256") != hashlib.sha256(image_bytes).hexdigest():
        raise ValueError("capture bracket/state/image bytes differ")
    times = [receipt.get(k) for k in ("before_started", "capture_started", "grab_finished", "after_sampled")]
    if (any(type(t) not in (float, int) or (not math.isfinite(t) or t < 0) for t in times)
            or not all(a <= b for a, b in zip(times, times[1:]))):
        raise ValueError("capture bracket timestamps malformed or reversed")
