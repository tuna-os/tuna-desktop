#!/usr/bin/env python3
"""Read every connector's CRTC and mode straight from the kernel (P-DS-02).

Usage: drm_kms_readback.py /dev/dri/cardN

Prints JSON: a list of {"connector": "Virtual-1", "connection":
"connected"|"disconnected"|"unknown", "crtc": id|null, "enabled": bool,
"active": bool, "fb": id, "mode": "WxH"|null, "refresh": Hz|null,
"position": [x, y]} per connector, in the kernel's connector order.
"enabled" is the CRTC's mode_valid (its atomic `enable`), "active" its
atomic ACTIVE property, "fb" the primary plane's framebuffer. A connector
the compositor switched off has no CRTC. Uses only the KMS GET ioctls and
DRM_CLIENT_CAP_ATOMIC (no DRM master, no libdrm), so the readback is
independent of the compositor that programmed it. Run it as root or as a
member of the card's group.
"""

import ctypes
import fcntl
import json
import os
import sys

DRM_MODE_OBJECT_CRTC = 0xCCCCCCCC
DRM_CLIENT_CAP_UNIVERSAL_PLANES = 2
DRM_CLIENT_CAP_ATOMIC = 3
# include/uapi/drm/drm_mode.h DRM_MODE_CONNECTOR_*, the names the kernel
# gives connectors in sysfs (drm_connector_enum_list).
CONNECTOR_TYPES = {
    0: "Unknown", 1: "VGA", 2: "DVI-I", 3: "DVI-D", 4: "DVI-A", 5: "Composite",
    6: "SVIDEO", 7: "LVDS", 8: "Component", 9: "DIN", 10: "DP", 11: "HDMI-A",
    12: "HDMI-B", 13: "TV", 14: "eDP", 15: "Virtual", 16: "DSI", 17: "DPI",
    18: "Writeback", 19: "SPI", 20: "USB",
}
CONNECTION = {1: "connected", 2: "disconnected", 3: "unknown"}


def iowr(nr, size):
    return (3 << 30) | (size << 16) | (ord("d") << 8) | nr


def iow(nr, size):
    return (1 << 30) | (size << 16) | (ord("d") << 8) | nr


class CardRes(ctypes.Structure):
    _fields_ = [
        ("fb_id_ptr", ctypes.c_uint64),
        ("crtc_id_ptr", ctypes.c_uint64),
        ("connector_id_ptr", ctypes.c_uint64),
        ("encoder_id_ptr", ctypes.c_uint64),
        ("count_fbs", ctypes.c_uint32),
        ("count_crtcs", ctypes.c_uint32),
        ("count_connectors", ctypes.c_uint32),
        ("count_encoders", ctypes.c_uint32),
        ("min_width", ctypes.c_uint32),
        ("max_width", ctypes.c_uint32),
        ("min_height", ctypes.c_uint32),
        ("max_height", ctypes.c_uint32),
    ]


class ModeInfo(ctypes.Structure):
    _fields_ = [
        ("clock", ctypes.c_uint32),
        ("hdisplay", ctypes.c_uint16),
        ("hsync_start", ctypes.c_uint16),
        ("hsync_end", ctypes.c_uint16),
        ("htotal", ctypes.c_uint16),
        ("hskew", ctypes.c_uint16),
        ("vdisplay", ctypes.c_uint16),
        ("vsync_start", ctypes.c_uint16),
        ("vsync_end", ctypes.c_uint16),
        ("vtotal", ctypes.c_uint16),
        ("vscan", ctypes.c_uint16),
        ("vrefresh", ctypes.c_uint32),
        ("flags", ctypes.c_uint32),
        ("type", ctypes.c_uint32),
        ("name", ctypes.c_char * 32),
    ]


class Crtc(ctypes.Structure):
    _fields_ = [
        ("set_connectors_ptr", ctypes.c_uint64),
        ("count_connectors", ctypes.c_uint32),
        ("crtc_id", ctypes.c_uint32),
        ("fb_id", ctypes.c_uint32),
        ("x", ctypes.c_uint32),
        ("y", ctypes.c_uint32),
        ("gamma_size", ctypes.c_uint32),
        ("mode_valid", ctypes.c_uint32),
        ("mode", ModeInfo),
    ]


class Encoder(ctypes.Structure):
    _fields_ = [
        ("encoder_id", ctypes.c_uint32),
        ("encoder_type", ctypes.c_uint32),
        ("crtc_id", ctypes.c_uint32),
        ("possible_crtcs", ctypes.c_uint32),
        ("possible_clones", ctypes.c_uint32),
    ]


class Connector(ctypes.Structure):
    _fields_ = [
        ("encoders_ptr", ctypes.c_uint64),
        ("modes_ptr", ctypes.c_uint64),
        ("props_ptr", ctypes.c_uint64),
        ("prop_values_ptr", ctypes.c_uint64),
        ("count_modes", ctypes.c_uint32),
        ("count_props", ctypes.c_uint32),
        ("count_encoders", ctypes.c_uint32),
        ("encoder_id", ctypes.c_uint32),
        ("connector_id", ctypes.c_uint32),
        ("connector_type", ctypes.c_uint32),
        ("connector_type_id", ctypes.c_uint32),
        ("connection", ctypes.c_uint32),
        ("mm_width", ctypes.c_uint32),
        ("mm_height", ctypes.c_uint32),
        ("subpixel", ctypes.c_uint32),
        ("pad", ctypes.c_uint32),
    ]


