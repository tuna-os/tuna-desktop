#!/usr/bin/env python3
"""Consumer unit/OS-boundary tests, not VM, signature, or native qualification.

Synthetic descriptors and short Python child processes exercise input admission,
filesystem ownership, event framing, and cleanup. Nothing here impersonates Corral
or produces a successful native qualification receipt. The actual signed-artifact
Linux/macOS qualifier remains authoritative.
"""
import copy
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest import mock

import try_tuna_desktop as launcher


def manifest_fixture(run=10):
    """Schema-shaped test data only; these are not production release pins."""
    commit, digest, tag = "a" * 40, "b" * 64, "v1.2.3"
    package_url = "https://repo.tunaos.org/pacman/arch/x86_64/tuna-desktop-1.2.3-1-x86_64.pkg.tar.zst"
    corral_tag = "v1.0.0"
    assets = {}
    for host in launcher.PLATFORMS:
        name = "corral-" + host
        url = launcher.release_url("tuna-os/corral", corral_tag, name)
        assets[host] = {"name": name, "sha256": digest, "url": url, "bundleUrl": url + ".sigstore.json"}
    return {
        "schema": launcher.SCHEMA,
        "release": {"repository": "tuna-os/tuna-desktop", "tag": tag, "commit": commit,
                    "version": tag[1:], "ciRunId": run, "installedControlRunId": run,
                    "reviewPath": "docs/reviews/" + tag + ".md", "reviewSha256": digest},
        "package": {"name": "tuna-desktop", "architecture": "x86_64", "sourceCommit": commit,
                    "sha256": digest, "factoryRunId": run, "version": "1.2.3-1",
                    "signerFingerprint": "4E5CC9F8B3B521793D95266E629BE6EA45188366",
                    "url": package_url, "signatureUrl": package_url + ".sig"},
        "image": {"repository": "ghcr.io/tuna-os/marlin", "architecture": "amd64",
                  "certificateIdentity": launcher.IMAGE_IDENTITY, "indexDigest": "sha256:" + digest,
                  "manifestDigest": "sha256:" + digest, "sourceCommit": commit,
                  "buildRunId": run, "qualificationRunId": run},
        "disk": {"name": "tuna-desktop-amd64.qcow2", "architecture": "amd64", "firmware": "uefi",
                 "url": launcher.release_url("tuna-os/tuna-desktop", tag, "tuna-desktop-amd64.qcow2"),
                 "sha256": digest, "size": 1024, "virtualSize": 1024 ** 3},
        "corral": {"repository": "tuna-os/corral", "tag": corral_tag, "commit": commit,
                   "certificateIdentity": "https://github.com/tuna-os/corral/.github/workflows/release.yml@refs/tags/" + corral_tag,
                   "assets": assets},
        "producer": {"repository": "tuna-os/tuna-desktop", "workflow": ".github/workflows/vm-release.yml",
                     "commit": commit, "runId": run, "runAttempt": run},
        "qualification": {"producerBootRunId": run, "producerEvidenceSha256": digest},
    }


