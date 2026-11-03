#!/usr/bin/env python3
"""Fail-closed current-run media receipts, promotion and freshness.

The checked-in maintained-media.tsv is the ONLY destination authority. Receipt
schema tuna-maintained-media-v1 records exact run/attempt, current capture commit
and tree, actual product build commit/tree, real producer status and evidence
of the underlying media bytes. VM receipts additionally bind strip-image.json,
actual manifest image metadata and capture-timing.json. Nested GTK/proof frames
are never installed-native or signed-release qualification. File ages and
broad documentation/path drift exemptions never qualify media.

receipt ARTIFACT --bundle KIND --producer-exit-code 0 <identity flags>
intake ARTIFACT... <identity flags> [--dest REPOSITORY_OR_STAGING_ROOT]
freshness --bundle KIND [--bundle KIND...] <identity flags> [--dest ROOT]
source-inputs [--source-root ROOT] [--ref EXACT_COMMIT]

Required identity flags: --run-id ID --attempt N --expect-merge COMMIT
--expect-head COMMIT --expect-tree TREE --expect-source-inputs DIGEST.
The commit/tree pins name the actual CURRENT CAPTURE checkout, never a fabricated
latest build SHA. built_commit/tree independently retain the actual product
source: only authenticated native images with strictly equal actual Git input
hashes may have an older build. Nested GTK and overview builds equal capture.
Only exact inventory media destinations and three generated manifests are
excluded; all other tracked Git mode/type/object/path records participate.
VM intake/freshness also require --expect-image sha256:DIGEST and
--expect-package DIGEST; optional --expect-base-index and --expect-base-platform
pin full immutable base references. receipt obtains image/package from the
actual strip-image.json and validates retained raw OCI index, modern platform
cosign output and exact-policy successful process witness (not a new crypto verify).
media-source.json comes from the actual build/launch boundary and contains
merged_commit, capture_commit, capture_tree, built_commit, built_tree,
required_source_inputs_sha256, run_id, run_attempt. Producers must
write receipts only after the actual command succeeds; a receipt is evidence,
not authorization to publish, bypass CI gates, or qualify a signed release.
"""
import argparse
import base64
import hashlib
import json
import math
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tempfile
from vm_gnome_reference import BASE_REPOSITORY, validate_platform_verification
from tour_media import validate_still_confirmations
from overview_recording import validate_recording

