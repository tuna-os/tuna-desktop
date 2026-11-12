#!/usr/bin/env python3
"""Layout and content checks for the date menu's World Clocks and Weather
cards (P-PN-04, #1036), over the AT-SPI dumps and frames
tuna-gtk-shell-proof takes.

Usage:
  tuna-datemenu-layout.py clocks DUMP.json 24h|12h FROM TO CITY=ZONE...
      The World Clocks card lists exactly CITY... in that order, each with
      the time in ZONE (an IANA name) and its offset from local time as
      gnome-shell 51 writes them, in aligned columns. FROM and TO are the
      Unix seconds just before and after the dump: the card may show the
      minute of either.
  tuna-datemenu-layout.py forecast DUMP.json 24h|12h FROM TO MINUTES=TEMP...
      The Weather card shows exactly these forecasts in this order, each
      a time (MINUTES after the fixture was read, between FROM and TO)
      over a 32px icon over TEMP, in evenly spaced columns.
  tuna-datemenu-layout.py fits BEFORE.png AFTER.png WIDTH HEIGHT PANEL GAP
      AFTER shows a menu that BEFORE lacks, whole on the output: under
      the panel, GAP pixels clear of every screen edge. Prints its
      left, top, right and bottom edges.
  tuna-datemenu-layout.py overflows DUMP.json CARD
      The displays section is a scroll pane inside the menu and the card
      named CARD continues below it: the section has to scroll.
  tuna-datemenu-layout.py reached DUMP.json CARD MARGIN
      The card named CARD lies wholly inside the scroll pane, MARGIN
      pixels clear of its top and bottom edges.

GTK reports a padded widget (a card, a forecast's time) at its content
origin with its border-box size, so cards are placed by their content
box and only unpadded siblings are compared.
"""
import json
import sys
from datetime import datetime, timezone
from zoneinfo import ZoneInfo


def fail(message):
    print(f"tuna-datemenu-layout: {message}", file=sys.stderr)
    sys.exit(1)


def load(path):
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def card(nodes, name):
    """The showing button named `name` and its showing descendants."""
    for index, node in enumerate(nodes):
        if (node["name"] == name and node["showing"]
                and node["role"] in ("push button", "button")):
            inside = []
            for child in nodes[index + 1:]:
                if child["depth"] <= node["depth"]:
                    break
                if child["showing"]:
                    inside.append(child)
            return node, inside
    fail(f"no showing card named {name!r}")


def ancestors(nodes, index):
    depth = nodes[index]["depth"]
    for node in reversed(nodes[:index]):
        if node["depth"] < depth:
            depth = node["depth"]
            yield node


def scroll_pane(nodes, name):
    """The top and bottom edges of the card named `name`, and the scroll
    pane it sits in."""
    for index, node in enumerate(nodes):
        if (node["name"] == name and node["showing"]
                and node["role"] in ("push button", "button")):
            # The card's border box is its content box and its padding.
            content = nodes[index + 1]
            if content["depth"] != node["depth"] + 1 or content["bounds"] is None:
                fail(f"the {name!r} card has no content box")
            padding = (node["bounds"][3] - content["bounds"][3]) // 2
            edges = (content["bounds"][1] - padding,
                     content["bounds"][1] + content["bounds"][3] + padding)
            for above in ancestors(nodes, index):
                if above["role"] == "scroll pane":
                    return edges, above
            fail(f"the {name!r} card is not inside a scroll pane")
    fail(f"no showing card named {name!r}")


def right(node):
    return node["bounds"][0] + node["bounds"][2]


def centre(node):
    return node["bounds"][0] + node["bounds"][2] / 2


def time_only(moment, clock_format, ampm=True):
    """dateUtils.js formatTime with timeOnly: %H:%M, or %l:%M %p with
    GLib's figure space before a single-digit hour."""
    if clock_format == "24h":
        return f"{moment.hour:02}:{moment.minute:02}"
    hour = moment.hour % 12 or 12
    pad = "\u2007" if hour < 10 else ""
    suffix = (" AM" if moment.hour < 12 else " PM") if ampm else ""
    return f"{pad}{hour}:{moment.minute:02}{suffix}"


