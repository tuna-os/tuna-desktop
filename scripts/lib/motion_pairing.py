"""Paired GNOME 51 / Tuna Desktop window-motion metrics.

Both sides reduce a sampled frame to the same placement-independent numbers,
so a GNOME 51 capture (tests/gnome-reference/motion.js, distilled by
scripts/tuna-motion-reference) and a Tuna Desktop run on the frozen
animation clock (scripts/tuna-motion-pairing-proof) compare directly:

- transform families (map, destroy, attached-dialog map/destroy): the drawn
  buffer rect (client shadows included, as GNOME's window actor) normalised
  to the window's own buffer rect, and its opacity;
- minimize families: progress of each rect edge from the buffer rect toward
  the leading-corner target, and opacity;
- modal dim: the parent's brightness multiplier;
- size families (maximize, tile, fullscreen): progress of the old-frame
  snapshot toward the new frame, its opacity and the live content's size
  relative to its final size;
- pixel probes on the screenshots: window-body opacity and parent
  brightness estimated from actual pixels against the frames without them.
"""
from PIL import Image

TOLERANCE = {
    "opacity": 1.5 / 255,   # Clutter stores a truncated byte
    "rect": 0.01,           # of the window's own size (~5 px at 500 px)
    "progress": 0.01,
    "multiplier": 0.5 / 127,
    "snapshot_alpha": 1.5 / 255,
    "live_size": 0.01,
    "body_alpha": 0.06,     # estimated from 8-bit pixels over a wallpaper
    "pixel_multiplier": 0.03,
}

TRANSFORM = {"map", "destroy", "reduced-map", "reduced-destroy"}
DIALOG = {"dialog-map-dim", "dialog-destroy-undim", "reduced-dialog-map-dim", "reduced-dialog-destroy-undim"}
MINIMIZE = {"minimize", "unminimize", "reduced-minimize", "fullscreen-minimize"}
SIZE = {"maximize", "unmaximize", "tile-left", "untile-left", "tile-right", "untile-right",
        "fullscreen", "unfullscreen", "reduced-maximize", "reduced-unmaximize"}
INSTANT = {"off-map", "off-destroy", "reduced-unminimize", "fullscreen-unminimize"}


def family_class(name):
    for cls, names in (("transform", TRANSFORM), ("dialog", DIALOG), ("minimize", MINIMIZE),
                       ("size", SIZE), ("instant", INSTANT)):
        if name in names:
            return cls
    raise KeyError(name)


def normalise(drawn, base):
    x, y, w, h = base
    return [(drawn[0] - x) / w, (drawn[1] - y) / h, drawn[2] / w, drawn[3] / h]


def toward(drawn, base, target):
    """Per-edge progress from base to target (edges that do not move are skipped)."""
    out = []
    for d, b, t in zip(drawn, base, target):
        out.append((d - b) / (t - b) if abs(t - b) > 1 else None)
    return out


def frame_drawn(drawn, buffer, frame):
    """The frame (without shadows) inside a drawn buffer rect."""
    sx, sy = drawn[2] / buffer[2], drawn[3] / buffer[3]
    return [drawn[0] + (frame[0] - buffer[0]) * sx, drawn[1] + (frame[1] - buffer[1]) * sy,
            frame[2] * sx, frame[3] * sy]


def corner(monitor_w, rtl=False):
    return [monitor_w if rtl else 0, 0, 0, 0]


def size_metrics(snapshot, snapshot_alpha, live, old, new):
    moving = [i for i in range(4) if abs(new[i] - old[i]) > 1]
    progress = sum((snapshot[i] - old[i]) / (new[i] - old[i]) for i in moving) / len(moving) if moving else 1.0
    return {
        "progress": progress,
        "snapshot_alpha": snapshot_alpha,
        "live_size": [live[2] / new[2], live[3] / new[3]],
    }


# ---- pixels ---------------------------------------------------------------

class Frames:
    """Lazily opened screenshots."""

    def __init__(self, root):
        self.root = root
        self.cache = {}

    def __call__(self, name):
        if name not in self.cache:
            self.cache[name] = Image.open(self.root / name).convert("RGB")
        return self.cache[name]


def visible(opacity, drawn):
    """Whether anything of an effect's subject is on screen."""
    return opacity > 0 and drawn is not None and drawn[2] >= 0.5 and drawn[3] >= 0.5


