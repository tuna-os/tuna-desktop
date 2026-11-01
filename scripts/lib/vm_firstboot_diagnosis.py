#!/usr/bin/env python3
"""Observe untouched current Marlin firstboot in native CI; never publish assets.

This uses an independently pinned, signed NORMAL MAIN Corral artifact only for
this diagnosis. It is not release admission and is not a consumer fallback.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import selectors
import signal
import shutil
import socket
import subprocess
import tempfile
import time
import uuid

from vm_release import canonical, download, need, run, sha256, verify_blob

INDEX = "sha256:5ccc023cc73296d5e4d17cfbe38526935a405abcfb0095aad43b3a28bff182e6"
PLATFORM = "sha256:8c7a952fd654c45a021bfb9318ce4027d7bb4b734a1a4f63fb1a5a294e4096f3"
IMAGE = "ghcr.io/tuna-os/marlin"
IMAGE_IDENTITY = "https://github.com/tuna-os/tunaOS/.github/workflows/reusable-build-image.yml@refs/heads/main"
CORRAL_SOURCE = "667edf09b70eb88b2a6398ae834d4c841c13f629"
CORRAL_SHA256 = "67673b95bee120b1305be7bb4d0f06f368fb934958fffb37353fdf86320b0e6d"
CORRAL_IDENTITY = "https://github.com/tuna-os/corral/.github/workflows/ci.yml@refs/heads/main"


def write(path, value):
    Path(path).write_bytes(canonical(value))


def process_identity(pid):
    root = Path("/proc") / str(pid)
    try:
        data = (root / "stat").read_text().rsplit(")", 1)[1].split()
        return {"pid": pid, "startTicks": int(data[19]), "state": data[0],
                "executable": os.readlink(root / "exe")}
    except FileNotFoundError:
        return None


def event(process, seconds=30):
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    data = bytearray()
    try:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if selector.select(.1):
                part = os.read(process.stdout.fileno(), 1)
                need(part, "Corral console closed before listener handoff")
                if part == b"\n":
                    return json.loads(data)
                data.extend(part)
                need(len(data) <= 65536, "oversized console handoff")
            need(process.poll() is None, "Corral console exited before listener handoff")
        raise TimeoutError("console handoff deadline")
    finally:
        selector.close()


def diagnose(args):
    from playwright.sync_api import sync_playwright
    from urllib.parse import urlsplit
    need(platform.system() == "Linux" and platform.machine() == "x86_64", "native Linux amd64 diagnosis host required")
    output = args.output.resolve()
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    receipt = {"scope": "untouched firstboot diagnosis ONLY; not release, native consumer or successful account onboarding qualification",
               "observerSource": os.environ["GITHUB_SHA"], "observerRun": os.environ["GITHUB_RUN_ID"],
               "imageIndex": INDEX, "imagePlatform": PLATFORM, "host": {"system": platform.system(), "machine": platform.machine()},
               "injectedAccounts": [], "injectedSshKeys": [], "injectedSmbiosCredentials": [],
               "cleanupComplete": False}
    work = Path(tempfile.mkdtemp(prefix="tuna-pristine-", dir="/tmp"))
    console = None
    identity = None
    endpoint = None
    vm_directory = None
    started = False
    error = None
    env = {key: value for key, value in os.environ.items() if not key.startswith(("CORRAL_", "COSIGN_", "SIGSTORE_"))}
    home = work / "home"
    home.mkdir(mode=0o700)
    env["HOME"] = str(home)
    corral = work / "corral-linux-amd64"
    name = "tuna-pristine-" + uuid.uuid4().hex[:16]
    ref = "//local/" + name
    def cli(*values, timeout=120):
        return run([corral, "--backend", "qemu", *values], env=env, timeout=timeout)
    try:
        raw_index = run(["skopeo", "inspect", "--raw", "docker://" + IMAGE + "@" + INDEX])
        need("sha256:" + hashlib.sha256(raw_index).hexdigest() == INDEX, "immutable index bytes differ")
        (output / "image-index.json").write_bytes(raw_index)
        descriptors = [row for row in json.loads(raw_index)["manifests"]
                       if row.get("platform") == {"architecture": "amd64", "os": "linux"}]
        need(len(descriptors) == 1 and descriptors[0]["digest"] == PLATFORM, "signed amd64 platform is not in the declared index")
        verification = run(["cosign", "verify", IMAGE + "@" + PLATFORM, "--certificate-identity", IMAGE_IDENTITY,
                            "--certificate-oidc-issuer", "https://token.actions.githubusercontent.com"])
        (output / "platform-signature.json").write_bytes(verification)
        receipt["indexSignatureClaim"] = "no index-signature claim; immutable descriptor membership and the platform signature are verified separately"
        receipt["signedPlatformVerified"] = True
        url = "https://github.com/tuna-os/corral/releases/download/binaries/corral-linux-amd64"
        download(url, corral)
        download(url + ".sigstore.json", Path(str(corral) + ".sigstore.json"))
        need(sha256(corral) == CORRAL_SHA256, "normal main Corral artifact bytes changed; no fallback allowed")
        verify_blob(corral, CORRAL_IDENTITY, CORRAL_SOURCE)
        corral.chmod(0o700)
        receipt["corral"] = {"source": CORRAL_SOURCE, "sha256": CORRAL_SHA256, "identity": CORRAL_IDENTITY,
                              "scope": "normal main CI artifact for diagnosis; NOT final tagged-release consumer admission"}
        # Inventory runs no service/entrypoint and changes no image or disk.
        inventory_script = "set -eu; echo '=== normal users ==='; awk -F: '$3 >= 1000 && $3 != 65534 {print $1, $3}' /etc/passwd; echo '=== initial setup package ==='; if pacman -Q gnome-initial-setup; then :; else status=$?; test \"$status\" = 1; fi; echo '=== root credentials ==='; awk -F: '$1==\"root\" {if ($2 ~ /^[!*]/) print \"root-locked\"; else exit 1}' /etc/shadow; test ! -s /root/.ssh/authorized_keys"
        inventory = run(["sudo", "podman", "run", "--rm", "--entrypoint", "/bin/sh", IMAGE + "@" + PLATFORM, "-c", inventory_script], timeout=300)
        (output / "pristine-image-inventory.txt").write_bytes(inventory)
        receipt["keylessBuilderSourceSha256"] = sha256(args.helper)
        need(run(["git", "rev-parse", "HEAD"], cwd=args.corral_source).decode().strip() == CORRAL_SOURCE, "builder source differs from signed Corral provenance")
        raw = work / "pristine.raw"
        # This is the exact existing producer API, with empty SSHKey,
        # Customize and ProvisionScript. No image fixture is constructed.
        with (output / "disk-build.log").open("wb") as log:
            builder = subprocess.Popen(["go", "run", str(args.helper.resolve()), IMAGE + "@" + PLATFORM, str(raw)],
                                       cwd=args.corral_source, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                need(builder.wait(timeout=1800) == 0, "keyless disk builder failed; see disk-build.log")
            finally:
                if builder.poll() is None:
                    try:
                        os.killpg(builder.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                    try:
                        builder.wait(timeout=20)
                    except subprocess.TimeoutExpired:
                        os.killpg(builder.pid, signal.SIGKILL)
                        builder.wait(timeout=10)
        pristine = work / "pristine.qcow2"
        run(["qemu-img", "convert", "-f", "raw", "-O", "qcow2", raw, pristine], timeout=600)
        raw.unlink()
        before = sha256(pristine)
        receipt["pristineDiskSha256"] = before
        (output / "disk-info.json").write_bytes(run(["qemu-img", "info", "--output=json", pristine]))
        cli("create", name, "--qcow", pristine, "--firmware", "uefi", "--disk", "40G", "--mem", "4G", "--cpu", "4", timeout=600)
        vm_directory = home / ".local/share/corral/vms" / name
        inode = vm_directory.stat()
        receipt["vmDirectoryIdentity"] = [inode.st_dev, inode.st_ino]
        started = True
        cli("start", ref)
        meta = json.loads(cli("info", ref))
        receipt["vmMetadata"] = meta
        pid = int((vm_directory / "qemu.pid").read_text())
        identity = process_identity(pid)
        need(identity is not None and identity["state"] != "Z", "QEMU process is not live")
        receipt["processBefore"] = identity
        log = (output / "console.log").open("wb")
        console = subprocess.Popen([str(corral), "--backend", "qemu", "--json", "console", ref, "--addr", "127.0.0.1:0"],
                                   env=env, stdout=subprocess.PIPE, stderr=log, start_new_session=True)
        log.close()
        endpoint = event(console)
        parsed = urlsplit(endpoint["url"])
        need(endpoint["vm"] == name and endpoint["namespace"] == "local" and parsed.hostname == "127.0.0.1", "wrong owned console handoff")
        receipt["console"] = endpoint
        with sync_playwright() as driver:
            browser = driver.chromium.launch()
            page = browser.new_page(viewport={"width": 1400, "height": 1000})
            try:
                page.goto(endpoint["url"])
                page.wait_for_selector('body[data-connected="true"] #vnc-canvas', timeout=60000)
                receipt["realBrowserRfbConnected"] = True
                for index, delay in enumerate((20, 20, 20, 60, 60), 1):
                    page.wait_for_timeout(delay * 1000)
                    shot = output / f"firstboot-{index:02d}.png"
                    page.locator("#vnc-canvas").screenshot(path=str(shot))
                    text = run(["tesseract", shot, "stdout", "--psm", "6"]).decode(errors="replace")
                    (output / f"firstboot-{index:02d}.txt").write_text(text)
                receipt["firstbootEvidence"] = "actual untouched console frames; no UI/account success inferred from a readiness marker"
            finally:
                browser.close()
        receipt["pristineDiskUnchanged"] = sha256(pristine) == before
        need(receipt["pristineDiskUnchanged"], "boot modified pristine input rather than its Corral copy")
    except Exception as caught:
        error = caught
        receipt["error"] = f"{type(caught).__name__}: {caught}"
    finally:
        cleanup_errors = []
        # The reusable builder mounts precisely this private job's raw disk.
        # Retire only containers with that exact mount, never by PID/name.
        try:
            for container_id in run(["sudo", "podman", "ps", "--all", "--quiet"]).decode().split():
                container = json.loads(run(["sudo", "podman", "inspect", container_id]))[0]
                if any(mount.get("Source") == str(work / "pristine.raw") and mount.get("Destination") == "/target.img"
                       for mount in container.get("Mounts", [])):
                    run(["sudo", "podman", "rm", "--force", "--ignore", container_id])
        except Exception as caught:
            cleanup_errors.append("owned builder retirement: " + str(caught))
        if console is not None:
            if console.poll() is None:
                console.terminate()
                try:
                    console.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    console.kill()
                    console.wait(timeout=5)
            console.stdout.close()
        if started and vm_directory is not None:
            try:
                # Legacy normal-main stop uses PID files; do not call it.
                # Authorize this VM's quit by its private QMP name instead.
                response = json.loads(cli("qmp", ref, "query-name"))
                need(response.get("name") == name, "QMP identifies a different VM")
                cli("qmp", ref, "quit")
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    current = process_identity(identity["pid"]) if identity else None
                    if current is None or current["state"] == "Z" or current["startTicks"] != identity["startTicks"]:
                        break
                    time.sleep(.1)
                else:
                    raise TimeoutError("owned QEMU process did not retire")
                receipt["originalQemuRetired"] = True
                run(["qemu-img", "info", "--output=json", vm_directory / "disk.qcow2"])
                receipt["diskLockReleased"] = True
                if endpoint:
                    parsed = urlsplit(endpoint["url"])
                    with socket.socket() as probe:
                        probe.settimeout(2)
                        need(probe.connect_ex(("127.0.0.1", parsed.port)) != 0, "owned console listener remains")
                serial = vm_directory / "serial.log"
                if serial.exists():
                    shutil.copyfile(serial, output / "serial.log")
            except Exception as caught:
                cleanup_errors.append(str(caught))
        receipt["cleanupErrors"] = cleanup_errors
        receipt["cleanupComplete"] = not cleanup_errors
        write(output / "diagnosis.json", receipt)
        if not cleanup_errors:
            shutil.rmtree(work)
    need(error is None, receipt.get("error", "pristine diagnosis failed"))
    need(receipt["cleanupComplete"], "pristine diagnosis cleanup failed; state retained at " + str(work))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--corral-source", type=Path, required=True)
    parser.add_argument("--helper", type=Path, required=True)
    diagnose(parser.parse_args())