def offset_label(moment, zone):
    """dateMenu.js _getTimezoneOffsetAtLocation."""
    there = moment.astimezone(zone).utcoffset().total_seconds()
    here = moment.astimezone().utcoffset().total_seconds()
    difference = int(there - here)
    hours, minutes = abs(difference) // 3600, abs(difference) % 3600 // 60
    prefix = "+" if difference >= 0 else "-"
    return f"{prefix}{hours}" if minutes == 0 else f"{prefix}{hours}:{minutes}"


def moments(start, end):
    return [datetime.fromtimestamp(int(stamp), timezone.utc) for stamp in (start, end)]


def clocks(path, clock_format, start, end, *expected):
    _, inside = card(load(path), "World Clocks")
    labels = [node for node in inside if node["role"] == "label"]
    if not labels or labels[0]["name"] != "World Clocks":
        fail("the World Clocks card does not start with its header")
    header, cells = labels[0], labels[1:]
    if len(cells) != 3 * len(expected):
        fail(f"expected {len(expected)} clocks of city, time and offset, "
             f"found labels {[node['name'] for node in cells]}")
    rows = [cells[i:i + 3] for i in range(0, len(cells), 3)]
    for (city, time, offset), want in zip(rows, expected):
        name, zone = want.split("=", 1)
        zone = ZoneInfo(zone)
        if city["name"] != name:
            fail(f"expected the clock for {name}, found {city['name']!r}")
        times = [time_only(m.astimezone(zone), clock_format) for m in moments(start, end)]
        if time["name"] not in times:
            fail(f"{name} shows {time['name']!r}, not {' or '.join(map(repr, times))}")
        offsets = [offset_label(m, zone) for m in moments(start, end)]
        if offset["name"] not in offsets:
            fail(f"{name} is {offset['name']!r} from local time, not {offsets[0]!r}")
    # GNOME's grid: cities start under the header, times end on one
    # edge, offsets on another, and no cell runs into the next.
    for city, time, offset in rows:
        if city["bounds"][0] != header["bounds"][0]:
            fail(f"{city['name']} does not start under the header")
        if right(time) != right(rows[0][1]) or right(offset) != right(rows[0][2]):
            fail(f"the time or offset of {city['name']} is out of its column")
        if not right(city) <= time["bounds"][0] < right(time) < offset["bounds"][0]:
            fail(f"the cells of {city['name']} overlap")
    grid = next(node for node in inside if node["role"] != "label")
    if right(rows[0][2]) != right(grid):
        fail("the offsets do not end at the card's content edge")
    print(f"tuna-datemenu-layout: {len(rows)} world clocks in aligned columns")


def forecast(path, clock_format, start, end, *expected):
    _, inside = card(load(path), "Weather")
    # The forecast grid is the box holding the icons: the card's last.
    icons = [node for node in inside if node["role"] in ("image", "icon")]
    if len(icons) != len(expected):
        fail(f"expected {len(expected)} forecast icons, found {len(icons)}")
    depth = icons[0]["depth"]
    last = max(i for i, node in enumerate(inside) if node["depth"] == depth - 1)
    grid, cells = inside[last], inside[last + 1:]
    if len(cells) != 3 * len(expected):
        fail(f"expected {len(expected)} forecasts of time, icon and temperature, "
             f"found {[node['name'] or node['role'] for node in cells]}")
    columns = [cells[i:i + 3] for i in range(0, len(cells), 3)]
    for (time, icon, temp), want in zip(columns, expected):
        minutes, degrees = want.split("=", 1)
        times = [time_only(datetime.fromtimestamp(int(stamp) + int(minutes) * 60),
                           clock_format, ampm=False) for stamp in (start, end)]
        if time["role"] != "label" or time["name"] not in times:
            fail(f"a forecast shows {time['name']!r}, not {' or '.join(map(repr, times))}")
        if icon["role"] not in ("image", "icon") or icon["bounds"][2:] != [32, 32]:
            fail(f"the {time['name']} forecast's icon is not 32px: {icon['bounds']}")
        if temp["role"] != "label" or temp["name"] != degrees:
            fail(f"the {time['name']} forecast reads {temp['name']!r}, not {degrees!r}")
        if not time["bounds"][1] < icon["bounds"][1] < temp["bounds"][1]:
            fail(f"the {time['name']} forecast is not time over icon over temperature")
        if max(abs(centre(cell) - centre(icon)) for cell in (time, temp)) > 1:
            fail(f"the {time['name']} forecast is not centred in its column")
        if time["bounds"][0] < grid["bounds"][0] or right(time) > right(grid):
            fail(f"the {time['name']} forecast leaves the card")
    centres = [centre(icon) for _, icon, _ in columns]
    pitches = [b - a for a, b in zip(centres, centres[1:])]
    if pitches and (min(pitches) <= 32 or max(pitches) - min(pitches) > 1):
        fail(f"the forecasts are not evenly spaced: {centres}")
    if centres and abs((centres[0] - grid["bounds"][0]) - (right(grid) - centres[-1])) > 1:
        fail("the forecasts do not span the card evenly")
    print(f"tuna-datemenu-layout: {len(columns)} forecasts in even columns")


