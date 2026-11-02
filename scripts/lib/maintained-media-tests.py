#!/usr/bin/env python3
"""Behavioral current-run intake regressions using real media/parser processes.

All media are temporary test fixtures, not production UI qualification. No test
asserts against source text or changes checked-in media/diagnostic receipts.
"""
import hashlib
import io
import base64
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import unittest
import zlib

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))
import maintained_media as contract
import gtk_media_source
from PIL import Image, ImageDraw
from overview_recording import FrameRecorder, recording_command, bind_recorded_interval
from tour_media import TOUR_PUBLIC_CONTROLS
from vm_gnome_reference import cosign_argv

COMMIT = subprocess.check_output(["git", "-C", str(ROOT), "rev-parse", "HEAD"], text=True).strip()
TREE = subprocess.check_output(["git", "-C", str(ROOT), "rev-parse", "HEAD^{tree}"], text=True).strip()
IMAGE = "sha256:" + "3" * 64
PACKAGE = "4" * 64
BASE = "ghcr.io/tuna-os/marlin@sha256:" + "5" * 64
SOURCE_INPUTS = contract.required_source_inputs(ROOT, COMMIT)
PINS = ["--run-id", "39123456789", "--attempt", "2", "--expect-merge", COMMIT,
        "--expect-head", COMMIT, "--expect-tree", TREE, "--expect-source-inputs", SOURCE_INPUTS]
VM_PINS = ["--expect-image", IMAGE, "--expect-package", PACKAGE]


def png(name):
    color = hashlib.sha256(name.encode()).digest()[:3]
    def chunk(kind, blob):
        return struct.pack(">I", len(blob)) + kind + blob + struct.pack(">I", zlib.crc32(kind + blob))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 4, 4, 8, 2, 0, 0, 0)) +
            chunk(b"IDAT", zlib.compress((b"\0" + color * 4) * 4)) + chunk(b"IEND", b""))


def put_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, sort_keys=True) + "\n")


def command(script, *arguments):
    return subprocess.run([str(ROOT / "scripts" / script), *map(str, arguments)],
                          capture_output=True, text=True, timeout=180)


def native_fixture_owner():
    return {"session_uid": 1000, "session": {
                "Id": "3", "User": "1000", "Type": "wayland", "Class": "user",
                "Service": "gdm-password", "VTNr": "2"},
            "processes": [{"pid": 110, "uid": 1000, "start_ticks": 500, "executable": "tuna-shell-gtk"}]}


def native_fixture_png(tag):
    # Synthetic parser inputs, never product screenshots or UI proof.
    image = Image.new("RGB", (1280, 800), "white")
    draw = ImageDraw.Draw(image)
    if tag == "overview-windows":
        draw.rectangle((0, 690, 169, 779), fill=(48, 48, 48))
    elif tag == "alt-tab":
        draw.rectangle((380, 300, 899, 301), fill=(48, 48, 48))
    stream = io.BytesIO()
    image.save(stream, format="PNG")
    return stream.getvalue()


def native_fixture_timing(artifact, mapping):
    frames, confirmations, stills = [], {}, {}
    for index, (filename, _) in enumerate(mapping):
        tag, stamp = Path(filename).stem[2:], 1000 + index * 100
        checksum = contract.digest((artifact / filename).read_bytes())
        frame = {"index": index, "mode": "still", "requested_monotonic_ns": stamp + 2,
                 "completed_monotonic_ns": stamp + 3, "sha256": checksum}
        frames.append(frame)
        stills[tag] = index
        confirmation = {"schema": 1, "tag": tag, "frame_index": index, "png_sha256": checksum,
                        "confirmed": True, "capture_requested_monotonic_ns": stamp + 2,
                        "capture_completed_monotonic_ns": stamp + 3}
        if tag in TOUR_PUBLIC_CONTROLS:
            def observation(start):
                return {"schema": 1, "method": "public-at-spi",
                        "session": native_fixture_owner()["session"],
                        "principal": {"pid": 110, "uid": 1000, "start_ticks": 500,
                                      "executable": "/usr/bin/tuna-shell-gtk", "display": "wayland-0",
                                      "session_id": "3"},
                        "atspi": {"owner": ":1.7", "bus_pid": 110, "bus_uid": 1000},
                        "started_monotonic_ns": start, "completed_monotonic_ns": start + 1,
                        "controls": [{"role": role, "label": label, "showing": True, "visible": True,
                                      "extents": [10, 10, 100, 50]}
                                     for role, label in TOUR_PUBLIC_CONTROLS[tag]]}
            confirmation.update(method="public-at-spi", evidence={
                "before": {"requested_monotonic_ns": stamp, "completed_monotonic_ns": stamp + 1,
                           "observation": observation(stamp)},
                "after": {"requested_monotonic_ns": stamp + 4, "completed_monotonic_ns": stamp + 5,
                          "observation": observation(stamp + 4)},
                "same_original_session": True, "within_deadline": True})
        else:
            confirmation.update(method="calibrated-framebuffer", evidence={
                "predicate": "overview_windows_shown" if tag == "overview-windows" else "switcher_shown",
                "exact_png": True, "within_deadline": True})
        confirmations[tag] = confirmation
    return {"schema": 1, "frames": frames, "stills": stills,
            "unconfirmed_stills": [], "still_confirmations": confirmations}


