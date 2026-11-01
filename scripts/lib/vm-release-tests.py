#!/usr/bin/env python3
"""Offline behavioral rejection tests; not VM/native qualification evidence."""
import copy
import hashlib
import io
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import vm_release as producer


class ParserTests(unittest.TestCase):
    def test_duplicate_and_nonfinite_json_rejected(self):
        for data in (b'{"source":1,"source":2}', b'{"source":NaN}', b'{"nested":{"a":1,"a":2}}'):
            with self.subTest(data=data), self.assertRaises(producer.Rejected):
                producer.parse(data)

    def test_canonical_bytes_stable(self):
        self.assertEqual(producer.canonical({"z": [1, True], "a": "tuna"}), b'{"a":"tuna","z":[1,true]}\n')

    def test_assertions_require_actual_pass_and_unique_required_gates(self):
        producer.assertions(b"A pass observed\nB pass observed\n", ("A", "B"))
        for data in (b"", b"A pass\nA pass\n", b"A skip\n", b"A pass\nB fail\n", b"B pass\n"):
            with self.subTest(data=data), self.assertRaises(producer.Rejected):
                producer.assertions(data, ("A",))


class ArtifactTests(unittest.TestCase):
    @staticmethod
    def archive(entries):
        out = io.BytesIO()
        with zipfile.ZipFile(out, "w") as archive:
            for name, content, mode in entries:
                info = zipfile.ZipInfo(name)
                info.external_attr = mode << 16
                archive.writestr(info, content)
        return out.getvalue()

    def test_extracts_receipt_bytes_without_filesystem_writes(self):
        raw = self.archive([("controls/assertions.txt", b"A pass\n", stat.S_IFREG | 0o644)])
        self.assertEqual(producer.decode_artifact(raw), {"controls/assertions.txt": b"A pass\n"})

    def test_refuses_traversal_absolute_symlink_and_duplicate_members(self):
        for entries in (
            [("../outside", b"x", stat.S_IFREG)],
            [("/outside", b"x", stat.S_IFREG)],
            [("a\\b", b"x", stat.S_IFREG)],
            [("alias", b"outside", stat.S_IFLNK | 0o777)],
            [("receipt", b"first", stat.S_IFREG), ("receipt", b"second", stat.S_IFREG)],
        ):
            with self.subTest(entries=entries), self.assertRaises(producer.Rejected):
                producer.decode_artifact(self.archive(entries))

    def test_expired_or_ambiguous_run_artifact_never_downloaded(self):
        for rows in ([{"name": "proof", "expired": True}], [{"name": "proof", "expired": False}] * 2, []):
            with patch.object(producer, "pages", return_value=rows), patch.object(producer, "run") as download:
                with self.assertRaises(producer.Rejected):
                    producer.artifact(producer.DESKTOP, 123, "proof")
                download.assert_not_called()

    def test_artifact_api_digest_mismatch_rejected(self):
        rows = [{"name": "proof", "expired": False, "id": 4, "digest": "sha256:" + "f" * 64}]
        with patch.object(producer, "pages", return_value=rows), patch.object(producer, "run", return_value=b"not the declared archive"):
            with self.assertRaises(producer.Rejected):
                producer.artifact(producer.DESKTOP, 123, "proof")


