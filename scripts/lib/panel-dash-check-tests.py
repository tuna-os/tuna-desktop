#!/usr/bin/env python3
"""Unit tests for tuna-panel-dash-check.py, G-PANEL-LAYOUT and
G-DASH-FAVORITES-RUNNING's measurements (#1198, #1199)."""
import importlib.util
import unittest
from pathlib import Path

from PIL import Image, ImageDraw

spec = importlib.util.spec_from_file_location(
    "panel_dash_check", Path(__file__).with_name("tuna-panel-dash-check.py")
)
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

FILL = (43, 43, 43)


def node(role, name, bounds, depth, showing=True):
    return {"role": role, "name": name, "bounds": bounds, "depth": depth, "showing": showing}


def state(width, dash_y=688):
    return {
        "outputs": [{"name": "tuna-0", "primary": True, "rect": [0, 0, width, 800]}],
        "layers": [
            {"namespace": "tuna-panel", "rect": [0, 0, width, 32]},
            {"namespace": "tuna-shell-dash", "rect": [0, dash_y, width, 100]},
        ],
    }


# The bar as the proof measured it at 1280x800, scale 1 (logical
# [left, width] of the outer accessible node; the drawn pill sits 3px in).
LTR = {"Activities": (3, 66), "Date and Time": (578, 125), "System": (1176, 104)}


def mirrored(controls, width):
    return {name: (width - left - size, size) for name, (left, size) in controls.items()}


def panel(controls, width=1280, scale=1.0, pills=None):
    """The accessible tree, state and three hover captures for CONTROLS;
    PILLS overrides the drawn [left, right) of a control's fill."""
    nodes = [node("frame", "Top Bar", [0, 0, width, 32], 1),
             node("panel", "", [0, 0, width, 32], 2)]
    roles = {"Activities": "toggle button", "Date and Time": "push button", "System": "push button"}
    for name, (left, size) in controls.items():
        nodes.append(node(roles[name], name, [left, 0, size, 32], 3))
    images = []
    for name, (left, size) in controls.items():
        image = Image.new("RGB", (round(width * scale), round(40 * scale)))
        drawn = (pills or {}).get(name)
        if drawn is None:
            # Outer menu buttons carry the 3px margin; Activities is its pill.
            inset = 0 if name == "Activities" else 3
            drawn = (round((left + inset) * scale), round((left + size - inset) * scale))
        if drawn != ():
            ImageDraw.Draw(image).rectangle(
                [drawn[0], round(3 * scale), drawn[1] - 1, round(29 * scale)], fill=FILL)
        images.append(image)
    return nodes, state(width), images


class Panel(unittest.TestCase):
    def run_check(self, controls, direction="ltr", width=1280, scale=1.0, pills=None):
        nodes, doc, images = panel(controls, width, scale, pills)
        return check.check_panel(nodes, doc, images, round(width * scale), direction)

    def test_gnome_layout_passes_left_to_right(self):
        detail = self.run_check(LTR)
        self.assertIn("Activities 3-69", detail)
        self.assertIn("System 1179-1277", detail)
        self.assertIn("clock centre +0.50 drawn", detail)

    def test_mirrored_layout_passes_right_to_left(self):
        self.run_check(mirrored(LTR, 1280), "rtl")

    def test_fractional_scale_measures_physical_pixels(self):
        controls = {"Activities": (3, 66), "Date and Time": (450, 124), "System": (920, 104)}
        detail = self.run_check(controls, width=1024, scale=1.25)
        self.assertIn("scale 1.25", detail)

    def test_off_centre_clock_fails_by_two_pixels(self):
        controls = dict(LTR, **{"Date and Time": (580, 125)})
        with self.assertRaisesRegex(check.CheckError, "accessible centre is \\+2.50"):
            self.run_check(controls)

    def test_drawn_clock_off_centre_fails_though_its_node_is_centred(self):
        with self.assertRaisesRegex(check.CheckError, "pill centre is \\+2.50"):
            self.run_check(LTR, pills={"Date and Time": (583, 702)})

    def test_activities_away_from_the_edge_fails(self):
        with self.assertRaisesRegex(check.CheckError, "Activities's pill starts 12"):
            self.run_check(LTR, pills={"Activities": (12, 78)})

    def test_system_menu_short_of_the_right_edge_fails(self):
        with self.assertRaisesRegex(check.CheckError, "System's pill ends 9"):
            self.run_check(LTR, pills={"System": (1173, 1271)})

    def test_left_to_right_bar_fails_a_right_to_left_locale(self):
        with self.assertRaisesRegex(check.CheckError, "reads .* rtl"):
            self.run_check(LTR, "rtl")

    def test_mirrored_bar_fails_a_left_to_right_locale(self):
        with self.assertRaisesRegex(check.CheckError, "reads .* ltr"):
            self.run_check(mirrored(LTR, 1280), "ltr")

    def test_capture_without_a_hover_fill_fails(self):
        with self.assertRaisesRegex(check.CheckError, "no hover fill across System"):
            self.run_check(LTR, pills={"System": ()})

    def test_missing_control_fails(self):
        nodes, doc, images = panel(LTR)
        nodes = [n for n in nodes if n["name"] != "System"]
        with self.assertRaisesRegex(check.CheckError, "no showing control named 'System'"):
            check.check_panel(nodes, doc, images, 1280, "ltr")

    def test_points_rest_below_the_measured_row(self):
        nodes, doc, _ = panel(LTR)
        self.assertEqual(check.panel_points(nodes, doc, 1280),
                         [("Activities", 36, 22), ("Date and Time", 640, 22), ("System", 1228, 22)])