ROOT = Path(__file__).resolve().parents[2]
INVENTORY = Path(__file__).with_name("maintained-media.tsv")
SCHEMA = "tuna-maintained-media-v1"
PRODUCERS = {
    "vm-tour": ("tuna-vm-lane", "installed-vm"),
    "gtk-walkthrough": ("tuna-gtk-shell-proof", "nested-gtk"),
    "overview-demo": ("tuna-capture", "nested-gtk"),
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(blob):
    return hashlib.sha256(blob).hexdigest()


def sha(value, length=64):
    return isinstance(value, str) and re.fullmatch("[0-9a-f]{%d}" % length, value) is not None


def relative(value):
    require(isinstance(value, str) and value and "\\" not in value,
            "invalid relative path")
    path = PurePosixPath(value)
    require(not path.is_absolute() and str(path) == value and
            all(part not in (".", "..") for part in path.parts),
            "unsafe relative path: " + value)
    return path


def confined(root, name):
    path = root / relative(name)
    # Reject even in-root symlinks: otherwise a canonical filename could alias
    # an immutable historical diagnostic receipt or another maintained bundle.
    require(not root.is_symlink(), "symlink artifact/destination root")
    for candidate in (path, *path.parents):
        require(not candidate.is_symlink(), "symlink path: " + str(candidate))
    require(path.resolve().is_relative_to(root.resolve()), "path escapes root")
    return path


def inventory_rows(text):
    result = {}
    destinations = set()
    for number, line in enumerate(text.splitlines(), 1):
        if not line.strip() or line.startswith("#"):
            continue
        fields = line.split("\t")
        require(len(fields) == 3, "invalid inventory row %d" % number)
        kind, source, destination = fields
        require(kind in PRODUCERS, "unknown inventory producer " + kind)
        relative(source)
        relative(destination)
        require(destination.startswith("docs/") and
                Path(destination).suffix in (".png", ".webp", ".gif", ".mp4", ".webm"),
                "inventory destination is not canonical documentation media")
        require(destination not in destinations, "duplicate inventory destination " + destination)
        destinations.add(destination)
        result.setdefault(kind, []).append((source, destination))
    return result


def inventory():
    return inventory_rows(INVENTORY.read_text())


def required_source_inputs(root, ref="HEAD"):
    """Hash actual Git inputs at HEAD or a strict original commit.

    Git mode/type/object/path records are hashed verbatim, NUL-delimited, with
    a domain separator. Read the exemption inventory from that very commit, not
    from an uncommitted map or a manifest supplied by an artifact.
    """
    root = str(Path(root).resolve())
    require(ref == "HEAD" or sha(ref, 40), "source ref must be HEAD or an exact commit")
    if ref == "HEAD":
        subprocess.run(["git", "-C", root, "diff", "--quiet", "HEAD", "--"], check=True)
    commit = subprocess.check_output(
        ["git", "-C", root, "rev-parse", "--verify", ref + "^{commit}"], text=True).strip()
    require(sha(commit, 40) and (ref == "HEAD" or commit == ref), "source ref is not an exact commit")
    text = subprocess.check_output(
        ["git", "-C", root, "show", commit + ":scripts/lib/maintained-media.tsv"], text=True)
    mapping = inventory_rows(text)
    excluded = {destination.encode() for rows in mapping.values() for _, destination in rows}
    excluded.update(manifest_name(kind).encode() for kind in mapping)
    entries = subprocess.check_output(["git", "-C", root, "ls-tree", "-r", "-z", commit])
    checksum = hashlib.sha256(b"tuna-required-source-inputs-v1\0")
    for entry in entries.split(b"\0"):
        if not entry:
            continue
        metadata, separator, path = entry.partition(b"\t")
        require(bool(separator) and len(metadata.split(b" ")) == 3, "invalid Git tree entry")
        if path not in excluded:
            checksum.update(entry + b"\0")
    return checksum.hexdigest()


def git_source(root, commit):
    """Independently resolve an actual source tree and its required Git inputs."""
    require(sha(commit, 40), "missing exact source commit")
    tree = subprocess.check_output(
        ["git", "-C", str(root), "rev-parse", "--verify", commit + "^{tree}"], text=True).strip()
    require(sha(tree, 40), "invalid actual source tree")
    return {"commit": commit, "tree": tree,
            "required_source_inputs_sha256": required_source_inputs(root, commit)}


def validate_source(source, pins, kind, root=ROOT):
    require(isinstance(source, dict) and all(source.get(key) == value for key, value in pins.items()),
            "receipt source differs from pinned current capture source")
    capture = git_source(root, source.get("capture_commit"))
    built = git_source(root, source.get("built_commit"))
    require(source.get("capture_tree") == capture["tree"] and
            source.get("built_tree") == built["tree"] and
            built["required_source_inputs_sha256"] == capture["required_source_inputs_sha256"] ==
            source.get("required_source_inputs_sha256"),
            "actual build/capture Git trees or required source inputs differ")
    require(kind == "vm-tour" or built == capture,
            "nested media must launch a build from the actual current capture source")


def json_blob(blob):
    def unique(pairs):
        value = {}
        for key, item in pairs:
            require(key not in value, "duplicate JSON key " + key)
            value[key] = item
        return value
    value = json.loads(blob, object_pairs_hook=unique)
    require(isinstance(value, dict), "expected JSON object")
    return value


def media(path):
    """Inspect and fully decode the real bytes, not extensions/PNG signatures."""
    probe = subprocess.run(["ffprobe", "-v", "error", "-count_frames", "-show_streams", "-show_format",
                            "-of", "json", str(path)], capture_output=True, check=True)
    streams = json_blob(probe.stdout).get("streams", [])
    require(any(s.get("codec_type") == "video" and s.get("width", 0) > 0 and
                s.get("height", 0) > 0 for s in streams), "no decoded visual stream: " + str(path))
    decoded = subprocess.run(["ffmpeg", "-nostdin", "-v", "error", "-xerror", "-i", str(path),
                              "-map", "0:v:0", "-f", "null", "-"], capture_output=True)
    require(decoded.returncode == 0 and not decoded.stderr.strip(),
            "media decode failed: " + str(path))
    visual = next(s for s in streams if s.get("codec_type") == "video")
    frames = int(visual.get("nb_read_frames", 0))
    require(frames > 0, "media has no actual frames")
    result = {"codec": visual.get("codec_name"), "width": visual["width"],
              "height": visual["height"], "frames": frames}
    suffix = Path(path).suffix
    codecs = {".png": {"png"}, ".webp": {"webp"}, ".gif": {"gif"},
              ".mp4": {"h264"}, ".webm": {"vp8", "vp9", "av1"}}
    require(result["codec"] in codecs.get(suffix, set()), "media codec disagrees with its canonical role")
    if suffix in (".mp4", ".webm", ".gif"):
        duration = float(json_blob(probe.stdout).get("format", {}).get("duration", 0))
        require(math.isfinite(duration) and duration > 0, "video/animation has no finite duration")
        result["duration"] = duration
    return result


def identity(args):
    require(re.fullmatch(r"[1-9][0-9]*", args.run_id) is not None, "invalid run id")
    require(re.fullmatch(r"[1-9][0-9]*", args.attempt) is not None, "invalid run attempt")
    for name in ("expect_merge", "expect_head", "expect_tree"):
        require(sha(getattr(args, name), 40), "invalid " + name)
    require(args.expect_merge == args.expect_head,
            "promotion requires the exact merged commit to be the current capture commit")
    require(sha(args.expect_source_inputs), "missing/invalid required source inputs pin")
    return {"id": args.run_id, "attempt": args.attempt}, {
        "merged_commit": args.expect_merge, "capture_commit": args.expect_head,
        "capture_tree": args.expect_tree,
        "required_source_inputs_sha256": args.expect_source_inputs,
    }


def assertions(blob):
    lines = blob.decode("utf-8").splitlines()
    require(bool(lines), "empty producer assertions")
    for line in lines:
        fields = line.split(maxsplit=2)
        # One gate ID may pass more than once for distinct sub-scenarios:
        # the GTK shell proof re-passes G-POINTER-TRACE-PRIVATE for the
        # locked scene and again for the D-Bus lock ripple. Every recorded
        # line must still be a well-formed pass.
        require(len(fields) == 3 and fields[1] == "pass" and fields[2],
                "producer assertion did not pass: " + line)


def evidence(root, name):
    blob = confined(root, name).read_bytes()
    return {"file": name, "sha256": digest(blob)}, blob


def evidence_files(kind):
    files = {"source_receipt": "media-source.json",
             "outcomes": "manifest.json" if kind == "vm-tour" else "assertions.txt"}
    if kind == "gtk-walkthrough":
        files["capture_receipt"] = "media-capture.json"
    elif kind == "vm-tour":
        files.update(image_receipt="strip-image.json", capture_timing="tour/capture-timing.json")
        files.update(base_index="native-base/index.json", base_platform="native-base/platform-cosign.json",
                     base_process="native-base/platform-verification.json")
    elif kind == "overview-demo":
        files["action_receipt"] = "overview-demo-actions.json"
    return files


def validate_receipt(receipt, blobs, args, mapping):
    kind = receipt.get("bundle")
    require(receipt.get("schema") == SCHEMA and kind in mapping, "unknown media receipt/bundle")
    run, source = identity(args)
    require(receipt.get("run") == run, "receipt run/attempt differs from pinned CI run")
    validate_source(receipt.get("source"), source, kind)
    source = receipt["source"]
    name, environment = PRODUCERS[kind]
    producer = receipt.get("producer", {})
    require(producer == {"name": name, "environment": environment, "status": "ok", "exit_code": 0},
            "producer did not report real successful completion in the required environment")
    require(type(producer["exit_code"]) is int, "producer exit code is not an integer")
    needed = evidence_files(kind)
    require(set(blobs) == set(needed), "missing or unexpected producer evidence")
    for key, filename in needed.items():
        entry = (receipt.get("base_verification", {}).get(key.removeprefix("base_"), {})
                 if key in ("base_index", "base_platform", "base_process") else receipt.get(key, {}))
        require(entry.get("file") == filename, "unexpected producer evidence path: " + key)
        require(sha(entry.get("sha256")) and digest(blobs[key]) == entry["sha256"],
                "producer evidence hash mismatch: " + key)
    source_evidence = json_blob(blobs["source_receipt"])
    require(all(source_evidence.get(key) == value for key, value in source.items()) and
            str(source_evidence.get("run_id")) == run["id"] and
            str(source_evidence.get("run_attempt")) == run["attempt"],
            "actual build/launch source evidence differs from pinned source/run")
    files = receipt.get("files")
    require(isinstance(files, list) and len(files) == len(mapping[kind]), "incomplete canonical media bundle")
    pairs = [(entry.get("source"), entry.get("destination")) for entry in files]
    require(len(set(pairs)) == len(pairs) and set(pairs) == set(mapping[kind]),
            "receipt files differ from canonical inventory; historical destinations are forbidden")
    if kind in ("gtk-walkthrough", "overview-demo"):
        elfs = source_evidence.get("launched_elfs", {})
        require(source_evidence.get("environment") == "nested-gtk" and
                source_evidence.get("tracked_source_clean") is True and
                set(elfs) == {"tuna-compositor", "tuna-shell-gtk"} and
                all(isinstance(value, dict) and sha(value.get("sha256")) and
                    type(value.get("pid")) is int and value["pid"] > 0 and
                    type(value.get("start_ticks")) is int and value["start_ticks"] > 0
                    for value in elfs.values()),
                "GTK media lacks actual clean build/launched ELF provenance")
    for entry in files:
        require(sha(entry.get("sha256")) and type(entry.get("bytes")) is int and entry["bytes"] > 0 and
                isinstance(entry.get("probe"), dict), "missing underlying media byte hash/size/inspection")
    if kind == "gtk-walkthrough":
        captured = json_blob(blobs["capture_receipt"])
        require(captured.get("schema") == 1 and captured.get("status") == "ok" and
                type(captured.get("exit_code")) is int and captured["exit_code"] == 0 and
                captured.get("source_sha256") == digest(blobs["source_receipt"]) and
                captured.get("outcomes_sha256") == digest(blobs["outcomes"]) and
                captured.get("files") == {entry["source"]: {"sha256": entry["sha256"], "bytes": entry["bytes"]}
                                          for entry in files},
                "GTK files are not sealed by the actual complete current-run producer")
    if kind != "vm-tour":
        require("image" not in receipt, "nested frames cannot claim installed VM identity")
        assertions(blobs["outcomes"])
        if kind == "overview-demo":
            validate_overview(receipt, blobs, source_evidence)
        return
    require(source_evidence.get("environment") == "installed-vm" and
            source_evidence.get("tracked_source_clean") is True and
            isinstance(source_evidence.get("original_session"), dict) and bool(source_evidence["original_session"]),
            "VM media lacks actual admitted original native session evidence")
    image = receipt.get("image", {})
    require(re.fullmatch(r"sha256:[0-9a-f]{64}", image.get("id", "")) is not None and
            re.fullmatch(r".+@sha256:[0-9a-f]{64}", image.get("base", "")) is not None and
            sha(image.get("package_sha256")), "missing exact VM image/base/package identity")
    require(args.expect_image == image["id"] and args.expect_package == image["package_sha256"],
            "VM image/package differs from pinned expected identities")
    strip = json_blob(blobs["image_receipt"])
    require(strip.get("schema") == 1 and strip.get("commit") == source["built_commit"] and
            strip.get("image_id") == image["id"] and strip.get("base_image") == image["base"] and
            strip.get("package_sha256") == image["package_sha256"] and
            isinstance(strip.get("member_sha256"), dict) and bool(strip["member_sha256"]) and
            all(sha(value) for value in strip["member_sha256"].values()),
            "actual VM image receipt disagrees with media identity")
    from vm_strip_image import validate_base_origin, validate_product_origin
    require(strip.get("capture_commit") == source["capture_commit"] and
            strip.get("capture_tree") == source["capture_tree"] and
            strip.get("built_tree") == source["built_tree"] and
            strip.get("required_source_inputs_sha256") == source["required_source_inputs_sha256"],
            "VM admission source facts differ from actual build/capture")
    origin = strip.get("product_origin")
    require(source_evidence.get("product_origin") == origin,
            "VM source and loaded-image capsule lineage differ")
    if origin is not None or source["built_commit"] != source["capture_commit"]:
        validate_product_origin(origin, strip, ROOT)
        validate_base_origin(origin, strip["base_image"], blobs["base_index"])
    validate_base_verification(receipt, blobs, source_evidence, args)
    outcome = json_blob(blobs["outcomes"])
    checks = outcome.get("assertions")
    require(isinstance(checks, dict) and bool(checks) and
            all(isinstance(check, dict) and check.get("pass") is True for check in checks.values()),
            "actual VM producer assertions did not all pass")
    require(outcome.get("commit") == source["built_commit"] and
            outcome.get("test_image_id") == image["id"] and outcome.get("base_image") == image["base"],
            "actual VM producer manifest differs from built source/image")
    timing = json_blob(blobs["capture_timing"])
    require(timing.get("schema") == 1 and timing.get("unconfirmed_stills") == [] and
            isinstance(timing.get("frames"), list) and bool(timing["frames"]) and
            isinstance(timing.get("stills"), dict) and bool(timing["stills"]),
            "tour has missing/unconfirmed real capture outcomes")
    validate_still_confirmations(timing, files, source_evidence["original_session"])
    tour = outcome.get("tour", {})
    video = receipt.get("video", {})
    require(video.get("file") == "tour/tuna-tour.webm" and sha(video.get("sha256")) and
            type(video.get("bytes")) is int and video["bytes"] > 0 and
            isinstance(video.get("probe"), dict) and video["probe"].get("frames", 0) > 1,
            "missing actual tour video byte identity/inspection")
    require(tour.get("video") == video["file"] and
            tour.get("capture_timing") == receipt["capture_timing"]["file"] and
            type(tour.get("frames")) is int and tour["frames"] > 0,
            "VM producer did not report the actual tour video")


def validate_base_verification(receipt, blobs, source, args):
    """Validate retained successful verification output, not a new crypto verify."""
    base = source.get("base", {})
    index_ref, platform_ref = base.get("index"), base.get("platform")
    index_digest = "sha256:" + digest(blobs["base_index"])
    require(index_ref == BASE_REPOSITORY + "@" + index_digest,
            "native base index does not match the retained actual raw index bytes")
    index = json_blob(blobs["base_index"])
    require(index.get("schemaVersion") == 2 and isinstance(index.get("manifests"), list),
            "native base evidence is not an OCI image index")
    members = [entry for entry in index["manifests"]
               if entry.get("platform", {}).get("os") == "linux" and
               entry.get("platform", {}).get("architecture") == "amd64"]
    require(len(members) == 1 and re.fullmatch(r"sha256:[0-9a-f]{64}", members[0].get("digest", "")),
            "native base index lacks a unique immutable linux/amd64 platform member")
    platform_digest = members[0]["digest"]
    require(platform_ref == BASE_REPOSITORY + "@" + platform_digest and
            receipt["image"]["base"] in (platform_ref, BASE_REPOSITORY + ":gnome@" + platform_digest),
            "actual source/base image does not name that verified platform member")
    require((not args.expect_base_index or args.expect_base_index == index_ref) and
            (not args.expect_base_platform or args.expect_base_platform == platform_ref),
            "native base differs from independently pinned expected index/platform")
    validate_platform_verification(blobs["base_platform"], blobs["base_process"], platform_digest)


def load_artifact(root):
    receipt = json_blob(confined(root, "media-receipt.json").read_bytes())
    blobs = {key: confined(root, filename).read_bytes()
             for key, filename in evidence_files(receipt.get("bundle")).items()}
    return receipt, blobs


def validate_overview(receipt, blobs, source):
    actions = json_blob(blobs["action_receipt"])
    validate_recording(actions)
    require(actions.get("schema") == 1 and actions.get("complete") is True and
            actions.get("source") == source and actions.get("clock_domain") == "host CLOCK_MONOTONIC" and
            type(actions.get("window")) is int and actions["window"] > 0,
            "overview capture lacks complete original-session action evidence")
    expected = ["open-overview", "select-beta", "reopen-overview", "dismiss-overview"]
    events = actions.get("events", [])
    require(isinstance(events, list) and [event.get("action") for event in events] == expected,
            "overview action sequence differs from README journey")
    snapshots = actions.get("snapshots", {})
    labels = ["before", "opened", "selected-beta", "reopened", "dismissed"]
    require(set(snapshots) == set(labels), "overview is missing real before/after observations")
    def finite(value):
        return type(value) in (int, float) and math.isfinite(value)
    begin = actions.get("capture_begin")
    require(finite(begin) and begin > 0, "missing monotonic recording boundary")
    previous = begin
    for index, label in enumerate(labels):
        observation = snapshots[label]
        observed = observation.get("observed_host_monotonic")
        require(finite(observed) and previous <= observed <= begin + 12.2,
                "overview observation lies outside its actual recording/action interval")
        if index < len(events):
            event = events[index]
            require(finite(event.get("begin")) and finite(event.get("end")) and
                    observed <= event["begin"] <= event["end"] <= begin + 12.2,
                    "overview inputs are missing real bounded monotonic outcomes")
            previous = event["end"]
    app_ids = {"tuna-test-alpha", "tuna-test-beta", "tuna-test-gamma"}
    states = {label: snapshots[label]["state"] for label in labels}
    before = states["before"]
    windows = before.get("windows", [])
    require(before.get("overview_open") is False and before.get("locked") is False and
            len(windows) == 3 and {window.get("app_id") for window in windows} == app_ids,
            "overview did not start with the actual three fixture windows")
    for label in ("opened", "reopened"):
        state = states[label]
        previews = state.get("previews", [])
        require(state.get("overview_open") is True and state.get("overview_progress") == 1 and
                len(previews) == 3 and {window.get("app_id") for window in previews} == app_ids,
                "overview previews were not actually observed: " + label)
    for label in ("selected-beta", "dismissed"):
        state = states[label]
        require(state.get("overview_open") is False and state.get("focused_app_id") == "tuna-test-beta",
                "actual Beta activation/dismissal is missing")
    for index, key in ((0, "Super_L"), (2, "Super_L"), (3, "Escape")):
        require(events[index].get("argv") == ["key", key], "overview action did not use the real expected input")
    beta = next(window for window in states["opened"]["previews"] if window["app_id"] == "tuna-test-beta")
    rect = beta.get("rect", [])
    require(len(rect) == 4 and all(finite(value) for value in rect) and rect[2] > 0 and rect[3] > 0,
            "actual Beta preview rectangle is missing")
    x, y, width, height = rect
    require(events[1].get("argv") == ["mousemove", "--window", str(actions["window"]),
            str(math.floor(x + width / 2)), str(math.floor(y + height / 2)), "click", "1"],
            "Beta selection did not click the observed real preview")
    files = {entry["source"]: entry for entry in receipt["files"]}
    video, derivative = actions.get("video", {}), actions.get("derivative", {})
    mp4, gif = files["overview-demo.mp4"], files["overview-demo.gif"]
    require(video.get("file") == mp4["source"] and video.get("sha256") == mp4["sha256"] and
            derivative.get("file") == gif["source"] and derivative.get("sha256") == gif["sha256"] and
            derivative.get("input_sha256") == mp4["sha256"] and derivative.get("fps") == 8 and
            derivative.get("size") == [640, 400],
            "overview GIF lacks exact new-MP4 derivative lineage")
    raw = receipt.get("raw_video", {})
    require(raw.get("file") == "overview-demo.raw.mp4" and sha(raw.get("sha256")) and
            raw["sha256"] == video.get("raw_sha256") and type(raw.get("bytes")) is int and raw["bytes"] > 0 and
            raw.get("probe", {}).get("codec") == "h264", "overview original recording evidence is missing")
    for entry, size, codec in ((mp4, (1280, 800), "h264"), (gif, (640, 400), "gif")):
        probe = entry.get("probe", {})
        require((probe.get("width"), probe.get("height")) == size and probe.get("codec") == codec and
                probe.get("frames", 0) > 1 and 11.8 <= probe.get("duration", 0) <= 12.2,
                "overview actual media inspection differs from recorded journey")
    gates = {line.split()[0] for line in blobs["outcomes"].decode().splitlines()}
    require({"C-README-" + name for name in ("OPEN", "SELECT-BETA", "REOPEN", "DISMISS", "VIDEO", "GIF")} <= gates,
            "overview real producer outcomes are incomplete")


def validate_auxiliary(root, receipt):
    key = {"vm-tour": "video", "overview-demo": "raw_video"}.get(receipt["bundle"])
    if key is not None:
        entry = receipt[key]
        path = confined(root, entry["file"])
        blob = path.read_bytes()
        require(len(blob) == entry["bytes"] and digest(blob) == entry["sha256"] and
                media(path) == entry["probe"], "actual source video differs from producer receipt")
    for entry in receipt["files"]:
        confined(root, entry["source"])
    if receipt["bundle"] == "vm-tour":
        timing = json_blob(confined(root, evidence_files("vm-tour")["capture_timing"]).read_bytes())
        source = json_blob(confined(root, "media-source.json").read_bytes())
        validate_still_confirmations(timing, receipt["files"], source["original_session"], root)
    elif receipt["bundle"] == "overview-demo":
        actions = json_blob(confined(root, evidence_files("overview-demo")["action_receipt"]).read_bytes())
        validate_recording(actions, confined(root, receipt["raw_video"]["file"]))


def atomic_write(path, blob):
    path.parent.mkdir(parents=True, exist_ok=True)
    handle, temporary = tempfile.mkstemp(prefix=".media-", dir=path.parent)
    try:
        with os.fdopen(handle, "wb") as stream:
            stream.write(blob)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def encoded(value):
    return (json.dumps(value, sort_keys=True, indent=2) + "\n").encode()


def manifest_name(kind):
    # Never reuse or overwrite docs/walkthrough/manifest.json or historical
    # diagnostic receipts: these new manifests describe only canonical media.
    return "docs/walkthrough/media-manifests/" + kind + ".json"


def emit(args, mapping):
    require(len(args.artifacts) == 1 and len(args.bundle) == 1, "receipt needs one artifact and bundle")
    require(args.producer_exit_code == 0, "receipt requires the actual producer exit code zero")
    root = Path(args.artifacts[0]).absolute()
    require(not root.resolve().is_relative_to(ROOT / "docs"),
            "checked-in documentation cannot emit a producer media receipt")
    kind = args.bundle[0]
    require(kind in mapping, "bundle has no canonical inventory")
    run, source = identity(args)
    name, environment = PRODUCERS[kind]
    receipt = {"schema": SCHEMA, "bundle": kind, "run": run, "source": source,
               "producer": {"name": name, "environment": environment, "status": "ok", "exit_code": 0},
               "files": []}
    blobs = {}
    for key, filename in evidence_files(kind).items():
        receipt[key], blobs[key] = evidence(root, filename)
    source_evidence = json_blob(blobs["source_receipt"])
    receipt["source"] = {key: source_evidence.get(key) for key in
                         ("merged_commit", "capture_commit", "capture_tree", "built_commit",
                          "built_tree", "required_source_inputs_sha256")}
    if kind == "vm-tour":
        receipt["base_verification"] = {
            "index": receipt.pop("base_index"), "platform": receipt.pop("base_platform"),
            "process": receipt.pop("base_process")}
        strip = json_blob(blobs["image_receipt"])
        receipt["image"] = {"id": strip.get("image_id"), "base": strip.get("base_image"),
                            "package_sha256": strip.get("package_sha256")}
        # Emitter reads the actual image receipt; independently pinned values,
        # when supplied, must agree. Intake/freshness ALWAYS require those pins.
        args.expect_image = args.expect_image or receipt["image"]["id"]
        args.expect_package = args.expect_package or receipt["image"]["package_sha256"]
        video_path = confined(root, "tour/tuna-tour.webm")
        video_bytes = video_path.read_bytes()
        receipt["video"] = {"file": "tour/tuna-tour.webm", "sha256": digest(video_bytes),
                            "bytes": len(video_bytes), "probe": media(video_path)}
    elif kind == "overview-demo":
        path = confined(root, "overview-demo.raw.mp4")
        blob = path.read_bytes()
        receipt["raw_video"] = {"file": "overview-demo.raw.mp4", "sha256": digest(blob),
                                "bytes": len(blob), "probe": media(path)}
    for source_file, destination in mapping[kind]:
        path = confined(root, source_file)
        blob = path.read_bytes()
        probe = media(path)
        receipt["files"].append({"source": source_file, "destination": destination,
                                 "sha256": digest(blob), "bytes": len(blob), "probe": probe})
    validate_receipt(receipt, blobs, args, mapping)
    validate_auxiliary(root, receipt)
    atomic_write(confined(root, "media-receipt.json"), encoded(receipt))
    print("RECEIPT %s: %d actual media files" % (kind, len(receipt["files"])))


def intake(args, mapping):
    require(args.artifacts, "intake needs artifact directories")
    dest = Path(args.dest).absolute()
    require(not dest.resolve().is_relative_to(ROOT / "docs"),
            "destination must be a repository/staging root, never a historical documentation directory")
    plans, manifests, kinds = [], [], set()
    for value in args.artifacts:
        root = Path(value).absolute()
        require(not root.resolve().is_relative_to(ROOT / "docs"),
                "checked-in documentation is not an artifact source")
        receipt, blobs = load_artifact(root)
        validate_receipt(receipt, blobs, args, mapping)
        kind = receipt["bundle"]
        require(kind not in kinds, "duplicate artifact bundle " + kind)
        kinds.add(kind)
        validate_auxiliary(root, receipt)
        for entry in receipt["files"]:
            path = confined(root, entry["source"])
            blob = path.read_bytes()
            require(len(blob) == entry["bytes"] and digest(blob) == entry["sha256"],
                    "underlying media bytes differ from producer: " + entry["source"])
            require(media(path) == entry["probe"], "media inspection differs from actual bytes")
            target = confined(dest, entry["destination"])
            require(path.resolve() != target.resolve(), "source-copy is not artifact intake")
            require(not path.resolve().is_relative_to(dest.resolve() / "docs"),
                    "checked-in documentation is not producer evidence")
            plans.append((target, blob))
        document = {"schema": SCHEMA, "receipt": receipt,
                    "evidence": {key: base64.b64encode(blob).decode("ascii") for key, blob in blobs.items()},
                    "installed_signed_release_qualified": False}
        manifests.append((confined(dest, manifest_name(kind)), encoded(document)))
    if args.bundle:
        require(kinds == set(args.bundle), "intake does not cover requested bundles")
    # All bundles/bytes/outcomes/destinations validate before the first write;
    # manifests are committed last so interrupted copies cannot appear fresh.
    for path, blob in plans + manifests:
        atomic_write(path, blob)
    for kind in sorted(kinds):
        print("PROMOTED %s (current-run media only; not native/signed-release qualification)" % kind)


def freshness(args, mapping):
    require(args.bundle, "freshness requires explicit --bundle selections")
    dest = Path(args.dest).absolute()
    documents = []
    source_artifacts = {}
    for value in args.source_artifact:
        root = Path(value).absolute()
        receipt, blobs = load_artifact(root)
        validate_receipt(receipt, blobs, args, mapping)
        require(receipt["bundle"] not in source_artifacts, "duplicate source artifact")
        validate_auxiliary(root, receipt)
        for entry in receipt["files"]:
            path = confined(root, entry["source"])
            blob = path.read_bytes()
            require(len(blob) == entry["bytes"] and digest(blob) == entry["sha256"] and
                    media(path) == entry["probe"], "actual source artifact media differs from receipt")
        source_artifacts[receipt["bundle"]] = (receipt, blobs)
    for kind in args.bundle:
        document = json_blob(confined(dest, manifest_name(kind)).read_bytes())
        require(document.get("schema") == SCHEMA and document.get("installed_signed_release_qualified") is False,
                "missing current-run canonical media manifest")
        receipt = document["receipt"]
        require(receipt.get("bundle") == kind, "manifest bundle mismatch")
        blobs = {key: base64.b64decode(value, validate=True) for key, value in document["evidence"].items()}
        validate_receipt(receipt, blobs, args, mapping)
        if source_artifacts:
            require(source_artifacts.get(kind) == (receipt, blobs),
                    "authenticated source artifact differs from canonical receipt/evidence")
        for entry in receipt["files"]:
            path = confined(dest, entry["destination"])
            blob = path.read_bytes()
            require(len(blob) == entry["bytes"] and digest(blob) == entry["sha256"],
                    "STALE underlying media bytes: " + entry["destination"])
            media(path)
        documents.append(receipt)
    # No CURRENT output until the complete requested set passes.
    for receipt in documents:
        print("CURRENT %s run %s attempt %s source %s environment %s" % (
            receipt["bundle"], args.run_id, args.attempt, args.expect_head, receipt["producer"]["environment"]))
        if receipt["bundle"] == "vm-tour":
            print("Tour source video hash retained; video bytes are not a local canonical destination"
                  + (" (source artifact revalidated)" if source_artifacts else " (use --source-artifact to revalidate)"))


def main(argv=None):
    arguments = list(sys.argv[1:] if argv is None else argv)
    if arguments and arguments[0] == "source-inputs":
        parser = argparse.ArgumentParser(description=required_source_inputs.__doc__)
        parser.add_argument("command", choices=("source-inputs",))
        parser.add_argument("--source-root", default=str(ROOT))
        parser.add_argument("--ref", default="HEAD")
        args = parser.parse_args(arguments)
        try:
            print(required_source_inputs(args.source_root, args.ref))
        except (ValueError, OSError, subprocess.SubprocessError) as error:
            print("tuna-maintained-media: source inputs refused: " + str(error), file=sys.stderr)
            return 1
        return 0
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=("receipt", "intake", "freshness"))
    parser.add_argument("artifacts", nargs="*")
    parser.add_argument("--bundle", action="append", default=[], choices=tuple(PRODUCERS))
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--attempt", required=True)
    parser.add_argument("--expect-merge", required=True, help="current checked capture merge commit")
    parser.add_argument("--expect-head", required=True, help="actual current capture producer commit")
    parser.add_argument("--expect-tree", required=True, help="actual current capture producer tree")
    parser.add_argument("--expect-source-inputs", required=True)
    parser.add_argument("--expect-image")
    parser.add_argument("--expect-package")
    parser.add_argument("--expect-base-index")
    parser.add_argument("--expect-base-platform")
    parser.add_argument("--producer-exit-code", type=int)
    parser.add_argument("--dest", default=str(ROOT))
    parser.add_argument("--source-artifact", action="append", default=[])
    args = parser.parse_args(arguments)
    try:
        identity(args)
        mapping = inventory()
        {"receipt": emit, "intake": intake, "freshness": freshness}[args.command](args, mapping)
    except (ValueError, OSError, KeyError, TypeError, AttributeError, IndexError, subprocess.SubprocessError) as error:
        print("tuna-maintained-media: STALE/refused: " + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