class HandoffTests(unittest.TestCase):
    def setUp(self):
        # Synthetic parser data only. Never a dispatch/release identity.
        tag, commit = "v2.0.0-beta.4", "a" * 40
        package_url = "https://repo.tunaos.org/pacman/arch/x86_64/tuna-desktop-2.0.0beta.4-1-x86_64.pkg.tar.zst"
        self.value = {
            "release": {"repository": producer.DESKTOP, "tag": tag, "commit": commit, "version": tag[1:],
                "ciRunId": "123", "reviewPath": f"docs/reviews/{tag}.md", "reviewSha256": "b" * 64, "installedControlRunId": "124"},
            "package": {"name": "tuna-desktop", "version": "2.0.0beta.4-1", "architecture": "x86_64", "url": package_url,
                "sha256": "c" * 64, "signatureUrl": package_url + ".sig", "signerFingerprint": producer.PACKAGE_FINGERPRINT,
                "sourceCommit": commit, "factoryRunId": "125"},
            "image": {"repository": producer.IMAGE_REPO, "indexDigest": "sha256:" + "d" * 64,
                "manifestDigest": "sha256:" + "e" * 64, "architecture": "amd64", "sourceCommit": "f" * 40,
                "buildRunId": "126", "qualificationRunId": "127", "certificateIdentity": producer.IMAGE_IDENTITY},
            "corral": {"repository": "tuna-os/corral", "tag": "v2.0.0", "commit": "b" * 40,
                "certificateIdentity": "https://github.com/tuna-os/corral/.github/workflows/release.yml@refs/tags/v2.0.0",
                "assets": {}},
        }
        for platform in producer.PLATFORMS:
            name = "corral-" + platform
            url = "https://github.com/tuna-os/corral/releases/download/v2.0.0/" + name
            self.value["corral"]["assets"][platform] = {"name": name, "url": url, "sha256": "a" * 64, "bundleUrl": url + ".sigstore.json"}

    def test_admit_cli_requires_explicit_read_token_before_any_child(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "tools"
            tools.mkdir()
            marker = root / "child-started"
            boundary = tools / "boundary"
            # A real OS process boundary, not a producer/API mock. A valid
            # credential control must reach it, but never actual network.
            boundary.write_text('#!/bin/sh\n: > "$VM_TEST_CHILD_MARKER"\nexit 97\n')
            boundary.chmod(0o700)
            for name in ("gh", "curl", "cosign", "skopeo", "sudo", "gpg", "zstd"):
                (tools / name).symlink_to(boundary)
            config = root / "gh-config"
            config.mkdir()
            persisted = "test-persisted-credential-must-not-be-used"
            (config / "hosts.yml").write_text("github.com:\n    oauth_token: " + persisted + "\n")
            handoff = root / "handoff.json"
            handoff.write_bytes(producer.canonical(self.value))
            env = {
                "PATH": str(tools), "HOME": str(root), "GH_CONFIG_DIR": str(config),
                "VM_TEST_CHILD_MARKER": str(marker),
                "GITHUB_REPOSITORY": producer.DESKTOP, "GITHUB_REF": "refs/heads/main",
                "GITHUB_WORKFLOW_REF": producer.DESKTOP + "/.github/workflows/vm-release.yml@refs/heads/main",
                "GH_TOKEN": "test-gh-fallback-must-not-be-used",
                "GITHUB_TOKEN": "test-job-fallback-must-not-be-used",
            }
            command = [sys.executable, str(Path(producer.__file__).resolve()), "admit",
                       "--handoff", str(handoff), "--output", str(root / "output")]
            expected = ("vm-release: rejected: configure secrets.VM_RELEASE_READ_TOKEN: "
                        "admission requires a read-only cross-repository GitHub credential\n")
            for token in (None, "", " \t\n"):
                child_env = dict(env)
                if token is not None:
                    child_env["VM_RELEASE_READ_TOKEN"] = token
                with self.subTest(token=token):
                    result = subprocess.run(command, env=child_env, capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, 1)
                    self.assertEqual(result.stdout, "")
                    self.assertEqual(result.stderr, expected)
                    self.assertFalse(marker.exists(), "admission started a tool before requiring its credential")
                    self.assertFalse((root / "output").exists())
                    for credential in (persisted, env["GH_TOKEN"], env["GITHUB_TOKEN"]):
                        self.assertNotIn(credential, result.stdout + result.stderr)
            # Prove the executable boundary is live through the actual CLI.
            # Its nonzero exit prevents these synthetic inputs qualifying.
            env["VM_RELEASE_READ_TOKEN"] = "test-explicit-read-credential"
            result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(marker.is_file())
            self.assertFalse((root / "output" / "admitted.json").exists())
            for credential in (persisted, env["GH_TOKEN"], env["GITHUB_TOKEN"], env["VM_RELEASE_READ_TOKEN"]):
                self.assertNotIn(credential, result.stdout + result.stderr)

    def test_exact_parser_shape_admitted(self):
        self.assertEqual(set(self.value["corral"]["assets"]), {
            "linux-amd64", "linux-arm64", "darwin-amd64", "darwin-arm64"})
        self.assertIs(producer.validate_handoff(self.value), self.value)

    def test_unknown_fields_do_not_become_trust_overrides(self):
        self.value["package"]["skipSignature"] = True
        with self.assertRaises(producer.Rejected):
            producer.validate_handoff(self.value)

    def test_wrong_package_source_key_bytes_url_and_guest_architecture_rejected(self):
        for section, key, invalid in (
            ("package", "sourceCommit", "b" * 40), ("package", "signerFingerprint", "B" * 40),
            ("package", "sha256", "0" * 64), ("package", "url", "https://attacker.invalid/package"),
            ("package", "signatureUrl", self.value["package"]["url"]), ("package", "version", "2.0.0-beta.4"),
            ("image", "architecture", "arm64"), ("image", "manifestDigest", "sha256:" + "0" * 64),
            ("image", "certificateIdentity", "https://github.com/attacker/build.yml@refs/heads/main"),
            ("release", "ciRunId", "PENDING"), ("release", "reviewPath", "../review.md"),
        ):
            original = self.value[section][key]
            self.value[section][key] = invalid
            with self.subTest(section=section, key=key), self.assertRaises(producer.Rejected):
                producer.validate_handoff(self.value)
            self.value[section][key] = original

    def test_offline_corral_filename_and_bundle_have_no_alternate_trust(self):
        asset = self.value["corral"]["assets"]["darwin-arm64"]
        for key, invalid in (("name", "../corral"), ("url", "file:///tmp/corral"), ("bundleUrl", "https://attacker.invalid/sig")):
            original = asset[key]
            asset[key] = invalid
            with self.subTest(key=key), self.assertRaises(producer.Rejected):
                producer.validate_handoff(self.value)
            asset[key] = original

    def test_all_four_host_assets_are_required_without_alternate_guest_image(self):
        for platform in producer.PLATFORMS:
            asset = self.value["corral"]["assets"].pop(platform)
            with self.subTest(platform=platform), self.assertRaises(producer.Rejected):
                producer.validate_handoff(self.value)
            self.value["corral"]["assets"][platform] = asset
        self.value["image"]["architecture"] = "arm64"
        with self.assertRaises(producer.Rejected):
            producer.validate_handoff(self.value)


class ProvenanceTests(unittest.TestCase):
    def setUp(self):
        self.commit = "a" * 40
        self.receipt = {"repository": {"full_name": producer.DESKTOP}, "head_repository": {"full_name": producer.DESKTOP},
            "path": ".github/workflows/ci.yml", "head_branch": "main", "event": "workflow_dispatch",
            "status": "completed", "conclusion": "success", "head_sha": self.commit}
        self.jobs = [{"name": "check", "conclusion": "success"}, {"name": "ci-gate", "conclusion": "success"}]

    def admit(self, receipt=None, jobs=None):
        with patch.object(producer, "gh", return_value=self.receipt if receipt is None else receipt), \
             patch.object(producer, "pages", return_value=self.jobs if jobs is None else jobs):
            return producer.successful_run(producer.DESKTOP, 123, "ci.yml", self.commit, ("check", "ci-gate"))

    def test_successful_exact_main_run_admitted(self):
        self.assertEqual(self.admit()["head_sha"], self.commit)

    def test_wrong_source_failed_fork_and_workflow_runs_rejected(self):
        mutations = (("head_sha", "b" * 40), ("conclusion", "failure"), ("status", "in_progress"),
            ("head_branch", "feature"), ("event", "pull_request"), ("path", ".github/workflows/other.yml"),
            ("head_repository", {"full_name": "attacker/tuna-desktop"}))
        for key, value in mutations:
            receipt = copy.deepcopy(self.receipt)
            receipt[key] = value
            with self.subTest(key=key), self.assertRaises(producer.Rejected):
                self.admit(receipt=receipt)

    def test_skipped_missing_duplicate_or_failed_required_job_rejected(self):
        for jobs in ([self.jobs[0]], [self.jobs[0]] * 2 + [self.jobs[1]],
                     [self.jobs[0], {"name": "ci-gate", "conclusion": "skipped"}],
                     self.jobs + [{"name": "native", "conclusion": "cancelled"}]):
            with self.subTest(jobs=jobs), self.assertRaises(producer.Rejected):
                self.admit(jobs=jobs)

    def test_moved_release_tag_rejected(self):
        def response(path):
            if "/releases/" in path:
                return {"tag_name": "v2.0.0", "draft": False, "published_at": "2026-01-01"}
            return {"object": {"type": "commit", "sha": "b" * 40}}
        with patch.object(producer, "gh", side_effect=response), self.assertRaises(producer.Rejected):
            producer.release(producer.DESKTOP, "v2.0.0", self.commit)

    def test_draft_release_rejected_before_resolving_tag(self):
        with patch.object(producer, "gh", return_value={"tag_name": "v2.0.0", "draft": True}), self.assertRaises(producer.Rejected):
            producer.release(producer.DESKTOP, "v2.0.0", self.commit)


class ImageProvenanceTests(unittest.TestCase):
    def test_index_wrong_architecture_duplicate_platform_and_unbuilt_digest_rejected(self):
        digest = "sha256:" + "e" * 64
        amd64 = {"digest": digest, "platform": {"architecture": "amd64", "os": "linux"}}
        indexes = (
            {"manifests": [{"digest": digest, "platform": {"architecture": "arm64", "os": "linux"}}]},
            {"manifests": [amd64, amd64]},
            {"manifests": [{**amd64, "digest": "sha256:" + "f" * 64}]},
        )
        for document in indexes:
            document["annotations"] = {"org.opencontainers.image.version": "parser-test-version"}
            raw = producer.canonical(document)
            image = {"buildRunId": "123", "sourceCommit": "a" * 40, "manifestDigest": digest,
                "indexDigest": "sha256:" + hashlib.sha256(raw).hexdigest()}
            def command(args, **kwargs):
                if args[0] == "cosign":
                    return b"[]"
                if args[:3] == ["skopeo", "inspect", "--raw"]:
                    return raw
                raise AssertionError("unadmitted image reached later production operation")
            with self.subTest(document=document), \
                 patch.object(producer, "successful_run", return_value={"head_sha": image["sourceCommit"]}), \
                 patch.object(producer, "artifact", return_value={"marlin-tuna-linux-amd64.txt": digest.encode()}), \
                 patch.object(producer, "run", side_effect=command), self.assertRaises(producer.Rejected):
                producer.verify_image(image, {}, {}, Path("/must-not-write"))

    def test_build_digest_receipt_cannot_name_another_platform_payload(self):
        image = {"buildRunId": "123", "sourceCommit": "a" * 40, "manifestDigest": "sha256:" + "e" * 64}
        with patch.object(producer, "successful_run", return_value={}), \
             patch.object(producer, "artifact", return_value={"marlin-tuna-linux-amd64.txt": b"sha256:wrong"}), \
             patch.object(producer, "run") as execute, self.assertRaises(producer.Rejected):
            producer.verify_image(image, {}, {}, Path("/must-not-write"))
        execute.assert_not_called()


class StrictImageReceiptTests(unittest.TestCase):
    def setUp(self):
        # Synthetic receipt data exercises admission, never release evidence.
        self.image = {"qualificationRunId": "123", "indexDigest": "sha256:" + "d" * 64}
        self.pkg = {"version": "2.0.0beta.4-1"}
        self.qualification = {"head_sha": "a" * 40}
        self.startup = {"pass": True, "detail": "synthetic observed startup"}
        boot = {
            **copy.deepcopy(producer.STRICT_PUBLISHED_FIELDS),
            "shipped_tuna_package": "tuna-desktop " + self.pkg["version"],
            "published_image": producer.IMAGE_REPO + "@" + self.image["indexDigest"],
            "harness_commit": self.qualification["head_sha"], "run_id": "123", "kvm": True,
            "assertions": {"V-STRICT-PUBLISHED": copy.deepcopy(self.startup)},
        }
        self.repeated = {
            **copy.deepcopy(producer.STRICT_PUBLISHED_FIELDS),
            "requested_boots": 5, "boots": [copy.deepcopy(boot) for _ in range(5)],
            "boot_results": [{"boot": index, "returncode": 0} for index in range(1, 6)],
        }
        for key in producer.PUBLISHED_LIFECYCLE_CHECKS:
            self.repeated["boots"][-1]["assertions"][key] = copy.deepcopy(self.startup)
        self.summary = {
            **copy.deepcopy(producer.STRICT_PUBLISHED_FIELDS),
            "run_id": "123", "published_image_digest": self.image["indexDigest"],
            "published_image": producer.IMAGE_REPO + "@" + self.image["indexDigest"],
            "checks": {key: copy.deepcopy(self.startup) for key in (
                *producer.PUBLISHED_LIFECYCLE_CHECKS, "V-STRICT-PUBLISHED", "V-STRICT-PUBLISHED-REPEAT", "V-REPEAT")},
        }

    def artifacts(self):
        summary = {**self.summary, "repeat": copy.deepcopy(self.repeated),
                   "final_boot": copy.deepcopy(self.repeated["boots"][-1])}
        return {
            "qualification.json": producer.canonical(summary),
            "manifest.json": producer.canonical(self.repeated["boots"][-1]),
            "repeat-manifest.json": producer.canonical(self.repeated),
            "assertions.txt": "".join(f"{key} pass synthetic observed\n" for key in self.summary["checks"]).encode(),
        }

    def admit(self, artifacts=None):
        return producer.strict_image_receipt(
            self.artifacts() if artifacts is None else artifacts, self.image, self.pkg, self.qualification)

    def test_explicit_strict_five_boot_receipts_with_final_clock_pass_admitted(self):
        self.assertEqual(self.admit()["run_id"], "123")
        self.assertNotIn(producer.CLOCK_CHECKS[0], self.repeated["boots"][0]["assertions"])

    def test_missing_mode_and_legacy_all_pass_receipts_rejected_at_every_level(self):
        for document in (self.summary, self.repeated, *self.repeated["boots"]):
            original = copy.deepcopy(document["qualification_mode"])
            for bad in (None, {}, {
                "schema": "org.tunaos.vm-qualification-mode/v1", "mode": "published-compatibility",
                "published": True, "legacy_waivers_allowed": True,
                "legacy_waivers": ["brightness-contract", "gsd-plugins-and-idle", "power-button"],
            }):
                document["qualification_mode"] = bad
                with self.subTest(bad=bad), self.assertRaises(producer.Rejected):
                    self.admit()
            document["qualification_mode"] = original

    def test_waivers_and_untyped_strict_mode_rejected_even_when_all_checks_pass(self):
        for document in (self.summary, self.repeated, *self.repeated["boots"]):
            for key, value in (("qualification_mode", "unqualified"), ("qualification_mode", True),
                               ("acquisition", "source-built"), ("acquisition", True),
                               ("waived_assertions", ["V-BRIGHTNESS-CONTRACT"]),
                               ("waived_assertions", ["current-build substituted proof"]),
                               ("skipped_assertions", ["V-POWER-BUTTON"]),
                               ("waived_assertions", False), ("skipped_assertions", {})):
                original = document[key]
                document[key] = value
                with self.subTest(key=key, value=value), self.assertRaises(producer.Rejected):
                    self.admit()
                document[key] = original
            for key in producer.STRICT_PUBLISHED_FIELDS:
                original = document.pop(key)
                with self.subTest(missing=key), self.assertRaises(producer.Rejected):
                    self.admit()
                document[key] = original
            document["legacy_waivers"] = ["brightness-contract"]
            with self.assertRaises(producer.Rejected):
                self.admit()
            del document["legacy_waivers"]

    def test_each_boot_exact_source_run_package_image_and_native_execution_required(self):
        for boot in self.repeated["boots"]:
            for key, value in (("harness_commit", "b" * 40), ("run_id", "124"), ("run_id", True),
                               ("shipped_tuna_package", "tuna-desktop obsolete"),
                               ("published_image", producer.IMAGE_REPO + ":tuna"), ("kvm", False)):
                original = boot[key]
                boot[key] = value
                with self.subTest(key=key), self.assertRaises(producer.Rejected):
                    self.admit()
                boot[key] = original

    def test_five_distinct_ordered_successful_process_receipts_required(self):
        original = copy.deepcopy(self.repeated["boot_results"])
        for bad in ([], original[:-1], [original[0]] * 5, list(reversed(original)),
                    [*original[:-1], {"boot": 5, "returncode": 1}],
                    [*original[:-1], {"boot": 5, "returncode": False}]):
            self.repeated["boot_results"] = bad
            with self.subTest(bad=bad), self.assertRaises(producer.Rejected):
                self.admit()
        self.repeated["boot_results"] = original

    def test_aggregate_and_final_must_equal_actual_standalone_receipts(self):
        for filename, key in (("qualification.json", "repeat"), ("qualification.json", "final_boot"),
                              ("manifest.json", None)):
            artifacts = self.artifacts()
            document = producer.parse(artifacts[filename])
            if key:
                document[key] = {}
            else:
                document["harness_commit"] = "b" * 40
            artifacts[filename] = producer.canonical(document)
            with self.subTest(filename=filename, key=key), self.assertRaises(producer.Rejected):
                self.admit(artifacts)

    def test_required_lifecycle_and_clock_pass_and_assertion_shapes_enforced(self):
        for checks in (self.summary["checks"], self.repeated["boots"][-1]["assertions"]):
            for key in producer.PUBLISHED_LIFECYCLE_CHECKS:
                original = checks.pop(key)
                with self.subTest(missing=key), self.assertRaises(producer.Rejected):
                    self.admit()
                for bad in ({"pass": False, "detail": "not observed"}, {"pass": 1, "detail": "not boolean"},
                            {"pass": True, "detail": "waived", "waived": True}):
                    checks[key] = bad
                    with self.subTest(key=key, bad=bad), self.assertRaises(producer.Rejected):
                        self.admit()
                checks[key] = original
        for boot in self.repeated["boots"]:
            boot["assertions"]["V-STRICT-PUBLISHED"]["pass"] = False
            with self.assertRaises(producer.Rejected):
                self.admit()
            boot["assertions"]["V-STRICT-PUBLISHED"]["pass"] = True

    def test_five_boot_count_and_preserved_startup_assertions_required(self):
        self.repeated["boots"][0]["assertions"]["V-STARTUP"] = copy.deepcopy(self.startup)
        self.summary["checks"]["V-STARTUP"] = copy.deepcopy(self.startup)
        with self.assertRaises(producer.Rejected):
            self.admit()
        for boot in self.repeated["boots"]:
            boot["assertions"]["V-STARTUP"] = copy.deepcopy(self.startup)
        self.admit()
        for count in (4, 6, True, "5"):
            self.repeated["requested_boots"] = count
            with self.subTest(count=count), self.assertRaises(producer.Rejected):
                self.admit()


class ReviewTests(unittest.TestCase):
    def review(self):
        return ("# Visual review: v2.0.0\n- Name: Release reviewer\n- Date: 2026-01-01\n"
            "https://github.com/tuna-os/tuna-desktop/actions/runs/123\n"
            + "\n".join(f"| {stage} | `gtk-shell/{index}.png` | `baseline.png` | match | Reviewed |"
                for index, stage in enumerate(producer.REVIEW_JOURNEYS))
            + "\n- [x] No `regression` verdicts remain\n- [x] Every `deviation` has a ledger row with an owner\n").encode()

    def admit(self, data):
        return producer.review_receipt(data, {"tag": "v2.0.0", "reviewSha256": hashlib.sha256(data).hexdigest()})

    def artifacts(self):
        from PIL import Image

        stream = io.BytesIO()
        Image.new("RGB", (2, 2), (30, 60, 90)).save(stream, format="PNG")
        png = stream.getvalue()
        return {"gtk-shell": {f"{index}.png": png for index in range(9)}, "journey-shots": {"launch.png": png}}

    def test_every_required_row_resolves_real_png_bytes_and_hashes(self):
        data = self.review().replace(b"`gtk-shell/8.png`", b"`gtk-shell/8.png`, `journey-shots/launch.png`")
        artifacts = self.artifacts()
        journeys = self.admit(data)
        hashes = producer.review_frame_hashes(journeys, artifacts)
        self.assertEqual(len(journeys), 9)
        self.assertEqual(len(hashes), 10)
        for reference, digest in hashes.items():
            artifact, member = reference.split("/", 1)
            self.assertEqual(digest, hashlib.sha256(artifacts[artifact][member]).hexdigest())

    def test_unrelated_valid_reference_cannot_rescue_invalid_required_cells(self):
        data = self.review()
        for index in range(9):
            data = data.replace(f"`gtk-shell/{index}.png`".encode(), b"nonexistent.png")
        data += b"\nUnrelated note: `gtk-shell/0.png`\n"
        with self.assertRaises(producer.Rejected):
            self.admit(data)

    def test_unwrapped_markdown_unsupported_and_unsafe_frame_cells_rejected(self):
        for cell in (b"gtk-shell/0.png", b"[frame](gtk-shell/0.png)", b"![frame](gtk-shell/0.png)",
                     b"`other/0.png`", b"`gtk-shell/../0.png`", b"`gtk-shell//0.png`",
                     b"`gtk-shell/0.png` arbitrary text", b"`gtk-shell/0.png`; `gtk-shell/1.png`"):
            data = self.review().replace(b"`gtk-shell/0.png`", cell) + b"\n`gtk-shell/1.png`\n"
            with self.subTest(cell=cell), self.assertRaises(producer.Rejected):
                self.admit(data)

    def test_missing_single_or_second_frame_never_uses_unrelated_member(self):
        for cell in (b"`gtk-shell/missing.png`", b"`gtk-shell/0.png`, `journey-shots/missing.png`"):
            journeys = self.admit(self.review().replace(b"`gtk-shell/0.png`", cell))
            with self.subTest(cell=cell), self.assertRaises(producer.Rejected):
                producer.review_frame_hashes(journeys, self.artifacts())

    def test_header_only_corrupt_and_non_png_artifact_bytes_rejected(self):
        journeys = self.admit(self.review())
        for invalid in (b"\x89PNG\r\n\x1a\n", b"not a PNG", self.artifacts()["gtk-shell"]["0.png"][:-12]):
            artifacts = self.artifacts()
            artifacts["gtk-shell"]["0.png"] = invalid
            with self.subTest(invalid=invalid), self.assertRaises(producer.Rejected):
                producer.review_frame_hashes(journeys, artifacts)

    def test_duplicate_required_stage_rejected(self):
        with self.assertRaises(producer.Rejected):
            self.admit(self.review() + b"\n| Overview open | `gtk-shell/0.png` | baseline | match | Reviewed |\n")

    def test_baseline_commentary_is_not_a_tuna_artifact_reference(self):
        data = self.review().replace(b"`baseline.png`", b"none in bundle (`B-BASE.png` used as the no-banner reference)")
        self.assertEqual(len(producer.review_frame_hashes(self.admit(data), self.artifacts())), 9)

    def test_filled_exact_review_admitted(self):
        data = self.review()
        producer.review_receipt(data, {"tag": "v2.0.0", "reviewSha256": hashlib.sha256(data).hexdigest()})

    def test_review_regression_placeholder_missing_stage_and_signoff_rejected(self):
        for data in (self.review().replace(b"| match |", b"| regression |", 1), self.review() + b"TBD\n",
                     self.review().replace(b"| Overview open", b"| Not a required stage"),
                     self.review().replace(b"- [x] No", b"- [ ] No")):
            with self.subTest(data=data), self.assertRaises(producer.Rejected):
                producer.review_receipt(data, {"tag": "v2.0.0", "reviewSha256": hashlib.sha256(data).hexdigest()})

    def test_unrelated_review_bytes_cannot_satisfy_declared_hash(self):
        with self.assertRaises(producer.Rejected):
            producer.review_receipt(self.review(), {"tag": "v2.0.0", "reviewSha256": "f" * 64})


class NativeEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.manifest_sha = "b" * 64
        self.manifest = {"disk": {"sha256": "c" * 64}, "corral": {"assets": {"darwin-arm64": {"sha256": "d" * 64}}},
            "producer": {"commit": "a" * 40, "runId": "123"}}
        (self.root / "console.png").write_bytes(b"test evidence bytes, not a real native qualification")
        self.receipt = {"schema": 1, "platform": "darwin-arm64", "manifestSha256": self.manifest_sha,
            "diskSha256": "c" * 64, "corralSha256": "d" * 64, "producerRunId": "123", "producerCommit": "a" * 40,
            "host": {"system": "Darwin", "machine": "arm64", "qemuVersion": "QEMU test parser receipt",
                "qemuSha256": "e" * 64, "guestArchitecture": "amd64", "accelerator": "tcg", "firmwareCodeSha256": "f" * 64},
            "checks": dict.fromkeys(producer.QUALIFICATION_CHECKS, True),
            "files": {"console.png": producer.sha256(self.root / "console.png")}}

    def admit(self):
        (self.root / "qualification.json").write_bytes(producer.canonical(self.receipt))
        return producer.native_receipt(self.root, "darwin-arm64", self.manifest, self.manifest_sha)

    def test_exact_receipt_and_evidence_bytes_admitted(self):
        self.assertEqual(self.admit()["platform"], "darwin-arm64")

    def test_manifest_disk_corral_source_run_bindings_enforced(self):
        for key in ("manifestSha256", "diskSha256", "corralSha256", "producerCommit", "producerRunId", "platform"):
            original = self.receipt[key]
            self.receipt[key] = "not-the-qualified-identity"
            with self.subTest(key=key), self.assertRaises(producer.Rejected):
                self.admit()
            self.receipt[key] = original

    def test_every_native_acceptance_check_is_mandatory_and_boolean(self):
        for check in producer.QUALIFICATION_CHECKS:
            for bad in (False, 1, "true", None):
                self.receipt["checks"][check] = bad
                with self.subTest(check=check, bad=bad), self.assertRaises(producer.Rejected):
                    self.admit()
            del self.receipt["checks"][check]
            with self.assertRaises(producer.Rejected):
                self.admit()
            self.receipt["checks"][check] = True

    def test_apple_silicon_cannot_use_native_arm_guest_or_hvf(self):
        for key, value in (("guestArchitecture", "arm64"), ("accelerator", "hvf"), ("machine", "x86_64"), ("system", "Linux")):
            original = self.receipt["host"][key]
            self.receipt["host"][key] = value
            with self.subTest(key=key), self.assertRaises(producer.Rejected):
                self.admit()
            self.receipt["host"][key] = original

    def test_four_native_host_matrix_preserves_same_amd64_disk(self):
        hosts = (
            ("linux-amd64", "Linux", "x86_64", "kvm"),
            ("linux-arm64", "Linux", "aarch64", "tcg"),
            ("darwin-amd64", "Darwin", "x86_64", "hvf"),
            ("darwin-arm64", "Darwin", "arm64", "tcg"),
        )
        for platform, system, machine, accelerator in hosts:
            self.manifest["corral"]["assets"][platform] = {"sha256": "d" * 64}
            self.receipt["platform"] = platform
            self.receipt["host"].update(system=system, machine=machine, accelerator=accelerator)
            (self.root / "qualification.json").write_bytes(producer.canonical(self.receipt))
            with self.subTest(platform=platform):
                result = producer.native_receipt(self.root, platform, self.manifest, self.manifest_sha)
                self.assertEqual(result["host"]["guestArchitecture"], "amd64")
                self.assertEqual(result["diskSha256"], self.manifest["disk"]["sha256"])

    def test_both_arm_hosts_require_x86_tcg_and_native_arm_machine(self):
        for platform, system in (("linux-arm64", "Linux"), ("darwin-arm64", "Darwin")):
            self.manifest["corral"]["assets"][platform] = {"sha256": "d" * 64}
            self.receipt["platform"] = platform
            self.receipt["host"].update(system=system, machine="aarch64", accelerator="tcg")
            for key, value in (("accelerator", "kvm"), ("accelerator", "hvf"),
                               ("guestArchitecture", "arm64"), ("machine", "x86_64"),
                               ("system", "Darwin" if system == "Linux" else "Linux")):
                original = self.receipt["host"][key]
                self.receipt["host"][key] = value
                (self.root / "qualification.json").write_bytes(producer.canonical(self.receipt))
                with self.subTest(platform=platform, key=key, value=value), self.assertRaises(producer.Rejected):
                    producer.native_receipt(self.root, platform, self.manifest, self.manifest_sha)
                self.receipt["host"][key] = original

    def test_changed_or_unindexed_evidence_rejected(self):
        (self.root / "console.png").write_bytes(b"changed")
        with self.assertRaises(producer.Rejected):
            self.admit()
        self.receipt["files"]["console.png"] = producer.sha256(self.root / "console.png")
        (self.root / "unindexed.txt").write_text("not indexed")
        with self.assertRaises(producer.Rejected):
            self.admit()

    def test_traversal_and_symlink_evidence_rejected(self):
        for name in ("../outside", "/outside", "a\\b", "./console.png"):
            with self.subTest(name=name), self.assertRaises(producer.Rejected):
                producer.evidence_files(self.root, {name: "a" * 64})
        (self.root / "alias").symlink_to(self.root / "console.png")
        with self.assertRaises(producer.Rejected):
            producer.evidence_files(self.root, {"alias": self.receipt["files"]["console.png"]})
        (self.root / "alias-dir").symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(producer.Rejected):
            producer.evidence_files(self.root, {"alias-dir/console.png": self.receipt["files"]["console.png"]})


class BundleTests(unittest.TestCase):
    def test_missing_full_transparency_material_rejected_before_verifier(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "corral"
            path.write_bytes(b"unsigned")
            for bundle in ({}, {"mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json", "verificationMaterial": {"tlogEntries": []}},
                           {"mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json", "verificationMaterial": {"tlogEntries": [{"inclusionPromise": {"signedEntryTimestamp": "not a proof"}}]}}):
                Path(str(path) + ".sigstore.json").write_bytes(producer.canonical(bundle))
                with patch.object(producer, "run") as verifier, self.assertRaises(producer.Rejected):
                    producer.verify_blob(path, producer.IDENTITY)
                verifier.assert_not_called()

    def test_crypto_failure_never_falls_back_to_unsigned_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "corral"
            path.write_bytes(b"synthetic untrusted parser fixture")
            bundle = {"mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
                "verificationMaterial": {"tlogEntries": [{"inclusionProof": {"checkpoint": {"envelope": "not cryptographic proof"}}}]}}
            Path(str(path) + ".sigstore.json").write_bytes(producer.canonical(bundle))
            failure = subprocess.CalledProcessError(1, ["cosign"])
            with patch.object(producer, "run", side_effect=failure), self.assertRaises(subprocess.CalledProcessError):
                producer.verify_blob(path, producer.IDENTITY)


if __name__ == "__main__":
    unittest.main()
