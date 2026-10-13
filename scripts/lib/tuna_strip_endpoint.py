"""Strict canonical strip endpoints; timing samples never wait past their delay."""
import math
import time


def endpoint(state, width, *, baseline=None, focused=None):
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
    own = [w for w in windows if w.get("id") == state.get("focused")]
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
        if endpoint(state, width, baseline=baseline, focused=focused):
            return state
        pause(min(0.01, max(0, deadline - now())))
    raise TimeoutError("strip endpoint did not render within the original bound")
