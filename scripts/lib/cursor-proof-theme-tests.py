#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("cursor_theme", Path(__file__).with_name("cursor-proof-theme.py"))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class CursorThemeTests(unittest.TestCase):
    def test_legacy_frames_keep_pixels_hotspots_and_individual_delays(self):
        data = fixture.xcursor([(24, color, delay) for color, delay in zip(fixture.LEGACY_COLORS, fixture.DELAYS)])
        self.assertEqual(struct.unpack_from("<4sIII", data), (b"Xcur", 16, 0x10000, 2))
        for index, (color, delay) in enumerate(zip(fixture.LEGACY_COLORS, fixture.DELAYS)):
            kind, side, offset = struct.unpack_from("<III", data, 16 + 12 * index)
            self.assertEqual((kind, side), (0xfffd0002, 24))
            self.assertEqual(struct.unpack_from("<IIIIIIIII", data, offset), (36, kind, side, 1, 24, 24, 1, 1, delay))
            self.assertEqual(data[offset + 36:offset + 36 + 24 * 24 * 4], struct.pack("<I", color) * (24 * 24))
        self.assertEqual(offset + 36 + 24 * 24 * 4, len(data))

    def test_generated_svg_frames_and_legacy_inheritance(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture.generate(root)
            for theme in ("TunaProof", "TunaProofSvg"):
                self.assertIn("Inherits=TunaProofBase", (root / theme / "index.theme").read_text())
            directory = root / "TunaProofSvg/cursors_scalable/wait"
            frames = json.loads((directory / "metadata.json").read_text())
            self.assertEqual(len(frames), 2)
            for index, frame in enumerate(frames):
                self.assertEqual(frame, {"filename": f"frame-{index}.svg", "nominal_size": 24, "hotspot_x": 4, "hotspot_y": 6, "delay": fixture.DELAYS[index]})
                self.assertIn(f'fill="{fixture.SVG_COLORS[index]}"', (directory / frame["filename"]).read_text())
            self.assertTrue((root / "TunaProofBase/cursors/wait").is_file())


if __name__ == "__main__":
    unittest.main()
