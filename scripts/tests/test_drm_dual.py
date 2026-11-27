#!/usr/bin/env python3
"""Unit tests of scripts/tuna-drm-dual's checks (scripts/lib/drm_dual.py)
and of the KMS readback's kernel ABI (scripts/lib/drm_kms_readback.py).

Each gate check runs on the passing shape and on what the old behaviour
(or a regression of it) produces, which must fail. They do not
substitute for the real two-output vkms run in the drm-smoke lane.
"""
import ctypes
from pathlib import Path
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/lib"))
import drm_dual as dual  # noqa: E402
import drm_kms_readback as kms  # noqa: E402


def row(name, crtc, mode="1024x768", refresh=60, connection="connected", enabled=True, active=True, fb=50):
    """A drm_kms_readback.py row as the kernel reports it."""
    if crtc is None:
        return {"connector": name, "connection": connection, "crtc": None, "enabled": False,
                "active": False, "fb": 0, "mode": None, "refresh": None, "position": None}
    return {"connector": name, "connection": connection, "crtc": crtc, "enabled": enabled,
            "active": active, "fb": fb if active else 0, "mode": mode if enabled else None,
            "refresh": refresh if enabled else None, "position": [0, 0]}


class KernelAbi(unittest.TestCase):
    def test_struct_sizes_match_drm_mode_h(self):
        self.assertEqual(ctypes.sizeof(kms.ModeInfo), 68)
        self.assertEqual(ctypes.sizeof(kms.Crtc), 104)
        self.assertEqual(ctypes.sizeof(kms.Encoder), 20)
        self.assertEqual(ctypes.sizeof(kms.Connector), 80)
        self.assertEqual(ctypes.sizeof(kms.CardRes), 64)
        self.assertEqual(ctypes.sizeof(kms.SetClientCap), 16)

    def test_ioctl_numbers_match_the_kernel(self):
        # Values of DRM_IOCTL_MODE_* and DRM_IOCTL_SET_CLIENT_CAP on x86_64.
        self.assertEqual(kms.GETCRTC, 0xC06864A1)
        self.assertEqual(kms.GETENCODER, 0xC01464A6)
        self.assertEqual(kms.GETCONNECTOR, 0xC05064A7)
        self.assertEqual(kms.GETRESOURCES, 0xC04064A0)
        self.assertEqual(kms.SET_CLIENT_CAP, 0x4010640D)

    def test_mode_and_connector_names(self):
        mode = kms.ModeInfo(hdisplay=1680, vdisplay=1050, vrefresh=60)
        self.assertEqual(kms.mode_name(mode), "1680x1050")
        mode.flags = 0x10
        self.assertEqual(kms.mode_name(mode), "1680x1050i")
        self.assertEqual(kms.connector_name(15, 2), "Virtual-2")
        self.assertEqual(kms.connector_name(11, 1), "HDMI-A-1")

    def test_readback_refuses_a_missing_card(self):
        result = subprocess.run([sys.executable, str(ROOT / "scripts/lib/drm_kms_readback.py"),
                                 "/nonexistent/card"], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)


class Configfs(unittest.TestCase):
    def test_two_connected_pipes_then_enable(self):
        script = dual.configfs_script("/sys/kernel/config/vkms/tuna-dual", 2)
        lines = script.splitlines()
        self.assertEqual(lines[-1], 'echo 1 > "$C/enabled"')
        for i in (0, 1):
            self.assertIn(f'echo 1 > "$C/connectors/connector{i}/status"', lines)
            self.assertIn(f'ln -s "$C/crtcs/crtc{i}" "$C/encoders/encoder{i}/possible_crtcs"', lines)
        self.assertNotIn("connector2", script)

    def test_apply_config_shape(self):
        self.assertEqual(dual.apply_config([(0, 0, 1, True, [("Virtual-2", "1024x768@60.000"),
                                                              ("Virtual-3", "1024x768@60.000")])]),
                         [(0, 0, 1.0, 0, True, [("Virtual-2", "1024x768@60.000", {}),
                                                ("Virtual-3", "1024x768@60.000", {})])])


