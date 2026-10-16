#!/usr/bin/python3
"""Ubuntu visual-runner IME font admission; not installed product qualification."""
import json
from pathlib import Path
import subprocess
import sys


def check_fixture(run=subprocess.check_output):
    version = run([
        "dpkg-query", "-W", "-f=${db:Status-Abbrev}\t${Version}", "fonts-noto-cjk",
    ], text=True, timeout=10).strip()
    status, package_version = version.split("\t", 1)
    if status.strip() != "ii" or not package_version:
        raise RuntimeError("fonts-noto-cjk must be installed for visual IME capture")
    glyph_fonts = {}
    # Actual input/candidate glyphs in the retained visual fixture: 你 好 尼 泥.
    for codepoint in (0x4F60, 0x597D, 0x5C3C, 0x6CE5):
        paths = run([
            "fc-list", "--format=%{file}\n", f":charset={codepoint:x}",
        ], text=True, timeout=10).splitlines()
        if not paths:
            raise RuntimeError(f"fontconfig has no font covering U+{codepoint:04X}")
        glyph_fonts[f"U+{codepoint:04X}"] = sorted(set(paths))
    return {
        "package": "fonts-noto-cjk", "version": package_version,
        "installed": True, "glyph_fonts": glyph_fonts,
        "scope": "Ubuntu CI IME visual fixture; no product font qualification",
    }


def main():
    destination = Path(sys.argv[1])
    destination.parent.mkdir(parents=True, exist_ok=True)
    receipt = {"completed": False}
    try:
        receipt.update(check_fixture())
        receipt["completed"] = True
    except Exception as error:
        receipt["error"] = str(error)
        raise
    finally:
        destination.write_text(json.dumps(receipt, indent=2) + "\n")


if __name__ == "__main__":
    main()