@unittest.skipUnless(shutil.which("ffmpeg") and shutil.which("ffprobe"), "real media parsers required")
class MediaIntake(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = tempfile.TemporaryDirectory()
        cls.fixtures = Path(cls.fixture.name)
        cls.mapping = contract.inventory()
        for kind in ("gtk-walkthrough", "vm-tour"):
            artifact = cls.fixtures / kind
            artifact.mkdir()
            source = {"merged_commit": COMMIT, "built_commit": COMMIT, "built_tree": TREE,
                      "capture_commit": COMMIT, "capture_tree": TREE,
                      "run_id": "39123456789", "run_attempt": "2"}
            source["required_source_inputs_sha256"] = SOURCE_INPUTS
            if kind == "gtk-walkthrough":
                source.update(environment="nested-gtk", tracked_source_clean=True,
                              launched_elfs={name: {"pid": index + 100, "start_ticks": index + 1000,
                                                    "sha256": str(index + 6) * 64}
                                             for index, name in enumerate(("tuna-compositor", "tuna-shell-gtk"))})
            else:
                source.update(environment="installed-vm", tracked_source_clean=True,
                              original_session=native_fixture_owner())
                index = {"schemaVersion": 2, "manifests": [
                    {"digest": "sha256:" + "5" * 64, "platform": {"os": "linux", "architecture": "amd64"}}]}
                put_json(artifact / "native-base/index.json", index)
                # Synthetic modern verifier-output/process parser fixtures,
                # NOT actual signatures or cryptographic native-image qualification.
                cosign_path = artifact / "native-base/platform-cosign.json"
                put_json(cosign_path, [{
                    "critical": {"image": {"docker-manifest-digest": "sha256:" + "5" * 64},
                                 "identity": {"docker-reference": BASE},
                                 "type": "https://sigstore.dev/cosign/sign/v1"},
                    "optional": {},
                }])
                output = cosign_path.read_bytes()
                put_json(artifact / "native-base/platform-verification.json", {
                    "schema": 1, "argv": cosign_argv("/usr/bin/cosign", "sha256:" + "5" * 64),
                    "exit_code": 0, "begin_host_monotonic_ns": 100, "end_host_monotonic_ns": 200,
                    "tool": {"executable": "/usr/bin/cosign", "sha256": "e" * 64,
                             "version": "GitVersion: v3.1.4\n"},
                    "stdout_sha256": contract.digest(output), "stdout_bytes": len(output), "stderr": "",
                })
                source["base"] = {
                    "index": contract.BASE_REPOSITORY + "@sha256:" + contract.digest(
                        (artifact / "native-base/index.json").read_bytes()), "platform": BASE}
            put_json(artifact / "media-source.json", source)
            for filename, _ in cls.mapping[kind]:
                path = artifact / filename
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(native_fixture_png(Path(filename).stem[2:]) if kind == "vm-tour" else png(filename))
            if kind == "gtk-walkthrough":
                (artifact / "assertions.txt").write_text("G-PANEL pass original test fixture producer\nG-END pass completed\n")
                capture = {"schema": 1, "status": "ok", "exit_code": 0,
                           "source_sha256": contract.digest((artifact / "media-source.json").read_bytes()),
                           "outcomes_sha256": contract.digest((artifact / "assertions.txt").read_bytes()),
                           "files": {filename: {"sha256": contract.digest((artifact / filename).read_bytes()),
                                                 "bytes": (artifact / filename).stat().st_size}
                                     for filename, _ in cls.mapping[kind]}}
                put_json(artifact / "media-capture.json", capture)
            else:
                subprocess.run(["ffmpeg", "-nostdin", "-v", "error", "-f", "lavfi", "-i",
                                "testsrc2=size=32x32:rate=2", "-t", "1", "-c:v", "libvpx-vp9",
                                str(artifact / "tour/tuna-tour.webm")], check=True, capture_output=True)
                put_json(artifact / "strip-image.json", {"schema": 1, "commit": COMMIT, "image_id": IMAGE,
                         "built_tree": TREE, "capture_commit": COMMIT, "capture_tree": TREE,
                         "required_source_inputs_sha256": SOURCE_INPUTS,
                         "base_image": BASE, "package_sha256": PACKAGE,
                         "member_sha256": {"usr/bin/tuna-compositor": "6" * 64},
                         "installed_signed_release_qualified": False})
                put_json(artifact / "manifest.json", {"commit": COMMIT, "test_image_id": IMAGE,
                         "base_image": BASE, "assertions": {"V-SESSION": {"pass": True}},
                         "tour": {"video": "tour/tuna-tour.webm", "frames": 2,
                                  "capture_timing": "tour/capture-timing.json"}})
                put_json(artifact / "tour/capture-timing.json", native_fixture_timing(artifact, cls.mapping[kind]))
            result = command("tuna-docs-shots", "receipt", artifact, "--bundle", kind,
                             "--producer-exit-code", "0", *PINS, *(VM_PINS if kind == "vm-tour" else []))
            if result.returncode:
                raise AssertionError(result.stderr)
        artifact = cls.fixtures / "overview-demo"
        artifact.mkdir()
        shutil.copyfile(cls.fixtures / "gtk-walkthrough/media-source.json", artifact / "media-source.json")
        windows = [{"app_id": "tuna-test-" + name, "id": index, "rect": [0, 0, 20, 40]}
                   for index, name in enumerate(("alpha", "beta", "gamma"))]
        closed = {"overview_open": False, "locked": False, "windows": windows, "focused_app_id": "tuna-test-beta"}
        opened = dict(closed, overview_open=True, overview_progress=1, previews=windows)
        actions = {"schema": 1, "complete": True,
                   "source": json.loads((artifact / "media-source.json").read_text()),
                   "clock_domain": "host CLOCK_MONOTONIC", "window": 42, "events": [], "snapshots": {}}
        def snapshot(label, state):
            lower = time.monotonic_ns()
            actions["snapshots"][label] = {"state": state, "observed_host_monotonic_lower_ns": lower}
            upper = time.monotonic_ns()
            actions["snapshots"][label].update(observed_host_monotonic_upper_ns=upper,
                                                observed_host_monotonic=upper / 1e9)
        origin = time.monotonic_ns()
        recorder = FrameRecorder(recording_command(artifact / "overview-demo.raw.mp4", [
            "-use_wallclock_as_timestamps", "1", "-f", "lavfi", "-i",
            "color=c=blue:size=1280x800:rate=15:duration=12,realtime,settb=AVTB"]),
            artifact / "overview-demo.raw.mp4", artifact / "ffmpeg.log")
        try:
            recorder.wait_ready(origin + 1_000_000_000)
            snapshot("before", closed)
            for offset, action, label, state, argv in (
                    (1, "open-overview", "opened", opened, ["key", "Super_L"]),
                    (3, "select-beta", "selected-beta", closed, ["mousemove", "--window", "42", "10", "20", "click", "1"]),
                    (5, "reopen-overview", "reopened", opened, ["key", "Super_L"]),
                    (7, "dismiss-overview", "dismissed", closed, ["key", "Escape"])):
                time.sleep(max(0, (origin + offset * 1_000_000_000 - time.monotonic_ns()) / 1e9))
                lower, upper = time.monotonic_ns(), time.monotonic_ns()
                actions["events"].append({"action": action, "argv": argv, "begin": lower / 1e9,
                                          "end": upper / 1e9, "begin_host_monotonic_ns": lower,
                                          "end_host_monotonic_ns": upper})
                snapshot(label, state)
            recorder.finish()
            bind_recorded_interval(actions, recorder.receipt(origin))
        finally:
            recorder.stop()
        subprocess.run(["ffmpeg", "-nostdin", "-v", "error", "-i", str(artifact / "overview-demo.raw.mp4"),
                        "-c", "copy", "-movflags", "+faststart", str(artifact / "overview-demo.mp4")],
                       check=True, capture_output=True)
        subprocess.run(["ffmpeg", "-nostdin", "-v", "error", "-i", str(artifact / "overview-demo.mp4"),
                        "-filter_complex", "fps=8,scale=640:400:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse",
                        "-loop", "0", str(artifact / "overview-demo.gif")], check=True, capture_output=True)
        actions.update(video={"file": "overview-demo.mp4",
                             "sha256": contract.digest((artifact / "overview-demo.mp4").read_bytes()),
                             "raw_sha256": contract.digest((artifact / "overview-demo.raw.mp4").read_bytes())},
                       derivative={"file": "overview-demo.gif",
                                   "sha256": contract.digest((artifact / "overview-demo.gif").read_bytes()),
                                   "input_sha256": contract.digest((artifact / "overview-demo.mp4").read_bytes()),
                                   "fps": 8, "size": [640, 400]})
        put_json(artifact / "overview-demo-actions.json", actions)
        (artifact / "assertions.txt").write_text("".join(
            "C-README-" + name + " pass actual parser test fixture\n"
            for name in ("OPEN", "SELECT-BETA", "REOPEN", "DISMISS", "VIDEO", "GIF")))
        result = command("tuna-docs-shots", "receipt", artifact, "--bundle", "overview-demo",
                         "--producer-exit-code", "0", *PINS)
        if result.returncode:
            raise AssertionError(result.stderr)

    @classmethod
    def tearDownClass(cls):
        cls.fixture.cleanup()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.dest = self.root / "destination"
        self.gtk = self.root / "gtk"
        self.vm = self.root / "vm"
        shutil.copytree(self.fixtures / "gtk-walkthrough", self.gtk)
        shutil.copytree(self.fixtures / "vm-tour", self.vm)
        self.overview = self.root / "overview"
        shutil.copytree(self.fixtures / "overview-demo", self.overview)

    def intake(self, *artifacts, extra=()):
        return command("tuna-docs-shots", "intake", *artifacts, *PINS, *VM_PINS,
                       "--dest", self.dest, *extra)

    def mutate_receipt(self, artifact, callback):
        path = artifact / "media-receipt.json"
        receipt = json.loads(path.read_text())
        callback(receipt)
        put_json(path, receipt)

    def test_combined_same_run_media_and_hash_freshness(self):
        historical = self.dest / "docs/marlin/old2476/receipt.json"
        historical.parent.mkdir(parents=True)
        historical.write_bytes(b"original immutable diagnostic")
        result = self.intake(self.gtk, self.vm, self.overview)
        self.assertEqual(result.returncode, 0, result.stderr)
        for kind, artifact in (("gtk-walkthrough", self.gtk), ("vm-tour", self.vm), ("overview-demo", self.overview)):
            for source, target in self.mapping[kind]:
                self.assertEqual((self.dest / target).read_bytes(), (artifact / source).read_bytes())
        document = json.loads((self.dest / contract.manifest_name("vm-tour")).read_bytes())
        self.assertEqual(document["receipt"]["base_verification"]["process"]["file"],
                         "native-base/platform-verification.json")
        self.assertEqual(base64.b64decode(document["evidence"]["base_process"]),
                         (self.vm / "native-base/platform-verification.json").read_bytes())
        self.assertEqual(historical.read_bytes(), b"original immutable diagnostic")
        result = command("tuna-walkthrough", "freshness", "--bundle", "gtk-walkthrough",
                         "--bundle", "vm-tour", "--bundle", "overview-demo", *PINS, *VM_PINS, "--dest", self.dest,
                         "--source-artifact", self.gtk, "--source-artifact", self.vm,
                         "--source-artifact", self.overview)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("CURRENT gtk-walkthrough", result.stdout)
        self.assertIn("environment nested-gtk", result.stdout)
        self.assertFalse((self.dest / "docs/walkthrough/tuna-tour.webm").exists())

    def test_rehashed_canonical_pixels_and_embedded_evidence_cannot_forge_uploaded_source(self):
        self.assertEqual(self.intake(self.gtk).returncode, 0)
        path = self.dest / contract.manifest_name("gtk-walkthrough")
        document = json.loads(path.read_bytes())
        entry = document["receipt"]["files"][0]
        forged = png("valid but never uploaded by this producer")
        (self.dest / entry["destination"]).write_bytes(forged)
        entry.update(sha256=contract.digest(forged), bytes=len(forged))
        capture = json.loads(base64.b64decode(document["evidence"]["capture_receipt"]))
        capture["files"][entry["source"]] = {"sha256": entry["sha256"], "bytes": entry["bytes"]}
        blob = json.dumps(capture).encode()
        document["evidence"]["capture_receipt"] = base64.b64encode(blob).decode()
        document["receipt"]["capture_receipt"]["sha256"] = contract.digest(blob)
        put_json(path, document)
        result = command("tuna-docs-shots", "freshness", "--bundle", "gtk-walkthrough", *PINS,
                         "--dest", self.dest, "--source-artifact", self.gtk)
        self.assertEqual(result.returncode, 1)
        self.assertIn("authenticated source artifact differs", result.stderr)
        self.assertNotIn("CURRENT", result.stdout)

    def test_bad_bundle_prevents_all_writes(self):
        (self.vm / "tour/T-lock.png").write_bytes(b"not an image")
        result = self.intake(self.gtk, self.vm)
        self.assertEqual(result.returncode, 1)
        self.assertFalse(self.dest.exists())

    def test_rehashed_fake_media_fails_real_parser(self):
        filename = self.mapping["vm-tour"][0][0]
        blob = b"PNG timestamp-only stub"
        (self.vm / filename).write_bytes(blob)
        def edit(receipt):
            receipt["files"][0].update(sha256=contract.digest(blob), bytes=len(blob))
        self.mutate_receipt(self.vm, edit)
        self.assertEqual(self.intake(self.vm).returncode, 1)
        self.assertFalse(self.dest.exists())

    def test_video_bytes_cannot_be_replaced_with_metadata(self):
        (self.vm / "tour/tuna-tour.webm").write_bytes(b"old2476")
        self.assertEqual(self.intake(self.vm).returncode, 1)
        self.assertFalse(self.dest.exists())

    def test_historical_destination_is_rejected(self):
        self.mutate_receipt(self.vm, lambda r: r["files"][0].update(destination="docs/marlin/old2476/T-lock.png"))
        self.assertEqual(self.intake(self.vm).returncode, 1)
        self.assertFalse(self.dest.exists())

    def test_symlink_cannot_alias_historical_receipt(self):
        original = self.root / "historical.png"
        original.write_bytes(png("historical"))
        target = self.dest / self.mapping["vm-tour"][0][1]
        target.parent.mkdir(parents=True)
        target.symlink_to(original)
        self.assertEqual(self.intake(self.vm).returncode, 1)
        self.assertEqual(original.read_bytes(), png("historical"))

    def test_exact_source_run_attempt_and_image_pins(self):
        mutations = (
            lambda r: r["run"].update(attempt="1"),
            lambda r: r["run"].update(id="2476"),
            lambda r: r["source"].update(built_tree="7" * 40),
            lambda r: r["source"].update(merged_commit="8" * 40),
            lambda r: r["image"].update(id="sha256:" + "9" * 64),
            lambda r: r["image"].update(package_sha256="a" * 64),
            lambda r: r["producer"].update(exit_code=1),
            lambda r: r["producer"].update(exit_code=False),
        )
        original = (self.vm / "media-receipt.json").read_bytes()
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                (self.vm / "media-receipt.json").write_bytes(original)
                self.mutate_receipt(self.vm, mutate)
                self.assertEqual(self.intake(self.vm).returncode, 1)
                self.assertFalse(self.dest.exists())

    def test_changed_evidence_cannot_retain_valid_hash(self):
        path = self.vm / "manifest.json"
        value = json.loads(path.read_text())
        value["assertions"]["V-SESSION"]["pass"] = False
        put_json(path, value)
        self.assertEqual(self.intake(self.vm).returncode, 1)

    def test_rehashed_failed_real_vm_outcome_still_refuses(self):
        path = self.vm / "manifest.json"
        value = json.loads(path.read_text())
        value["assertions"]["V-SESSION"]["pass"] = False
        put_json(path, value)
        self.mutate_receipt(self.vm, lambda r: r["outcomes"].update(sha256=contract.digest(path.read_bytes())))
        self.assertEqual(self.intake(self.vm).returncode, 1)

    def test_unconfirmed_still_or_missing_timing_is_not_success(self):
        path = self.vm / "tour/capture-timing.json"
        value = json.loads(path.read_text())
        value["unconfirmed_stills"] = ["lock"]
        put_json(path, value)
        self.mutate_receipt(self.vm, lambda r: r["capture_timing"].update(sha256=contract.digest(path.read_bytes())))
        self.assertEqual(self.intake(self.vm).returncode, 1)

    def test_named_stills_without_positive_public_capture_proof_are_rejected(self):
        path = self.vm / "tour/capture-timing.json"
        original = path.read_bytes()
        for change in (
                lambda r: r.pop("still_confirmations"),
                lambda r: r["still_confirmations"]["lock"]["evidence"]["before"]["observation"].update(controls=[]),
                lambda r: r["still_confirmations"]["calendar"]["evidence"]["after"]["observation"]["principal"].update(pid=999),
                lambda r: r["still_confirmations"]["app-grid"].update(capture_completed_monotonic_ns=999999)):
            with self.subTest(change=change):
                value = json.loads(original)
                change(value)
                put_json(path, value)
                self.mutate_receipt(self.vm, lambda r: r["capture_timing"].update(sha256=contract.digest(path.read_bytes())))
                result = self.intake(self.vm)
                self.assertEqual(result.returncode, 1)
                self.assertFalse(self.dest.exists())

    def test_rehashed_confirmed_tag_cannot_replace_exact_public_framebuffer_view(self):
        path = self.vm / "tour/T-alt-tab.png"
        wrong = native_fixture_png("not-a-switcher")
        path.write_bytes(wrong)
        checksum = contract.digest(wrong)
        timing_path = self.vm / "tour/capture-timing.json"
        timing = json.loads(timing_path.read_bytes())
        confirmation = timing["still_confirmations"]["alt-tab"]
        confirmation["png_sha256"] = checksum
        timing["frames"][confirmation["frame_index"]]["sha256"] = checksum
        put_json(timing_path, timing)
        def forge(receipt):
            receipt["capture_timing"]["sha256"] = contract.digest(timing_path.read_bytes())
            entry = next(row for row in receipt["files"] if row["source"] == "tour/T-alt-tab.png")
            entry.update(sha256=checksum, bytes=len(wrong), probe=contract.media(path))
        self.mutate_receipt(self.vm, forge)
        result = self.intake(self.vm)
        self.assertEqual(result.returncode, 1)
        self.assertIn("wrong public view", result.stderr)
        self.assertFalse(self.dest.exists())

    def test_nested_frames_cannot_claim_vm_or_native_environment(self):
        self.mutate_receipt(self.gtk, lambda r: r["producer"].update(environment="installed-vm"))
        self.assertEqual(self.intake(self.gtk).returncode, 1)

    def test_complete_current_producer_seal_is_required(self):
        (self.gtk / "media-capture.json").unlink()
        self.assertEqual(self.intake(self.gtk).returncode, 1)
        result = command("tuna-walkthrough", "receipt", self.gtk, "--bundle", "gtk-walkthrough",
                         "--producer-exit-code", "0", *PINS)
        self.assertEqual(result.returncode, 1)

    def test_source_copy_is_not_artifact_promotion(self):
        inside = self.dest / "docs/old-artifact"
        shutil.copytree(self.gtk, inside)
        self.assertEqual(self.intake(inside).returncode, 1)
        self.assertFalse((self.dest / "docs/walkthrough/panel.png").exists())

    def test_freshness_requires_local_actual_bytes_not_timestamp(self):
        self.assertEqual(self.intake(self.vm).returncode, 0)
        target = self.dest / self.mapping["vm-tour"][0][1]
        target.write_bytes(png("different real pixels"))
        os.utime(target, None)
        result = command("tuna-docs-shots", "freshness", "--bundle", "vm-tour", *PINS,
                         *VM_PINS, "--dest", self.dest)
        self.assertEqual(result.returncode, 1)
        self.assertNotIn("CURRENT", result.stdout)

    def test_missing_video_never_emits_new_receipt(self):
        (self.vm / "media-receipt.json").unlink()
        (self.vm / "tour/tuna-tour.webm").unlink()
        result = command("tuna-docs-shots", "receipt", self.vm, "--bundle", "vm-tour",
                         "--producer-exit-code", "0", *PINS, *VM_PINS)
        self.assertEqual(result.returncode, 1)
        self.assertFalse((self.vm / "media-receipt.json").exists())

    def test_legacy_copies_cannot_overwrite_canonical_or_historical_docs(self):
        forbidden = ROOT / "docs/marlin/unit-test-must-never-exist"
        for script, arguments in (
            ("tuna-walkthrough", ("build", self.gtk)),
            ("tuna-docs-shots", ("collect", self.gtk, self.gtk, self.gtk)),
        ):
            result = command(script, *arguments, "--dest", forbidden)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("cannot overwrite", result.stderr)
        self.assertFalse(forbidden.exists())

    def test_legacy_docs_shots_preflights_complete_temp_byte_set(self):
        journey, app, scroll = (self.root / name for name in ("journey", "app", "scroll"))
        for directory, names in (
            (journey, ("01-baseline.png", "02-overview.png", "03-search.png")),
            (app, ("02-app.png", "03-tiled.png")),
            (scroll, ("02-strip.png",)),
        ):
            directory.mkdir()
            for filename in names:
                (directory / filename).write_bytes(png(filename))
        result = command("tuna-docs-shots", "collect", journey, app, scroll, "--dest", self.dest)
        self.assertEqual(result.returncode, 1)
        self.assertFalse(self.dest.exists())
        (scroll / "04-wheel.png").write_bytes(png("wheel"))
        result = command("tuna-docs-shots", "collect", journey, app, scroll, "--dest", self.dest)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(list(self.dest.iterdir())), 7)
        self.assertEqual((self.dest / "scroll-wheel.png").read_bytes(), png("wheel"))

    def test_inventory_exactly_covers_walkthrough_manifest(self):
        expected = set()
        for line in (ROOT / "docs/walkthrough.tsv").read_text().splitlines():
            if line and not line.startswith("#"):
                ident, filename, *_ = line.split("\t")
                expected.add((filename, "docs/walkthrough/" + ident + ".png"))
        self.assertEqual(set(self.mapping["gtk-walkthrough"]), expected)
        self.assertEqual(len(self.mapping["vm-tour"]), 6)
        self.assertNotIn("docs-shots", self.mapping)

    def test_actual_process_elf_binding_uses_proc_not_a_claim(self):
        process = subprocess.Popen(["sleep", "30"])
        self.addCleanup(process.wait)
        self.addCleanup(process.terminate)
        expected = Path(shutil.which("sleep"))
        value = gtk_media_source.process(process.pid, expected)
        self.assertEqual(value["sha256"], contract.digest(expected.read_bytes()))
        self.assertGreater(value["start_ticks"], 0)
        with self.assertRaises(ValueError):
            gtk_media_source.process(process.pid, Path(sys.executable))

    def test_overview_real_video_and_gif_lineage(self):
        result = self.intake(self.overview)
        self.assertEqual(result.returncode, 0, result.stderr)
        result = command("tuna-docs-shots", "freshness", "--bundle", "overview-demo", *PINS,
                         "--dest", self.dest, "--source-artifact", self.overview)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.dest / "docs/walkthrough/overview-demo.raw.mp4").exists())

    def test_overview_actual_state_and_derivative_cannot_be_relabelled(self):
        path = self.overview / "overview-demo-actions.json"
        original = path.read_bytes()
        mutations = (
            lambda r: r["snapshots"]["selected-beta"]["state"].update(focused_app_id="tuna-test-alpha"),
            lambda r: r["derivative"].update(input_sha256="f" * 64),
            lambda r: r["events"][1].update(argv=["key", "Escape"]),
            lambda r: r.update(complete=False),
            lambda r: r.pop("recording"),
            lambda r: r["recording"].pop("ready_ack"),
            lambda r: r["recording"]["ready_ack"].update(host_monotonic_ns=0),
            lambda r: r["events"][0].update(begin=99.),
        )
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                value = json.loads(original)
                mutate(value)
                put_json(path, value)
                self.mutate_receipt(self.overview, lambda r: r["action_receipt"].update(
                    sha256=contract.digest(path.read_bytes())))
                self.assertEqual(self.intake(self.overview).returncode, 1)
                self.assertFalse(self.dest.exists())

    def test_required_inputs_pin_cannot_be_relabelled_as_latest_source(self):
        result = self.intake(self.vm, extra=("--expect-source-inputs", "c" * 64))
        self.assertEqual(result.returncode, 1)
        self.assertFalse(self.dest.exists())
        self.mutate_receipt(self.vm, lambda r: r["source"].update(required_source_inputs_sha256="c" * 64))
        result = self.intake(self.vm, extra=("--expect-source-inputs", "c" * 64))
        self.assertEqual(result.returncode, 1)
        self.assertFalse(self.dest.exists())

    def test_rehashed_wrong_native_certificate_constraint_argv_is_not_verified_evidence(self):
        path = self.vm / "native-base/platform-verification.json"
        original = path.read_bytes()
        for flag, wrong in (("--certificate-identity", "https://github.com/untrusted/workflow"),
                            ("--certificate-oidc-issuer", "https://untrusted.example")):
            with self.subTest(flag=flag):
                process = json.loads(original)
                process["argv"][process["argv"].index(flag) + 1] = wrong
                put_json(path, process)
                self.mutate_receipt(self.vm, lambda r: r["base_verification"]["process"].update(
                    sha256=contract.digest(path.read_bytes())))
                self.assertEqual(self.intake(self.vm).returncode, 1)
                self.assertFalse(self.dest.exists())

    def test_native_process_witness_is_required_in_artifact_and_receipt(self):
        path = self.vm / "native-base/platform-verification.json"
        self.mutate_receipt(self.vm, lambda r: r["base_verification"].pop("process"))
        self.assertEqual(self.intake(self.vm).returncode, 1)
        path.unlink()
        self.assertEqual(self.intake(self.vm).returncode, 1)
        self.assertFalse(self.dest.exists())

    def test_rehashed_failed_or_mismatched_native_process_is_rejected(self):
        path = self.vm / "native-base/platform-verification.json"
        original = path.read_bytes()
        mutations = (
            lambda p: p.update(exit_code=1),
            lambda p: p.update(exit_code=False),
            lambda p: p.update(stdout_sha256="f" * 64),
            lambda p: p.update(stdout_bytes=p["stdout_bytes"] + 1),
            lambda p: p.update(end_host_monotonic_ns=p["begin_host_monotonic_ns"]),
            lambda p: p["tool"].update(version="GitVersion: v3.1.3\n"),
            lambda p: p["tool"].update(sha256="not-a-tool-hash"),
            lambda p: p["argv"].__setitem__(2, contract.BASE_REPOSITORY + ":gnome"),
        )
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                process = json.loads(original)
                mutate(process)
                put_json(path, process)
                self.mutate_receipt(self.vm, lambda r: r["base_verification"]["process"].update(
                    sha256=contract.digest(path.read_bytes())))
                self.assertEqual(self.intake(self.vm).returncode, 1)
                self.assertFalse(self.dest.exists())

    def test_rehashed_native_stdout_must_match_actual_process_witness(self):
        path = self.vm / "native-base/platform-cosign.json"
        path.write_bytes(path.read_bytes() + b" ")
        self.mutate_receipt(self.vm, lambda r: r["base_verification"]["platform"].update(
            sha256=contract.digest(path.read_bytes())))
        self.assertEqual(self.intake(self.vm).returncode, 1)
        self.assertFalse(self.dest.exists())

    def test_modern_native_envelope_must_name_the_resolved_platform(self):
        path = self.vm / "native-base/platform-cosign.json"
        process_path = self.vm / "native-base/platform-verification.json"
        original = path.read_bytes()
        mutations = (
            lambda s: s["critical"]["image"].update(**{"docker-manifest-digest": "sha256:" + "6" * 64}),
            lambda s: s["critical"]["identity"].update(
                **{"docker-reference": contract.BASE_REPOSITORY + "@sha256:" + "6" * 64}),
            lambda s: s["critical"].update(type="https://untrusted.example/signature"),
            lambda s: s.update(optional=None),
        )
        for mutate in mutations:
            with self.subTest(mutation=mutate):
                signatures = json.loads(original)
                mutate(signatures[0])
                put_json(path, signatures)
                output = path.read_bytes()
                process = json.loads(process_path.read_bytes())
                process.update(stdout_sha256=contract.digest(output), stdout_bytes=len(output))
                put_json(process_path, process)
                self.mutate_receipt(self.vm, lambda r: r["base_verification"]["platform"].update(
                    sha256=contract.digest(output)))
                self.mutate_receipt(self.vm, lambda r: r["base_verification"]["process"].update(
                    sha256=contract.digest(process_path.read_bytes())))
                self.assertEqual(self.intake(self.vm).returncode, 1)
                self.assertFalse(self.dest.exists())

    def test_actual_raw_native_index_and_independent_platform_pin_are_required(self):
        result = self.intake(self.vm, extra=("--expect-base-platform", contract.BASE_REPOSITORY + "@sha256:" + "a" * 64))
        self.assertEqual(result.returncode, 1)
        (self.vm / "native-base/index.json").write_bytes(b"unretained prior index")
        self.assertEqual(self.intake(self.vm).returncode, 1)
        self.assertFalse(self.dest.exists())


