"""Validator controls only: these tests do not qualify an app, portal or device."""
import importlib.util
import json
import math
import os
from pathlib import Path
import pwd
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import uuid

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/lib"))
from core_apps_media import video_pixels, vivid_gallery_pixels

spec = importlib.util.spec_from_file_location(
    "core_media_guest", ROOT / "packaging/marlin/vm-lane/core-apps/media.py")
guest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guest)

context_spec = importlib.util.spec_from_file_location(
    "core_media_context", ROOT / "packaging/marlin/vm-lane/core-apps/guest.py")
context_module = importlib.util.module_from_spec(context_spec)
context_spec.loader.exec_module(context_module)


@unittest.skipUnless(os.geteuid() == 0 and shutil.which("runuser"),
                     "real root/original-UID permission boundary requires root and runuser")
class MediaFileBoundaryControls(unittest.TestCase):
    """Real filesystem/UID controls, not application or native-session proof."""

    def setUp(self):
        try:
            owner = pwd.getpwnam("nobody")
        except KeyError:
            self.skipTest("a real unprivileged nobody account is required")
        self.assertNotEqual(owner.pw_uid, 0)
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.root.chmod(0o755)
        self.home = self.root / "home"
        self.home.mkdir()
        os.chown(self.home, owner.pw_uid, owner.pw_gid)
        self.c = context_module.Context.__new__(context_module.Context)
        self.c.owner, self.c.uid = owner, owner.pw_uid
        self.c.home, self.c.nonce = self.home, uuid.uuid4().hex
        self.c.root = self.home / "tuna-core-apps"
        self.c.fixture = self.c.root / self.c.nonce
        # Keep actual Context.user/runuser and control-file syscalls. Only the
        # native session environment/run admission are isolated for this
        # filesystem test; no product app or service is replaced.
        self.c.environment = lambda: {"HOME": str(self.home), "LC_ALL": "C.UTF-8"}
        self.c.user(["mkdir", "-p", str(self.c.fixture / "media")])
        self.sane = self.c.root / "media-sane"
        self.c.user(["mkdir", str(self.sane)])
        sane_patch = mock.patch.object(guest, "SANE_DIR", self.sane)
        sane_patch.start()
        self.addCleanup(sane_patch.stop)
        run_state = self.root / "original-run.json"
        run_state.write_text(json.dumps({
            "nonce": self.c.nonce,
            "identity": {"boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
                         "uid": self.c.uid}}))
        run_state.chmod(0o600)
        run_patch = mock.patch.object(context_module, "RUN_STATE", run_state)
        run_patch.start()
        self.addCleanup(run_patch.stop)
        self.control = self.c._control_file("media-state")
        self.addCleanup(shutil.rmtree, self.control.parent)
        self.state = {
            "schema": 1, "music_dir": str(self.home / "Music" / ("Tuna-core-" + self.c.nonce)),
            "pictures": str(self.home / "Pictures" / "Camera"), "old_photos": [],
            "vivid_owned": False, "vivid_bound": False, "audio_stopped": False,
            "index_stopped": False, "camera_stopped": False, "permission": None,
            "sane_owned": True, "permission_owned": False, "restored": False}
        guest._save(self.c, self.state)

    def test_config_is_written_and_read_by_actual_original_uid(self):
        destination = self.sane / "test.conf"
        guest._owned(self.c, destination, "number_of_devices 1\n")
        self.assertEqual(destination.stat().st_uid, self.c.uid)
        self.assertEqual(destination.stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.c.user(["id", "-u"]).stdout.strip(), str(self.c.uid))
        self.assertEqual(self.c.user(["cat", str(destination)]).stdout, "number_of_devices 1\n")

    def test_sane_symlink_cannot_modify_or_chown_existing_root_file(self):
        protected = self.root / "root-owned.conf"
        protected.write_bytes(b"original root bytes\n")
        protected.chmod(0o600)
        before = protected.stat()
        self.c.user(["ln", "-s", str(protected), str(self.sane / "test.conf")])
        with self.assertRaises(subprocess.CalledProcessError):
            guest._owned(self.c, self.sane / "test.conf", "replacement\n")
        after = protected.stat()
        self.assertEqual(protected.read_bytes(), b"original root bytes\n")
        self.assertEqual((after.st_ino, after.st_uid, after.st_gid, after.st_mode),
                         (before.st_ino, before.st_uid, before.st_gid, before.st_mode))
        self.assertTrue((self.sane / "test.conf").is_symlink())

    def test_fixed_sane_parent_symlink_is_not_traversed(self):
        protected = self.root / "root-directory"
        protected.mkdir()
        target = protected / "test.conf"
        target.write_bytes(b"untouched\n")
        target.chmod(0o600)
        self.c.user(["rmdir", str(self.sane)])
        self.c.user(["ln", "-s", str(protected), str(self.sane)])
        with self.assertRaises(subprocess.CalledProcessError):
            guest._owned(self.c, self.sane / "test.conf", "replacement\n")
        self.assertEqual(target.read_bytes(), b"untouched\n")
        self.assertEqual(target.stat().st_uid, 0)

    def test_cleanup_rejects_parent_symlink_without_unlinking_root_file(self):
        protected = self.root / "root-directory"
        protected.mkdir()
        target = protected / "dll.conf"
        target.write_bytes(b"root-owned original\n")
        self.c.user(["rmdir", str(self.sane)])
        self.c.user(["ln", "-s", str(protected), str(self.sane)])
        with self.assertRaises(RuntimeError):
            guest._cleanup(self.c)
        self.assertEqual(target.read_bytes(), b"root-owned original\n")
        self.assertEqual(target.stat().st_uid, 0)
        self.assertTrue(guest._state(self.c)["sane_owned"])
        self.assertFalse(guest._state(self.c)["restored"])

    def test_hardlinked_user_input_is_not_truncated(self):
        source = self.sane / "original"
        guest._owned(self.c, source, "original user bytes\n")
        self.c.user(["ln", str(source), str(self.sane / "test.conf")])
        with self.assertRaises(subprocess.CalledProcessError):
            guest._owned(self.c, self.sane / "test.conf", "replacement\n")
        self.assertEqual(source.read_text(), "original user bytes\n")

    def test_user_receipt_deletion_cannot_clear_authoritative_service_effect(self):
        self.state["audio_stopped"] = True
        self.state["audio_before"] = [
            {"Id": unit, "ActiveState": "active", "UnitFileState": "enabled"}
            for unit in ("pipewire-pulse.socket", "pipewire-pulse.service")]
        guest._save(self.c, self.state)
        forged = self.c.fixture / "media/control-state.json"
        guest._owned(self.c, forged, '{"audio_stopped":false,"restored":true}')
        self.c.user(["rm", "-r", str(self.c.fixture)])
        with self.assertRaises(subprocess.CalledProcessError):
            self.c.user(["python3", "-c",
                         "import pathlib,sys; pathlib.Path(sys.argv[1]).write_text('{}')",
                         str(self.control)])
        self.assertTrue(guest._state(self.c)["audio_stopped"])
        self.assertFalse(guest._state(self.c)["restored"])

    def test_cleanup_uses_authority_when_entire_user_fixture_is_deleted(self):
        guest._owned(self.c, self.sane / "dll.conf", "test\n")
        guest._owned(self.c, self.sane / "test.conf", "number_of_devices 1\n")
        self.c.user(["rm", "-r", str(self.c.fixture)])
        self.assertEqual(guest._cleanup(self.c)["status"], "restored")
        self.assertFalse((self.sane / "dll.conf").exists())
        self.assertFalse((self.sane / "test.conf").exists())
        self.assertFalse(guest._state(self.c)["sane_owned"])

    def test_missing_or_malformed_authority_is_not_a_restored_default(self):
        self.control.unlink()
        with self.assertRaises(FileNotFoundError):
            guest._state(self.c)
        for alteration in (
                {"audio_stopped": "false"},
                {"vivid_owned": True},
                {"index_stopped": True, "index_unit": "../other.service"},
                {"permission": {"entry_exists": False, "all_permissions": {}}},
                {"restored": True}):
            with self.subTest(alteration=alteration):
                guest._save(self.c, dict(self.state, **alteration))
                with self.assertRaises(RuntimeError):
                    guest._cleanup(self.c)

    def test_control_envelope_rejects_wrong_run_boot_user_and_permissions(self):
        for key, wrong in (("nonce", "f" * 32), ("boot_id", "other-boot"),
                           ("uid", self.c.uid + 1)):
            with self.subTest(key=key):
                guest._save(self.c, self.state)
                envelope = json.loads(self.control.read_text())
                envelope[key] = wrong
                self.control.write_text(json.dumps(envelope))
                with self.assertRaises(RuntimeError):
                    guest._cleanup(self.c)
        guest._save(self.c, self.state)
        self.control.chmod(0o644)
        with self.assertRaises(RuntimeError):
            guest._cleanup(self.c)