class ObjGetProperties(ctypes.Structure):
    _fields_ = [
        ("props_ptr", ctypes.c_uint64),
        ("prop_values_ptr", ctypes.c_uint64),
        ("count_props", ctypes.c_uint32),
        ("obj_id", ctypes.c_uint32),
        ("obj_type", ctypes.c_uint32),
    ]


class GetProperty(ctypes.Structure):
    _fields_ = [
        ("values_ptr", ctypes.c_uint64),
        ("enum_blob_ptr", ctypes.c_uint64),
        ("prop_id", ctypes.c_uint32),
        ("flags", ctypes.c_uint32),
        ("name", ctypes.c_char * 32),
        ("count_values", ctypes.c_uint32),
        ("count_enum_blobs", ctypes.c_uint32),
    ]


class SetClientCap(ctypes.Structure):
    _fields_ = [("capability", ctypes.c_uint64), ("value", ctypes.c_uint64)]


SET_CLIENT_CAP = iow(0x0D, ctypes.sizeof(SetClientCap))
GETRESOURCES = iowr(0xA0, ctypes.sizeof(CardRes))
GETCRTC = iowr(0xA1, ctypes.sizeof(Crtc))
GETENCODER = iowr(0xA6, ctypes.sizeof(Encoder))
GETCONNECTOR = iowr(0xA7, ctypes.sizeof(Connector))
GETPROPERTY = iowr(0xAA, ctypes.sizeof(GetProperty))
OBJ_GETPROPERTIES = iowr(0xB9, ctypes.sizeof(ObjGetProperties))


def mode_name(mode):
    """`WxH` (with `i` when interlaced) of a valid drm_mode_modeinfo."""
    interlaced = "i" if mode.flags & 0x10 else ""  # DRM_MODE_FLAG_INTERLACE
    return f"{mode.hdisplay}x{mode.vdisplay}{interlaced}"


def connector_name(connector_type, type_id):
    return f"{CONNECTOR_TYPES.get(connector_type, 'Unknown')}-{type_id}"


def resources(fd):
    res = CardRes()
    fcntl.ioctl(fd, GETRESOURCES, res)
    crtcs = (ctypes.c_uint32 * res.count_crtcs)()
    connectors = (ctypes.c_uint32 * res.count_connectors)()
    full = CardRes(crtc_id_ptr=ctypes.addressof(crtcs), count_crtcs=res.count_crtcs,
                   connector_id_ptr=ctypes.addressof(connectors),
                   count_connectors=res.count_connectors)
    fcntl.ioctl(fd, GETRESOURCES, full)
    return list(crtcs)[: full.count_crtcs], list(connectors)[: full.count_connectors]


def crtc_properties(fd, crtc):
    req = ObjGetProperties(obj_id=crtc, obj_type=DRM_MODE_OBJECT_CRTC)
    fcntl.ioctl(fd, OBJ_GETPROPERTIES, req)
    n = req.count_props
    props = (ctypes.c_uint32 * n)()
    values = (ctypes.c_uint64 * n)()
    req = ObjGetProperties(props_ptr=ctypes.addressof(props),
                           prop_values_ptr=ctypes.addressof(values), count_props=n,
                           obj_id=crtc, obj_type=DRM_MODE_OBJECT_CRTC)
    fcntl.ioctl(fd, OBJ_GETPROPERTIES, req)
    out = {}
    for prop, value in zip(props, values):
        info = GetProperty(prop_id=prop)
        fcntl.ioctl(fd, GETPROPERTY, info)
        out[info.name.decode()] = value
    return out


def readback(path):
    fd = os.open(path, os.O_RDONLY | os.O_CLOEXEC)
    try:
        # ACTIVE is an atomic property: only atomic clients see it.
        fcntl.ioctl(fd, SET_CLIENT_CAP, SetClientCap(DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1))
        fcntl.ioctl(fd, SET_CLIENT_CAP, SetClientCap(DRM_CLIENT_CAP_ATOMIC, 1))
        _crtcs, connectors = resources(fd)
        result = []
        for connector_id in connectors:
            # count_modes stays 0: a non-master caller never probes.
            info = Connector(connector_id=connector_id)
            fcntl.ioctl(fd, GETCONNECTOR, info)
            row = {
                "connector": connector_name(info.connector_type, info.connector_type_id),
                "connection": CONNECTION.get(info.connection, "unknown"),
                "crtc": None, "enabled": False, "active": False, "fb": 0,
                "mode": None, "refresh": None, "position": None,
            }
            if info.encoder_id:
                encoder = Encoder(encoder_id=info.encoder_id)
                fcntl.ioctl(fd, GETENCODER, encoder)
                if encoder.crtc_id:
                    crtc = Crtc(crtc_id=encoder.crtc_id)
                    fcntl.ioctl(fd, GETCRTC, crtc)
                    row.update(crtc=crtc.crtc_id, enabled=bool(crtc.mode_valid), fb=crtc.fb_id,
                               active=bool(crtc_properties(fd, crtc.crtc_id).get("ACTIVE", 0)),
                               position=[crtc.x, crtc.y])
                    if crtc.mode_valid:
                        row.update(mode=mode_name(crtc.mode), refresh=crtc.mode.vrefresh)
            result.append(row)
        return result
    finally:
        os.close(fd)


def main():
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    json.dump(readback(sys.argv[1]), sys.stdout)
    print()


if __name__ == "__main__":
    main()