GNOME_STATE = (7, [
    (("Virtual-2", "unknown", "unknown", "unknown"), [
        ("1024x768@60.004", 1024, 768, 60.004, 1.0, [1.0, 1.254901960784314], {"is-current": True, "is-preferred": True}),
        ("1680x1050@59.954", 1680, 1050, 59.954, 1.0, [1.0, 1.25, 1.5, 1.75, 2.0], {}),
        ("1920x1080@60.000", 1920, 1080, 60.0, 1.0, [1.0, 1.25, 1.5, 1.7777777777777777, 2.0], {}),
        ("1920x1080i@60.000", 1920, 1080, 60.0, 1.0, [1.0, 1.25, 2.0], {"is-interlaced": True}),
    ], {}),
    (("Virtual-3", "unknown", "unknown", "unknown"), [
        ("1024x768@60.004", 1024, 768, 60.004, 1.0, [1.0, 1.254901960784314], {"is-current": True}),
    ], {}),
], [], {})


class Modes(unittest.TestCase):
    def test_current_modes(self):
        self.assertEqual(dual.current_modes(GNOME_STATE), {
            "Virtual-2": ("1024x768@60.004", 1024, 768, [1.0, 1.254901960784314]),
            "Virtual-3": ("1024x768@60.004", 1024, 768, [1.0, 1.254901960784314]),
        })

    def test_mixed_mode_is_the_smallest_with_scale_two_and_a_fraction(self):
        self.assertEqual(dual.mixed_scale_mode(GNOME_STATE[1][0][1]),
                         ("1680x1050@59.954", 1680, 1050, 59.954, 1.25))
        # vkms's 1024x768 default offers no scale 2 (under 800x480 logical).
        self.assertIsNone(dual.mixed_scale_mode(GNOME_STATE[1][0][1][:1]))

    def test_fractional_scale_needs_an_integral_layout(self):
        self.assertAlmostEqual(dual.fractional_scale(1024, 768, [1.0, 1.254901960784314]), 1.254901960784314)
        self.assertIsNone(dual.fractional_scale(1024, 768, [1.0, 1.3, 2.0]))
        # smithay sends round(scale * 120) (wayland/fractional_scale).
        self.assertEqual(dual.wire_scale(1.254901960784314), 151 / 120)
        self.assertEqual(dual.wire_scale(1.25), 1.25)


class Kms(unittest.TestCase):
    def test_extended(self):
        good = [row("Virtual-2", 40), row("Virtual-3", 41)]
        self.assertEqual(dual.check_extended(good, {"Virtual-2": "1024x768", "Virtual-3": "1024x768"}), [])
        self.assertTrue(dual.check_extended([row("Virtual-2", 40), row("Virtual-3", None)],
                                            {"Virtual-2": "1024x768", "Virtual-3": "1024x768"}))
        self.assertTrue(dual.check_extended([row("Virtual-2", 40), row("Virtual-3", 40)],
                                            {"Virtual-2": "1024x768", "Virtual-3": "1024x768"}))
        self.assertTrue(dual.check_extended(good, {"Virtual-2": "1680x1050"}))

    def test_mirror(self):
        self.assertEqual(dual.check_mirrored([row("Virtual-2", 40), row("Virtual-3", 41)],
                                             "Virtual-2", "Virtual-3"), [])
        # Old behaviour: the clone left dark, or lit in another mode.
        self.assertTrue(dual.check_mirrored([row("Virtual-2", 40), row("Virtual-3", None)],
                                            "Virtual-2", "Virtual-3"))
        self.assertTrue(dual.check_mirrored([row("Virtual-2", 40), row("Virtual-3", 41, mode="800x600")],
                                            "Virtual-2", "Virtual-3"))
        self.assertTrue(dual.check_mirrored([row("Virtual-2", 40), row("Virtual-3", 41, refresh=75)],
                                            "Virtual-2", "Virtual-3"))
        # A CRTC on but without a framebuffer is not scanning out.
        self.assertTrue(dual.check_mirrored([row("Virtual-2", 40), row("Virtual-3", 41, fb=0)],
                                            "Virtual-2", "Virtual-3"))

    def test_disabled(self):
        self.assertEqual(dual.check_disabled([row("Virtual-2", 40), row("Virtual-3", None)],
                                             "Virtual-3", ["Virtual-2"]), [])
        # A CRTC kept but switched off also counts as off.
        self.assertEqual(dual.check_disabled([row("Virtual-2", 40), row("Virtual-3", 41, enabled=False, active=False)],
                                             "Virtual-3", ["Virtual-2"]), [])
        # Old behaviour: the disabled monitor kept scanning out.
        self.assertTrue(dual.check_disabled([row("Virtual-2", 40), row("Virtual-3", 41)],
                                            "Virtual-3", ["Virtual-2"]))
        # Only blanked (DPMS off) is not disabled.
        self.assertTrue(dual.check_disabled([row("Virtual-2", 40), row("Virtual-3", 41, active=False)],
                                            "Virtual-3", ["Virtual-2"]))
        # Disabling must not take the other output down, nor be an unplug.
        self.assertTrue(dual.check_disabled([row("Virtual-2", None), row("Virtual-3", None)],
                                            "Virtual-3", ["Virtual-2"]))
        self.assertTrue(dual.check_disabled([row("Virtual-2", 40), row("Virtual-3", None, connection="disconnected")],
                                            "Virtual-3", ["Virtual-2"]))


