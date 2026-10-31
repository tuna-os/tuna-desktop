#!/usr/bin/env python3
"""Check end-session traffic in a dbus-monitor log (#589).

Usage: tuna-end-session-bus.py LOG FROM_LINE SHELL MANAGER CHECK ARGS...

LOG is `dbus-monitor --session` output; only messages starting at line
FROM_LINE (1-based) count. SHELL and MANAGER are the unique bus names of
org.gnome.Shell and org.gnome.SessionManager. CHECK is one of:

  open METHOD TYPE INHIBITORS
                     the shell called org.gnome.SessionManager.METHOD,
                     then the manager called the shell's
                     EndSessionDialog.Open(TYPE, 0, 60, [inhibitor, ...])
                     (gsm-shell.c), with inhibitor paths when INHIBITORS
                     is "some" and with none when it is "none"
  signal NAME        the shell emitted EndSessionDialog.NAME
  cancelled METHOD   the manager failed the shell's METHOD call as
                     cancelled (G_IO_ERROR_CANCELLED, as gnome-session)
  returned METHOD    the manager answered the shell's METHOD call

Exit 0 when the check holds; else 1 with the parsed messages on stderr.
"""
import json
import re
import sys

CANCELLED = "org.gtk.GDBus.UnmappedGError.Quark._g_2dio_2derror_2dquark.Code19"
DIALOG = "org.gnome.SessionManager.EndSessionDialog"
HEADER = re.compile(r"^(method call|method return|signal|error) ")
INHIBITOR = 'object path "/org/gnome/SessionManager/Inhibitor'


def parse(path, first):
    messages = []
    with open(path, encoding="utf-8", errors="replace") as stream:
        for number, line in enumerate(stream, 1):
            if number < first:
                continue
            header = HEADER.match(line)
            if header:
                fields = dict(re.findall(r"(\w+)=([^ ;]+)", line.rstrip("\n")))
                messages.append({"type": header.group(1), "line": number, **fields, "args": []})
            elif messages and line.startswith("   "):
                messages[-1]["args"].append(line.strip())
    return messages


def main():
    if len(sys.argv) < 7:
        print(__doc__, file=sys.stderr)
        return 2
    log, first, shell, manager, check, *args = sys.argv[1:]
    messages = parse(log, int(first))
    calls = [m for m in messages if m["type"] == "method call" and m.get("sender") == shell
             and m.get("interface") == "org.gnome.SessionManager"
             and m.get("member") == args[0]]

    def reply_to(call, kind):
        return any(m["type"] == kind and m.get("destination") == shell
                   and m.get("reply_serial") == call.get("serial")
                   and (kind != "error" or m.get("error_name") == CANCELLED)
                   for m in messages)

    def opened(call, kind, inhibitors):
        for m in messages:
            paths = [a for a in m["args"][3:] if a.startswith(INHIBITOR)]
            # gsm-shell.c calls org.gnome.Shell by its well-known name.
            if (m["line"] > call["line"] and m["type"] == "method call"
                    and m.get("sender") == manager
                    and m.get("destination") in (shell, "org.gnome.Shell")
                    and m.get("interface") == DIALOG and m.get("member") == "Open"
                    and m["args"][:3] == [f"uint32 {kind}", "uint32 0", "uint32 60"]
                    and bool(paths) == (inhibitors == "some")):
                return True
        return False

    if check == "open":
        _, kind, inhibitors = args
        ok = any(opened(call, kind, inhibitors) for call in calls)
    elif check == "signal":
        ok = any(m["type"] == "signal" and m.get("sender") == shell
                 and m.get("interface") == DIALOG and m.get("member") == args[0]
                 for m in messages)
    elif check == "cancelled":
        ok = any(reply_to(call, "error") for call in calls)
    elif check == "returned":
        ok = any(reply_to(call, "method return") for call in calls)
    else:
        print(f"unknown check {check}", file=sys.stderr)
        return 2
    if not ok:
        print(f"check failed: {check} {' '.join(args)}", file=sys.stderr)
        json.dump(messages, sys.stderr, indent=1)
        print(file=sys.stderr)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
