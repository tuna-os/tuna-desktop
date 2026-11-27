#!/usr/bin/env python3
"""Print the fixed-offset zone a nested proof session runs in.

GNOME's screen-time day, and the shell's wellbeing tracker
(crates/shell-gtk/src/wellbeing.rs DAY_START_HOUR), starts at 03:00 local
time. A proof that crosses it resets the day's usage mid-gate. The zone
chosen here puts the proof's start at TARGET_HOUR local time, twelve hours
from either boundary, with the minutes unchanged.

The result is a POSIX TZ rule, so no tzdata is needed; glibc, GLib, Python
and jiff all read it from TZ.
"""
import sys
import time

DAY_START_HOUR = 3
TARGET_HOUR = 15


def offset_hours(utc_seconds):
    """Whole hours east of UTC putting `utc_seconds` at TARGET_HOUR local."""
    offset = (TARGET_HOUR - time.gmtime(utc_seconds).tm_hour) % 24
    return offset - 24 if offset > 12 else offset


def posix_rule(offset):
    # The quoted name reads east-positive; POSIX offsets count west.
    return f'<{offset:+03d}>{-offset}'


def main():
    print(posix_rule(offset_hours(time.time())))
    return 0


if __name__ == '__main__':
    sys.exit(main())