class MonitorPayloadControls(unittest.TestCase):
    def tone(self, *frequencies):
        return [round(32767 * .18 * sum(math.sin(2 * math.pi * f * i / 48000)
                                       for f in frequencies)) for i in range(48000)]

    def test_both_encoded_frequencies_are_required(self):
        measured = guest.audio_metrics(self.tone(997, 1499), 48000)
        self.assertTrue(measured["matches_tones"])
        self.assertFalse(measured["silence"])
        for frequencies in ((997,), (1499,), (613, 2203), (440,)):
            with self.subTest(frequencies=frequencies):
                self.assertFalse(guest.audio_metrics(self.tone(*frequencies), 48000)["matches_tones"])

    def test_pause_is_silence_not_an_unrelated_sound(self):
        self.assertTrue(guest.audio_metrics([0] * 48000, 48000)["silence"])
        self.assertFalse(guest.audio_metrics(self.tone(440), 48000)["silence"])
        with self.assertRaises(RuntimeError):
            guest.audio_metrics([0] * 100, 48000)


class SaneRasterControls(unittest.TestCase):
    def raster(self):
        # Independently build the driver's six horizontal channel bands from
        # 4x4 cells and 1px gray separators, then repeat the 30px vertical tile.
        tile = Image.new("RGB", (300, 30), (85, 85, 85))
        draw = ImageDraw.Draw(tile)
        for band in range(6):
            for column in range(60):
                value = column if band % 2 == 0 else 255 - column
                rgb = [0, 0, 0]
                rgb[band // 2] = value
                draw.rectangle((column * 5 + 1, band * 5 + 1,
                                column * 5 + 4, band * 5 + 4), fill=tuple(rgb))
        result = Image.new("RGB", (300, 300))
        for y in range(0, 300, 30):
            result.paste(tile, (0, y))
        return result

    def test_exact_driver_cells_not_just_color_or_nonempty(self):
        raster = self.raster()
        self.assertEqual(guest.sane_pattern(raster)["mean_absolute_error"], 0)
        controls = [Image.new("RGB", raster.size, "white"),
                    Image.new("RGB", raster.size, "black"),
                    raster.transpose(Image.Transpose.FLIP_LEFT_RIGHT),
                    raster.transpose(Image.Transpose.FLIP_TOP_BOTTOM),
                    Image.merge("RGB", tuple(reversed(raster.split())))]
        for number, control in enumerate(controls):
            with self.subTest(control=number), self.assertRaises(RuntimeError):
                guest.sane_pattern(control)

    def test_every_export_pixel_and_exact_device_extent_are_checked(self):
        raster = self.raster()
        self.assertEqual(guest.sane_pattern(raster, expected_size=(300, 300))["samples"], 90000)
        for point in ((0, 0), (1, 1), (299, 299), (12, 14)):
            changed = raster.copy()
            old = changed.getpixel(point)
            changed.putpixel(point, ((old[0] + 1) % 256, old[1], old[2]))
            with self.subTest(point=point), self.assertRaises(RuntimeError):
                guest.sane_pattern(changed)
        with self.assertRaises(RuntimeError):
            guest.sane_pattern(raster.crop((0, 0, 295, 295)), expected_size=(300, 300))

    def test_lossy_pdf_tolerance_still_rejects_wrong_content(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "compressed.jpg"
            self.raster().save(path, quality=95, subsampling=0)
            with Image.open(path) as decoded:
                guest.sane_pattern(decoded, tolerance=18)
        with self.assertRaises(RuntimeError):
            guest.sane_pattern(Image.new("RGB", (300, 300), (85, 85, 85)), tolerance=22)


class RenderedPayloadControls(unittest.TestCase):
    colors = ((255,0,0), (0,255,0), (0,0,255), (255,255,0), (255,0,255), (0,255,255))

    def video(self, square_x=120, swap=False, square=True):
        result = Image.new("RGB", (900, 600), (30, 30, 30))
        draw = ImageDraw.Draw(result)
        colors = list(self.colors)
        if swap:
            colors[0], colors[2] = colors[2], colors[0]
        for index, color in enumerate(colors):
            draw.rectangle((100 + index * 108, 100, 207 + index * 108, 459), fill=color)
        if square:
            draw.rectangle((100 + square_x, 320, 147 + square_x, 367), fill="white")
        return result

    def test_rendered_square_location_and_ordered_bars(self):
        with tempfile.TemporaryDirectory() as directory:
            first, second = Path(directory) / "first.png", Path(directory) / "second.png"
            self.video(square_x=100).save(first)
            self.video(square_x=300).save(second)
            a, b = video_pixels(first), video_pixels(second)
            self.assertAlmostEqual(b["square_center"][0] - a["square_center"][0], 200, delta=3)
            with self.assertRaises(RuntimeError):
                video_pixels(first, bounds=[0, 0, 90, 90])
            with self.assertRaises(RuntimeError):
                video_pixels(first, bounds=[-1, 0, 900, 600])
            for kwargs in ({"swap": True}, {"square": False}):
                self.video(**kwargs).save(first)
                with self.subTest(kwargs=kwargs), self.assertRaises(RuntimeError):
                    video_pixels(first)

    def test_camera_white_control_and_ordered_colorbars_are_distinct(self):
        palette = ((255,255,255), (255,255,0), (0,255,255), (0,255,0),
                   (255,0,255), (255,0,0), (0,0,255), (0,0,0))
        bars = Image.new("RGB", (640, 360))
        draw = ImageDraw.Draw(bars)
        for index, color in enumerate(palette):
            draw.rectangle((index * 80, 0, index * 80 + 79, 359), fill=color)
        guest.camera_pixels(bars, "bars")
        guest.camera_pixels(Image.new("RGB", bars.size, "white"), "white")
        with self.assertRaises(RuntimeError):
            guest.camera_pixels(bars, "white")
        with self.assertRaises(RuntimeError):
            guest.camera_pixels(Image.new("RGB", bars.size, "white"), "bars")
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "gallery.png"
            canvas = Image.new("RGB", (900, 600), (30, 30, 30))
            canvas.paste(bars, (100, 100))
            canvas.save(path)
            self.assertTrue(vivid_gallery_pixels(path)["matches"])
            self.assertFalse(vivid_gallery_pixels(path, bounds=[0, 0, 90, 90])["matches"])
            self.video().save(path)
            self.assertFalse(vivid_gallery_pixels(path)["matches"])


if __name__ == "__main__":
    unittest.main()