def state(outputs=None, monitors=None, windows=None, throttled=(), active=2):
    return {
        "outputs": outputs or [
            {"name": "Virtual-2", "primary": True, "rect": [0, 0, 1024, 768]},
            {"name": "Virtual-3", "primary": False, "rect": [1024, 0, 1024, 768]},
        ],
        "display_config": {"monitors": monitors or []},
        "windows": windows or [],
        "throttled_windows": list(throttled),
        "active_workspace": active,
    }


class Layout(unittest.TestCase):
    def test_outputs_and_display_config(self):
        s = state(monitors=[{"connector": "Virtual-2", "position": [0, 0], "mirrors": None},
                            {"connector": "Virtual-3", "position": [0, 0], "mirrors": "Virtual-2"}])
        self.assertEqual(dual.check_display_config(s, {"Virtual-2": ([0, 0], None),
                                                       "Virtual-3": ([0, 0], "Virtual-2")}), [])
        self.assertTrue(dual.check_display_config(s, {"Virtual-2": ([0, 0], None),
                                                      "Virtual-3": (None, None)}))
        self.assertEqual(dual.check_outputs(s, {"Virtual-2": [0, 0, 1024, 768],
                                                "Virtual-3": [1024, 0, 1024, 768]}), [])
        self.assertTrue(dual.check_outputs(s, {"Virtual-2": [0, 0, 1024, 768]}))

    MIRROR_XML = """<monitors version="2"><configuration>
<logicalmonitor><x>0</x><y>0</y><scale>1</scale><primary>yes</primary>
<monitor><monitorspec><connector>Virtual-2</connector><vendor>Tuna Desktop</vendor></monitorspec></monitor>
<monitor><monitorspec><connector>Virtual-3</connector><vendor>Tuna Desktop</vendor></monitorspec></monitor>
</logicalmonitor></configuration></monitors>"""
    DISABLED_XML = """<monitors version="2"><configuration>
<logicalmonitor><x>0</x><y>0</y><scale>1</scale><primary>yes</primary>
<monitor><monitorspec><connector>Virtual-2</connector></monitorspec></monitor></logicalmonitor>
<disabled><monitorspec><connector>Virtual-3</connector></monitorspec></disabled>
</configuration></monitors>"""
    EXTENDED_XML = """<monitors version="2"><configuration>
<logicalmonitor><x>0</x><y>0</y><scale>1</scale><primary>yes</primary>
<monitor><monitorspec><connector>Virtual-2</connector></monitorspec></monitor></logicalmonitor>
<logicalmonitor><x>1024</x><y>0</y><scale>1</scale>
<monitor><monitorspec><connector>Virtual-3</connector></monitorspec></monitor></logicalmonitor>
</configuration></monitors>"""

    def test_kept_layouts(self):
        both = ["Virtual-2", "Virtual-3"]
        self.assertEqual(dual.check_kept(self.MIRROR_XML, mirrored=both, connectors=both), [])
        self.assertEqual(dual.check_kept(self.DISABLED_XML, disabled="Virtual-3", connectors=both), [])
        # Old behaviour: the change saved as an extended layout, or for
        # another monitor set, does not count.
        self.assertTrue(dual.check_kept(self.EXTENDED_XML, mirrored=both, connectors=both))
        self.assertTrue(dual.check_kept(self.EXTENDED_XML, disabled="Virtual-3", connectors=both))
        self.assertTrue(dual.check_kept(self.MIRROR_XML, disabled="Virtual-3", connectors=both))
        self.assertTrue(dual.check_kept(self.DISABLED_XML, disabled="Virtual-3", connectors=["Virtual-2"]))


