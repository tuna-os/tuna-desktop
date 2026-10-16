#!/usr/bin/env python3
"""Admit the candidate's dynamically loaded X11/EGL libraries before capture."""
import ctypes
import json
import sys

LIBRARIES = (
    "libX11.so.6", "libXcursor.so.1", "libX11-xcb.so.1", "libXi.so.6",
    "libxkbcommon.so.0", "libxkbcommon-x11.so.0", "libEGL.so.1", "libGLESv2.so.2",
)


def admit(loader=ctypes.CDLL):
    loaded, missing = [], {}
    for library in LIBRARIES:
        try:
            loader(library)
            loaded.append(library)
        except OSError as error:
            missing[library] = str(error)
    return {"loaded": loaded, "missing": missing, "admitted": not missing}


if __name__ == "__main__":
    receipt = admit()
    print(json.dumps(receipt, sort_keys=True))
    if not receipt["admitted"]:
        print("candidate runtime libraries unavailable; see runtime-admission.json", file=sys.stderr)
        sys.exit(1)
