#!/usr/bin/python3
"""Self-test for the P-ST-01 live-settings gate assertions (#594).

Runs the `tuna-set-gates.py` checks against synthetic fixtures: no
compositor, no display, no gsettings. Needs PIL only for pixel-diff.
"""

import importlib.util
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "gates", Path(__file__).with_name("tuna-set-gates.py"))
gates = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gates)


def text(prefix):
    return {"truncated": False, "bytes": len(prefix), "prefix": prefix}


def a11y(*names):
    return [{"role": "label", "name": name, "showing": True} for name in names]


class ClockCheckTests(unittest.TestCase):
    def test_twelve_hour_without_date(self):
        # logic::clock_text: figure-space-padded 12h time, no date parts.
        ok, report = gates.check_clock(
            a11y("\u20071:06 PM", "Activities"), "12h", "off")
        self.assertTrue(ok, report)

    def test_twelve_hour_with_date(self):
        # "Oct 9" + EN SPACE + 12h time, as gnome-desktop builds it.
        ok, report = gates.check_clock(
            a11y("Oct 9\u2002\u20071:06 PM"), "12h", "on")
        self.assertTrue(ok, report)

    def test_twenty_four_hour_with_date(self):
        ok, report = gates.check_clock(
            a11y("Thu Oct 9\u200213:06"), "24h", "on")
        self.assertTrue(ok, report)

    def test_twenty_four_hour_without_date(self):
        ok, report = gates.check_clock(a11y("13:06"), "24h", "off")
        self.assertTrue(ok, report)

    def test_wrong_format_fails(self):
        ok, _ = gates.check_clock(a11y("13:06"), "12h", "off")
        self.assertFalse(ok)

    def test_meridiem_leak_fails_24h(self):
        ok, _ = gates.check_clock(a11y("\u20071:06 PM"), "24h", "off")
        self.assertFalse(ok)

    def test_missing_date_fails(self):
        ok, _ = gates.check_clock(a11y("\u20071:06 PM"), "12h", "on")
        self.assertFalse(ok)

    def test_leaked_date_fails(self):
        ok, _ = gates.check_clock(a11y("Oct 9\u200213:06"), "24h", "off")
        self.assertFalse(ok)

    def test_no_clock_fails(self):
        ok, _ = gates.check_clock(a11y("Activities"), "24h", "off")
        self.assertFalse(ok)


class WallpaperCheckTests(unittest.TestCase):
    URI = "file:///tmp/set-wallpaper-green.png"

    def state(self, identity, decoded):
        state = {"wallpaper_diagnostics": {"identities": [], "decode_results": []}}
        if identity is not None:
            state["wallpaper_diagnostics"]["identities"].append({
                "uri": text(self.URI),
                "identity": text(identity),
                "age_ms": 12,
            })
        if decoded is not None:
            available, pixels = decoded
            # Render keys join the URI with settings, geometry and file
            # identity; the gate matches on the first line only.
            key = self.URI + "\nPictureSettings { .. }\nGeometry { .. }\nSome(..)"
            state["wallpaper_diagnostics"]["decode_results"].append({
                "request_key": text(key),
                "size": [1280, 800] if available else None,
                "pixel_bytes": pixels,
                "buffer_available": available,
            })
        return state

    def test_file_identity_and_decoded_buffer(self):
        ok, report = gates.check_wallpaper(
            self.state("FileIdentity { path: \"/tmp/x\" }", (True, 1024)),
            self.URI, identity="file", decoded=True)
        self.assertTrue(ok, report)

    def test_missing_identity_fails(self):
        ok, _ = gates.check_wallpaper(
            self.state(None, (True, 1024)), self.URI,
            identity="file", decoded=False)
        self.assertFalse(ok)

    def test_none_identity_is_not_a_file(self):
        ok, _ = gates.check_wallpaper(
            self.state("None", (True, 1024)), self.URI,
            identity="file", decoded=False)
        self.assertFalse(ok)

    def test_none_identity_negative_control(self):
        # A non-file URI is ignored as an image (identity None) while
        # the compositor keeps serving a fallback buffer, not crashing.
        http = self.state("None", (True, 1024))
        http["wallpaper_diagnostics"]["identities"][0]["uri"] = text(
            "http://example.invalid/x.png")
        http["wallpaper_diagnostics"]["decode_results"][0]["request_key"] = text(
            "http://example.invalid/x.png\nPictureSettings { .. }\nNone")
        ok, report = gates.check_wallpaper(
            http, "http://example.invalid/x.png", identity="none", decoded=False)
        self.assertTrue(ok, report)

    def test_missing_decode_fails(self):
        ok, _ = gates.check_wallpaper(
            self.state("FileIdentity { path: \"/tmp/x\" }", None), self.URI,
            identity=None, decoded=True)
        self.assertFalse(ok)

    def test_empty_buffer_fails(self):
        ok, _ = gates.check_wallpaper(
            self.state("FileIdentity { path: \"/tmp/x\" }", (False, None)),
            self.URI, identity=None, decoded=True)
        self.assertFalse(ok)