class ManifestTests(unittest.TestCase):
    def test_positive_run_identity_and_four_host_contract(self):
        for value in (1, 12345678901234567890, "1", "12345678901234567890"):
            with self.subTest(value=value):
                self.assertTrue(launcher.run_identity(value))
                manifest = manifest_fixture(value)
                self.assertIs(launcher.validate_manifest(manifest), manifest)
        self.assertEqual(set(manifest_fixture()["corral"]["assets"]), set(launcher.PLATFORMS))

    def test_noncanonical_run_identities_are_rejected(self):
        for value in (True, False, 0, -1, 1.0, None, [], {}, "", "01", "0", "+1", "-1", "1.0", " 1", "1\n", "١"):
            with self.subTest(value=value):
                self.assertFalse(launcher.run_identity(value))
                manifest = manifest_fixture(value)
                with self.assertRaises(ValueError):
                    launcher.validate_manifest(manifest)

    def test_malformed_sections_are_controlled_rejections(self):
        baseline = manifest_fixture()
        for section in baseline:
            if section == "schema":
                continue
            for value in (None, [], "object", 1, True):
                with self.subTest(section=section, value=value):
                    manifest = copy.deepcopy(baseline)
                    manifest[section] = value
                    with self.assertRaises(ValueError):
                        launcher.validate_manifest(manifest)
            manifest = copy.deepcopy(baseline)
            del manifest[section]
            with self.assertRaises(ValueError):
                launcher.validate_manifest(manifest)
        for value in (None, [], "manifest", True):
            with self.assertRaises(ValueError):
                launcher.validate_manifest(value)

    def test_required_fields_reject_wrong_types_and_missing_members(self):
        baseline = manifest_fixture()
        for section, fields in baseline.items():
            if not isinstance(fields, dict):
                continue
            for name, original in fields.items():
                for value in (None, [], {}, True):
                    with self.subTest(section=section, field=name, value=value):
                        manifest = copy.deepcopy(baseline)
                        manifest[section][name] = value
                        with self.assertRaises(ValueError):
                            launcher.validate_manifest(manifest)
                manifest = copy.deepcopy(baseline)
                del manifest[section][name]
                with self.subTest(section=section, missing=name), self.assertRaises(ValueError):
                    launcher.validate_manifest(manifest)

    def test_asset_maps_and_signed_identity_cannot_be_redirected(self):
        mutations = [
            ("release", "repository", "other/desktop"),
            ("release", "tag", "../../latest"),
            ("package", "url", "https://example.invalid/package"),
            ("package", "signatureUrl", "https://example.invalid/package.sig"),
            ("package", "signerFingerprint", "c" * 40),
            ("image", "certificateIdentity", "https://example.invalid/signer"),
            ("image", "architecture", "arm64"),
            ("disk", "architecture", "arm64"),
            ("disk", "firmware", "bios"),
            ("disk", "url", "file:///tmp/disk.qcow2"),
            ("disk", "name", "../tuna-desktop-amd64.qcow2"),
            ("corral", "certificateIdentity", launcher.MANIFEST_IDENTITY),
            ("producer", "workflow", ".github/workflows/other.yml"),
        ]
        for section, key, value in mutations:
            with self.subTest(section=section, key=key):
                manifest = manifest_fixture()
                manifest[section][key] = value
                with self.assertRaises(ValueError):
                    launcher.validate_manifest(manifest)
        for assets in ([], ["linux-amd64"], {}, {"linux-amd64": {}}, dict.fromkeys(launcher.PLATFORMS, [])):
            manifest = manifest_fixture()
            manifest["corral"]["assets"] = assets
            with self.subTest(assets=assets), self.assertRaises(ValueError):
                launcher.validate_manifest(manifest)
        manifest = manifest_fixture()
        manifest["corral"]["assets"]["windows-amd64"] = {}
        with self.assertRaises(ValueError):
            launcher.validate_manifest(manifest)
        for key, value in (("name", "../corral-linux-amd64"), ("url", "https://example.invalid/corral"),
                           ("bundleUrl", "https://example.invalid/key"), ("sha256", "A" * 64)):
            manifest = manifest_fixture()
            manifest["corral"]["assets"]["linux-amd64"][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                launcher.validate_manifest(manifest)

    def test_json_duplicates_are_rejected_at_every_depth(self):
        for payload in ('{"schema":"first","schema":"second"}',
                        '{"disk":{"sha256":"first","sha256":"second"}}',
                        '{"corral":{"assets":{"linux-amd64":{},"linux-amd64":{}}}}'):
            with self.subTest(payload=payload), self.assertRaisesRegex(ValueError, "duplicate JSON"):
                json.loads(payload, object_pairs_hook=launcher.no_duplicates)


class TemporaryFilesystemCase(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="tuna-consumer-tests-")
        self.addCleanup(self.temporary.cleanup)
        # Resolve the OS-provided temporary root (e.g. macOS /var -> /private/var).
        self.root = Path(self.temporary.name).resolve()
        cancel = mock.patch.object(launcher, "CANCELLED", False)
        cancel.start()
        self.addCleanup(cancel.stop)


class PackageTransactionTests(TemporaryFilesystemCase):
    def test_active_owned_transaction_finishes_after_cancellation_without_starting_another(self):
        started, cancelled, finished = [self.root / name for name in ("started", "cancelled", "finished")]
        script = ("import pathlib, sys, time\n"
                  "started, cancelled, finished = map(pathlib.Path, sys.argv[1:])\n"
                  "started.write_text('started')\n"
                  "deadline = time.monotonic() + 10\n"
                  "while not cancelled.exists():\n"
                  "    if time.monotonic() >= deadline: raise SystemExit(19)\n"
                  "    time.sleep(.01)\n"
                  "finished.write_text('finished after cancellation')\n")
        errors = []
        def cancel_after_start():
            import time
            deadline = time.monotonic() + 10
            while not started.exists() and time.monotonic() < deadline:
                time.sleep(.01)
            if not started.exists():
                errors.append("actual child never started")
                return
            launcher.CANCELLED = True
            cancelled.write_text("cancelled")
        controller = threading.Thread(target=cancel_after_start)
        controller.start()
        try:
            launcher.package_transaction([sys.executable, "-c", script, started, cancelled, finished])
        finally:
            controller.join(timeout=11)
        self.assertFalse(controller.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(finished.read_text(), "finished after cancellation")
        never_started = self.root / "never-started"
        with self.assertRaisesRegex(ValueError, "cancelled before native package"):
            launcher.package_transaction([sys.executable, "-c", "import pathlib,sys; pathlib.Path(sys.argv[1]).touch()", never_started])
        self.assertFalse(never_started.exists())


class HostBinaryTests(TemporaryFilesystemCase):
    def binary(self, host):
        # Header parser fixtures only; these are never executed as VM proof.
        header = bytearray(32)
        if host.startswith("linux-"):
            header[:6] = b"\x7fELF\x02\x01"
            header[18:20] = (62 if host.endswith("-amd64") else 183).to_bytes(2, "little")
        else:
            header[:4] = b"\xcf\xfa\xed\xfe"
            header[4:8] = (0x01000007 if host.endswith("-amd64") else 0x0100000C).to_bytes(4, "little")
        path = self.root / host
        path.write_bytes(header)
        return path

    def test_each_declared_host_rejects_foreign_os_and_cpu_even_if_translatable(self):
        for actual in launcher.PLATFORMS:
            path = self.binary(actual)
            for expected in launcher.PLATFORMS:
                with self.subTest(actual=actual, expected=expected):
                    if actual == expected:
                        launcher.verify_host_binary(path, expected)
                    else:
                        with self.assertRaises(ValueError):
                            launcher.verify_host_binary(path, expected)

    def test_truncated_foreign_and_nonregular_headers_fail_closed(self):
        path = self.root / "bad"
        for payload in (b"", b"\x7fELF", b"MZ" + bytes(30), b"\xca\xfe\xba\xbe" + bytes(28)):
            path.write_bytes(payload)
            for host in launcher.PLATFORMS:
                with self.subTest(payload=payload, host=host), self.assertRaises(ValueError):
                    launcher.verify_host_binary(path, host)
        link = self.root / "link"
        link.symlink_to(self.binary("linux-amd64"))
        with self.assertRaises(OSError):
            launcher.verify_host_binary(link, "linux-amd64")
        fifo = self.root / "fifo"
        os.mkfifo(fifo)
        with self.assertRaises(ValueError):
            launcher.verify_host_binary(fifo, "linux-amd64")

    def test_real_query_exit_status_distinguishes_absence_from_failure(self):
        self.assertFalse(launcher.package_present([sys.executable, "-c", "raise SystemExit(1)"]))
        with self.assertRaises(launcher.CommandFailure) as failure:
            launcher.package_present([sys.executable, "-c", "raise SystemExit(17)"])
        self.assertEqual(failure.exception.returncode, 17)


class FilesystemTests(TemporaryFilesystemCase):
    def setUp(self):
        super().setUp()
        self.offline = self.root / "offline"
        self.offline.mkdir(mode=0o700)
        self.payload = b"exact declared release bytes\x00\xff\n"
        self.name = "tuna-desktop-amd64.qcow2"
        (self.offline / self.name).write_bytes(self.payload)
        self.url = launcher.release_url("tuna-os/tuna-desktop", "v1.2.3", self.name)
        self.digest = hashlib.sha256(self.payload).hexdigest()
        self.destination = self.root / "cache" / self.digest / self.name

    def acquire(self, **options):
        defaults = {"expected": self.digest, "size": len(self.payload), "artifact_dir": self.offline}
        defaults.update(options)
        return launcher.acquire(self.url, self.destination, len(self.payload), **defaults)

    def test_offline_acquisition_uses_exact_name_and_bytes_without_network(self):
        with mock.patch.object(launcher, "urlopen", side_effect=AssertionError("offline input used network")):
            self.assertEqual(self.acquire(), self.destination)
            self.assertEqual(self.destination.read_bytes(), self.payload)
            self.assertEqual(self.destination.stat().st_mode & 0o777, 0o600)
            self.assertEqual(self.acquire(), self.destination)

    def test_offline_missing_declared_name_does_not_accept_alternate(self):
        (self.offline / self.name).rename(self.offline / "alternate.qcow2")
        with self.assertRaises(FileNotFoundError):
            self.acquire()
        self.assertFalse(self.destination.exists())

    def test_offline_wrong_bytes_size_and_bound_leave_no_partial_cache(self):
        for payload, size in ((b"x" * len(self.payload), len(self.payload)),
                              (self.payload[:-1], len(self.payload)),
                              (self.payload + b"extra", len(self.payload)),
                              (self.payload, len(self.payload) - 1)):
            with self.subTest(payload=payload, size=size):
                (self.offline / self.name).write_bytes(payload)
                with self.assertRaises(ValueError):
                    self.acquire(size=size)
                self.assertFalse(self.destination.exists())
                self.assertEqual(list(self.destination.parent.iterdir()), [])

    def test_unsafe_offline_directory_is_rejected_before_dependency_acquisition(self):
        link = self.root / "offline-link"
        link.symlink_to(self.offline, target_is_directory=True)
        with mock.patch.object(launcher, "ensure_dependencies") as dependencies:
            with self.assertRaises(OSError):
                launcher.admit(self.root / "missing-manifest", self.root / "missing-bundle", self.root / "state", link)
            dependencies.assert_not_called()

    def test_cancellation_is_honored_on_cache_hits(self):
        self.acquire()
        with mock.patch.object(launcher, "CANCELLED", True):
            with self.assertRaisesRegex(ValueError, "cancelled"):
                self.acquire()
        self.assertEqual(self.destination.read_bytes(), self.payload)

    def test_unsafe_offline_directory_is_rejected_even_for_matching_cache(self):
        self.acquire()
        link = self.root / "offline-link"
        link.symlink_to(self.offline, target_is_directory=True)
        ancestor = self.root / "ancestor-link"
        ancestor.symlink_to(self.root, target_is_directory=True)
        not_directory = self.root / "file"
        not_directory.write_bytes(b"file")
        writable = self.root / "public"
        writable.mkdir(mode=0o700)
        writable.chmod(0o777)
        for root in (link, ancestor / "offline", not_directory, self.root / "missing", writable):
            with self.subTest(root=root), self.assertRaises((ValueError, OSError)):
                self.acquire(artifact_dir=root)
        self.assertEqual(self.destination.read_bytes(), self.payload)

    def test_offline_file_symlink_fifo_and_directory_are_rejected(self):
        source = self.offline / self.name
        source.unlink()
        target = self.root / "outside"
        target.write_bytes(self.payload)
        source.symlink_to(target)
        with self.assertRaises(OSError):
            self.acquire()
        source.unlink()
        source.mkdir()
        with self.assertRaises(ValueError):
            self.acquire()
        source.rmdir()
        os.mkfifo(source)
        with self.assertRaises(ValueError):
            self.acquire()
        self.assertFalse(self.destination.exists())

    def test_url_filename_traversal_is_rejected(self):
        for name in ("..", ".", "%2e%2e", "a%2fb", "disk%00.qcow2"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                launcher.acquire("https://github.com/assets/" + name, self.destination, 1024,
                                 expected=self.digest, artifact_dir=self.offline)
        for name in ("../disk", "/disk", "a/b", "a\\b", "", "x" * 129):
            with self.subTest(name=name), self.assertRaises(ValueError):
                launcher.filename(name)

    def test_cache_symlinks_and_oversize_entries_are_not_trusted(self):
        launcher.private_directory(self.destination.parent)
        outside = self.root / "outside"
        outside.write_bytes(self.payload)
        self.destination.symlink_to(outside)
        with self.assertRaises(ValueError):
            self.acquire()
        self.assertEqual(outside.read_bytes(), self.payload)
        self.destination.unlink()
        self.destination.write_bytes(self.payload + b"extra")
        with self.assertRaises(ValueError):
            self.acquire()
        self.destination.unlink()
        self.destination.symlink_to(self.root / "missing")
        with self.assertRaises(ValueError):
            self.acquire()

    def test_private_directory_and_bounded_regular_file_boundaries(self):
        directory = launcher.private_directory(self.root / "private")
        path = directory / "payload"
        path.write_bytes(b"abcd")
        self.assertEqual(launcher.safe_file(path, 4), b"abcd")
        with self.assertRaises(ValueError):
            launcher.safe_file(path, 3)
        with self.assertRaises(ValueError):
            launcher.sha256(path, 3)
        link = directory / "link"
        link.symlink_to(path)
        with self.assertRaises(OSError):
            launcher.safe_file(link, 4)
        with self.assertRaises(OSError):
            launcher.sha256(link)
        fifo = directory / "fifo"
        os.mkfifo(fifo)
        with self.assertRaises(ValueError):
            launcher.safe_file(fifo, 4)
        directory.chmod(0o755)
        with self.assertRaises(ValueError):
            launcher.private_directory(directory)
        link_directory = self.root / "directory-link"
        link_directory.symlink_to(self.offline, target_is_directory=True)
        with self.assertRaises(ValueError):
            launcher.private_directory(link_directory)

    def test_directory_descriptor_does_not_follow_replaced_offline_path(self):
        fd = launcher.offline_directory(self.offline)
        try:
            retained = self.root / "retained"
            self.offline.rename(retained)
            self.offline.mkdir(mode=0o700)
            (self.offline / self.name).write_bytes(b"different bytes")
            self.assertEqual(launcher.safe_file(self.name, 1024, dir_fd=fd), self.payload)
        finally:
            os.close(fd)


class OwnershipTests(TemporaryFilesystemCase):
    def owner(self):
        home = self.root / "home"
        name = "try-tuna-" + "a" * 20
        directory = home / ".local/share/corral/vms" / name
        directory.mkdir(parents=True, mode=0o700)
        info = directory.stat()
        instance = object.__new__(launcher.Launch)
        instance.manifest_sha = "b" * 64
        instance.state = launcher.private_directory(self.root / "state")
        instance.record = {"name": name, "directory": str(directory), "directoryIdentity": [info.st_dev, info.st_ino],
                           "manifestSha256": instance.manifest_sha, "nonce": "c" * 32, "created": True}
        instance.browser = instance.console = instance.endpoint = None
        instance.started = False
        instance.receipt = {"cleanupComplete": False}
        launcher.atomic_json(directory / ".try-tuna-owner.json", instance.record)
        patch = mock.patch.object(launcher.Path, "home", return_value=home)
        patch.start()
        self.addCleanup(patch.stop)
        return instance, directory

    def test_owned_record_binds_marker_manifest_device_and_inode(self):
        instance, directory = self.owner()
        self.assertEqual(instance.owned(), directory)
        original = copy.deepcopy(instance.record)
        for field, value in (("manifestSha256", "d" * 64), ("directoryIdentity", [0, 0]),
                             ("directoryIdentity", [True, directory.stat().st_ino]),
                             ("nonce", "bad"), ("created", 1), ("name", "foreign"),
                             ("directory", str(directory / ".."))):
            instance.record = dict(original, **{field: value})
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                instance.owned()
        instance.record = original
        marker = directory / ".try-tuna-owner.json"
        launcher.atomic_json(marker, dict(original, nonce="d" * 32))
        with self.assertRaisesRegex(ValueError, "marker differs"):
            instance.owned()

    def test_replaced_directory_inode_and_symlink_are_not_owned(self):
        instance, directory = self.owner()
        retained = directory.with_name("retained")
        directory.rename(retained)
        directory.mkdir(mode=0o700)
        launcher.atomic_json(directory / ".try-tuna-owner.json", instance.record)
        with self.assertRaisesRegex(ValueError, "directory changed"):
            instance.owned()
        (directory / ".try-tuna-owner.json").unlink()
        directory.rmdir()
        directory.symlink_to(retained, target_is_directory=True)
        with self.assertRaises(OSError):
            instance.owned()

    def test_owner_marker_duplicates_and_symlink_are_rejected(self):
        instance, directory = self.owner()
        marker = directory / ".try-tuna-owner.json"
        marker.write_text('{"name":"first","name":"second"}')
        with self.assertRaisesRegex(ValueError, "duplicate JSON"):
            instance.owned()
        marker.unlink()
        outside = self.root / "marker"
        launcher.atomic_json(outside, instance.record)
        marker.symlink_to(outside)
        with self.assertRaises(OSError):
            instance.owned()

    def test_directory_owner_mismatch_is_not_owned(self):
        instance, _ = self.owner()
        uid = os.getuid()
        with mock.patch.object(launcher.os, "getuid", return_value=uid + 1):
            with self.assertRaisesRegex(ValueError, "must be owned"):
                instance.owned()

    def test_cleanup_never_stops_an_inode_mismatched_guest(self):
        instance, directory = self.owner()
        instance.started = True
        instance.record["directoryIdentity"][1] += 1
        with mock.patch.object(instance, "command") as command:
            with self.assertRaisesRegex(RuntimeError, "owned cleanup failed"):
                instance.cleanup()
            command.assert_not_called()
        receipt = json.loads((instance.state / "receipt.json").read_bytes())
        self.assertFalse(receipt["cleanupComplete"])
        self.assertTrue(directory.is_dir())
        self.assertEqual(receipt["cleanupErrors"], ["owned VM directory changed"])

    def test_current_record_symlink_and_nonobject_are_rejected_before_create(self):
        instance, _ = self.owner()
        current = instance.state / "current.json"
        current.write_text("[]")
        with mock.patch.object(instance, "command") as command:
            with self.assertRaisesRegex(ValueError, "must be an object"):
                instance.prepare()
            command.assert_not_called()
        current.unlink()
        current.symlink_to(self.root / "missing")
        with self.assertRaisesRegex(ValueError, "must not be a symlink"):
            instance.prepare()


class PlatformAndTrustTests(unittest.TestCase):
    def test_four_platforms_and_architecture_aliases(self):
        for system, machine, expected in (("Linux", "x86_64", "linux-amd64"), ("Linux", "amd64", "linux-amd64"),
                                          ("Linux", "aarch64", "linux-arm64"), ("Darwin", "x86_64", "darwin-amd64"),
                                          ("Darwin", "arm64", "darwin-arm64")):
            with self.subTest(system=system, machine=machine), mock.patch.object(launcher.platform, "system", return_value=system), mock.patch.object(launcher.platform, "machine", return_value=machine):
                self.assertEqual(launcher.host_platform(), expected)
        for system, machine in (("Windows", "x86_64"), ("Linux", "riscv64"), ("Darwin", "i386")):
            with mock.patch.object(launcher.platform, "system", return_value=system), mock.patch.object(launcher.platform, "machine", return_value=machine), self.assertRaises(ValueError):
                launcher.host_platform()

    def test_arm_hosts_require_explicit_tcg_for_amd64_guest(self):
        runtime = {"architecture": "amd64", "firmware": "uefi", "tailscale_ip": "127.0.0.1", "vnc_port": 5901, "acceleration": "tcg"}
        for host in launcher.PLATFORMS:
            launcher.validate_runtime(runtime, "amd64", host)
        for host in ("linux-arm64", "darwin-arm64"):
            for acceleration in (None, "", "hvf", "kvm"):
                changed = dict(runtime, acceleration=acceleration)
                with self.subTest(host=host, acceleration=acceleration), self.assertRaisesRegex(ValueError, "TCG"):
                    launcher.validate_runtime(changed, "amd64", host)
        for key, value in (("architecture", "arm64"), ("firmware", "bios"), ("tailscale_ip", "0.0.0.0"),
                           ("vnc_port", True), ("vnc_port", 0), ("vnc_port", 65536), ("vnc_port", "5901")):
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                launcher.validate_runtime(dict(runtime, **{key: value}), "amd64", "linux-amd64")

    def test_verifier_and_corral_trust_overrides_are_removed(self):
        untrusted = {"COSIGN_REPOSITORY": "attacker", "COSIGN_EXPERIMENTAL": "1", "SIGSTORE_ROOT_FILE": "/evil/root",
                     "CORRAL_BACKEND": "remote", "CORRAL_CONTEXT": "foreign", "LD_PRELOAD": "/evil/library",
                     "DYLD_INSERT_LIBRARIES": "/evil/library", "SSL_CERT_FILE": "/evil/ca", "SSL_CERT_DIR": "/evil/cas",
                     "REQUESTS_CA_BUNDLE": "/evil/ca", "CURL_CA_BUNDLE": "/evil/ca", "WAYLAND_DEBUG": "1",
                     "PWDEBUG": "1", "DEBUG": "pw:api"}
        with mock.patch.dict(os.environ, dict(untrusted, HOME="/safe/home", PATH="/safe/bin", DISPLAY=":1"), clear=True):
            self.assertEqual(launcher.trusted_environment(), {"HOME": "/safe/home", "PATH": "/safe/bin", "DISPLAY": ":1"})
            self.assertEqual(os.environ["SIGSTORE_ROOT_FILE"], "/evil/root")


class ProcessAndConsoleTests(unittest.TestCase):
    def setUp(self):
        patch = mock.patch.object(launcher, "CANCELLED", False)
        patch.start()
        self.addCleanup(patch.stop)

    def child(self, code):
        process = subprocess.Popen([sys.executable, "-c", code], stdout=subprocess.PIPE,
                                   stderr=subprocess.DEVNULL, start_new_session=True)
        def finish():
            launcher.terminate_child(process, group=True, timeout=1)
            process.stdout.close()
        self.addCleanup(finish)
        return process

    def test_event_reader_consumes_actual_pipe_one_record_at_a_time(self):
        process = self.child('import sys,time; print(\'{"vm":"owned"}\', flush=True); print(\'{"event":"rfb-connected"}\', flush=True); time.sleep(60)')
        self.assertEqual(launcher.read_event(process, 5), {"vm": "owned"})
        self.assertEqual(launcher.read_event(process, 5), {"event": "rfb-connected"})

    def test_event_reader_rejects_duplicates_shapes_oversize_eof_and_cancel(self):
        for payload in ('{"event":"a","event":"b"}\n', '[]\n', '"event"\n', 'x' * 65537):
            with self.subTest(payload=payload[:80]):
                process = self.child("import sys,time; sys.stdout.write(" + repr(payload) + "); sys.stdout.flush(); time.sleep(60)")
                with self.assertRaises(ValueError):
                    launcher.read_event(process, 5)
                launcher.terminate_child(process, group=True, timeout=1)
        process = self.child("pass")
        with self.assertRaisesRegex(ValueError, "exited"):
            launcher.read_event(process, 5)
        process = self.child("import time; time.sleep(60)")
        with self.assertRaises(TimeoutError):
            launcher.read_event(process, .05)
        with mock.patch.object(launcher, "CANCELLED", True), self.assertRaisesRegex(ValueError, "cancelled"):
            launcher.read_event(process, 5)

    def test_browser_handshake_rejects_foreign_event_without_receipting_success(self):
        with tempfile.TemporaryDirectory(prefix="tuna-handshake-boundary-") as temporary:
            instance = object.__new__(launcher.Launch)
            instance.state = Path(temporary).resolve()
            instance.record = {"name": "owned"}
            instance.receipt = {"browserRfbConnected": False}
            for event in ({"event": "rfb-connected", "vm": "foreign", "namespace": "local"},
                          {"event": "rfb-connected", "vm": "owned", "namespace": "remote"},
                          {"event": "listening", "vm": "owned", "namespace": "local"}):
                instance.console = self.child("import time; print(" + repr(json.dumps(event)) + ", flush=True); time.sleep(60)")
                with self.subTest(event=event), self.assertRaisesRegex(ValueError, "handshake identity"):
                    instance.browser_ready(None)
                self.assertFalse(instance.receipt["browserRfbConnected"])
                self.assertFalse((instance.state / "receipt.json").exists())
                launcher.terminate_child(instance.console, group=True, timeout=1)

    def test_actual_browser_wait_accepts_owned_stop_without_receipting_rfb_success(self):
        with tempfile.TemporaryDirectory(prefix="tuna-stop-readiness-", dir="/tmp") as temporary:
            state = Path(temporary).resolve()
            instance = object.__new__(launcher.Launch)
            instance.state = state
            instance.record = {"name": "owned"}
            instance.receipt = {"browserRfbConnected": False}
            instance.console = self.child("import time; time.sleep(2)")
            with socket.socket(socket.AF_UNIX) as listener, socket.socket(socket.AF_UNIX) as caller:
                listener.bind(str(state / "control.sock"))
                listener.listen(1)
                caller.settimeout(5)
                caller.connect(str(state / "control.sock"))
                caller.sendall(b"stop\n")
                with self.assertRaises(launcher._StopRequested) as stopped:
                    instance.browser_ready(listener)
                # The production browser waiter transfers this exact live OS
                # connection to launch's cleanup owner; it must not ack cleanup.
                with stopped.exception.client as accepted:
                    accepted.sendall(b"accepted\n")
                    self.assertEqual(caller.recv(16), b"accepted\n")
                self.assertFalse(instance.receipt["browserRfbConnected"])
                self.assertFalse((state / "receipt.json").exists())

    def test_console_endpoint_requires_owned_loopback_and_readiness_nonce(self):
        name = "try-tuna-" + "a" * 20
        base = "http://127.0.0.1:12345/console/local/" + name
        endpoint = {"vm": name, "namespace": "local", "url": base + "?ready_nonce=" + "A" * 43,
                    "websocket": "ws://127.0.0.1:12345/api/vnc/local/" + name}
        self.assertEqual(launcher.console_endpoint(endpoint, name), endpoint["url"])
        for key, value in (("vm", "foreign"), ("namespace", "remote"), ("context", "foreign"),
                           ("websocket", "ws://127.0.0.1:12346/api/vnc/local/" + name)):
            with self.subTest(key=key), self.assertRaises(ValueError):
                launcher.console_endpoint(dict(endpoint, **{key: value}), name)
        for url in (base, base + "?ready_nonce=short", base + "?ready_nonce=" + "a" * 43 + "&ready_nonce=" + "b" * 43,
                    endpoint["url"] + "&context=foreign", endpoint["url"] + "#fragment",
                    endpoint["url"].replace("127.0.0.1", "localhost"), endpoint["url"].replace("127.0.0.1", "0.0.0.0"),
                    endpoint["url"].replace("127.0.0.1", "user@127.0.0.1"), endpoint["url"].replace("/local/", "/remote/"),
                    endpoint["url"].replace("http:", "https:"), endpoint["url"].replace(":12345", ":65536")):
            with self.subTest(url=url), self.assertRaises(ValueError):
                launcher.console_endpoint(dict(endpoint, url=url), name)

    def test_child_command_bounds_timeout_failure_and_precancellation(self):
        self.assertEqual(launcher.run([sys.executable, "-c", "print('output')"]), b"output\n")
        with self.assertRaisesRegex(RuntimeError, "failed.*7"):
            launcher.run([sys.executable, "-c", "import sys; print('failure',file=sys.stderr); sys.exit(7)"])
        with self.assertRaisesRegex(TimeoutError, "deadline"):
            launcher.run([sys.executable, "-c", "import time; time.sleep(60)"], timeout=.05)
        for stream in ("stdout", "stderr"):
            with self.subTest(stream=stream), self.assertRaisesRegex(ValueError, "output exceeds bound"):
                launcher.run([sys.executable, "-c", "import sys; sys." + stream + ".buffer.write(b'x'*(8*1024*1024+1))"])
        with mock.patch.object(launcher, "CANCELLED", True):
            with self.assertRaisesRegex(TimeoutError, "cancelled"):
                launcher.run([sys.executable, "-c", "raise SystemExit('should not execute')"])
            self.assertEqual(launcher.run([sys.executable, "-c", "print('cleanup')"], cancellable=False), b"cleanup\n")

    def test_cancellation_cleans_actual_child_and_preserves_unrelated_process(self):
        foreign = self.child("import time; time.sleep(60)")
        with tempfile.TemporaryDirectory(prefix="tuna-cancel-child-") as temporary:
            marker = Path(temporary) / "child.pid"
            observed, finished = threading.Event(), threading.Event()
            def cancel_running_child():
                while not finished.wait(.01):
                    if marker.exists() and marker.stat().st_size:
                        observed.set()
                        launcher.CANCELLED = True
                        return
            observer = threading.Thread(target=cancel_running_child)
            observer.start()
            code = ("import os,time; from pathlib import Path; Path(" + repr(str(marker))
                    + ").write_text(str(os.getpid())); time.sleep(60)")
            try:
                with self.assertRaisesRegex(TimeoutError, "cancelled"):
                    launcher.run([sys.executable, "-c", code], timeout=10)
            finally:
                finished.set()
                observer.join()
            self.assertTrue(observed.is_set(), "cancellation must follow actual child execution")
            with self.assertRaises(ProcessLookupError):
                os.kill(int(marker.read_text()), 0)
        self.assertIsNone(foreign.poll())

    def test_cleanup_handles_exited_opener_and_stops_console_without_touching_foreign_child(self):
        with tempfile.TemporaryDirectory(prefix="tuna-process-cleanup-") as temporary:
            instance = object.__new__(launcher.Launch)
            instance.state = Path(temporary).resolve()
            instance.browser = self.child("raise SystemExit(7)")
            instance.browser.wait(timeout=5)
            instance.console = self.child("import time; time.sleep(60)")
            instance.record = None
            instance.receipt = {}
            foreign = self.child("import time; time.sleep(60)")
            instance.cleanup()
            self.assertIsNotNone(instance.console.poll())
            self.assertIsNone(foreign.poll())
            self.assertTrue(json.loads((instance.state / "receipt.json").read_bytes())["cleanupComplete"])
            instance.cleanup()
            self.assertIsNone(foreign.poll())

    def test_console_cleanup_continues_after_browser_termination_error(self):
        with tempfile.TemporaryDirectory(prefix="tuna-process-error-") as temporary:
            instance = object.__new__(launcher.Launch)
            instance.state = Path(temporary).resolve()
            instance.browser = self.child("import time; time.sleep(60)")
            instance.console = self.child("import time; time.sleep(60)")
            instance.record = None
            instance.receipt = {}
            terminate = launcher.terminate_child
            def fail_browser(process, **options):
                if process is instance.browser:
                    raise OSError("browser signal denied")
                return terminate(process, **options)
            with mock.patch.object(launcher, "terminate_child", side_effect=fail_browser):
                with self.assertRaisesRegex(RuntimeError, "browser signal denied"):
                    instance.cleanup()
            self.assertIsNotNone(instance.console.poll())
            receipt = json.loads((instance.state / "receipt.json").read_bytes())
            self.assertFalse(receipt["cleanupComplete"])
            self.assertEqual(receipt["cleanupErrors"], ["browser: browser signal denied"])


if __name__ == "__main__":
    unittest.main()
