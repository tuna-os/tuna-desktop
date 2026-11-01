#!/usr/bin/python3
"""Run after the existing GTK4 FileChooser matrix in portal-reference/run.sh.

Integration (workflow/container ownership stays with the parent):
  * Add Fedora package gtk3 to tests/portal-reference/Containerfile; retain
    python3-gobject, python3-pyatspi, GTK4, Nautilus 51, the GNOME 51 backend,
    xdotool/scrot, AT-SPI, the ordinary UID 1000 session and candidate identity.
  * In that SAME dbus-run-session, with its existing real installed providers,
    original nested /candidate/usr/bin/tuna-compositor and unchanged host input:
      python3 /repo/scripts/lib/tuna-gtk3-portal-proof.py /out
  * This is additive: do not remove/replace the GTK4 none/x11/wayland matrix.
    Do not mask globals, force xdg-foreign v1, replace Nautilus, add mock portal
    responses, loosen 1-pixel centering, or substitute compositor input hooks.
  * Retain filechooser-gtk3-* (including full client WAYLAND_DEBUG logs),
    gtk3-runtime-versions.txt and gtk3-portal-suite.json. Require this command's
    exit status. Suite JSON exists only after all seven actual consumer cases
    finish successfully and their real evidence passes fail-closed checks.

Stage scope: six GTK3 Wayland Open/Save cancel/grant/Request.Close journeys,
plus an actual export->unexport->roundtrip->old-handle OpenFile cancel negative.
Each live case shares the GTK4 A/B/C positive/denied/restored input thresholds.
The revoked case proves only rejection of the actual old parent association;
it does NOT claim that an unparented chooser restores A instead of B.
Actual wire version is recorded, not assumed to be v1. This does not qualify
GTK3's own gtk_shell1 modal hints, decorated-titlebar actions or DBus metadata.
No test/build/format/lint or external CI is launched by this producer.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time

from tuna_gtk3_portal_evidence import validate_case


def run_case(directory, method, decision, lifetime):
    prefix = directory / f"filechooser-gtk3-{method}-{decision}-{lifetime}"
    helper = Path(__file__).parent
    with prefix.with_suffix(".log").open("x") as client_log:
        environment = dict(os.environ, WAYLAND_DEBUG="client")
        client = subprocess.Popen([sys.executable, str(helper / "tuna-portal-filechooser-client.py"),
            str(prefix), method, decision, "wayland", "gtk3", lifetime],
            env=environment, stdout=client_log, stderr=subprocess.STDOUT)
        try:
            end = time.monotonic() + 45
            while not prefix.with_suffix(".waiting.json").is_file():
                if client.poll() is not None:
                    raise RuntimeError(f"GTK3 client exited {client.returncode} before real chooser readiness: {prefix}.log")
                if time.monotonic() >= end:
                    raise RuntimeError(f"GTK3 client did not reach actual portal choice: {prefix}.log")
                time.sleep(.05)
            with prefix.with_suffix(".ui.log").open("x") as ui_log:
                subprocess.run([sys.executable, str(helper / "tuna-portal-filechooser-ui.py"), str(prefix)],
                    stdout=ui_log, stderr=subprocess.STDOUT, check=True, timeout=45)
            status = client.wait(timeout=60)
            if status != 0:
                raise RuntimeError(f"GTK3 genuine consumer failed with status {status}: {prefix}.log")
        finally:
            if client.poll() is None:
                client.terminate()
                try:
                    client.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    client.kill()
                    client.wait(timeout=5)
    return validate_case(prefix, method, decision, lifetime)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    if os.getuid() != 1000:
        raise RuntimeError("GTK3 portal stage requires the existing ordinary UID 1000 fixture session")
    if args.output.resolve() != Path("/out") or not args.output.is_dir():
        raise RuntimeError("Run inside the existing portal-reference fixture with /out mounted")
    for name in ("DBUS_SESSION_BUS_ADDRESS", "DISPLAY", "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR"):
        if not os.environ.get(name):
            raise RuntimeError("Missing actual portal-reference session environment: " + name)
    if list(args.output.glob("filechooser-gtk3-*")) or (args.output / "gtk3-portal-suite.json").exists():
        raise RuntimeError("Refusing stale GTK3 journey artifacts; use a fresh qualification output directory")
    versions = subprocess.check_output(["rpm", "-q", "gtk3", "gtk4", "python3-gobject", "python3-pyatspi",
        "nautilus", "xdg-desktop-portal", "xdg-desktop-portal-gnome"], text=True, timeout=10)
    (args.output / "gtk3-runtime-versions.txt").write_text(versions)
    cases = []
    for method in ("OpenFile", "SaveFile"):
        for decision in ("cancel", "grant", "close"):
            cases.append(run_case(args.output, method, decision, "live"))
    cases.append(run_case(args.output, "OpenFile", "cancel", "revoked"))
    result = {"scope": "installed GTK3 ordinary Wayland to actual GNOME/Nautilus portal parenting",
              "cases": cases, "gtk4_matrix_replaced": False,
              "foreign_versions_observed": sorted({case["wire"]["xdg_foreign_version"] for case in cases}),
              "not_qualified": ["gtk_shell1 modal/titlebar/DBus metadata semantics", "unobserved foreign versions"]}
    (args.output / "gtk3-portal-suite.json").write_text(json.dumps(result, indent=2) + "\n")
    print("GTK3 real GNOME/Nautilus portal parenting: six live journeys and actual revoked-handle negative")


if __name__ == "__main__":
    main()