class FontsCheckTests(unittest.TestCase):
    def test_app_observed_fonts(self):
        ok, report = gates.check_fonts({
            "app_document": "Serif 24", "app_monospace": "Monospace 24",
        }, "Serif 24", "Monospace 24")
        self.assertTrue(ok, report)

    def test_stale_app_read_fails(self):
        ok, report = gates.check_fonts({
            "app_document": "Cantarell 11",
            "app_monospace": "Source Code Pro 10",
        }, "Serif 24", "Monospace 24")
        self.assertFalse(ok)
        self.assertIn("Cantarell 11", report)

    def test_fixture_error_fails(self):
        ok, report = gates.check_fonts({"error": "no display"}, "Serif 24", "Monospace 24")
        self.assertFalse(ok)
        self.assertIn("no display", report)


class DesktopPixelTests(unittest.TestCase):
    def test_avoids_windows_and_panel(self):
        state = {"windows": [
            {"rect": [0, 32, 640, 420]},
            {"rect": [80, 80, 640, 420]},
        ]}
        found = gates.uncovered_desktop_pixel(state, 1280, 800)
        self.assertIsNotNone(found)
        x, y = found
        self.assertGreaterEqual(y, 40)
        for rect in ([0, 32, 640, 420], [80, 80, 640, 420]):
            rx, ry, rw, rh = rect
            self.assertFalse(rx - 2 <= x < rx + rw + 2
                             and ry - 2 <= y < ry + rh + 2)

    def test_fully_covered_returns_none(self):
        state = {"windows": [{"rect": [0, 0, 1280, 800]}]}
        self.assertIsNone(gates.uncovered_desktop_pixel(state, 1280, 800))

    def test_no_windows_uses_corner(self):
        self.assertEqual(
            gates.uncovered_desktop_pixel({"windows": []}, 1280, 800),
            (10, 790))


class PixelDiffTests(unittest.TestCase):
    def test_solid_color_change(self):
        from PIL import Image
        with tempfile.TemporaryDirectory() as tmp:
            before = Path(tmp) / "before.png"
            after = Path(tmp) / "after.png"
            Image.new("RGB", (64, 64), (0, 255, 0)).save(before)
            Image.new("RGB", (64, 64), (255, 0, 255)).save(after)
            ok, report = gates.check_pixel_diff(
                str(before), str(after), (0, 0, 64, 64), 0.5)
            self.assertTrue(ok, report)

    def test_identical_fails(self):
        from PIL import Image
        with tempfile.TemporaryDirectory() as tmp:
            before = Path(tmp) / "before.png"
            after = Path(tmp) / "after.png"
            Image.new("RGB", (64, 64), (0, 255, 0)).save(before)
            Image.new("RGB", (64, 64), (0, 255, 0)).save(after)
            ok, _ = gates.check_pixel_diff(
                str(before), str(after), (0, 0, 64, 64), 0.02)
            self.assertFalse(ok)


if __name__ == "__main__":
    unittest.main()