class RequiredSourceInputs(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.git("init", "-q")
        inventory = self.repo / "scripts/lib/maintained-media.tsv"
        inventory.parent.mkdir(parents=True)
        shutil.copyfile(contract.INVENTORY, inventory)
        (self.repo / "source.rs").write_text("fn main() {}\n")
        self.media = self.repo / "docs/walkthrough/panel.png"
        self.media.parent.mkdir(parents=True)
        self.media.write_bytes(png("initial"))
        self.commit()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), *args], text=True).strip()

    def commit(self):
        self.git("add", ".")
        self.git("-c", "user.name=MediaTest", "-c", "user.email=media@example.test",
                 "commit", "-qm", "temporary source identity fixture")

    def test_only_exact_registered_generated_outputs_do_not_self_invalidate(self):
        initial_head = self.git("rev-parse", "HEAD")
        initial_tree = self.git("rev-parse", "HEAD^{tree}")
        initial = contract.required_source_inputs(self.repo)
        self.media.write_bytes(png("fresh real media bytes"))
        for kind in contract.inventory():
            put_json(self.repo / contract.manifest_name(kind), {"fixture": "new generated output"})
        self.commit()
        self.assertNotEqual(initial_head, self.git("rev-parse", "HEAD"))
        self.assertNotEqual(initial_tree, self.git("rev-parse", "HEAD^{tree}"))
        self.assertEqual(initial, contract.required_source_inputs(self.repo))

    def test_every_other_tracked_source_including_docs_map_and_mode_is_required(self):
        previous = contract.required_source_inputs(self.repo)
        for filename, blob in (
            ("source.rs", b"fn main() { println!(\"changed\"); }\n"),
            ("docs/input.txt", b"not a generated media output\n"),
            ("docs/walkthrough/unregistered.png", png("unregistered input")),
            ("scripts/lib/maintained-media.tsv", contract.INVENTORY.read_bytes() + b"# changed source map\n"),
        ):
            path = self.repo / filename
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(blob)
            self.commit()
            current = contract.required_source_inputs(self.repo)
            self.assertNotEqual(previous, current, filename)
            previous = current
        os.chmod(self.repo / "source.rs", 0o755)
        self.commit()
        self.assertNotEqual(previous, contract.required_source_inputs(self.repo))

    def test_dirty_tracked_source_is_not_a_clean_head_identity(self):
        (self.repo / "source.rs").write_text("dirty input\n")
        with self.assertRaises(subprocess.CalledProcessError):
            contract.required_source_inputs(self.repo)

    def test_independent_cli_hashes_the_actual_requested_clean_head(self):
        result = command("tuna-docs-shots", "source-inputs", "--source-root", self.repo)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), contract.required_source_inputs(self.repo))

    def product_pair(self):
        """Real Git lineage with controlled image bytes, not native GUI proof."""
        from vm_strip_image import admission
        for name in ("tuna-vm-input", "tuna-vm-lifecycle"):
            path = self.repo / "packaging/marlin/vm-lane" / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(("controlled " + name).encode())
        self.commit()
        product = contract.git_source(self.repo, self.git("rev-parse", "HEAD"))
        package = {"package": {"sha256": PACKAGE, "size": 4096, "device": 1, "inode": 2},
                   "required_members": {"usr/bin/tuna-compositor": {"sha256": "6" * 64}},
                   "pkginfo": {"pkgname": "tuna"}, "member_count": 1, "decompressed_bytes": 8192}
        members = {"/usr/bin/tuna-compositor": "6" * 64}
        for name in ("tuna-vm-input", "tuna-vm-lifecycle"):
            members["/usr/libexec/" + name] = contract.digest(
                (self.repo / "packaging/marlin/vm-lane" / name).read_bytes())
        observed = {"schema": 1, "image_id": IMAGE, "member_sha256": members}
        base = "ghcr.io/tuna-os/marlin:gnome@sha256:" + "5" * 64
        original = admission(package, observed, self.repo, product["commit"], IMAGE, base)
        raw = json.dumps(original).encode()
        self.media.write_bytes(png("new capture input excluded from product"))
        self.commit()
        capture = contract.git_source(self.repo, self.git("rev-parse", "HEAD"))
        lineage = {"repository": "tuna-os/tuna-desktop", "workflow_id": 123,
                   "workflow_path": ".github/workflows/ci.yml", "run_id": 1234, "run_attempt": 1,
                   "head_sha": product["commit"],
                   "pointer_artifact": {"id": 1, "name": "native-product-distribution",
                                        "digest": "sha256:" + "a" * 64},
                   "data_artifact": {"id": 2, "name": "native-product-DATA-distribution",
                                     "digest": "sha256:" + "b" * 64},
                   "provenance": {"sha256": "c" * 64, "bytes": 1024}}
        origin = {"schema": "tuna-native-product-selection-v1", "mode": "reused", "profile": "distribution",
                  "current_capture_source": capture, "product_source": product, "image_id": IMAGE,
                  "package": {"file": "/controlled/tuna.pkg.tar.zst", "basename": "tuna-1-1-x86_64.pkg.tar.zst",
                              "sha256": PACKAGE},
                  "base": {"platform": "sha256:" + "5" * 64, "index_digest": "sha256:" + "d" * 64},
                  "authenticated_origin": lineage,
                  "strip_image": {"sha256": contract.digest(raw), "bytes": len(raw)},
                  "original_image_receipt": {"sha256": contract.digest(raw), "bytes": len(raw),
                                             "receipt": json.loads(raw),
                                             "raw_base64": base64.b64encode(raw).decode()}}
        source = {"merged_commit": capture["commit"], "capture_commit": capture["commit"],
                  "capture_tree": capture["tree"], "built_commit": product["commit"],
                  "built_tree": product["tree"], "required_source_inputs_sha256":
                  product["required_source_inputs_sha256"]}
        pins = {key: value for key, value in source.items() if key not in ("built_commit", "built_tree")}
        return package, observed, base, origin, source, pins

    def test_old_actual_product_current_capture_uses_equal_real_git_inputs(self):
        from vm_strip_image import admission
        package, observed, base, origin, source, pins = self.product_pair()
        self.assertNotEqual(source["built_commit"], source["capture_commit"])
        self.assertNotEqual(source["built_tree"], source["capture_tree"])
        contract.validate_source(source, pins, "vm-tour", self.repo)
        package["package"].update(device=9, inode=10)
        receipt = admission(package, observed, self.repo, source["built_commit"], IMAGE, base,
                            capture_commit=source["capture_commit"], product_origin=origin)
        self.assertEqual(receipt["commit"], source["built_commit"])
        self.assertEqual(receipt["capture_commit"], source["capture_commit"])
        self.assertEqual(receipt["product_origin"], origin)
        self.assertEqual(contract.required_source_inputs(self.repo, source["built_commit"]),
                         contract.required_source_inputs(self.repo, source["capture_commit"]))
        with self.assertRaises(ValueError):
            contract.validate_source(source, pins, "gtk-walkthrough", self.repo)
        with self.assertRaises(ValueError):
            contract.validate_source(source, pins, "overview-demo", self.repo)

    def test_changed_product_input_cannot_reuse_old_image(self):
        from vm_strip_image import admission
        package, observed, base, origin, source, pins = self.product_pair()
        (self.repo / "source.rs").write_text("fn main() { panic!(); }\n")
        self.commit()
        capture = contract.git_source(self.repo, self.git("rev-parse", "HEAD"))
        origin["current_capture_source"] = capture
        with self.assertRaisesRegex(ValueError, "inputs differ"):
            admission(package, observed, self.repo, source["built_commit"], IMAGE, base,
                      capture_commit=capture["commit"], product_origin=origin)

    def test_forged_build_capture_trees_and_missing_capture_fail_closed(self):
        _, _, _, _, source, pins = self.product_pair()
        for key in ("built_tree", "capture_tree", "required_source_inputs_sha256"):
            forged = dict(source)
            forged[key] = "0" * len(forged[key])
            with self.subTest(field=key), self.assertRaises(ValueError):
                contract.validate_source(forged, pins, "vm-tour", self.repo)
        for key in ("capture_commit", "capture_tree"):
            forged = dict(source)
            del forged[key]
            with self.subTest(missing=key), self.assertRaises(ValueError):
                contract.validate_source(forged, pins, "vm-tour", self.repo)
        forged = dict(source, built_commit="f" * 40)
        with self.assertRaises((ValueError, subprocess.CalledProcessError)):
            contract.validate_source(forged, pins, "vm-tour", self.repo)

    def test_forged_origin_loaded_image_package_and_original_receipt_rejected(self):
        from vm_strip_image import admission
        package, observed, base, origin, source, _ = self.product_pair()
        mutations = (
            lambda o: o.update(image_id="sha256:" + "0" * 64),
            lambda o: o["package"].update(sha256="0" * 64),
            lambda o: o["product_source"].update(tree="0" * 40),
            lambda o: o["authenticated_origin"].update(repository="attacker/repo"),
            lambda o: o["authenticated_origin"]["data_artifact"].update(name="other"),
            lambda o: o["base"].update(platform="sha256:" + "0" * 64),
            lambda o: o["original_image_receipt"].update(raw_base64=base64.b64encode(b"{}").decode()),
            lambda o: o["original_image_receipt"]["receipt"].update(commit=source["capture_commit"]),
            lambda o: o.update(mode="fresh"),
        )
        for mutate in mutations:
            forged = json.loads(json.dumps(origin))
            mutate(forged)
            with self.subTest(mutation=mutate), self.assertRaises(ValueError):
                admission(package, observed, self.repo, source["built_commit"], IMAGE, base,
                          capture_commit=source["capture_commit"], product_origin=forged)
        with self.assertRaisesRegex(ValueError, "explicit authenticated"):
            admission(package, observed, self.repo, source["built_commit"], IMAGE, base,
                      capture_commit=source["capture_commit"])
        forged_observed = dict(observed, image_id="sha256:" + "0" * 64)
        with self.assertRaises(ValueError):
            admission(package, forged_observed, self.repo, source["built_commit"], IMAGE, base,
                      capture_commit=source["capture_commit"], product_origin=origin)
        package["package"]["sha256"] = "0" * 64
        with self.assertRaises(ValueError):
            admission(package, observed, self.repo, source["built_commit"], IMAGE, base,
                      capture_commit=source["capture_commit"], product_origin=origin)

    def test_original_ref_is_strict_not_an_alias_or_revision_expression(self):
        for ref in ("main", "HEAD~0", "--all", "a" * 39):
            with self.subTest(ref=ref), self.assertRaises(ValueError):
                contract.required_source_inputs(self.repo, ref)


if __name__ == "__main__":
    unittest.main()
