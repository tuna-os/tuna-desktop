"""Public installed GTK3 APIs for the shared real portal consumer journey.

GTK 3.24.49 builds Gdk/GdkX11 introspection, but no GdkWayland GIR:
https://github.com/GNOME/gtk/blob/3.24.49/gdk/meson.build
The ctypes bridge below only calls the installed GDK's public API on the
original PyGObject-owned GdkWindow. GDK itself exports the real wl_surface.
https://github.com/GNOME/gtk/blob/3.24.49/gdk/wayland/gdkwaylandwindow.h

This ordinary undecorated window proves portal parenting, not GTK3's optional
gtk_shell1 modal hints, titlebar gestures or shell DBus metadata integration.
Those paths explicitly return when gtk_surface is absent in GTK 3.24.49:
https://github.com/GNOME/gtk/blob/3.24.49/gdk/wayland/gdkwindow-wayland.c
Mutter 51 implements those additional semantics, not just foreign parenting:
https://github.com/GNOME/mutter/blob/51.0/src/wayland/meta-wayland-gtk-shell.c
https://github.com/GNOME/mutter/blob/51.0/src/wayland/protocol/gtk-shell.xml
"""
import ctypes
import hashlib
import os
from pathlib import Path
import subprocess


class WaylandWindow:
    """Narrow bridge for the three native GDK3 operations this journey needs."""

    _gdk = ctypes.CDLL("libgdk-3.so.0")
    _exported_callback = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_char_p, ctypes.c_void_p)
    _capsule_pointer = ctypes.pythonapi.PyCapsule_GetPointer
    _capsule_pointer.argtypes = [ctypes.py_object, ctypes.c_char_p]
    _capsule_pointer.restype = ctypes.c_void_p
    _gdk.gdk_wayland_window_set_application_id.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
    _gdk.gdk_wayland_window_set_application_id.restype = None
    _gdk.gdk_wayland_window_export_handle.argtypes = [ctypes.c_void_p, _exported_callback, ctypes.c_void_p, ctypes.c_void_p]
    _gdk.gdk_wayland_window_export_handle.restype = ctypes.c_int
    _gdk.gdk_wayland_window_unexport_handle.argtypes = [ctypes.c_void_p]
    _gdk.gdk_wayland_window_unexport_handle.restype = None
    _callbacks = {}

    @classmethod
    def pointer(cls, window):
        if window.__gtype__.name != "GdkWaylandWindow":
            raise RuntimeError("GTK3 parent is not an actual GDK Wayland window")
        return cls._capsule_pointer(window.__gpointer__, None)

    @classmethod
    def set_application_id(cls, window, app_id):
        cls._gdk.gdk_wayland_window_set_application_id(cls.pointer(window), app_id.encode())

    @classmethod
    def export_handle(cls, window, callback, data):
        pointer = cls.pointer(window)
        if pointer in cls._callbacks:
            raise RuntimeError("GTK3 proof must export its real window exactly once")
        def exported(_window, handle, _data):
            callback(window, handle.decode(), data)
        native_callback = cls._exported_callback(exported)
        cls._callbacks[pointer] = native_callback
        if not cls._gdk.gdk_wayland_window_export_handle(pointer, native_callback, None, None):
            del cls._callbacks[pointer]
            return False
        return True

    @classmethod
    def unexport_handle(cls, window):
        pointer = cls.pointer(window)
        if pointer not in cls._callbacks:
            raise RuntimeError("GTK3 proof has no actual export to revoke")
        cls._gdk.gdk_wayland_window_unexport_handle(pointer)
        del cls._callbacks[pointer]


def measured_target(window, button, paints, painted_size):
    """Measure GTK3 allocation and native coordinates; never guess CSD offsets."""
    if window.get_decorated() or not button.get_mapped() or not button.is_sensitive():
        raise RuntimeError("GTK3 parent input target is not mapped undecorated content")
    origin = button.translate_coordinates(window, 0, 0)
    if origin is None:
        raise RuntimeError("GTK3 cannot translate actual button into parent coordinates")
    bx, by = origin
    bw, bh = button.get_allocated_width(), button.get_allocated_height()
    if bw < 100 or bh < 150:
        raise RuntimeError("Original parent button bounds are too small for input proof")
    wx, wy = bx + min(50, bw / 2), by + min(100, bh / 2)
    # The sole child is a no-window GtkButton. Its event window must be the
    # actual parent GdkWindow; otherwise these are not native surface coords.
    native = window.get_window()
    if button.get_parent() != window or button.get_window() != native:
        raise RuntimeError("GTK3 measured button does not use the original native window")
    if not (bx <= wx < bx + bw and by <= wy < by + bh
            and 0 <= wx < native.get_width() and 0 <= wy < native.get_height()):
        raise RuntimeError("GTK3 actual content point lies outside its button/native buffer")
    return {"bounds": [bx, by, bw, bh], "widget_point": [wx, wy],
            "surface_transform": [0, 0], "surface_point": [wx, wy],
            "surface_size": [native.get_width(), native.get_height()],
            "picked_ancestry": [button.__gtype__.name, window.__gtype__.name],
            "measurement": "gtk3-translated-sole-child-native-window",
            "client_after_paint_count": paints, "client_last_painted_size": painted_size}


def installed_toolkit(Gtk):
    """Bind reported version to the root-owned installed libraries in this PID."""
    libraries = {}
    for line in Path("/proc/self/maps").read_text().splitlines():
        fields = line.split(maxsplit=5)
        if len(fields) != 6 or not fields[5].startswith("/"):
            continue
        path = Path(fields[5])
        for name in ("libgtk-3.so", "libgdk-3.so"):
            if path.name.startswith(name):
                libraries[name] = path.resolve(strict=True)
    if set(libraries) != {"libgtk-3.so", "libgdk-3.so"}:
        raise RuntimeError("Actual GTK3/GDK3 libraries are not mapped in the caller")
    records = []
    for name, path in sorted(libraries.items()):
        info = path.stat()
        if info.st_uid != 0 or info.st_mode & 0o022 or not str(path).startswith("/usr/"):
            raise RuntimeError("GTK3 caller is not using root-owned installed libraries")
        package = subprocess.check_output(["rpm", "-qf", str(path)], text=True, timeout=5).strip()
        records.append({"name": name, "path": str(path), "package": package,
                        "sha256": hashlib.sha256(path.read_bytes()).hexdigest()})
    version = [Gtk.get_major_version(), Gtk.get_minor_version(), Gtk.get_micro_version()]
    if version[0] != 3 or version < [3, 24, 0]:
        raise RuntimeError("Installed GTK 3.24 or newer required for application identity API")
    return {"toolkit": "gtk3", "version": version, "pid": os.getpid(),
            "uid": os.getuid(), "libraries": records, "decorated": False,
            "qualification_scope": "ordinary GTK3 Wayland parent to installed GNOME/Nautilus FileChooser",
            "not_qualified": ["gtk_shell1 own-modal hints", "gtk_shell1 titlebar gestures",
                              "gtk_shell1 shell DBus metadata", "forced xdg-foreign version"]}