class Windows(unittest.TestCase):
    def test_overlap_picks_the_output_and_the_primary_on_ties(self):
        s = state()
        self.assertEqual(dual.output_overlapping_most([916, 40, 360, 240], s), "Virtual-3")
        self.assertEqual(dual.output_overlapping_most([772, 40, 360, 240], s), "Virtual-2")
        self.assertEqual(dual.output_overlapping_most([844, 40, 360, 240], s), "Virtual-2")
        self.assertTrue(dual.spans([916, 40, 360, 240], s, "Virtual-2", "Virtual-3"))
        self.assertFalse(dual.spans([100, 40, 360, 240], s, "Virtual-2", "Virtual-3"))
        self.assertIsNone(dual.output_overlapping_most([5000, 0, 10, 10], s))

    def test_client_scale(self):
        self.assertEqual(dual.check_client_scale(151 / 120, 1.254901960784314), [])
        self.assertEqual(dual.check_client_scale(1.25, 1.25), [])
        self.assertEqual(dual.check_client_scale(2.0, 2.0), [])
        # Old behaviour: a spanning window kept the primary's scale.
        self.assertTrue(dual.check_client_scale(1.25, 2.0))
        self.assertTrue(dual.check_client_scale(None, 2.0))
        # GTK without wp_fractional_scale rounds up to the integer scale.
        self.assertTrue(dual.check_client_scale(2.0, 1.254901960784314))

    def test_pointer(self):
        self.assertEqual(dual.check_pointer([180.5, 120.25], [1096.5, 160.25], [916, 40, 360, 240]), [])
        # Physical (scale 2) coordinates instead of logical ones.
        self.assertTrue(dual.check_pointer([361.0, 240.5], [1096.5, 160.25], [916, 40, 360, 240]))
        self.assertTrue(dual.check_pointer(None, [1096.5, 160.25], [916, 40, 360, 240]))

    def test_workspace_split(self):
        p = {"id": 1, "workspace": 2, "rect": [100, 100, 360, 240]}
        s_sticky = {"id": 2, "workspace": 3, "rect": [1200, 100, 360, 240]}
        good = state(windows=[p, s_sticky], throttled=[1], active=3)
        self.assertEqual(dual.check_workspace_split(good, 1, 2, True), [])
        # Old behaviour: the secondary-output window stays on its
        # workspace and hides with the primary's.
        old = state(windows=[p, dict(s_sticky, workspace=2)], throttled=[1, 2], active=3)
        self.assertTrue(dual.check_workspace_split(old, 1, 2, True))
        # Or it reports the active workspace but is still throttled.
        self.assertTrue(dual.check_workspace_split(state(windows=[p, s_sticky], throttled=[1, 2], active=3),
                                                   1, 2, True))
        # With workspaces on every output it must hide.
        self.assertEqual(dual.check_workspace_split(old, 1, 2, False), [])
        self.assertTrue(dual.check_workspace_split(good, 1, 2, False))
        # The primary window still shown fails either way.
        self.assertTrue(dual.check_workspace_split(state(windows=[dict(p, workspace=3), s_sticky], active=3),
                                                   1, 2, True))

    def test_carried_window_lands_on_the_active_workspace(self):
        carried = state(windows=[{"id": 2, "workspace": 3, "rect": [80, 120, 360, 240]}], active=3)
        self.assertEqual(dual.check_window_on(carried, 2, output="Virtual-2", workspace=3, hidden=False), [])
        # Old behaviour: back on the workspace it opened on.
        stale = state(windows=[{"id": 2, "workspace": 2, "rect": [80, 120, 360, 240]}], throttled=[2], active=3)
        self.assertTrue(dual.check_window_on(stale, 2, output="Virtual-2", workspace=3, hidden=False))
        self.assertTrue(dual.check_window_on(carried, 2, output="Virtual-3"))
        self.assertTrue(dual.check_window_on(carried, 9))

    def test_frame_rates(self):
        self.assertEqual(dual.check_frame_rate("S", 150, 3, True, 10, 3), [])
        self.assertTrue(dual.check_frame_rate("S", 4, 3, True, 10, 3))
        self.assertEqual(dual.check_frame_rate("P", 3, 3, False, 10, 3), [])
        self.assertTrue(dual.check_frame_rate("P", 150, 3, False, 10, 3))

    @staticmethod
    def heartbeats(rate_at, start=50.0, seconds=8.0):
        """A client's beats (ten a second) drawing at rate_at(t) frames/s."""
        beats, frames, t, owed = [], 0, start, 0.0
        while t <= start + seconds:
            owed += rate_at(t) * 0.1
            frames += int(owed)
            owed -= int(owed)
            beats.append({"t": round(t, 3), "frames": frames})
            t += 0.1
        return beats

    def test_rates_count_after_the_switch_settles(self):
        switch_end = 52.0
        # The CI run's shape (run 44873094927, client S): full rate through
        # the slide, the callbacks owed from it trickling in for a moment,
        # then HIDDEN_FRAME_INTERVAL's one frame a second.
        def hidden_after(t):
            if t < switch_end:
                return 60
            if t < switch_end + 0.15:
                return 40
            return 1
        beats = self.heartbeats(hidden_after)
        # Counting straight from the switch (the old window) takes in the
        # tail and fails a throttled window.
        from_switch = dual.frames_between(beats, switch_end - 0.1, switch_end + 2.9)
        self.assertTrue(dual.check_frame_rate("S", from_switch, 3, False, 10, 3))
        # The settled window counts only the hidden rate.
        settled = dual.frames_between(beats, switch_end + 1.5, switch_end + 4.5)
        self.assertLessEqual(settled, 4)
        self.assertEqual(dual.check_frame_rate("S", settled, 3, False, 10, 3), [])
        # A window that is not throttled at all still fails, settled or not.
        unthrottled = self.heartbeats(lambda t: 60)
        frames = dual.frames_between(unthrottled, switch_end + 1.5, switch_end + 4.5)
        self.assertGreater(frames, 150)
        self.assertTrue(dual.check_frame_rate("S", frames, 3, False, 10, 3))
        # So does one throttled only to a few frames a second.
        slow = self.heartbeats(lambda t: 6)
        self.assertTrue(dual.check_frame_rate(
            "S", dual.frames_between(slow, switch_end + 1.5, switch_end + 4.5), 3, False, 10, 3))
        # And a shown window must keep its rate in the same window.
        self.assertEqual(dual.check_frame_rate("P", frames, 3, True, 10, 3), [])

    def test_a_window_the_beats_do_not_cover_is_no_count(self):
        beats = self.heartbeats(lambda t: 60, seconds=2.0)
        self.assertIsNone(dual.frames_between(beats, 49.0, 51.0))
        self.assertIsNone(dual.frames_between(beats, 50.5, 55.0))
        self.assertIsNotNone(dual.frames_between(beats, 50.5, 51.9))


if __name__ == "__main__":
    unittest.main()
