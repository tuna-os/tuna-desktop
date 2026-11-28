#!/usr/bin/env python3
"""Validate the pinned Project Bluefin AT-SPI scope manifest (issue #475).

The manifest is an inventory, not a test result: it never reports a suite as
passing desktop compatibility.
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "docs/testing/bluefin-atspi-scope.json"
FORBIDDEN_POLICY = (
    "whole_scenario_retry", "cli_only_fallback", "start_gnome_shell",
    "fake_shell_eval_provider", "counts_as_desktop_compatibility",
)


def validate(data):
    errors = []
    up = data.get("upstream", {})
    if not re.fullmatch(r"[0-9a-f]{40}", up.get("revision", "")):
        errors.append("upstream.revision must be a full 40-hex commit")
    for name, ver in up.get("dependencies", {}).items():
        if not re.fullmatch(r"\d+(\.\d+)*", ver):
            errors.append(f"dependency {name} is not pinned to an exact version")
    for need in ("qecore", "dogtail", "behave"):
        if need not in up.get("dependencies", {}):
            errors.append(f"dependency {need} missing")
    policy = data.get("policy", {})
    for key in FORBIDDEN_POLICY:
        if policy.get(key) is not False:
            errors.append(f"policy.{key} must be false")
    allowed = set(data.get("dispositions", []))
    seen = set()
    for sc in data.get("scenarios", []):
        key = (sc.get("feature"), sc.get("scenario"))
        if key in seen:
            errors.append(f"duplicate scenario {key}")
        seen.add(key)
        if sc.get("disposition") not in allowed:
            errors.append(f"{key}: unknown disposition")
        if not sc.get("reason", "").strip():
            errors.append(f"{key}: reason is required")
    if not data.get("scenarios"):
        errors.append("no scenarios inventoried")
    return errors


def main():
    errors = validate(json.loads(MANIFEST.read_text()))
    for e in errors:
        print(f"bluefin-atspi-scope: {e}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
