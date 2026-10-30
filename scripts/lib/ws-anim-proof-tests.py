#!/usr/bin/env python3
import importlib.util
import io
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "ws_anim_proof", Path(__file__).with_name("ws-anim-proof.py"))
proof = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proof)

P = "tuna-shell-gtk: "


def frame(position, n=2):
    dots = proof.gnome_dots(n, position)
    join = lambda i, places: ",".join(f"{d[i]:.{places}f}" for d in dots)  # noqa: E731
    return (f"{P}ws-dots frame position={position:.4f} slots={join(0, 3)} "
            f"widths={join(1, 3)} heights={join(2, 3)} opacities={join(3, 4)}")


class GnomeDotsTests(unittest.TestCase):
    def test_settled_dots_follow_panel_js(self):
        # Two workspaces: a 29px opaque pill and a 6px half-opaque dot in
        # an 8px slot.
        active, idle = proof.gnome_dots(2, 0.0)
        self.assertEqual(active, (29.0, 29.0, 8.0, 1.0))
        self.assertEqual(idle, (8.0, 6.0, 6.0, 0.5))
        self.assertEqual(proof.gnome_dots(5, 0.0)[0][0], 26.0)
        self.assertEqual(proof.gnome_dots(6, 0.0)[0][0], 22.0)

    def test_a_quarter_swipe_splits_the_expansion(self):
        first, second = proof.gnome_dots(2, 0.25)
        self.assertAlmostEqual(first[3], 0.875)
        self.assertAlmostEqual(second[3], 0.625)
        self.assertAlmostEqual(first[0], 8 * (1 + 2.625 * 0.75))


class TraceTests(unittest.TestCase):
    def run_cmd(self, lines, *args, timeout="0.2"):
        with tempfile.NamedTemporaryFile("w", suffix=".log", delete=False) as fh:
            fh.write("\n".join(lines) + "\n")
        out = io.StringIO()
        with redirect_stdout(out):
            proof.main([args[0], fh.name, "1", *args[1:], "--timeout", timeout])
        return out.getvalue().strip()

    def test_dots_accept_gnomes_frame_and_reject_another(self):
        self.run_cmd([frame(0.0), frame(0.875)], "dots", "0.875")
        with self.assertRaises(SystemExit):
            self.run_cmd([frame(0.875)], "dots", "0.5")
        # A frame whose sizes are not GNOME's at its own position fails.
        bad = frame(0.5).replace("opacities=0.7500", "opacities=0.7000")
        with self.assertRaises(SystemExit):
            self.run_cmd([bad], "dots", "0.5")

    def test_eased_starts_from_the_held_position(self):
        lines = [
            f"{P}ws-dots follow position=0.2000 duration=0.0 motion=true clock=manual",
            f"{P}ws-dots follow position=1.0000 duration=160.000 motion=true clock=manual",
            frame(0.2 + 0.8 * 0.875),
        ]
        self.assertEqual(self.run_cmd(lines, "eased", "80"), f"{0.2 + 0.8 * 0.875:.4f}")
        self.assertEqual(self.run_cmd(lines, "release"), "1.0000 160.000 0.2")

    def test_popup_reads_the_latest_frame(self):
        lines = [f"{P}ws-popup frame opacity=0.0000", f"{P}ws-popup frame opacity=0.7500"]
        self.run_cmd(lines, "popup", "0.75")
        with self.assertRaises(SystemExit):
            self.run_cmd(lines, "popup", "0")

    def test_line_and_absent(self):
        lines = [f"{P}ws-popup fade to=0 from=1.0000 duration=100 clock=manual"]
        self.run_cmd(lines, "line", "ws-popup fade to=0")
        self.run_cmd(lines, "absent", "ws-popup hidden", timeout="0")
        with self.assertRaises(SystemExit):
            self.run_cmd(lines, "absent", "ws-popup fade", timeout="0")


if __name__ == "__main__":
    unittest.main()