def fits(before, after, width, height, panel, gap):
    from PIL import Image, ImageChops
    width, height, panel, gap = int(width), int(height), int(panel), int(gap)
    frames = [Image.open(path).convert("RGB") for path in (before, after)]
    if any(frame.size != (width, height) for frame in frames):
        fail(f"the frames are not {width}x{height}")
    # The menu's own surface, without its faint shadow: every pixel a
    # channel of which changed by more than a shadow can, under the panel.
    red, green, blue = ImageChops.difference(*frames).split()
    changed = ImageChops.lighter(ImageChops.lighter(red, green), blue)
    changed = changed.point(lambda v: 255 if v > 40 else 0)
    box = changed.crop((0, panel, width, height)).getbbox()
    if box is None:
        fail("the frames do not differ under the panel: no menu opened")
    left, top, right_edge, bottom = box[0], box[1] + panel, box[2], box[3] + panel
    print(f"{left} {top} {right_edge} {bottom}")
    if top != panel + gap:
        fail(f"the menu starts at y={top}, not {gap}px under the {panel}px panel")
    if bottom > height - gap:
        fail(f"the menu ends at y={bottom}, less than {gap}px from the bottom edge at {height}")
    if left < gap or right_edge > width - gap:
        fail(f"the menu spans x={left}..{right_edge}, off the {width}px output")
    # The frame's last rows hold the menu's bottom border, not a cut.
    solid = changed.crop((left, bottom - 1, right_edge, bottom)).histogram()[255]
    if solid < (right_edge - left) // 2:
        fail("the menu has no bottom edge on the output")


def overflows(path, name):
    nodes = load(path)
    (_, bottom), pane = scroll_pane(nodes, name)
    # The section ends with the calendar column that holds it.
    column = next(ancestors(nodes, nodes.index(pane)))
    if pane["bounds"][1] + pane["bounds"][3] > column["bounds"][1] + column["bounds"][3]:
        fail("the scroll pane runs past the calendar column")
    below = bottom - (pane["bounds"][1] + pane["bounds"][3])
    if below <= 0:
        fail(f"the {name} card ends inside the scroll pane: nothing to scroll")
    print(f"tuna-datemenu-layout: {name} continues {below}px below the section")


def reached(path, name, margin):
    (top, bottom), pane = scroll_pane(load(path), name)
    top -= pane["bounds"][1]
    below = bottom - (pane["bounds"][1] + pane["bounds"][3])
    if top < int(margin) or below > -int(margin):
        fail(f"the {name} card and its {margin}px margin are not wholly in view: "
             f"{top}px from the section's top, {below}px past its bottom")
    print(f"tuna-datemenu-layout: {name} is wholly in view")


COMMANDS = {"clocks": clocks, "forecast": forecast, "fits": fits,
            "overflows": overflows, "reached": reached}


def main():
    if len(sys.argv) < 3 or sys.argv[1] not in COMMANDS:
        print(__doc__, file=sys.stderr)
        return 2
    COMMANDS[sys.argv[1]](*sys.argv[2:])
    return 0


if __name__ == "__main__":
    sys.exit(main())
