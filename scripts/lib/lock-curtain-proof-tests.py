#!/usr/bin/env python3
import importlib.util
import io
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "lock_curtain_proof", Path(__file__).with_name("lock-curtain-proof.py"))
proof = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proof)

P = "tuna-shell-gtk: unlock-dialog "


class GnomeCurtainTests(unittest.TestCase):
    def test_frames_follow_set_transition_progress(self):
        clock, prompt = proof.frames(0.75, True)
        for got, want in zip(clock + prompt, (0.25, -150, 0.475, 0.75, 50, 0.825)):
            self.assertAlmostEqual(got, want)
        clock, prompt = proof.frames(0.75, False)
        self.assertEqual(clock, (0.25, 0.0, 1.0))
        self.assertEqual(prompt, (0.75, 0.0, 1.0))

    def test_release_matches_swipe_tracker(self):
        # Slow: the nearer page, at ANIMATION_BASE_VELOCITY, capped.
        self.assertEqual(proof.release(0.3, 0, 0, 0.0, 300, False), (0, 400))
        self.assertEqual(proof.release(0.7, 0, 0, 0.0, 300, False), (1, 400))
        self.assertEqual(proof.release(0.5, 0, 0, 0.0, 300, False)[0], 0)
        # A flick from a short drag reaches the prompt.
        self.assertEqual(proof.release(0.1, 0, 0, 0.6, 300, False)[0], 1)
        end, duration = proof.release(0.1, 0, 0, 6.0, 300, False)
        self.assertEqual(end, 1)
        self.assertAlmostEqual(duration, 135.0)
        # Below the touch threshold the nearer page wins.
        self.assertEqual(proof.release(0.1, 0, 0, 0.29, 300, False)[0], 0)
        # Down from the prompt, and a cancel going back.
        self.assertEqual(proof.release(0.9, 1, 1, -0.6, 300, False)[0], 0)
        self.assertEqual(proof.release(0.8, 0, 0, 5.0, 300, True)[0], 0)
        # Never shorter than 100 ms.
        self.assertEqual(proof.release(0.99, 0, 0, 0.0, 300, False), (1, 100))


class TraceTests(unittest.TestCase):
    def run_cli(self, lines, *argv):
        with tempfile.NamedTemporaryFile("w", suffix=".log", delete=False) as fh:
            fh.write("\n".join(lines) + "\n")
            log = fh.name
        out = io.StringIO()
        with redirect_stdout(out):
            code = proof.main([argv[0], log, "1", *argv[1:], "--timeout", "0"])
        Path(log).unlink()
        return code, out.getvalue().strip()

    def test_frame_and_line(self):
        lines = [
            "noise",
            P + "ease to=prompt from=0.0000 duration=300 nominal=300 mode=ease-out-quad motion=true clock=manual",
            P + "frame progress=0.7500 motion=true clock=0.250,-150.0,0.475 prompt=0.750,50.0,0.825",
        ]
        self.assertEqual(self.run_cli(lines, "line", "ease to=prompt from=0.0000 duration=300")[0], 0)
        self.assertEqual(self.run_cli(lines, "frame", "0.75", "true")[0], 0)
        with self.assertRaises(SystemExit):
            self.run_cli(lines, "frame", "0.75", "false")
        with self.assertRaises(SystemExit):
            self.run_cli(lines, "frame", "0.5", "true")
        with self.assertRaises(SystemExit):
            self.run_cli(lines, "line", "settled page=prompt")

    def test_follow_release_and_eased(self):
        lines = [
            P + "swipe begin source=drag progress=0.0000 distance=300",
            P + "frame progress=0.3000 motion=true clock=0.700,-60.0,0.790 prompt=0.300,140.0,0.510",
            P + "swipe end source=drag progress=0.300000 initial=0.000000 velocity=0.000000 "
                "distance=300 cancelled=false end=0 duration=400.000",
            P + "ease to=clock from=0.3000 duration=400 nominal=400 mode=ease-out-cubic motion=true clock=manual",
        ]
        self.assertEqual(self.run_cli(lines, "distance"), (0, "300"))
        self.assertEqual(self.run_cli(lines, "follow", "100", "10"), (0, "0.3000"))
        with self.assertRaises(SystemExit):
            self.run_cli(lines, "follow", "200", "10")
        self.assertEqual(self.run_cli(lines, "release"), (0, "0 400.000 0.300000 0.000000"))
        # 100 ms of 400: ease-out cubic 0.578125 of the way back.
        eased = 0.3 * (1 - 0.578125)
        clock, prompt = proof.frames(eased, True)
        frame = (f"frame progress={eased:.4f} motion=true "
                 f"clock={clock[0]:.3f},{clock[1]:.1f},{clock[2]:.3f} "
                 f"prompt={prompt[0]:.3f},{prompt[1]:.1f},{prompt[2]:.3f}")
        self.assertEqual(self.run_cli(lines + [P + frame], "eased", "100", "true"), (0, f"{eased:.4f}"))
        with self.assertRaises(SystemExit):
            self.run_cli(lines + [P + frame], "eased", "200", "true")

    def test_a_release_that_is_not_gnomes_fails(self):
        lines = [P + "swipe end source=drag progress=0.300000 initial=0.000000 velocity=0.000000 "
                     "distance=300 cancelled=false end=1 duration=400.000"]
        with self.assertRaises(SystemExit):
            self.run_cli(lines, "release")


if __name__ == "__main__":
    unittest.main()
