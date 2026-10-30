#!/usr/bin/env python3
"""Read every CRTC's GAMMA_LUT straight from the kernel (P-DS-03).

Usage: drm-gamma-readback.py /dev/dri/cardN

Prints JSON: a list of {"crtc": id, "gamma_lut_size": n, "ctm": bool,
"gamma_lut": {"red": [...], "green": [...], "blue": [...]} | null}.
Uses only the KMS object-property ioctls (no DRM master, no libdrm), so
the readback is independent of the compositor that programmed it.
"""

import ctypes
import fcntl
import json
import os
import struct
import sys

DRM_MODE_OBJECT_CRTC = 0xCCCCCCCC


def iowr(nr, size):
    return (3 << 30) | (size << 16) | (ord("d") << 8) | nr


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


class GetBlob(ctypes.Structure):
    _fields_ = [
        ("blob_id", ctypes.c_uint32),
        ("length", ctypes.c_uint32),
        ("data", ctypes.c_uint64),
    ]


GETRESOURCES = iowr(0xA0, ctypes.sizeof(CardRes))
GETPROPERTY = iowr(0xAA, ctypes.sizeof(GetProperty))
GETPROPBLOB = iowr(0xAC, ctypes.sizeof(GetBlob))
OBJ_GETPROPERTIES = iowr(0xB9, ctypes.sizeof(ObjGetProperties))


def crtcs(fd):
    res = CardRes()
    fcntl.ioctl(fd, GETRESOURCES, res)
    ids = (ctypes.c_uint32 * res.count_crtcs)()
    res2 = CardRes()
    res2.crtc_id_ptr = ctypes.addressof(ids)
    res2.count_crtcs = res.count_crtcs
    fcntl.ioctl(fd, GETRESOURCES, res2)
    return list(ids)[: res2.count_crtcs]


def properties(fd, obj):
    req = ObjGetProperties(obj_id=obj, obj_type=DRM_MODE_OBJECT_CRTC)
    fcntl.ioctl(fd, OBJ_GETPROPERTIES, req)
    n = req.count_props
    props = (ctypes.c_uint32 * n)()
    values = (ctypes.c_uint64 * n)()
    req = ObjGetProperties(
        props_ptr=ctypes.addressof(props),
        prop_values_ptr=ctypes.addressof(values),
        count_props=n,
        obj_id=obj,
        obj_type=DRM_MODE_OBJECT_CRTC,
    )
    fcntl.ioctl(fd, OBJ_GETPROPERTIES, req)
    out = {}
    for prop, value in zip(props, values):
        info = GetProperty(prop_id=prop)
        fcntl.ioctl(fd, GETPROPERTY, info)
        out[info.name.decode()] = value
    return out


def blob(fd, blob_id):
    req = GetBlob(blob_id=blob_id)
    fcntl.ioctl(fd, GETPROPBLOB, req)
    data = ctypes.create_string_buffer(req.length)
    req2 = GetBlob(blob_id=blob_id, length=req.length, data=ctypes.addressof(data))
    fcntl.ioctl(fd, GETPROPBLOB, req2)
    return data.raw[: req.length]


def main():
    fd = os.open(sys.argv[1], os.O_RDONLY | os.O_CLOEXEC)
    result = []
    for crtc in crtcs(fd):
        props = properties(fd, crtc)
        lut = None
        blob_id = props.get("GAMMA_LUT", 0)
        if blob_id:
            raw = blob(fd, blob_id)
            entries = [struct.unpack_from("=HHHH", raw, i) for i in range(0, len(raw), 8)]
            lut = {
                "red": [e[0] for e in entries],
                "green": [e[1] for e in entries],
                "blue": [e[2] for e in entries],
            }
        result.append(
            {
                "crtc": crtc,
                "gamma_lut_size": props.get("GAMMA_LUT_SIZE", 0),
                "ctm": "CTM" in props,
                "gamma_lut": lut,
            }
        )
    json.dump(result, sys.stdout)
    print()


if __name__ == "__main__":
    main()