DASH = ["Tuna Dash Zeta", "Tuna Dash Epsilon", "Tuna Dash Eta", "Show Apps"]
RUNNING = ["Tuna Dash Epsilon", "Tuna Dash Eta"]


def dash(order, running, separator_after="Tuna Dash Epsilon", rtl=False,
         accessible_dots=None, drawn_dots=None, scale=1.0):
    """The dash's accessible tree, state and capture: 76px tiles 4px
    apart from x=477 (mirrored right to left), each running tile with a
    5px dot 12px above its bottom."""
    width = 1280
    nodes = [node("frame", "Dash", [0, 0, width, 100], 1), node("panel", "", [475, 12, 349, 100], 2)]
    image = Image.new("RGB", (round(width * scale), round(800 * scale)), (36, 36, 40))
    draw = ImageDraw.Draw(image)
    accessible_dots = running if accessible_dots is None else accessible_dots
    drawn_dots = running if drawn_dots is None else drawn_dots
    for index, name in enumerate(order):
        left = 477 + 80 * index
        if rtl:
            left = width - left - 76
        nodes.append(node("panel", "", [left - 2, 12, 80, 76], 3))
        role = "toggle button" if name == "Show Apps" else "push button"
        nodes.append(node(role, name, [left, 12, 76, 76], 4))
        nodes.append(node("image", "", [left + 6, 18, 64, 64], 5))
        if name in accessible_dots:
            nodes.append(node("panel", "", [left + 36, 83, 5, 5], 5))
        if name in drawn_dots:
            x, y = (left + 36) * scale, (688 + 83) * scale
            draw.rectangle([round(x), round(y), round(x + 5 * scale) - 1, round(y + 5 * scale) - 1],
                           fill=(250, 250, 251))
        if name == separator_after:
            edge = left + 78 if not rtl else left - 3
            nodes.append(node("separator", "", [edge, 18, 1, 64], 3))
    return nodes, state(width), image


class Dash(unittest.TestCase):
    def run_check(self, nodes, doc, image, direction="ltr", scale=1.0):
        return check.check_dash(nodes, doc, image, round(1280 * scale), direction, DASH,
                                RUNNING, "Tuna Dash Epsilon")

    def test_favourites_then_running_with_dots_passes(self):
        detail = self.run_check(*dash(DASH, RUNNING))
        self.assertIn("under ['Tuna Dash Epsilon', 'Tuna Dash Eta']", detail)

    def test_right_to_left_dash_passes_mirrored(self):
        self.run_check(*dash(DASH, RUNNING, rtl=True), direction="rtl")

    def test_fractional_scale_finds_the_dots(self):
        self.run_check(*dash(DASH, RUNNING, scale=1.25), scale=1.25)

    def test_running_app_before_the_favourites_fails(self):
        order = ["Tuna Dash Eta", "Tuna Dash Zeta", "Tuna Dash Epsilon", "Show Apps"]
        with self.assertRaisesRegex(check.CheckError, "the dash lists"):
            self.run_check(*dash(order, RUNNING))

    def test_favourites_out_of_schema_order_fail(self):
        order = ["Tuna Dash Epsilon", "Tuna Dash Zeta", "Tuna Dash Eta", "Show Apps"]
        with self.assertRaisesRegex(check.CheckError, "the dash lists"):
            self.run_check(*dash(order, RUNNING))

    def test_left_to_right_dash_fails_right_to_left(self):
        with self.assertRaisesRegex(check.CheckError, "not laid out rtl"):
            self.run_check(*dash(DASH, RUNNING), direction="rtl")

    def test_missing_drawn_dot_fails(self):
        with self.assertRaisesRegex(check.CheckError, "drawn running dots under \\['Tuna Dash Epsilon'\\]"):
            self.run_check(*dash(DASH, RUNNING, drawn_dots=["Tuna Dash Epsilon"]))

    def test_dot_under_a_favourite_that_is_not_running_fails(self):
        dots = ["Tuna Dash Zeta", *RUNNING]
        with self.assertRaisesRegex(check.CheckError, "running dots under \\['Tuna Dash Zeta'"):
            self.run_check(*dash(DASH, RUNNING, accessible_dots=dots, drawn_dots=dots))

    def test_dot_missing_from_the_accessible_tree_fails(self):
        with self.assertRaisesRegex(check.CheckError, "accessible running dots"):
            self.run_check(*dash(DASH, RUNNING, accessible_dots=["Tuna Dash Eta"]))

    def test_missing_separator_fails(self):
        with self.assertRaisesRegex(check.CheckError, "no separator between"):
            self.run_check(*dash(DASH, RUNNING, separator_after=None))


if __name__ == "__main__":
    unittest.main()
