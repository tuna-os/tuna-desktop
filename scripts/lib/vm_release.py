#!/usr/bin/env python3
"""Normal desktop VM release producer. No dispatch input is a trust assertion.

Handoff contains exactly release/package/image/corral from the launcher contract.
All receipts are reacquired from their owning GitHub runs; unsigned candidate
manifests never enter the launcher. Signing and publication are separate jobs.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import signal
import subprocess
import tarfile
import tempfile
import time
import urllib.parse
import zipfile

DESKTOP = "tuna-os/tuna-desktop"
PACKAGES = "tuna-os/tunaos-packages"
IMAGE_REPO = "ghcr.io/tuna-os/marlin"
ISSUER = "https://token.actions.githubusercontent.com"
IMAGE_IDENTITY = "https://github.com/tuna-os/tunaOS/.github/workflows/reusable-build-image.yml@refs/heads/main"
IDENTITY = f"https://github.com/{DESKTOP}/.github/workflows/vm-release.yml@refs/heads/main"
PACKAGE_FINGERPRINT = "4E5CC9F8B3B521793D95266E629BE6EA45188366"
PLATFORMS = ("linux-amd64", "linux-arm64", "darwin-amd64", "darwin-arm64")
BINS = ("tuna-compositor", "tuna-session", "tuna-shell-gtk", "tuna-shell-host", "tuna-ibus-bridge", "tuna-wallpaper-svg")
SCHEMA = "org.tunaos.desktop.vm/v1"
HERE = Path(__file__).resolve().parent


class Rejected(ValueError):
    pass


def need(condition, message):
    if not condition:
        raise Rejected(message)


def canonical(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False) + "\n").encode()


def pairs(rows):
    result = {}
    for key, value in rows:
        need(key not in result, "duplicate JSON key: " + key)
        result[key] = value
    return result


def parse(data):
    return json.loads(data, object_pairs_hook=pairs, parse_constant=lambda value: (_ for _ in ()).throw(Rejected("nonfinite JSON")))


def regular(path):
    path = Path(path)
    need(not path.is_symlink() and stat.S_ISREG(path.stat().st_mode), f"regular file required: {path}")
    return path


def sha256(path):
    h = hashlib.sha256()
    with regular(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def hash_value(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) and value != "0" * 64


def commit_value(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{40}", value) and value != "0" * 40


def number(value):
    return type(value) is int and value > 0 or isinstance(value, str) and re.fullmatch(r"[1-9][0-9]*", value)




def run(args, *, cwd=None, env=None, timeout=1800, stderr=None):
    return subprocess.check_output([str(x) for x in args], cwd=cwd, env=env, timeout=timeout, stderr=stderr)


def gh(path):
    return parse(run(["gh", "api", path]))


def pages(path, key):
    result = []
    for page in range(1, 101):
        rows = gh(path + ("&" if "?" in path else "?") + f"per_page=100&page={page}")[key]
        result.extend(rows)
        if len(rows) < 100:
            return result
    raise Rejected("GitHub pagination exceeded")


def source(repo, commit, path):
    need(commit_value(commit), "source must be an exact commit")
    value = gh(f"repos/{repo}/contents/{path}?ref={commit}")
    need(value.get("type") == "file" and value.get("encoding") == "base64", "source receipt is not a file")
    return base64.b64decode(value["content"], validate=False)


def download(url, destination):
    need(url.startswith("https://") and not urllib.parse.urlsplit(url).username, "HTTPS download required")
    need(not destination.exists() and not destination.is_symlink(), "download destination already exists")
    run(["curl", "--fail", "--location", "--proto", "=https", "--proto-redir", "=https", "--user-agent", "TunaOS-VM-Release/1.0", "--output", destination, url])


def release(repo, tag, commit):
    need(re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z][A-Za-z0-9.]*)?", tag), "actual release tag required")
    data = gh(f"repos/{repo}/releases/tags/{tag}")
    need(data["tag_name"] == tag and not data["draft"] and data.get("published_at"), "release is not published")
    ref = gh(f"repos/{repo}/git/ref/tags/{tag}")["object"]
    for _ in range(8):
        if ref["type"] == "commit":
            break
        need(ref["type"] == "tag", "release tag does not resolve to commit")
        ref = gh(f"repos/{repo}/git/tags/{ref['sha']}")["object"]
    need(ref["type"] == "commit" and ref["sha"] == commit, "release tag/source mismatch")
    return data


def successful_run(repo, run_id, workflow, commit=None, required_jobs=()):
    need(number(run_id), "actual run ID required")
    data = gh(f"repos/{repo}/actions/runs/{run_id}")
    need(data["repository"]["full_name"] == repo and data["head_repository"]["full_name"] == repo,
         "fork run is not trusted")
    need(data["path"] == ".github/workflows/" + workflow and data["head_branch"] == "main"
         and data["event"] in ("push", "workflow_dispatch", "schedule")
         and data["status"] == "completed" and data["conclusion"] == "success", "unsuccessful or untrusted workflow run")
    if commit is not None:
        need(data["head_sha"] == commit, "workflow source mismatch")
    jobs = pages(f"repos/{repo}/actions/runs/{run_id}/jobs?filter=latest", "jobs")
    for expected in required_jobs:
        matches = [job for job in jobs if job["name"] == expected]
        need(len(matches) == 1 and matches[0]["conclusion"] == "success", "missing successful job: " + expected)
    need(all(job["conclusion"] in ("success", "skipped") for job in jobs), "failed/cancelled workflow job")
    return data


def successful_step(repo, run_id, expected):
    jobs = pages(f"repos/{repo}/actions/runs/{run_id}/jobs?filter=latest", "jobs")
    matches = [step for job in jobs if job["conclusion"] == "success"
               for step in job.get("steps", []) if step["name"] == expected and step["conclusion"] == "success"]
    need(matches, "required producer step did not execute: " + expected)


def artifact(repo, run_id, name):
    rows = [row for row in pages(f"repos/{repo}/actions/runs/{run_id}/artifacts", "artifacts") if row["name"] == name]
    need(len(rows) == 1 and not rows[0]["expired"], "missing/expired exact receipt artifact: " + name)
    raw = run(["gh", "api", f"repos/{repo}/actions/artifacts/{rows[0]['id']}/zip"], timeout=1800)
    digest = rows[0].get("digest")
    if digest:
        need(digest == "sha256:" + hashlib.sha256(raw).hexdigest(), "GitHub artifact digest mismatch")
    return decode_artifact(raw)


def decode_artifact(raw):
    result = {}
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        need(len(archive.infolist()) <= 20000, "artifact file-count limit")
        total = 0
        for entry in archive.infolist():
            path = PurePosixPath(entry.filename)
            need(not path.is_absolute() and ".." not in path.parts and "\\" not in entry.filename, "unsafe artifact path")
            mode = entry.external_attr >> 16
            need(not stat.S_ISLNK(mode), "artifact symlink refused")
            if entry.is_dir():
                continue
            need(stat.S_IFMT(mode) in (0, stat.S_IFREG), "nonregular artifact entry refused")
            total += entry.file_size
            need(total <= 2 * 1024**3 and entry.file_size <= 1024**3, "artifact byte limit")
            need(entry.filename not in result, "duplicate artifact entry")
            result[entry.filename] = archive.read(entry)
    return result


def assertions(data, required):
    rows = {}
    for line in data.decode().splitlines():
        fields = line.split(None, 2)
        need(len(fields) >= 2 and fields[0] not in rows, "invalid/duplicate assertion")
        rows[fields[0]] = fields[1]
    need(rows and all(value == "pass" for value in rows.values()), "failed or empty assertion receipt")
    need(set(required) <= rows.keys(), "missing required assertion")


REVIEW_JOURNEYS = (
    "Overview open", "Search typed", "App launched from search", "Workspace switch",
    "Alt-Tab switcher", "Tiled window", "Quick settings open", "Notification banner", "Lock screen",
)


def frame_references(cell):
    # Existing Tuna frame cells use backticked paths, comma-separated for pairs.
    need(re.fullmatch(r"`[^`]+\.png`(?:,\s*`[^`]+\.png`)*", cell), "unsupported review frame cell: " + cell)
    references = re.findall(r"`([^`]+)`", cell)
    for reference in references:
        path = PurePosixPath(reference)
        need(not path.is_absolute() and ".." not in path.parts and "\\" not in reference
             and str(path) == reference and not any(c.isspace() for c in reference), "unsafe review frame reference")
        need(len(path.parts) >= 2 and path.parts[0] in ("gtk-shell", "journey-shots"),
             "review Tuna frame must identify its supported CI artifact")
    return references


def review_receipt(data, rel):
    text = data.decode()
    need(hashlib.sha256(data).hexdigest() == rel["reviewSha256"], "review byte mismatch")
    need(f"# Visual review: {rel['tag']}" in text and not re.search(r"\b(TBD|PENDING|PLACEHOLDER)\b", text, re.I), "unfilled release review")
    need(re.search(r"https://github\.com/tuna-os/tuna-desktop/actions/runs/[1-9][0-9]*", text), "review lacks actual CI input")
    need(re.search(r"^- Name:\s*\S.+$", text, re.M)
         and re.search(r"^- Date:\s*[0-9]{4}-[0-9]{2}-[0-9]{2}\s*$", text, re.M), "reviewer/date missing")
    rows = [line.split("|")[1:-1] for line in text.splitlines() if line.startswith("|")]
    journeys = {}
    for cells in rows:
        if cells and cells[0].strip() in REVIEW_JOURNEYS:
            stage = cells[0].strip()
            need(len(cells) == 5 and stage not in journeys, "invalid/duplicate review journey: " + stage)
            journeys[stage] = [cell.strip() for cell in cells]
    frames = {}
    for stage in REVIEW_JOURNEYS:
        need(stage in journeys, "missing review journey: " + stage)
        cells = journeys[stage]
        need(cells[3] in ("match", "deviation") and cells[2], "unqualified review journey")
        frames[stage] = frame_references(cells[1])
        if cells[3] == "deviation":
            need(re.search(r"(?:parity-ledger|Ledger P-)", cells[4]), "unowned review deviation")
    need("- [x] No `regression` verdicts remain" in text and "- [x] Every `deviation` has a ledger row with an owner" in text,
         "review signoffs incomplete")
    return frames


def review_frame_hashes(journeys, artifacts):
    from PIL import Image

    hashes = {}
    for references in journeys.values():
        for reference in references:
            artifact_name, name = reference.split("/", 1)
            files = artifacts[artifact_name]
            need(name in files, "reviewed CI frame absent: " + reference)
            data = files[name]
            try:
                with Image.open(io.BytesIO(data)) as image:
                    need(image.format == "PNG", "review frame is not a PNG: " + reference)
                    image.verify()
            except (OSError, SyntaxError, ValueError) as error:
                raise Rejected("invalid reviewed PNG: " + reference) from error
            hashes[reference] = hashlib.sha256(data).hexdigest()
    return hashes


def validate_handoff(value):
    need(type(value) is dict and set(value) == {"release", "package", "image", "corral"}, "handoff must contain only release/package/image/corral")
    rel, pkg, image, corral = (value[k] for k in ("release", "package", "image", "corral"))
    fields = {
        "release": {"repository", "tag", "commit", "version", "ciRunId", "reviewPath", "reviewSha256", "installedControlRunId"},
        "package": {"name", "version", "architecture", "url", "sha256", "signatureUrl", "signerFingerprint", "sourceCommit", "factoryRunId"},
        "image": {"repository", "indexDigest", "manifestDigest", "architecture", "sourceCommit", "buildRunId", "qualificationRunId", "certificateIdentity"},
        "corral": {"repository", "tag", "commit", "certificateIdentity", "assets"},
    }
    for key, expected in fields.items():
        need(type(value[key]) is dict and set(value[key]) == expected, "handoff fields differ: " + key)
    for owner, keys in ((rel, ("ciRunId", "installedControlRunId")), (pkg, ("factoryRunId",)), (image, ("buildRunId", "qualificationRunId"))):
        need(all(number(owner[key]) for key in keys), "actual upstream receipt IDs required")
    need(rel["repository"] == DESKTOP and commit_value(rel["commit"]) and rel["version"] == rel["tag"][1:], "desktop release identity")
    need(rel["reviewPath"] == f"docs/reviews/{rel['tag']}.md" and hash_value(rel["reviewSha256"]), "review identity")
    need(pkg["name"] == "tuna-desktop" and pkg["architecture"] == "x86_64" and pkg["sourceCommit"] == rel["commit"]
         and pkg["signerFingerprint"] == PACKAGE_FINGERPRINT and hash_value(pkg["sha256"]), "package identity")
    expected_version = rel["version"].replace("-", "", 1)
    need(re.fullmatch(re.escape(expected_version) + r"-[1-9][0-9]*", pkg["version"]), "package/release version mismatch")
    expected_url = f"https://repo.tunaos.org/pacman/arch/x86_64/tuna-desktop-{pkg['version']}-x86_64.pkg.tar.zst"
    need(pkg["url"] == expected_url and pkg["signatureUrl"] == expected_url + ".sig", "package served URL mismatch")
    need(image["repository"] == IMAGE_REPO and image["architecture"] == "amd64" and image["certificateIdentity"] == IMAGE_IDENTITY
         and commit_value(image["sourceCommit"]), "image trust identity")
    for key in ("indexDigest", "manifestDigest"):
        need(isinstance(image[key], str) and image[key].startswith("sha256:") and hash_value(image[key][7:]), "image digest required")
    need(corral["repository"] == "tuna-os/corral" and commit_value(corral["commit"])
         and corral["certificateIdentity"] == f"https://github.com/tuna-os/corral/.github/workflows/release.yml@refs/tags/{corral['tag']}"
         and set(corral["assets"]) == set(PLATFORMS), "Corral release identity")
    for platform, asset in corral["assets"].items():
        name = "corral-" + platform
        need(set(asset) == {"name", "url", "sha256", "bundleUrl"}, "Corral asset fields differ")
        url = f"https://github.com/tuna-os/corral/releases/download/{corral['tag']}/{name}"
        need(asset["name"] == name and asset["url"] == url and asset["bundleUrl"] == url + ".sigstore.json"
             and hash_value(asset["sha256"]), "Corral signed platform asset mismatch")
    return value


def verify_blob(path, identity, commit=None):
    bundle = Path(str(path) + ".sigstore.json")
    document = parse(regular(bundle).read_bytes())
    need(document.get("mediaType") in ("application/vnd.dev.sigstore.bundle.v0.3+json", "application/vnd.dev.sigstore.bundle+json;version=0.3"), "standard full Sigstore v0.3 bundle required")
    material = document.get("verificationMaterial", {})
    need(material.get("tlogEntries") and all(entry.get("inclusionProof", {}).get("checkpoint") for entry in material["tlogEntries"]),
         "full transparency inclusion proof/checkpoint required")
    args = ["cosign", "verify-blob", "--bundle", bundle, "--certificate-identity", identity, "--certificate-oidc-issuer", ISSUER]
    if commit:
        args += ["--certificate-github-workflow-sha", commit]
    run(args + [path])


def admit(args):
    value = validate_handoff(parse(regular(args.handoff).read_bytes()))
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    provenance = out / "provenance"
    provenance.mkdir()
    rel, pkg, image, corral = (value[k] for k in ("release", "package", "image", "corral"))
    release(DESKTOP, rel["tag"], rel["commit"])
    ci = successful_run(DESKTOP, rel["ciRunId"], "ci.yml", rel["commit"], ("check", "ci-gate", "journey", "arch-package"))
    # ci-gate permits lane skips; require a full manual qualification run.
    need(ci["event"] == "workflow_dispatch", "release requires full dispatched CI, not path-selected success")
    review = source(DESKTOP, rel["commit"], rel["reviewPath"])
    review_journeys = review_receipt(review, rel)
    review_run_ids = set(re.findall(r"https://github\.com/tuna-os/tuna-desktop/actions/runs/([1-9][0-9]*)", review.decode()))
    need(len(review_run_ids) == 1, "review must identify one proof-frames CI run")
    reviewed_run_id = review_run_ids.pop()
    reviewed_run = successful_run(DESKTOP, reviewed_run_id, "ci.yml", required_jobs=("check", "ci-gate", "gtk-shell", "journey"))
    reviewed_artifacts = {name: artifact(DESKTOP, reviewed_run_id, name) for name in ("gtk-shell", "journey-shots")}
    reviewed_frames = review_frame_hashes(review_journeys, reviewed_artifacts)
    (provenance / "review-inputs.json").write_bytes(canonical({
        "run": reviewed_run, "journeys": review_journeys, "frames": reviewed_frames}))
    (provenance / "review.md").write_bytes(review)
    installed_run = successful_run(DESKTOP, rel["installedControlRunId"], "ci.yml", rel["commit"], ("journey", "ci-gate"))
    installed = artifact(DESKTOP, rel["installedControlRunId"], "tuna-deb-installed-proof")
    assertions(installed["controls/assertions.txt"], ("G-INSTALLED-START", "G-INSTALLED-STABLE", "G-INSTALLED-RECOVERY"))
    preflight = parse(installed["controls/installed-package.json"])
    need(preflight["companions"]["commit"] == rel["commit"] and preflight["version"].replace("~", "-", 1) == rel["version"], "installed controls source/version mismatch")
    need(set(preflight["files"]) == set(BINS) and all(hash_value(row["sha256"]) for row in preflight["files"].values()), "installed binary receipts missing")
    for name in ("os-release", "dpkg-versions.txt", "apt-policy.txt", "package.sha256"):
        need(installed.get(name), "missing installed dependency receipt: " + name)
    debs = artifact(DESKTOP, rel["installedControlRunId"], "tuna-deb")
    deb_hashes = {hashlib.sha256(data).hexdigest() for name, data in debs.items() if name.endswith(".deb")}
    need(len(deb_hashes) == 1 and installed["package.sha256"].decode().split()[0] in deb_hashes, "installed package bytes do not match CI package")
    (provenance / "installed.json").write_bytes(canonical({"run": installed_run, "preflight": preflight,
        "artifacts": {name: hashlib.sha256(data).hexdigest() for name, data in installed.items()}}))
    factory = successful_run(PACKAGES, pkg["factoryRunId"], "publish-tideforge-arch.yml")
    successful_step(PACKAGES, pkg["factoryRunId"], "Install the published wave from the served repository")
    recipe_bytes = source(PACKAGES, factory["head_sha"], "packages/tuna-desktop/package.yaml")
    import yaml
    recipe = yaml.safe_load(recipe_bytes)
    need(recipe["build"]["environment"]["TUNA_VERSION"] == rel["tag"]
         and recipe["source"]["url"] == f"https://github.com/{DESKTOP}/archive/refs/tags/{rel['tag']}.tar.gz"
         and str(recipe["version"]) == rel["version"].replace("-", "~", 1), "factory recipe source mismatch")
    need(hash_value(recipe["source"]["sha256"]), "factory source hash absent")
    with tempfile.TemporaryDirectory(prefix="vm-package-") as work:
        work = Path(work)
        archive = work / "source.tar.gz"
        download(recipe["source"]["url"], archive)
        need(sha256(archive) == recipe["source"]["sha256"], "factory tag archive hash mismatch")
        package = work / "package.pkg.tar.zst"
        download(pkg["url"], package)
        need(sha256(package) == pkg["sha256"], "served package byte mismatch")
        factory_files = artifact(PACKAGES, pkg["factoryRunId"], "publish-arch-x86_64-tuna-desktop")
        need(any(hashlib.sha256(data).hexdigest() == pkg["sha256"] for name, data in factory_files.items() if name.endswith(".pkg.tar.zst")), "served package is not factory output")
        signature = work / "package.sig"
        download(pkg["signatureUrl"], signature)
        key = work / "public.gpg"
        key.write_bytes(source(PACKAGES, factory["head_sha"], "public.gpg"))
        home = work / "gnupg"
        home.mkdir(mode=0o700)
        run(["gpg", "--batch", "--homedir", home, "--import", key])
        verified = run(["gpg", "--batch", "--homedir", home, "--status-fd", "1", "--verify", signature, package]).decode()
        signatures = [line.split() for line in verified.splitlines() if line.startswith("[GNUPG:] VALIDSIG ")]
        need(len(signatures) == 1 and signatures[0][-1] == PACKAGE_FINGERPRINT, "untrusted package signing key")
        # Read, never execute, signed package data. All installed regular files
        # must match the digest-pinned image, not merely its version string.
        package_tar = work / "package.tar"
        with package_tar.open("wb") as output:
            subprocess.run(["zstd", "-dc", str(package)], stdout=output, check=True, timeout=300)
        pkg_files = {}
        with tarfile.open(package_tar) as tar:
            for member in tar:
                path = PurePosixPath(member.name)
                need(not path.is_absolute() and ".." not in path.parts, "unsafe signed package entry")
                if member.isfile() and not str(path).startswith("."):
                    need(member.size <= 256 * 1024**2, "package entry exceeds limit")
                    pkg_files["/" + str(path)] = hashlib.sha256(tar.extractfile(member).read()).hexdigest()
            info = tar.extractfile(".PKGINFO").read().decode()
        need(f"pkgname = tuna-desktop\n" in info and f"pkgver = {pkg['version']}\n" in info and "arch = x86_64\n" in info, "signed package metadata mismatch")
        verify_image(image, pkg, pkg_files, provenance)
    corral_release = release("tuna-os/corral", corral["tag"], corral["commit"])
    assets = {entry["name"]: entry for entry in corral_release["assets"]}
    for asset in corral["assets"].values():
        for name, url in ((asset["name"], asset["url"]), (asset["name"] + ".sigstore.json", asset["bundleUrl"])):
            need(name in assets and assets[name]["browser_download_url"] == url, "Corral asset absent from actual release")
            download(url, out / name)
        need(sha256(out / asset["name"]) == asset["sha256"], "Corral asset hash mismatch")
        verify_blob(out / asset["name"], corral["certificateIdentity"], corral["commit"])
    (provenance / "ci.json").write_bytes(canonical(ci))
    (provenance / "factory.json").write_bytes(canonical(factory))
    (provenance / "recipe.yaml").write_bytes(recipe_bytes)
    (out / "admitted.json").write_bytes(canonical(value))


STRICT_PUBLISHED_FIELDS = {
    "acquisition": "published",
    "qualification_mode": "strict",
    "waived_assertions": [],
    "skipped_assertions": [],
}
CLOCK_CHECKS = ("G-DATETIME-CLOCK-TZ", "G-DATETIME-CLOCK-SET")
PUBLISHED_LIFECYCLE_CHECKS = (
    *CLOCK_CHECKS, "V-BRIGHTNESS-CONTRACT", "V-GSD-PLUGINS", "V-GSD-POWER-IDLE",
    "V-LOCKED-HINT", "V-POWER-BUTTON", "V-POINTER-LIVE", "V-POINTER-HOTPLUG",
    "V-MOUSE-HANDEDNESS", "V-AUDIO-AMPLIFICATION", "V-HOT-CORNER-PRESSURE",
    "V-HOT-CORNER-CONSTRAINTS", "V-SLEEP-AUTH-FAIL-CLOSED", "V-SLEEP-FRESH-AUTH",
    "V-ORCA-ACTIVATION", "V-ORCA-LOGOUT", "V-ORCA-LOGOUT-REAP", "V-LOGOUT", "V-LIFECYCLE",
)


def strict_mode(receipt):
    need(type(receipt) is dict
         and not {"legacy_waivers", "legacy_waivers_allowed"}.intersection(receipt)
         and canonical({key: receipt.get(key) for key in STRICT_PUBLISHED_FIELDS}) == canonical(STRICT_PUBLISHED_FIELDS),
         "typed strict published acquisition with zero waived/skipped assertions required")


def strict_checks(checks, required=()):
    need(type(checks) is dict and checks, "strict image assertions missing")
    for row in checks.values():
        need(type(row) is dict and set(row) == {"pass", "detail"} and row["pass"] is True
             and isinstance(row["detail"], str), "strict image assertion failed or invalid")
    need(set(required) <= checks.keys(), "strict image capability unqualified")


def strict_image_receipt(receipts, image, pkg, qualification):
    receipt = parse(receipts["qualification.json"])
    final = parse(receipts["manifest.json"])
    repeated = parse(receipts["repeat-manifest.json"])
    for document in (receipt, final, repeated):
        strict_mode(document)
    need(number(receipt["run_id"]) and str(receipt["run_id"]) == str(image["qualificationRunId"])
         and receipt["published_image_digest"] == image["indexDigest"]
         and receipt["published_image"] == IMAGE_REPO + "@" + image["indexDigest"],
         "strict qualification image/run mismatch")
    strict_checks(receipt["checks"], (*PUBLISHED_LIFECYCLE_CHECKS, "V-STRICT-PUBLISHED", "V-STRICT-PUBLISHED-REPEAT", "V-REPEAT"))
    need(type(repeated["requested_boots"]) is int and repeated["requested_boots"] == 5
         and type(repeated["boots"]) is list and len(repeated["boots"]) == 5,
         "strict five-boot qualification incomplete")
    expected_results = [{"boot": index, "returncode": 0} for index in range(1, 6)]
    need(canonical(repeated["boot_results"]) == canonical(expected_results), "strict five-boot process results incomplete")
    need(canonical(receipt["repeat"]) == canonical(repeated)
         and canonical(receipt["final_boot"]) == canonical(final)
         and canonical(repeated["boots"][-1]) == canonical(final), "strict aggregate/final boot receipts differ")
    for row in repeated["boots"]:
        strict_mode(row)
        need(row["shipped_tuna_package"] == "tuna-desktop " + pkg["version"]
             and row["published_image"] == IMAGE_REPO + "@" + image["indexDigest"]
             and row["harness_commit"] == qualification["head_sha"]
             and number(row["run_id"]) and str(row["run_id"]) == str(image["qualificationRunId"])
             and row["kvm"] is True, "strict repeated-boot identity mismatch")
        strict_checks(row["assertions"], ("V-STRICT-PUBLISHED",))
        need(set(repeated["boots"][0]["assertions"]) <= row["assertions"].keys()
             and row["assertions"].keys() <= receipt["checks"].keys(),
             "strict boot assertions absent from boot/aggregate")
    strict_checks(final["assertions"], PUBLISHED_LIFECYCLE_CHECKS)
    assertions(receipts["assertions.txt"], (*PUBLISHED_LIFECYCLE_CHECKS, "V-STRICT-PUBLISHED", "V-STRICT-PUBLISHED-REPEAT", "V-REPEAT"))
    return receipt


def verify_image(image, pkg, pkg_files, provenance):
    build = successful_run("tuna-os/tunaOS", image["buildRunId"], "build-marlin.yml", image["sourceCommit"])
    artifacts = artifact("tuna-os/tunaOS", image["buildRunId"], f"marlin-tuna-linux-amd64-{image['buildRunId']}")
    need(artifacts.get("marlin-tuna-linux-amd64.txt", b"").decode().strip() == image["manifestDigest"], "image platform digest not built by declared run")
    # The platform is the signed payload object in normal image releases.
    # An index need not itself have a signature: its exact bytes and unique
    # platform membership are checked below, and the final manifest binds both.
    ref = IMAGE_REPO + "@" + image["manifestDigest"]
    run(["cosign", "verify", "--certificate-identity", IMAGE_IDENTITY, "--certificate-oidc-issuer", ISSUER,
         "--certificate-github-workflow-sha", image["sourceCommit"], ref])
    raw = run(["skopeo", "inspect", "--raw", "docker://" + IMAGE_REPO + "@" + image["indexDigest"]])
    need("sha256:" + hashlib.sha256(raw).hexdigest() == image["indexDigest"], "OCI index byte mismatch")
    index = parse(raw)
    annotations = index.get("annotations", {})
    need(isinstance(annotations.get("org.opencontainers.image.version"), str)
         and annotations["org.opencontainers.image.version"], "trusted OCI index lacks actual image version")
    manifests = [entry for entry in index["manifests"] if entry.get("platform", {}).get("os") == "linux" and entry.get("platform", {}).get("architecture") == "amd64"]
    need(len(manifests) == 1 and manifests[0]["digest"] == image["manifestDigest"], "index/platform architecture mismatch")
    ref = IMAGE_REPO + "@" + image["manifestDigest"]
    config = parse(run(["skopeo", "inspect", "--config", "docker://" + ref]))
    need(config["architecture"] == "amd64" and config["os"] == "linux", "OCI config architecture mismatch")
    qualification = successful_run(DESKTOP, image["qualificationRunId"], "marlin-tuna-nightly.yml", required_jobs=("published-flavor",))
    receipts = artifact(DESKTOP, image["qualificationRunId"], "published-marlin-tuna-vm-" + str(image["qualificationRunId"]))
    receipt = strict_image_receipt(receipts, image, pkg, qualification)
    expected_package = "tuna-desktop " + pkg["version"]
    run(["sudo", "-n", "podman", "pull", "--arch", "amd64", ref])
    prefix = ["sudo", "-n", "podman", "run", "--rm", "--network=none", "--read-only", "--entrypoint"]
    need(run(prefix + ["/usr/bin/pacman", ref, "-Q", "tuna-desktop"]).decode().strip() == expected_package, "trusted image package version mismatch")
    need(all("/usr/bin/" + name in pkg_files for name in BINS), "package binaries missing")
    actual = run(prefix + ["/usr/bin/sha256sum", ref, "--", *sorted(pkg_files)]).decode()
    observed = {line.split(None, 1)[1].strip(): line.split()[0] for line in actual.splitlines()}
    need(observed == pkg_files, "trusted image payload differs from actual signed package")
    # The real shipping image must itself be pristine. Never repair it by
    # deleting keys/accounts after the fact or add fixture bypasses.
    audit = r'''set -eu
    test ! -e /usr/share/tuna-vm
    test ! -e /etc/gdm/custom.conf.d/tuna-vm.conf
    set -- /root /home
    if test -e /etc/ssh; then set -- "$@" /etc/ssh; fi
    credentials="$(find -L "$@" -type f \( -name authorized_keys -o -name authorized_keys2 -o -name 'id_rsa*' -o -name 'id_dsa*' -o -name 'id_ed25519*' -o -name 'id_ecdsa*' -o -name 'ssh_host_*_key' \) -print)"
    test -z "$credentials"
    private_keys="$(find -L "$@" -type f -exec sh -c '
        for path do
            if grep -qE -- "^-----BEGIN (OPENSSH |RSA |DSA |EC |ENCRYPTED )?PRIVATE KEY-----" "$path"; then
                printf "%s\n" "$path"
            else
                status=$?
                test "$status" -eq 1 || exit "$status"
            fi
        done
    ' sh {} +)"
    test -z "$private_keys"
    awk -F: '$3 >= 1000 && $3 < 65534 {exit 1}' /etc/passwd
    awk -F: '$2 != "" && $2 !~ /^[!*]/ {exit 1} $2 == "" {exit 1}' /etc/shadow
    printf 'fixture_paths=absent\nordinary_users=0\nunlocked_passwords=0\nssh_credentials=absent\n'
    awk -F: '{printf "account=%s uid=%s gid=%s shell=%s\n", $1, $3, $4, $7}' /etc/passwd
    awk -F: '{printf "password=%s locked_or_disabled\n", $1}' /etc/shadow
    '''
    inventory = run(prefix + ["/bin/sh", ref, "-ec", audit]).decode()
    (provenance / "image.json").write_bytes(canonical({"build": build, "qualification": qualification, "receipt": receipt,
        "config": config, "packageFiles": pkg_files, "pristineImageAudit": {
            "image": ref, "passed": True, "inventory": inventory}}))


def build_disk(args):
    out = args.output.resolve()
    value = validate_handoff(parse(regular(out / "admitted.json").read_bytes()))
    corral = value["corral"]
    checkout = args.source.resolve()
    need(not checkout.exists(), "Corral source directory must not exist")
    run(["git", "clone", "--filter=blob:none", "--no-checkout", "https://github.com/tuna-os/corral.git", checkout])
    run(["git", "-C", checkout, "checkout", "--detach", corral["commit"]])
    need(run(["git", "-C", checkout, "rev-parse", "HEAD"]).decode().strip() == corral["commit"], "Corral source checkout mismatch")
    # This fixes owned QMP retirement. Architecture/browser/lifecycle APIs are
    # additionally exercised by all native gates before publication.
    required = gh("repos/tuna-os/corral/commits/dc877f43")["sha"]
    run(["git", "-C", checkout, "merge-base", "--is-ancestor", required, corral["commit"]])
    raw = out / "shipping.raw"
    image = IMAGE_REPO + "@" + value["image"]["manifestDigest"]
    run(["go", "run", HERE / "vm_release_disk.go", image, raw], cwd=checkout, timeout=14400)
    destination = out / "tuna-desktop-amd64.qcow2"
    need(not destination.exists(), "shipping disk already exists")
    run(["qemu-img", "convert", "-c", "-f", "raw", "-O", "qcow2", raw, destination], timeout=3600)
    raw.unlink()
    info = parse(run(["qemu-img", "info", "--output=json", destination]))
    need(info["format"] == "qcow2" and info["virtual-size"] == 40 * 1024**3
         and not info.get("backing-filename") and not info.get("encrypted"), "shipping qcow2 metadata rejected")
    need(0 < destination.stat().st_size < 2 * 1024**3, "disk exceeds GitHub's per-release-asset size limit")
    disk = {"name": destination.name, "url": f"https://github.com/{DESKTOP}/releases/download/{value['release']['tag']}/{destination.name}",
        "sha256": sha256(destination), "size": destination.stat().st_size, "virtualSize": info["virtual-size"], "architecture": "amd64", "firmware": "uefi"}
    (out / "disk.json").write_bytes(canonical(disk))
    destination.chmod(0o444)


def boot(args):
    out = args.output.resolve()
    value = validate_handoff(parse(regular(out / "admitted.json").read_bytes()))
    disk = parse(regular(out / "disk.json").read_bytes())
    pristine = out / disk["name"]
    need(sha256(pristine) == disk["sha256"], "shipping disk changed before boot")
    corral = out / "corral-linux-amd64"
    corral.chmod(0o755)
    verify_blob(corral, value["corral"]["certificateIdentity"], value["corral"]["commit"])
    evidence = out / "producer-boot"
    evidence.mkdir()
    name = "tuna-release-" + os.environ["GITHUB_RUN_ID"] + "-" + os.environ["GITHUB_RUN_ATTEMPT"]
    ref = "qemu//local/" + name
    # Corral consumes a copy; the read-only shipping disk is never booted.
    with tempfile.TemporaryDirectory(prefix="tuna-release-boot-") as work:
        copy = Path(work) / "boot.qcow2"
        shutil.copyfile(pristine, copy)
        created = False
        try:
            run([corral, "--backend", "qemu", "create", name, "--qcow", copy, "--firmware", "uefi", "--arch", "amd64", "--loopback"])
            created = True
            run([corral, "start", ref])
            deadline = time.monotonic() + 900
            while True:
                serial = run([corral, "logs", ref, "--serial"]).decode(errors="replace")
                need(not re.search(r"TUNAOS_(?:DESKTOP|BASE)_CONTRACT_FAIL|Kernel panic|Entering emergency mode|Dracut Emergency Shell|You are in emergency mode", serial), "shipping boot contract failed")
                if "TUNAOS_DESKTOP_CONTRACT_OK" in serial and "TUNAOS_BASE_CONTRACT_OK" in serial:
                    break
                need(time.monotonic() < deadline, "shipping boot contract timed out")
                time.sleep(5)
            (evidence / "serial.log").write_text(serial)
            run([corral, "wait", ref, "--paint", "--timeout", "120s"])
            run([corral, "screenshot", ref, "--require-paint", "-o", evidence / "boot.png"])
        finally:
            if created:
                try:
                    run([corral, "stop", ref])
                finally:
                    run([corral, "delete", ref, "--force"])
    need(sha256(pristine) == disk["sha256"], "shipping disk changed during copy qualification")
    receipt = {"scope": "boot-only; NOT untouched firstboot/account/login UX qualification", "diskSha256": disk["sha256"],
        "runId": os.environ["GITHUB_RUN_ID"], "commit": os.environ["GITHUB_SHA"],
        "files": {path.name: sha256(path) for path in sorted(evidence.iterdir())}}
    (evidence / "receipt.json").write_bytes(canonical(receipt))
    manifest = {"schema": SCHEMA, **value, "disk": disk,
        "producer": {"repository": DESKTOP, "workflow": ".github/workflows/vm-release.yml", "commit": os.environ["GITHUB_SHA"],
            "runId": os.environ["GITHUB_RUN_ID"], "runAttempt": int(os.environ["GITHUB_RUN_ATTEMPT"])},
        "qualification": {"producerBootRunId": os.environ["GITHUB_RUN_ID"], "producerEvidenceSha256": sha256(evidence / "receipt.json")}}
    (out / "manifest.json").write_bytes(canonical(manifest))


QUALIFICATION_CHECKS = {
    "signatureAdmission", "declaredGuestArchitecture", "nativeExecution", "browserRfbConnected",
    "pristineFirstbootAccountCreated", "pristineTunaSession", "pristineLogoutLogin",
    "adminWheelPasswordRequired", "rebootPersistence",
    "stopCleanup", "cancellationCleanup", "failureCleanup", "browserFailureCleanup",
    "ownershipIsolation", "diskLockReleased",
}


def evidence_files(root, declared):
    root = Path(root)
    need(root.is_dir() and not root.is_symlink() and type(declared) is dict and declared, "real evidence directory/files required")
    for name, expected in declared.items():
        relative = PurePosixPath(name)
        need(name and not relative.is_absolute() and ".." not in relative.parts and "\\" not in name
             and str(relative) == name and hash_value(expected), "unsafe evidence name/hash")
        path = root
        for component in relative.parts:
            path = path / component
            need(not path.is_symlink(), "evidence symlink refused")
        need(sha256(path) == expected, "evidence byte mismatch: " + name)


def native_receipt(root, platform, manifest, manifest_sha):
    receipt = parse(regular(root / "qualification.json").read_bytes())
    need(set(receipt) == {"schema", "platform", "manifestSha256", "diskSha256", "corralSha256",
         "producerRunId", "producerCommit", "host", "checks", "files"} and type(receipt["schema"]) is int
         and receipt["schema"] == 1, "native receipt schema mismatch")
    need(receipt["platform"] == platform and receipt["manifestSha256"] == manifest_sha
         and receipt["diskSha256"] == manifest["disk"]["sha256"]
         and receipt["corralSha256"] == manifest["corral"]["assets"][platform]["sha256"]
         and str(receipt["producerRunId"]) == str(manifest["producer"]["runId"])
         and receipt["producerCommit"] == manifest["producer"]["commit"], "native proof identity mismatch")
    need(type(receipt["checks"]) is dict and set(receipt["checks"]) == QUALIFICATION_CHECKS
         and all(value is True for value in receipt["checks"].values()), "native acceptance check absent/failed")
    host = receipt["host"]
    need(set(host) == {"system", "machine", "qemuVersion", "qemuSha256", "guestArchitecture", "accelerator", "firmwareCodeSha256"},
         "native host receipt schema mismatch")
    need(host["guestArchitecture"] == "amd64" and isinstance(host["qemuVersion"], str) and host["qemuVersion"]
         and hash_value(host["qemuSha256"]) and hash_value(host["firmwareCodeSha256"]), "native QEMU/firmware evidence missing")
    expected_system = "Linux" if platform.startswith("linux-") else "Darwin"
    expected_machines = ("arm64", "aarch64") if platform.endswith("-arm64") else ("x86_64", "AMD64")
    need(host["system"] == expected_system and host["machine"] in expected_machines, "wrong native host architecture")
    accelerators = {"linux-amd64": ("kvm", "tcg"), "linux-arm64": ("tcg",),
                    "darwin-amd64": ("hvf", "tcg"), "darwin-arm64": ("tcg",)}
    need(host["accelerator"] in accelerators[platform], "wrong native accelerator; ARM hosts require explicit x86 TCG")
    evidence_files(root, receipt["files"])
    actual = set()
    for path in root.rglob("*"):
        need(not path.is_symlink(), "native evidence symlink refused")
        if path.is_file() and path != root / "qualification.json":
            actual.add(str(path.relative_to(root)))
    need(actual == set(receipt["files"]), "unindexed or missing native evidence")
    return receipt


def finalize(args):
    out = args.output.resolve()
    manifest_path = regular(out / "manifest.json")
    manifest = parse(manifest_path.read_bytes())
    need(manifest_path.read_bytes() == canonical(manifest), "manifest is not canonical")
    need(manifest["producer"]["commit"] == os.environ["GITHUB_SHA"]
         and str(manifest["producer"]["runId"]) == os.environ["GITHUB_RUN_ID"]
         and manifest["producer"]["runAttempt"] == int(os.environ["GITHUB_RUN_ATTEMPT"]), "producer run/source binding mismatch")
    verify_blob(manifest_path, IDENTITY, manifest["producer"]["commit"])
    need(sha256(out / manifest["disk"]["name"]) == manifest["disk"]["sha256"], "shipping disk changed")
    need(sha256(out / "producer-boot" / "receipt.json") == manifest["qualification"]["producerEvidenceSha256"], "producer boot receipt changed")
    boot_receipt = parse((out / "producer-boot" / "receipt.json").read_bytes())
    need(boot_receipt["diskSha256"] == manifest["disk"]["sha256"]
         and boot_receipt["commit"] == manifest["producer"]["commit"]
         and str(boot_receipt["runId"]) == str(manifest["producer"]["runId"]), "boot receipt identity mismatch")
    evidence_files(out / "producer-boot", boot_receipt["files"])
    # Reuse consumer admission; strictly bind every native receipt and byte.
    # There is no alternate trust descriptor or native-qualification waiver.
    run(["python3", HERE / "try_tuna_desktop.py", "inspect", "--manifest", manifest_path,
         "--bundle", str(manifest_path) + ".sigstore.json", "--artifact-dir", out])
    for platform in PLATFORMS:
        native_receipt(args.evidence_dir / platform, platform, manifest, sha256(manifest_path))
    # Save only a hash index of actual qualification bytes. Never rewrite the
    # signed manifest after native qualification.
    receipts = {}
    for path in sorted(args.evidence_dir.rglob("*")):
        if path.is_dir():
            continue
        need(not path.is_symlink(), "native evidence symlink refused")
        receipts[str(path.relative_to(args.evidence_dir))] = sha256(path)
    need(receipts, "empty native evidence")
    (out / "native-evidence.json").write_bytes(canonical(receipts))


def publish(args):
    finalize(args)
    out = args.output.resolve()
    manifest = parse((out / "manifest.json").read_bytes())
    rel = manifest["release"]
    actual_release = release(DESKTOP, rel["tag"], rel["commit"])
    release("tuna-os/corral", manifest["corral"]["tag"], manifest["corral"]["commit"])
    evidence = out / "vm-release-evidence.tar.gz"
    need(not evidence.exists(), "release evidence archive already exists")
    with tarfile.open(evidence, "w:gz") as archive:
        for name in ("producer-boot", "provenance", "native-evidence.json"):
            archive.add(out / name, arcname=name)
        archive.add(args.evidence_dir, arcname="native")
    assets = [out / manifest["disk"]["name"], evidence, out / "manifest.json.sigstore.json", out / "manifest.json"]
    existing = {asset["name"] for asset in actual_release["assets"]}
    need(not existing.intersection(path.name for path in assets), "refusing to overwrite immutable VM release assets")
    need(all(path.stat().st_size < 2 * 1024**3 for path in assets), "GitHub release asset exceeds size limit")
    # Manifest is last. A partial upload never advertises an admitted manifest
    # before its disk/signature exist. No --clobber, tag creation or gate waiver.
    for path in assets:
        run(["gh", "release", "upload", rel["tag"], path, "--repo", DESKTOP], timeout=3600)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for command in ("admit", "build", "boot", "finalize", "publish"):
        sub = commands.add_parser(command)
        sub.add_argument("--output", type=Path, required=True)
        if command == "admit":
            sub.add_argument("--handoff", type=Path, required=True)
        if command == "build":
            sub.add_argument("--source", type=Path, required=True)
        if command in ("finalize", "publish"):
            sub.add_argument("--evidence-dir", type=Path, required=True)
    args = parser.parse_args()
    def interrupted(signum, frame):
        raise InterruptedError("release producer interrupted")
    signal.signal(signal.SIGTERM, interrupted)
    need(os.environ.get("GITHUB_REPOSITORY") == DESKTOP and os.environ.get("GITHUB_REF") == "refs/heads/main"
         and os.environ.get("GITHUB_WORKFLOW_REF") == DESKTOP + "/.github/workflows/vm-release.yml@refs/heads/main",
         "producer must run from canonical main workflow")
    {"admit": admit, "build": build_disk, "boot": boot, "finalize": finalize, "publish": publish}[args.command](args)


if __name__ == "__main__":
    try:
        main()
    except (Rejected, KeyError, ValueError, OSError, subprocess.SubprocessError) as error:
        raise SystemExit("vm-release: rejected: " + str(error))
