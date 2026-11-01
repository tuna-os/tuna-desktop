"""Exercise the actual snapshot fence without a display, Cargo, or wall sleeps."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
PROOF = (ROOT / "scripts/tuna-magnifier-proof").read_text()
SNAPSHOT = "snapshot() {" + PROOF.split("snapshot() {", 1)[1].split("\n}\n", 1)[0] + "\n}\n"
HARNESS = r"""
set -eu
clock=0
ticks=0
date() { printf '%s\n' "$clock"; }
sleep() {
    ticks=$((ticks + 1))
    clock=$((clock + 1))
    if [ "$ticks" -eq "$SHELL_AFTER" ]; then
        cp "$RUN_PRIV/next-settings.json" "$RUN_PRIV/tuna-magnifier.json"
    fi
    if [ "$ticks" -eq "$COMPOSITOR_AFTER" ]; then
        cp "$RUN_PRIV/next-state.json" "$CSTATE"
    fi
}
""" + SNAPSHOT + r"""
if snapshot "$@"; then status=0; else status=$?; fi
printf '%s %s\n' "$status" "$ticks"
"""


def submitted_predicate(value):
    return ('.magnifier | .regions["tuna-0"].submitted.generation == .generation and '
            f'.regions["tuna-0"].submitted.invert_lightness == {value}')


class InversionObservation(unittest.TestCase):
    def observe(self, *, shell=True, compositor=False, expected=True,
                shell_after=-1, compositor_after=-1, acknowledge=True, submitted_generation=7):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "tuna-magnifier.json").write_text(json.dumps({"invert_lightness": shell}))
            submitted = None if compositor is None else {
                "generation": submitted_generation, "invert_lightness": compositor,
            }
            initial = {"magnifier": {
                "generation": 7, "invert_lightness": expected,
                "regions": {"tuna-0": {"submitted": submitted}},
            }}
            (root / "state.json").write_text(json.dumps(initial))
            (root / "next-settings.json").write_text(json.dumps({"invert_lightness": expected}))
            (root / "next-state.json").write_text(json.dumps({
                "magnifier": {
                    "generation": 8,
                    "regions": {"tuna-0": {"submitted": {
                        "generation": 8, "invert_lightness": expected,
                    }}},
                },
                "rendered_frames": 179,
            }))
            value = "true" if expected else "false"
            predicates = [f".invert_lightness == {value}"]
            if acknowledge:
                predicates.append(submitted_predicate(value))
            result = subprocess.run(
                ["sh", "-c", HARNESS, "snapshot-regression", *predicates],
                env=dict(os.environ, RUN_PRIV=directory, CSTATE=str(root / "state.json"),
                         SHELL_AFTER=str(shell_after), COMPOSITOR_AFTER=str(compositor_after)),
                capture_output=True, text=True, timeout=5,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            return tuple(map(int, result.stdout.split()))

    def test_shell_publication_cannot_accept_the_previous_unfiltered_frame(self):
        self.assertEqual(self.observe(compositor_after=2), (0, 2))

    def test_compositor_acknowledgement_cannot_replace_shell_publication(self):
        self.assertEqual(self.observe(shell=False, compositor=True, shell_after=3), (0, 3))

    def test_missing_compositor_effect_field_is_not_an_acknowledgement(self):
        self.assertEqual(self.observe(compositor=None, compositor_after=2), (0, 2))

    def test_older_submitted_generation_cannot_acknowledge_new_settings(self):
        self.assertEqual(self.observe(compositor=True, submitted_generation=6,
                                      compositor_after=2), (0, 2))

    def test_both_handoffs_share_the_original_twenty_second_deadline(self):
        self.assertEqual(self.observe(shell=False, shell_after=15, compositor_after=22), (1, 21))

    def test_reset_waits_for_the_compositor_to_remove_inversion(self):
        self.assertEqual(self.observe(shell=False, compositor=True, expected=False,
                                      compositor_after=2), (0, 2))

    def test_already_applied_effect_adds_no_wait(self):
        self.assertEqual(self.observe(compositor=True), (0, 0))

    def test_other_snapshot_callers_keep_their_original_contract(self):
        self.assertEqual(self.observe(acknowledge=False), (0, 0))

    def test_gate_keeps_both_original_rgb_checks_and_one_capture(self):
        gate = PROOF.split("# M-INVERT: Shell.InvertLightnessEffect", 1)[1].split("# M-CROSSHAIRS:", 1)[0]
        for value in ("true", "false"):
            self.assertIn(f"snapshot '.invert_lightness == {value}' '{submitted_predicate(value)}'", gate)
        self.assertEqual(gate.count("grab 16-invert M-INVERT"), 1)
        self.assertIn('expect M-INVERT 16-invert 440 520 "93 217 160" 14 "inverted green"', gate)
        self.assertIn('expect M-INVERT 16-invert 840 520 "255 122 25" 14 "inverted orange"', gate)
        self.assertLess(gate.index('"$ARTIFACTS/16-invert-settings.json"'), gate.index("grab 16-invert"))
        self.assertLess(gate.index('"$ARTIFACTS/16-invert-state.json"'), gate.index("grab 16-invert"))
        self.assertIn("end=$(($(date +%s) + 20))", SNAPSHOT)
        self.assertIn("sleep 0.3", SNAPSHOT)

    def test_only_successful_output_submission_records_the_receipt(self):
        runtime = (ROOT / "crates/compositor/src/runtime.rs").read_text()
        drawing = runtime.split("let queued = match &mut self.backend {", 1)[1]
        drawing = drawing.split("let hidden = hidden_surfaces", 1)[0]
        nested, native = drawing.split("Backend::Drm(drm) => {", 1)
        receipt_start = nested.rindex("if self.state_path.is_some() {")
        nested_before, nested_after = nested[:receipt_start], nested[receipt_start:]
        # The real error-propagating submit must precede acknowledgement:
        # a failed submission exits, rather than recording the planned effect.
        self.assertRegex(nested_before, re.compile(
            r"\.submit\(Some\(&damage\)\)\s*"
            r"\.map_err\(\|e\| RuntimeError::Dispatch\(e\.to_string\(\)\)\)\?;\s*$"))
        self.assertGreater(nested_before.index(".submit(Some(&damage))"),
                           nested_before.rindex(".finish()"))
        self.assertIn('.mark_submitted("tuna-0", generation, invert_lightness)', nested_after)
        self.assertIn(".map(|plan| (plan.generation, plan.effects.invert))", nested_before)
        # An unfiltered fallback after shader compilation failure must not
        # acknowledge a requested nonidentity effect on either backend.
        shader_guard = re.escape(
            ".filter(|plan| plan.effects.identity || zoom_program.is_some())")
        self.assertRegex(nested_before, re.compile(
            r"if self\.state_path\.is_some\(\) {\s*zoom_submission = zoom_plan\s*"
            r"\.as_ref\(\)\s*" + shader_guard + r"\s*"
            + re.escape(".map(|plan| (plan.generation, plan.effects.invert))")))
        self.assertRegex(nested_after, re.compile(
            r"if self\.state_path\.is_some\(\) {\s*"
            r"if let Some\(\(generation, invert_lightness\)\) = zoom_submission {"))

        failed_queue = native.split("if let Err(e) = out.surface.queue_buffer(", 1)[1]
        failed_queue, native_receipt = failed_queue.split("if self.state_path.is_some() {", 1)
        self.assertRegex(failed_queue, re.compile(
            r"out\.revert_failed_mode\(\);\s*continue;\s*}\s*$"))
        self.assertNotIn("mark_submitted", failed_queue)
        self.assertRegex(native, re.compile(
            r"\.mark_submitted\(\s*&out\.name,\s*plan\.generation,\s*"
            r"plan\.effects\.invert,?\s*\)"))
        self.assertRegex(native_receipt, re.compile(
            r"^\s*if let Some\(plan\) = zoom_plan\s*\.as_ref\(\)\s*"
            + shader_guard + r"\s*{"))
        # Captures/casts can plan settings, but must not acknowledge an output.
        offscreen = runtime.split("fn render_pixels(", 1)[1].split("fn magnifier_inputs(", 1)[0]
        self.assertIn("self.magnifier.plan_for(", offscreen)
        self.assertNotIn("mark_submitted", offscreen)
        self.assertEqual(runtime.count(".mark_submitted("), 2)


if __name__ == "__main__":
    unittest.main()
