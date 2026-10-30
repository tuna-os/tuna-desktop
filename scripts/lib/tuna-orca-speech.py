#!/usr/bin/python3
"""Helpers for scripts/tuna-orca-speech-proof: what Orca said, and a probe window.

Usage:
  tuna-orca-speech.py expect LOG MARK TIMEOUT LINE...
      Wait up to TIMEOUT seconds until the lines Orca spoke after the
      marker line "=== MARK" in LOG hold every LINE, in order, each
      matched exactly (other speech, such as key echo, may come between).
      Prints the matched lines; on timeout prints the section and exits 1.
  tuna-orca-speech.py never LOG MARK TEXT...
      Exit 1 (printing the offending lines) when any line spoken after
      "=== MARK" contains one of TEXT or is exactly one of TEXT's
      characters: the typed password never reaches the speech log, as a
      word or letter by letter.
  tuna-orca-speech.py count LOG MARK LINE
      Print how many lines spoken after "=== MARK" are exactly LINE.
  tuna-orca-speech.py window TITLE
      Map one GTK 4 window titled TITLE (app id org.tuna.Probe<TITLE>)
      holding a "<TITLE> button".

LOG is the text speech-dispatcher's logging output module appends: one
line per utterance Orca sends, as Orca sends it; the proof appends its
"=== MARK" lines between the steps.
"""
import os
import sys
import time

MARK = "=== "


def section(text, mark):
    """The lines after the last "=== mark" line, up to the next marker."""
    lines = text.splitlines()
    marker = MARK + mark
    starts = [i for i, line in enumerate(lines) if line == marker]
    if not starts:
        return None
    out = []
    for line in lines[starts[-1] + 1:]:
        if line.startswith(MARK):
            break
        out.append(line)
    return out


def subsequence(spoken, wanted):
    """The spoken lines matching `wanted` in order, or None."""
    matched, at = [], 0
    for line in spoken:
        if at < len(wanted) and line == wanted[at]:
            matched.append(line)
            at += 1
    return matched if at == len(wanted) else None


def leaks(spoken, secrets):
    """Spoken lines that give a secret away, whole or one character at a time."""
    chars = {c for secret in secrets for c in secret}
    return [line for line in spoken
            if any(secret in line for secret in secrets) or line.strip() in chars]


def read(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except FileNotFoundError:
        return ""


def expect(log, mark, timeout, wanted):
    deadline = time.monotonic() + float(timeout)
    while True:
        spoken = section(read(log), mark)
        if spoken is not None and subsequence(spoken, wanted) is not None:
            print("\n".join(wanted))
            return 0
        if time.monotonic() >= deadline:
            print(f"after '{mark}' Orca never said {wanted}; it said:", file=sys.stderr)
            for line in spoken or []:
                print(f"  {line}", file=sys.stderr)
            return 1
        time.sleep(0.2)


def window(title):
    os.environ.setdefault("GSK_RENDERER", "cairo")
    import gi

    gi.require_version("Gtk", "4.0")
    from gi.repository import Gtk

    app = Gtk.Application(application_id=f"org.tuna.Probe{title}")

    def activate(app):
        win = Gtk.ApplicationWindow(application=app, title=title)
        win.set_default_size(480, 320)
        win.set_child(Gtk.Button(label=f"{title} button"))
        win.present()

    app.connect("activate", activate)
    return app.run([])


def main(argv):
    if len(argv) >= 5 and argv[1] == "expect":
        return expect(argv[2], argv[3], argv[4], argv[5:])
    if len(argv) >= 4 and argv[1] == "never":
        spoken = section(read(argv[2]), argv[3])
        if spoken is None:
            print(f"no '{argv[3]}' section", file=sys.stderr)
            return 1
        bad = leaks(spoken, argv[4:])
        for line in bad:
            print(f"leaked: {line}", file=sys.stderr)
        return 1 if bad else 0
    if len(argv) == 5 and argv[1] == "count":
        spoken = section(read(argv[2]), argv[3]) or []
        print(sum(1 for line in spoken if line == argv[4]))
        return 0
    if len(argv) == 3 and argv[1] == "window":
        return window(argv[2])
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
