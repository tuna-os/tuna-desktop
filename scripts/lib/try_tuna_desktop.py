#!/usr/bin/env python3
"""Verified Tuna release-disk consumer; Corral owns VM and console execution.

Offline input changes only where exact declared release bytes are read. It does
not change release identities, verifier pins, signatures, or native qualification.
Cleanup signals only children started by this process and stops marker/inode-bound
Corral guests; a PID, process name, or VM name alone never grants ownership.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import selectors
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
from urllib.parse import parse_qsl, quote, urlsplit
from urllib.request import urlopen
import uuid

SCHEMA = "org.tunaos.desktop.vm/v1"
ISSUER = "https://token.actions.githubusercontent.com"
MANIFEST_IDENTITY = "https://github.com/tuna-os/tuna-desktop/.github/workflows/vm-release.yml@refs/heads/main"
IMAGE_IDENTITY = "https://github.com/tuna-os/tunaOS/.github/workflows/reusable-build-image.yml@refs/heads/main"
SHA = re.compile(r"[0-9a-f]{64}\Z")
COMMIT = re.compile(r"[0-9a-f]{40}\Z")
TAG = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-(?:beta|rc)\.[0-9]+)?\Z")
PLATFORMS = ("linux-amd64", "linux-arm64", "darwin-amd64", "darwin-arm64")
# Audited upstream v3.1.4 cosign_checksums.txt; this source pin bootstraps
# the verifier before any release manifest or Corral bytes are trusted.
COSIGN_SHA256 = {
    "linux-amd64": "424e2c97d0b45fd7a6b608a87f090f35633f4c23ece5f092d48bba11aaf4bbfa",
    "linux-arm64": "ade83b7aac62e131cacfe96d958948b2150cc9efec46a570a2649b8e27dedad2",
    "darwin-amd64": "9d1e4abe2b6be279ed0d2ff706fde1ccd738747c834e3e6d44039c238867334f",
    "darwin-arm64": "9f9b8cd557cf2e5b3df3373a00baa005a86b02391e3e06dd0ce37b3a1533d3a4",
}
CHECKS = ("signatureAdmission", "declaredGuestArchitecture", "nativeExecution",
          "browserRfbConnected", "pristineFirstbootAccountCreated", "pristineTunaSession",
          "pristineLogoutLogin", "adminWheelPasswordRequired", "rebootPersistence",
          "stopCleanup", "cancellationCleanup", "failureCleanup",
          "browserFailureCleanup", "ownershipIsolation", "diskLockReleased")
CANCELLED = False
PROOF_DEBUG_KEYS = ("WAYLAND_DEBUG", "PWDEBUG", "DEBUG")


class CommandFailure(RuntimeError):
    def __init__(self, command, returncode, stderr):
        self.returncode = returncode
        super().__init__(f"{command[0]} failed ({returncode}): " + stderr[-4096:].decode(errors="replace"))


def sha256(path, maximum=None):
    digest = hashlib.sha256()
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        require(stat.S_ISREG(info.st_mode) and (maximum is None or info.st_size <= maximum),
                "bounded regular artifact required for hashing")
        count = 0
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            count += len(chunk)
            require(maximum is None or count <= maximum, "artifact grew beyond hashing bound")
            digest.update(chunk)
    return digest.hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def positive(value):
    return type(value) is int and value > 0


def run_identity(value):
    return positive(value) or (isinstance(value, str) and re.fullmatch(r"[1-9][0-9]*", value) is not None)


def filename(value):
    require(isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}", value)
            and value not in (".", ".."), "unsafe artifact filename")
    return value


def release_url(repository, tag, name):
    return f"https://github.com/{repository}/releases/download/{quote(tag, safe='')}/{filename(name)}"


def validate_manifest(manifest):
    require(isinstance(manifest, dict) and manifest.get("schema") == SCHEMA, "unsupported Tuna VM manifest")
    fields = {
        "release": ("repository", "tag", "commit", "version", "reviewPath", "reviewSha256"),
        "package": ("name", "architecture", "sourceCommit", "sha256", "signerFingerprint", "version", "url", "signatureUrl"),
        "image": ("repository", "architecture", "certificateIdentity", "indexDigest", "manifestDigest", "sourceCommit"),
        "disk": ("name", "architecture", "firmware", "url", "sha256"),
        "corral": ("repository", "tag", "commit", "certificateIdentity"),
        "producer": ("repository", "workflow", "commit"),
        "qualification": ("producerEvidenceSha256",),
    }
    runs = {"release": ("ciRunId", "installedControlRunId"), "package": ("factoryRunId",),
            "image": ("buildRunId", "qualificationRunId"), "producer": ("runId", "runAttempt"),
            "qualification": ("producerBootRunId",)}
    for section, names in fields.items():
        value = manifest.get(section)
        require(isinstance(value, dict), f"manifest {section} must be an object")
        require(all(isinstance(value.get(name), str) for name in names), f"manifest {section} string fields missing")
        require(all(run_identity(value.get(name)) for name in runs.get(section, ())), f"manifest {section} run identity missing")
    require(all(positive(manifest["disk"].get(name)) for name in ("size", "virtualSize")), "exact disk bytes/size required")
    assets = manifest["corral"].get("assets")
    require(isinstance(assets, dict) and set(assets) == set(PLATFORMS), "qualified native Corral platforms missing")
    for asset in assets.values():
        require(isinstance(asset, dict) and all(isinstance(asset.get(name), str)
                for name in ("name", "sha256", "url", "bundleUrl")), "Corral asset fields missing")
    release = manifest["release"]
    require(release["repository"] == "tuna-os/tuna-desktop" and TAG.fullmatch(release["tag"]), "invalid desktop release identity")
    require(COMMIT.fullmatch(release["commit"]) and release["version"] == release["tag"][1:], "release source/version differs")
    require(run_identity(release["ciRunId"]) and run_identity(release["installedControlRunId"]), "release qualification runs missing")
    require(release["reviewPath"] == f"docs/reviews/{release['tag']}.md" and SHA.fullmatch(release["reviewSha256"]), "release review identity missing")
    package = manifest["package"]
    require(package["name"] == "tuna-desktop" and package["architecture"] == "x86_64" and package["sourceCommit"] == release["commit"], "package source/architecture differs")
    require(SHA.fullmatch(package["sha256"]) and run_identity(package["factoryRunId"]), "signed package receipt missing")
    require(package["signerFingerprint"] == "4E5CC9F8B3B521793D95266E629BE6EA45188366", "package signer identity differs")
    require(isinstance(package["version"], str) and re.fullmatch(r"[A-Za-z0-9.+:~_-]+", package["version"]), "package version missing")
    package_url = f"https://repo.tunaos.org/pacman/arch/x86_64/tuna-desktop-{package['version']}-x86_64.pkg.tar.zst"
    require(package["url"] == package_url and package["signatureUrl"] == package_url + ".sig",
            "package is not the exact signed served release artifact")
    image = manifest["image"]
    require(image["repository"] == "ghcr.io/tuna-os/marlin" and image["architecture"] == "amd64" and image["certificateIdentity"] == IMAGE_IDENTITY, "trusted Marlin image identity differs")
    require(all(re.fullmatch(r"sha256:[0-9a-f]{64}", image[k]) for k in ("indexDigest", "manifestDigest")), "pinned image digests required")
    require(COMMIT.fullmatch(image["sourceCommit"]) and run_identity(image["buildRunId"]) and run_identity(image["qualificationRunId"]), "image provenance/qualification missing")
    disk = manifest["disk"]
    require(disk["name"] == "tuna-desktop-amd64.qcow2" and disk["architecture"] == image["architecture"] and disk["firmware"] == "uefi", "disk architecture/UEFI contract differs")
    require(disk["url"] == release_url(release["repository"], release["tag"], disk["name"]), "disk URL is not the declared release asset")
    require(SHA.fullmatch(disk["sha256"]) and positive(disk["size"]) and positive(disk["virtualSize"]), "exact disk bytes/size required")
    corral = manifest["corral"]
    require(corral["repository"] == "tuna-os/corral" and TAG.fullmatch(corral["tag"]) and COMMIT.fullmatch(corral["commit"]), "Corral immutable release identity missing")
    identity = f"https://github.com/tuna-os/corral/.github/workflows/release.yml@refs/tags/{corral['tag']}"
    require(corral["certificateIdentity"] == identity, "Corral release signer differs")
    require(all(p in corral["assets"] for p in PLATFORMS), "qualified native Corral platforms missing")
    for host, asset in corral["assets"].items():
        require(host in PLATFORMS, "unknown Corral host platform")
        require(asset["name"] == "corral-" + host and SHA.fullmatch(asset["sha256"]), "Corral asset identity differs")
        require(asset["url"] == release_url(corral["repository"], corral["tag"], asset["name"]) and asset["bundleUrl"] == asset["url"] + ".sigstore.json", "Corral asset URL differs")
    producer = manifest["producer"]
    require(producer["repository"] == "tuna-os/tuna-desktop" and producer["workflow"] == ".github/workflows/vm-release.yml" and COMMIT.fullmatch(producer["commit"]), "producer source identity differs")
    require(run_identity(producer["runId"]) and run_identity(producer["runAttempt"]), "producer run missing")
    qualification = manifest["qualification"]
    require(qualification["producerBootRunId"] == producer["runId"] and SHA.fullmatch(qualification["producerEvidenceSha256"]), "producer boot evidence missing")
    return manifest


def no_duplicates(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON member")
        result[key] = value
    return result


def private_directory(path):
    path = Path(path)
    path.mkdir(parents=True, mode=0o700, exist_ok=True)
    info = path.lstat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077, f"private owned directory required: {path}")
    return path


def safe_file(path, maximum, *, dir_fd=None):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=dir_fd)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_size <= maximum, f"bounded regular artifact required: {path}")
        with os.fdopen(fd, "rb", closefd=False) as stream:
            payload = stream.read(maximum + 1)
        require(len(payload) <= maximum, f"artifact grew beyond bound: {path}")
        return payload
    finally:
        os.close(fd)


def atomic_json(path, value):
    path = Path(path)
    temporary = path.with_name(path.name + "." + uuid.uuid4().hex)
    with temporary.open("x", encoding="utf-8") as stream:
        os.chmod(temporary, 0o600)
        json.dump(value, stream, sort_keys=True, separators=(",", ":"))
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def trusted_environment():
    return {key: value for key, value in os.environ.items()
            if not key.startswith(("COSIGN_", "SIGSTORE_", "CORRAL_", "LD_", "DYLD_"))
            and key not in ("SSL_CERT_FILE", "SSL_CERT_DIR", "CURL_CA_BUNDLE", "REQUESTS_CA_BUNDLE")
            and key not in PROOF_DEBUG_KEYS}


def terminate_child(process, *, group=False, timeout=10):
    """Signal only a still-unreaped child (and its private process group)."""
    if process.poll() is not None:
        return
    def send(signum):
        try:
            if group:
                os.killpg(process.pid, signum)
            else:
                process.send_signal(signum)
        except ProcessLookupError:
            pass
    send(signal.SIGTERM)
    try:
        process.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        send(signal.SIGKILL)
        process.wait(timeout=5)


def run(command, *, timeout=120, cancellable=True, env=None):
    if CANCELLED and cancellable:
        raise TimeoutError("cancelled")
    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as error:
        process = subprocess.Popen([str(x) for x in command], stdout=output, stderr=error,
                                   env=trusted_environment() if env is None else env, start_new_session=True)
        try:
            deadline = time.monotonic() + timeout
            while process.poll() is None:
                if (CANCELLED and cancellable) or time.monotonic() >= deadline:
                    raise TimeoutError("cancelled" if CANCELLED else f"command deadline: {command[0]}")
                require(os.fstat(output.fileno()).st_size <= 8 * 1024 * 1024
                        and os.fstat(error.fileno()).st_size <= 8 * 1024 * 1024, "command output exceeds bound")
                time.sleep(.05)
            require(os.fstat(output.fileno()).st_size <= 8 * 1024 * 1024
                    and os.fstat(error.fileno()).st_size <= 8 * 1024 * 1024, "command output exceeds bound")
            output.seek(0)
            error.seek(0)
            stdout, stderr = output.read(), error.read()
            if process.returncode:
                raise CommandFailure(command, process.returncode, stderr)
            return stdout
        finally:
            terminate_child(process, group=True, timeout=5)


def host_platform():
    system = {"Linux": "linux", "Darwin": "darwin"}.get(platform.system())
    arch = {"x86_64": "amd64", "amd64": "amd64", "aarch64": "arm64", "arm64": "arm64"}.get(platform.machine())
    host = f"{system}-{arch}"
    require(host in PLATFORMS, f"native host is not qualified by this release: {host}")
    return host


def verify_host_binary(path, host):
    """Reject a foreign executable even where Rosetta/binfmt could run it."""
    require(host in PLATFORMS, "unsupported native binary platform")
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        require(stat.S_ISREG(os.fstat(fd).st_mode), "host executable must be a regular file")
        header = os.read(fd, 32)
    finally:
        os.close(fd)
    if host.startswith("linux-"):
        expected = 62 if host.endswith("-amd64") else 183
        require(len(header) >= 20 and header[:6] == b"\x7fELF\x02\x01"
                and header[7] in (0, 3) and int.from_bytes(header[18:20], "little") == expected,
                "signed executable does not match native Linux host architecture")
    else:
        expected = 0x01000007 if host.endswith("-amd64") else 0x0100000C
        require(len(header) >= 8 and header[:4] == b"\xcf\xfa\xed\xfe"
                and int.from_bytes(header[4:8], "little") == expected,
                "signed executable does not match native macOS host architecture")


def acquire_verifier(state):
    host = host_platform()
    name = "cosign-" + host
    verifier = acquire(f"https://github.com/sigstore/cosign/releases/download/v3.1.4/{name}",
                       Path(state) / "tools" / name, 256 * 1024 * 1024,
                       expected=COSIGN_SHA256[host])
    verify_host_binary(verifier, host)
    verifier.chmod(0o700)
    return verifier


def package_present(command, expected=None):
    try:
        result = run(command).decode().strip()
    except CommandFailure as error:
        if error.returncode == 1:
            return False
        raise
    return result == expected if expected is not None else bool(result)


def package_transaction(command):
    """Do not SIGKILL a native package database transaction on wrapper deadlines."""
    require(not CANCELLED, "cancelled before native package transaction; VM was not started")
    command = [str(value) for value in command]
    terminal = None
    if command[0] == "sudo":
        try:
            # The child has its own session so terminal cancellation cannot
            # interrupt a package database transaction. Let normal sudo read
            # the real user's terminal directly; Python never reads a password.
            terminal = open("/dev/tty", "rb", buffering=0)
        except OSError:
            command.insert(1, "-n")
        else:
            command.insert(1, "-S")
    try:
        process = subprocess.Popen(command, stdin=terminal if terminal is not None else subprocess.DEVNULL,
                                   stdout=sys.stderr, stderr=sys.stderr,
                                   env=trusted_environment(), start_new_session=True)
        try:
            returncode = process.wait()
        finally:
            # Even an unrelated exception must not kill the native installer.
            if process.poll() is None:
                process.wait()
    finally:
        if terminal is not None:
            terminal.close()
    if returncode:
        raise CommandFailure(command, returncode, b"native package manager failed; see its output above")


def ensure_dependencies(verifier):
    """Use native package trust for QEMU and the declared amd64 UEFI firmware."""
    needed = [tool for tool in ("qemu-img", "qemu-system-x86_64") if not shutil.which(tool)]
    if platform.system() == "Darwin" and shutil.which("brew"):
        if needed or not package_present(["brew", "list", "--versions", "qemu"]):
            package_transaction(["brew", "install", "qemu"])
    elif platform.system() == "Linux":
        sudo = [] if os.geteuid() == 0 else ["sudo"]
        if shutil.which("apt-get"):
            if needed or not package_present(["dpkg-query", "-W", "-f=${db:Status-Status}", "ovmf"], "installed"):
                package_transaction([*sudo, "apt-get", "update"])
                package_transaction([*sudo, "apt-get", "install", "-y", "qemu-system-x86", "qemu-utils", "ovmf"])
        elif shutil.which("dnf"):
            if needed or not package_present(["rpm", "-q", "edk2-ovmf"]):
                package_transaction([*sudo, "dnf", "install", "-y", "qemu-system-x86", "qemu-img", "edk2-ovmf"])
        elif shutil.which("pacman"):
            if needed or not package_present(["pacman", "-Q", "edk2-ovmf"]):
                package_transaction([*sudo, "pacman", "-S", "--needed", "--noconfirm", "qemu-system-x86", "qemu-img", "edk2-ovmf"])
    require(not CANCELLED, "cancelled after native package manager finished; VM was not started")
    needed = [tool for tool in ("qemu-img", "qemu-system-x86_64") if not shutil.which(tool)]
    require(not needed, "missing host tools: " + ", ".join(needed) +
            "; supported native package manager required (macOS: install Homebrew, then brew install qemu)")
    tools = {tool: str(Path(shutil.which(tool)).resolve()) for tool in ("qemu-img", "qemu-system-x86_64")}
    for binary in tools.values():
        verify_host_binary(binary, host_platform())
    tools["cosign"] = str(verifier)
    return tools


def verify_blob(verifier, blob, bundle, identity, commit):
    require(isinstance(commit, str) and COMMIT.fullmatch(commit), "signature source commit missing")
    run([verifier, "verify-blob", "--bundle", bundle, "--certificate-identity", identity,
         "--certificate-oidc-issuer", ISSUER, "--certificate-github-workflow-sha", commit, blob],
        timeout=180, env=trusted_environment())


def offline_directory(path):
    """Pin a directory descriptor without following any symlink component."""
    absolute = Path(path).absolute()
    fd = os.open(absolute.anchor, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for part in absolute.parts[1:]:
            child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = child
        info = os.fstat(fd)
        require(info.st_uid == os.getuid() and not info.st_mode & 0o022,
                "offline artifact directory must be owned and not publicly writable")
        return fd
    except BaseException:
        os.close(fd)
        raise


def acquire(url, destination, maximum, *, expected=None, size=None, artifact_dir=None):
    require(not CANCELLED, "artifact acquisition cancelled/deadline")
    require(urlsplit(url).scheme == "https", "HTTPS artifact URL required")
    name = filename(urlsplit(url).path.rsplit("/", 1)[1])
    root_fd = offline_directory(artifact_dir) if artifact_dir is not None else None
    destination = Path(destination)
    temporary = destination.with_name(destination.name + "." + uuid.uuid4().hex)
    digest, count = hashlib.sha256(), 0
    try:
        private_directory(destination.parent)
        require(not destination.is_symlink(), "cached artifact must not be a symlink")
        if destination.exists():
            info = destination.lstat()
            require(stat.S_ISREG(info.st_mode) and info.st_size <= maximum, "cached artifact must be a bounded regular file")
            if expected and sha256(destination, maximum) == expected:
                require(size is None or info.st_size == size, "cached artifact metadata differs")
                return destination
        if root_fd is not None:
            fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=root_fd)
            try:
                info = os.fstat(fd)
                require(stat.S_ISREG(info.st_mode) and info.st_size <= maximum, "offline artifact must be a bounded regular file")
                stream = os.fdopen(fd, "rb")
            except BaseException:
                os.close(fd)
                raise
        else:
            stream = urlopen(url, timeout=30)
            if urlsplit(stream.geturl()).scheme != "https":
                stream.close()
                raise ValueError("artifact redirect downgraded transport")
        with stream, temporary.open("xb") as output:
            os.chmod(temporary, 0o600)
            deadline = time.monotonic() + 1800
            while True:
                require(not CANCELLED and time.monotonic() < deadline, "artifact acquisition cancelled/deadline")
                chunk = stream.read(1024 * 1024)
                if not chunk:
                    break
                count += len(chunk)
                require(count <= maximum, "artifact exceeds declared download bound")
                output.write(chunk)
                digest.update(chunk)
        require(size is None or count == size, "artifact transfer size differs")
        require(expected is None or digest.hexdigest() == expected, "artifact SHA256 differs")
        os.replace(temporary, destination)
        return destination
    finally:
        temporary.unlink(missing_ok=True)
        if root_fd is not None:
            os.close(root_fd)


def admit(manifest_path, bundle_path, state, artifact_dir=None, expected=None):
    if artifact_dir is not None:
        os.close(offline_directory(artifact_dir))
    payload = safe_file(manifest_path, 1024 * 1024)
    require(expected is None or hashlib.sha256(payload).hexdigest() == expected, "manifest pin differs")
    verifier = acquire_verifier(state)
    # Verify the exact bounded bytes we parse, not a pathname an offline caller
    # could replace between verification and use.
    session = private_directory(Path(state) / ("admission-" + uuid.uuid4().hex))
    try:
        blob, bundle = session / "manifest.json", session / "manifest.json.sigstore.json"
        blob.write_bytes(payload)
        bundle.write_bytes(safe_file(bundle_path, 4 * 1024 * 1024))
        manifest = validate_manifest(json.loads(payload, object_pairs_hook=no_duplicates))
        verify_blob(verifier, blob, bundle, MANIFEST_IDENTITY, manifest["producer"]["commit"])
    finally:
        shutil.rmtree(session)
    host = host_platform()
    cache = private_directory(Path(state) / "cache")
    asset = manifest["corral"]["assets"][host]
    binary = acquire(asset["url"], cache / asset["sha256"] / asset["name"], 512 * 1024 * 1024,
                     expected=asset["sha256"], artifact_dir=artifact_dir)
    signature = acquire(asset["bundleUrl"], binary.with_name(binary.name + ".sigstore.json"),
                        4 * 1024 * 1024, artifact_dir=artifact_dir)
    verify_blob(verifier, binary, signature, manifest["corral"]["certificateIdentity"], manifest["corral"]["commit"])
    verify_host_binary(binary, host)
    binary.chmod(0o700)
    disk = manifest["disk"]
    free = shutil.disk_usage(state).free
    require(free >= max(10 * 1024**3, 2 * disk["size"] + 4 * 1024**3),
            "insufficient disk space for verified artifact, sparse guest copy and boot writes (at least 10 GiB)")
    pristine = acquire(disk["url"], cache / disk["sha256"] / disk["name"], disk["size"],
                       expected=disk["sha256"], size=disk["size"], artifact_dir=artifact_dir)
    tools = ensure_dependencies(verifier)
    info = json.loads(run([tools["qemu-img"], "info", "--output=json", pristine]), object_pairs_hook=no_duplicates)
    require(isinstance(info, dict) and info.get("format") == "qcow2" and info.get("virtual-size") == disk["virtualSize"]
            and not info.get("backing-filename") and not info.get("full-backing-filename")
            and not info.get("data-file"), "self-contained qcow2 geometry differs")
    specific = info.get("format-specific", {})
    require(isinstance(specific, dict) and isinstance(specific.get("data", {}), dict)
            and not specific.get("data", {}).get("data-file"), "external qcow2 data file is not allowed")
    measured = json.loads(run([tools["qemu-img"], "measure", "--output=json", "-O", "qcow2", pristine]), object_pairs_hook=no_duplicates)
    require(isinstance(measured, dict) and positive(measured.get("required"))
            and shutil.disk_usage(state).free >= measured["required"] + 4 * 1024**3,
            "insufficient space for measured sparse guest copy and boot writes")
    return manifest, hashlib.sha256(payload).hexdigest(), binary, pristine, tools


def read_event(process, timeout):
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    data = bytearray()
    try:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            require(not CANCELLED, "console handoff cancelled")
            if selector.select(.1):
                char = os.read(process.stdout.fileno(), 1)
                require(char, "console exited before its readiness event")
                if char == b"\n":
                    event = json.loads(data, object_pairs_hook=no_duplicates)
                    require(isinstance(event, dict), "console event must be an object")
                    return event
                data.extend(char)
                require(len(data) <= 65536, "console event exceeds bound")
            require(process.poll() is None, "console exited before readiness")
        raise TimeoutError("browser/console readiness deadline")
    finally:
        selector.close()


def validate_runtime(runtime, architecture, host):
    require(isinstance(runtime, dict) and runtime.get("architecture") == architecture
            and runtime.get("firmware") == "uefi" and runtime.get("tailscale_ip") == "127.0.0.1"
            and type(runtime.get("vnc_port")) is int and 0 < runtime["vnc_port"] < 65536,
            "created guest architecture/firmware/local network binding differs")
    if host.endswith("-arm64"):
        require(runtime.get("acceleration") == "tcg", "cross-architecture guest must explicitly use TCG")


def console_endpoint(endpoint, name):
    require(isinstance(endpoint, dict) and isinstance(endpoint.get("url"), str), "owned console endpoint URL missing")
    url = urlsplit(endpoint["url"])
    require(endpoint.get("vm") == name and endpoint.get("namespace") == "local"
            and not endpoint.get("context")
            and url.scheme == "http" and url.hostname == "127.0.0.1" and url.port
            and url.netloc == f"127.0.0.1:{url.port}"
            and url.path == "/console/local/" + name and not url.fragment,
            "owned console endpoint identity differs")
    query = parse_qsl(url.query, keep_blank_values=True, strict_parsing=True)
    require(len(query) == 1 and query[0][0] == "ready_nonce"
            and re.fullmatch(r"[A-Za-z0-9_-]{43}", query[0][1]), "owned console readiness nonce differs")
    require(endpoint.get("websocket") == f"ws://127.0.0.1:{url.port}/api/vnc/local/{name}",
            "owned console websocket identity differs")
    return endpoint["url"]


class Launch:
    def __init__(self, state, admitted, *, memory="4G", cpus=4):
        self.state = private_directory(state)
        self.manifest, self.manifest_sha, self.corral, self.pristine, self.tools = admitted
        require(re.fullmatch(r"[1-9][0-9]*[MG]", memory) and type(cpus) is int and cpus > 0, "unitful memory/positive CPU count required")
        self.memory, self.cpus = memory, cpus
        self.console = None
        self.browser = None
        self.started = False
        self.endpoint = None
        self.record = None
        self.receipt = {"schema": 1, "manifestSha256": self.manifest_sha,
                        "diskSha256": self.manifest["disk"]["sha256"], "hostPlatform": host_platform(),
                        "corralSha256": sha256(self.corral), "browserRfbConnected": False,
                        "diskSha256Scope": "pristine release input; retained guest data is writable",
                        "cleanupComplete": False, "userDataRetained": True}

    def command(self, *args, timeout=120, cancellable=True):
        return run([self.corral, "--backend", "qemu", *args], timeout=timeout, env=trusted_environment(), cancellable=cancellable)

    def owned(self):
        require(isinstance(self.record, dict), "no owned VM record")
        require(all(isinstance(self.record.get(key), str)
                    for key in ("name", "directory", "manifestSha256", "nonce")), "owned VM record fields missing")
        identity = self.record.get("directoryIdentity")
        require(isinstance(identity, list) and len(identity) == 2 and all(type(value) is int and value >= 0 for value in identity)
                and type(self.record.get("created")) is bool
                and re.fullmatch(r"[0-9a-f]{32}", self.record["nonce"]), "owned VM record identity invalid")
        directory = Path.home() / ".local/share/corral/vms" / self.record["name"]
        require(re.fullmatch(r"try-tuna-[0-9a-f]{20}", self.record["name"])
                and self.record["directory"] == str(directory)
                and self.record["manifestSha256"] == self.manifest_sha,
                "owned VM path/source identity differs")
        fd = offline_directory(directory)
        try:
            info = os.fstat(fd)
            require([info.st_dev, info.st_ino] == identity, "owned VM directory changed")
            marker = json.loads(safe_file(".try-tuna-owner.json", 65536, dir_fd=fd), object_pairs_hook=no_duplicates)
            require(marker == self.record, "owned VM marker differs")
        finally:
            os.close(fd)
        return directory

    def prepare(self):
        current = self.state / "current.json"
        require(not current.is_symlink(), "owned VM current record must not be a symlink")
        if current.exists():
            record = json.loads(safe_file(current, 65536), object_pairs_hook=no_duplicates)
            require(isinstance(record, dict), "owned VM current record must be an object")
            if record.get("manifestSha256") == self.manifest_sha:
                self.record = record
                self.owned()
                return
            history = private_directory(self.state / "history")
            atomic_json(history / (uuid.uuid4().hex + ".json"), record)
            print("Preserving previous release guest data at " + str(record.get("directory", "recorded in history")), file=sys.stderr)
        name = "try-tuna-" + uuid.uuid4().hex[:20]
        vm_home = Path.home() / ".local/share/corral/vms"
        vm_home.mkdir(parents=True, exist_ok=True)
        directory = vm_home / name
        directory.mkdir(mode=0o700)
        info = directory.stat()
        self.record = {"name": name, "directory": str(directory), "directoryIdentity": [info.st_dev, info.st_ino],
                       "manifestSha256": self.manifest_sha, "nonce": uuid.uuid4().hex}
        self.record["created"] = False
        atomic_json(directory / ".try-tuna-owner.json", self.record)
        atomic_json(current, self.record)
        # --force applies only to the empty directory reserved exclusively above;
        # it never authorizes replacement of a pre-existing or unknown VM.
        self.command("create", name, "--force", "--loopback", "--qcow", self.pristine, "--firmware", "uefi",
                     "--arch", self.manifest["disk"]["architecture"], "--disk", str(self.manifest["disk"]["virtualSize"]),
                     "--mem", self.memory, "--cpu", str(self.cpus), timeout=600)
        self.owned()
        self.record["created"] = True
        atomic_json(directory / ".try-tuna-owner.json", self.record)
        atomic_json(current, self.record)

    def start(self):
        self.prepare()
        self.owned()
        require(self.record.get("created") is True, "previous incomplete creation retained for diagnosis; inspect its owned data path")
        self.started = True
        name = self.record["name"]
        self.command("start", "qemu//local/" + name)
        runtime = json.loads(self.command("info", "qemu//local/" + name), object_pairs_hook=no_duplicates)
        validate_runtime(runtime, self.manifest["disk"]["architecture"], host_platform())
        self.receipt["runtime"] = runtime
        self.receipt["vm"] = self.record
        log = (self.state / "console.log").open("ab")
        try:
            self.console = subprocess.Popen([str(self.corral), "--backend", "qemu", "--json", "console",
                                             "qemu//local/" + name, "--addr", "127.0.0.1:0", "--notify-ready"],
                                            env=trusted_environment(), stdout=subprocess.PIPE, stderr=log, start_new_session=True)
        finally:
            log.close()
        endpoint = read_event(self.console, 30)
        console_endpoint(endpoint, name)
        self.endpoint = endpoint
        self.receipt["console"] = endpoint
        atomic_json(self.state / "receipt.json", self.receipt)
        return endpoint["url"]

    def browser_ready(self):
        event = read_event(self.console, 180)
        require(event.get("event") == "rfb-connected" and event.get("vm") == self.record["name"]
                and event.get("namespace") == "local", "browser handshake identity differs")
        self.receipt["browserRfbConnected"] = True
        atomic_json(self.state / "receipt.json", self.receipt)

    def cleanup(self):
        errors = []
        for process, group, label in ((self.browser, True, "browser"), (self.console, False, "console")):
            if process is not None:
                try:
                    terminate_child(process, group=group)
                except Exception as error:
                    errors.append(f"{label}: {error}")
                finally:
                    try:
                        if process is self.console and process.stdout is not None:
                            process.stdout.close()
                    except OSError as error:
                        errors.append(f"{label} output: {error}")
        if self.record is not None:
            try:
                directory = self.owned()
                if self.started:
                    self.command("stop", "qemu//local/" + self.record["name"], cancellable=False)
                if (directory / "disk.qcow2").exists():
                    # Without --force-share this must acquire QEMU's disk lock.
                    json.loads(run([self.tools["qemu-img"], "info", "--output=json", directory / "disk.qcow2"], cancellable=False))
                    self.receipt["diskLockReleased"] = True
                if self.endpoint:
                    url = urlsplit(self.endpoint["url"])
                    with socket.socket() as probe:
                        probe.settimeout(2)
                        require(probe.connect_ex(("127.0.0.1", url.port)) != 0, "owned console listener remains")
                runtime = self.receipt.get("runtime")
                if runtime:
                    with socket.socket() as probe:
                        probe.settimeout(2)
                        require(probe.connect_ex(("127.0.0.1", runtime["vnc_port"])) != 0,
                                "owned QEMU VNC listener remains")
                self.receipt["userDataPath"] = str(directory / "disk.qcow2")
            except Exception as error:
                errors.append(str(error))
        self.receipt["cleanupComplete"] = not errors
        self.receipt["cleanupErrors"] = errors
        atomic_json(self.state / "receipt.json", self.receipt)
        if errors:
            raise RuntimeError("owned cleanup failed; state retained: " + "; ".join(errors))


def stop(state):
    control = socket.socket(socket.AF_UNIX)
    try:
        control.settimeout(150)
        control.connect(str(Path(state) / "control.sock"))
        control.sendall(b"stop\n")
        require(control.recv(16) == b"stopped\n", "launcher did not acknowledge complete cleanup")
    finally:
        control.close()


def launch(options):
    global CANCELLED
    state = private_directory(options.state_dir)
    fd = os.open(state / "launcher.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    # Acquire inside the lifetime guard so lock contention also closes the fd.
    listener, client, instance = None, None, None
    control_path = state / "control.sock"
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        admitted = admit(options.manifest, options.bundle, state, options.artifact_dir, options.manifest_sha256)
        instance = Launch(state, admitted, memory=options.memory, cpus=options.cpus)
        control_path.unlink(missing_ok=True)
        require(len(str(control_path).encode()) < 104, "state directory too long for local stop socket")
        listener = socket.socket(socket.AF_UNIX)
        listener.bind(str(control_path))
        os.chmod(control_path, 0o600)
        listener.listen(1)
        listener.settimeout(.25)
        url = instance.start()
        print(json.dumps({"event": "console-listening", "url": url, "state": str(state)}), flush=True)
        if options.browser:
            instance.browser = subprocess.Popen([*options.browser, url], env=trusted_environment(), start_new_session=True)
            time.sleep(.25)
            require(instance.browser.poll() in (None, 0), "browser opener failed")
        elif platform.system() == "Darwin":
            run(["open", url], timeout=30)
        else:
            require(shutil.which("xdg-open"), "xdg-open is required to open your browser")
            run(["xdg-open", url], timeout=30)
        instance.browser_ready()
        print("Browser connected to the VM console. The guest may still be booting or need first setup.\n"
              "Close the browser to disconnect only; Ctrl-C or `stop` stops the VM and retains your guest data.", flush=True)
        while not CANCELLED:
            require(instance.console.poll() is None, "owned console process exited")
            try:
                client, _ = listener.accept()
                client.settimeout(2)
                require(client.recv(16) == b"stop\n", "unknown launcher control request")
                break
            except socket.timeout:
                pass
    finally:
        try:
            if instance is not None:
                instance.cleanup()
            if client is not None:
                client.sendall(b"stopped\n")
        finally:
            if client is not None:
                client.close()
            if listener is not None:
                listener.close()
                control_path.unlink(missing_ok=True)
            os.close(fd)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    for command in ("inspect", "launch"):
        child = sub.add_parser(command)
        child.add_argument("--manifest", type=Path, required=True)
        child.add_argument("--bundle", type=Path, required=True)
        child.add_argument("--manifest-sha256")
        child.add_argument("--artifact-dir", type=Path)
        child.add_argument("--state-dir", type=Path, default=Path.home() / ".local/state/try-tuna-desktop")
        if command == "launch":
            child.add_argument("--memory", default="4G")
            child.add_argument("--cpus", type=int, default=4)
            child.add_argument("--browser", nargs="+")
    child = sub.add_parser("qualify", help="native CI-only actual browser/pristine firstboot/lifecycle qualification")
    child.add_argument("--artifact-dir", type=Path, required=True)
    child.add_argument("--evidence-dir", type=Path, required=True)
    child.add_argument("--platform", choices=PLATFORMS, required=True)
    child = sub.add_parser("stop")
    child.add_argument("--state-dir", type=Path, default=Path.home() / ".local/state/try-tuna-desktop")
    options = parser.parse_args()
    def cancel(_signum, _frame):
        global CANCELLED
        CANCELLED = True
    signal.signal(signal.SIGINT, cancel)
    signal.signal(signal.SIGTERM, cancel)
    try:
        if options.command == "stop":
            stop(options.state_dir)
        elif options.command == "launch":
            launch(options)
        elif options.command == "qualify":
            from try_tuna_qualification import qualify
            qualify(options)
        else:
            state = private_directory(options.state_dir)
            admitted = admit(options.manifest, options.bundle, state, options.artifact_dir, options.manifest_sha256)
            print(json.dumps({"manifestSha256": admitted[1], "diskSha256": admitted[0]["disk"]["sha256"], "hostPlatform": host_platform(), "corralSha256": sha256(admitted[2])}, sort_keys=True))
    except (ValueError, RuntimeError, TimeoutError, OSError, KeyError, json.JSONDecodeError) as error:
        print("try-tuna-desktop: " + str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
