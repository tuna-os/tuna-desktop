"""Identity-bound readiness for one real independent picker input control."""


def application_id(pid, start_time):
    return f"org.example.TunaPickerUnrelated.p{pid}.s{start_time}"


def new_parent_window(scene, previous_ids, *, mode, xid, app_id):
    """Admit one newly mapped parent, never a prior caller's recycled XID.

    Snapshot compositor IDs before creating the GTK parent. Xwayland can reuse
    an XID while the compositor's last published scene still contains the old
    window. A compositor ID already present before creation cannot identify
    this new caller. This is initial admission only; the admitted identity stays
    fixed throughout all later input, parenting and ownership assertions.
    """
    if mode not in ("x11", "wayland"):
        raise ValueError("Unknown picker parent mode")
    if mode == "x11" and (type(xid) is not int or xid <= 0):
        return None
    if mode == "wayland" and (not isinstance(app_id, str) or not app_id):
        return None
    candidates = [window for window in scene["windows"]
                  if type(window.get("id")) is int and window["id"] > 0
                  and window["id"] not in previous_ids
                  and (window.get("x11_window_id") == xid if mode == "x11"
                       else window.get("app_id") == app_id
                       and window.get("x11_window_id") is None)]
    return candidates[0] if len(candidates) == 1 else None


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
