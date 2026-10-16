"""tuna-fade-match fails closed on ambiguous merged bytes.

The shell and compositor share nested.log, so one line can carry both the
compositor's "false" and the shell's "true". A greedy animated=.*VALUE
query exits 0 for BOTH policies on such bytes — unsound as motion
evidence. The matcher therefore confirms a start value only when the text
after "animated=" runs to end-of-line exactly, and refuses (exit 1) any
polluted line for BOTH policies. "settled" stays substring-based: all
three tokens are shell-exclusive vocabulary (sole emitters
switcher.rs:169/174/192; no compositor log macro emits them), and one
merged line holds pieces of at most one shell line.
"""
import subprocess
from pathlib import Path
import unittest

MATCH = Path(__file__).with_name("tuna-fade-match")

# Exact bytes from ci.yml run 38293527173, journey job 114960925690: the
# Reduced Motion popup-out fade (animated=true) split mid-line by a
# compositor modifier line. The old exact grep missed it and failed
# G-ANIMATIONS-OFF ("Reduced Motion dropped the switcher popup's fade").
INTERLEAVED_TRUE = (
    "tuna-compositor: modifier alt keycode="
    "tuna-shell-gtk: switcher fade "
    "56"
    "popup-out"
    " pressed="
    " start animated="
    "false"
    "true\n"
)

INTACT_TRUE = "tuna-shell-gtk: switcher fade popup-out start animated=true\n"
INTACT_FALSE = "tuna-shell-gtk: switcher fade popup-out start animated=false\n"
INTACT_SETTLED = "tuna-shell-gtk: switcher fade popup-out settled\n"
INTACT_THUMBS = "tuna-shell-gtk: switcher fade thumbnails-in start animated=true\n"


def match(segment, *args):
    proc = subprocess.run(
        [str(MATCH), *args], input=segment,
        capture_output=True, text=True, timeout=30,
    )
    return proc.returncode


class FadeMatchTests(unittest.TestCase):
    def test_intact_lines_match(self):
        self.assertEqual(match(INTACT_TRUE, "popup-out", "start", "true"), 0)
        self.assertEqual(match(INTACT_FALSE, "popup-out", "start", "false"), 0)
        self.assertEqual(match(INTACT_SETTLED, "popup-out", "settled"), 0)
        self.assertEqual(match(INTACT_THUMBS, "thumbnails-in", "start", "true"), 0)

    def test_observed_interleave_refuses_both_policies(self):
        # Soundness regression: the exact observed bytes carry the
        # compositor's "false" ("...animated=falsetrue"). The same segment
        # must NEVER confirm opposite states — both queries refuse it.
        self.assertEqual(match(INTERLEAVED_TRUE, "popup-out", "start", "true"), 1)
        self.assertEqual(match(INTERLEAVED_TRUE, "popup-out", "start", "false"), 1)

    def test_observed_interleave_inside_a_segment(self):
        segment = (
            "tuna-compositor: switcher Cancel\n"
            "tuna-compositor: switcher Cycle { forward: true, group: false }\n"
            + INTERLEAVED_TRUE
            + "\n"
            "tuna-shell-gtk: switcher fade popup-out settled\n"
        )
        # Ambiguous start value: both policies refuse. The intact settled
        # line on its own line still confirms (shell-exclusive tokens).
        self.assertEqual(match(segment, "popup-out", "start", "true"), 1)
        self.assertEqual(match(segment, "popup-out", "start", "false"), 1)
        self.assertEqual(match(segment, "popup-out", "settled"), 0)

    def test_wrong_value_does_not_match_intact(self):
        self.assertEqual(match(INTACT_FALSE, "popup-out", "start", "true"), 1)
        self.assertEqual(match(INTACT_TRUE, "popup-out", "start", "false"), 1)
        self.assertEqual(match(INTACT_TRUE, "thumbnails-in", "start", "true"), 1)

    def test_settled_interleave_matches(self):
        interleaved = (
            "tuna-shell-gtk: switcher fade "
            "56"
            "popup-out"
            " pressed="
            " settled\n"
        )
        self.assertEqual(match(interleaved, "popup-out", "settled"), 0)

    def test_empty_and_unrelated_segments_do_not_match(self):
        for segment in ["", "\n", "tuna-compositor: switcher Cancel\n"]:
            with self.subTest(segment=segment):
                self.assertEqual(match(segment, "popup-out", "start", "true"), 1)
                self.assertEqual(match(segment, "popup-out", "settled"), 1)

    def test_bad_arguments_exit_two(self):
        for args in [[], ["popup-out"], ["popup-out", "start"],
                     ["popup-out", "start", "maybe"],
                     ["popup-out", "settled", "extra"],
                     ["popup-out", "bogus"], ["../x", "settled"],
                     ["a b", "settled"]]:
            with self.subTest(args=args):
                self.assertEqual(match(INTACT_TRUE, *args), 2)


if __name__ == "__main__":
    unittest.main()
