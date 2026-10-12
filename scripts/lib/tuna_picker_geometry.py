"""Identity-bound readiness for one real independent picker input control."""


def application_id(pid, start_time):
    return f"org.example.TunaPickerUnrelated.p{pid}.s{start_time}"


def committed_window(scene, record, *, pid, uid, start_time, parent_id):
    """Return B only after its mapped allocation matches committed placement.

    The compositor initially publishes an 800x600 placeholder. GTK being
    mapped or focused alone does not mean that placement has been replaced.
    The fixture's app id is unique to its exact PID and process lifetime.
    """
    if (record.get("pid") != pid or record.get("uid") != uid
            or record.get("start_time") != start_time
            or record.get("app_id") != application_id(pid, start_time)):
        raise RuntimeError("Independent application geometry identity changed")
    windows = [window for window in scene["windows"]
               if window.get("app_id") == application_id(pid, start_time)
               and window.get("x11_window_id") is None]
    if not record.get("mapped") or len(windows) != 1:
        return None
    window = windows[0]
    size = record.get("window_size")
    if (not isinstance(size, list) or len(size) != 2
            or not all(isinstance(value, int) and value > 0 for value in size)):
        return None
    if (window["id"] == parent_id or scene["focused"] != window["id"]
            or window["rect"][2:] != size):
        return None
    return window