def gnome_visible(row):
    if row.get("opacity", 1) == 0:
        return False
    rect = row.get("rect")
    if rect is not None and (rect[2] == 0 or rect[3] == 0):
        return False
    progress = row.get("progress")
    if isinstance(progress, list) and progress[2] is not None and progress[2] >= 1:
        return False
    return True


def body_point(frame, fx=0.5, fy=0.75):
    return (round(frame[0] + frame[2] * fx), round(frame[1] + frame[3] * fy))


def body_alpha(image, without, full, drawn_frame, settled_frame):
    """Window-body opacity at the same body point the full frame shows."""
    if drawn_frame[2] < 12 or drawn_frame[3] < 12:
        return None
    here = body_point(drawn_frame)
    there = body_point(settled_frame)
    p = image.getpixel(here)
    b = without.getpixel(here)
    c = full.getpixel(there)
    den = sum((ci - bi) ** 2 for ci, bi in zip(c, b))
    if den < 40 ** 2:
        return None
    return sum((pi - bi) * (ci - bi) for pi, bi, ci in zip(p, b, c)) / den


def pixel_multiplier(image, undimmed, point):
    p = image.getpixel(point)
    u = undimmed.getpixel(point)
    den = sum(ui * ui for ui in u)
    if den < 60 ** 2:
        return None
    return sum(pi * ui for pi, ui in zip(p, u)) / den


def parent_probe(parent_frame):
    # Inside the parent, clear of the centred attached dialog.
    return body_point(parent_frame, 0.1, 0.85)


# ---- comparison -----------------------------------------------------------

def compare(reference, measured):
    """Per-family, per-sample deltas; returns (report, failures)."""
    report, failures = {}, []
    for name, ref in reference["families"].items():
        got = measured.get(name)
        if got is None:
            failures.append(f"{name}: not measured")
            continue
        fam = {"class": ref["class"], "samples": []}
        if ref["class"] == "instant":
            ok = got.get("instant") is True
            fam["instant"] = got.get("instant")
            if not ok:
                failures.append(f"{name}: GNOME 51 shows this at once; Tuna Desktop animated it ({got})")
            report[name] = fam
            continue
        by_t = {s["t_ms"]: s for s in got["samples"]}
        for want in ref["samples"]:
            have = by_t.get(want["t_ms"])
            row = {"t_ms": want["t_ms"], "gnome": want, "tuna": have, "delta": {}}
            if have is None:
                failures.append(f"{name} {want['t_ms']} ms: not sampled")
                fam["samples"].append(row)
                continue
            # Where GNOME draws nothing of the subject (opacity 0, or scaled
            # to nothing), Tuna Desktop must draw nothing either; its
            # geometry and opacity then have no picture to compare.
            shown = gnome_visible(want)
            if "visible" in have and have["visible"] != shown:
                failures.append(f"{name} {want['t_ms']} ms: GNOME {'shows' if shown else 'hides'} the subject, Tuna Desktop does not")
            for key, tol in TOLERANCE.items():
                if key not in want or want[key] is None:
                    continue
                if not shown and key in ("opacity", "rect", "progress", "body_alpha"):
                    continue
                g, t = want[key], have.get(key)
                if key == "live_size":
                    # #496: no frame shows the new content stretched beyond its
                    # final size; GNOME scales it from 1/scale (windowManager.js
                    # _sizeChangedWindow), so a shrink compares to min(1, GNOME).
                    g = [min(1.0, v) for v in g]
                if t is None:
                    failures.append(f"{name} {want['t_ms']} ms {key}: GNOME {g}, Tuna Desktop has none")
                    continue
                gs = g if isinstance(g, list) else [g]
                ts = t if isinstance(t, list) else [t]
                deltas = [None if a is None or b is None else abs(a - b) for a, b in zip(gs, ts)]
                row["delta"][key] = deltas
                worst = max((d for d in deltas if d is not None), default=0.0)
                if worst > tol:
                    failures.append(f"{name} {want['t_ms']} ms {key}: GNOME {g}, Tuna Desktop {t} (|d| {worst:.4f} > {tol:.4f})")
            fam["samples"].append(row)
        report[name] = fam
    return report, failures
