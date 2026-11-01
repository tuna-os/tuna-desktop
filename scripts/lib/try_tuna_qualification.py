#!/usr/bin/env python3
"""Real native CLI/noVNC/pristine-account qualification for the Tuna launcher."""
import base64
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import runpy
import secrets
import shlex
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time

import try_tuna_desktop as launcher

ROOT = Path(__file__).resolve().parents[2]
CONSUMER = ROOT / "scripts/lib/try_tuna_desktop.py"


def browser_process(url):
    """A real native Chromium executable, not a simulated browser opener."""
    for key in launcher.PROOF_DEBUG_KEYS:
        os.environ.pop(key, None)
    from playwright.sync_api import sync_playwright
    profile = launcher.private_directory(os.environ["TUNA_VM_BROWSER_PROFILE"])
    with sync_playwright() as driver:
        executable = driver.chromium.executable_path
    launcher.require(Path(executable).is_file(), "native Chromium is not installed")
    launcher.verify_host_binary(executable, launcher.host_platform())
    process = None
    requested_stop = None
    def stop(signum, _frame):
        nonlocal requested_stop
        requested_stop = signum
        if process is not None and process.poll() is None:
            process.send_signal(signum)
    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGHUP, stop)
    if requested_stop is not None:
        raise SystemExit(128 + requested_stop)
    process = subprocess.Popen([executable, "--headless=new", "--no-first-run", "--no-default-browser-check", "--no-sandbox",
                                "--remote-debugging-address=127.0.0.1", "--remote-debugging-port=0",
                                "--user-data-dir=" + str(profile), url], stdin=subprocess.DEVNULL, env=launcher.trusted_environment())
    try:
        if requested_stop is not None:
            process.send_signal(requested_stop)
        launcher.atomic_json(profile.parent / (profile.name + "-identity.json"),
                             {"executable": executable, "sha256": launcher.sha256(executable), "pid": process.pid,
                              "system": platform.system(), "machine": platform.machine(),
                              "profile": str(profile), "url": url})
        returncode = process.wait()
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait()
    launcher.atomic_json(profile.parent / (profile.name + "-exit.json"),
                         {"pid": process.pid, "returncode": returncode, "ownedChildWaited": True})
    raise SystemExit(returncode if returncode >= 0 else 128 - returncode)


def no_rfb_opener(url):
    """Observe an actual successful OS opener without connecting any browser."""
    profile = launcher.private_directory(os.environ["TUNA_VM_BROWSER_PROFILE"])
    executable = "/usr/bin/true"
    process = subprocess.Popen([executable, url], stdin=subprocess.DEVNULL, env=launcher.trusted_environment())
    returncode = process.wait()
    launcher.atomic_json(profile.parent / (profile.name + "-opener-exit.json"),
                         {"executable": executable, "sha256": launcher.sha256(executable), "pid": process.pid,
                          "profile": str(profile), "url": url, "returncode": returncode, "ownedChildWaited": True})
    raise SystemExit(returncode)


class Screen:
    def __init__(self, console, output):
        self.console, self.output = console, output
        self.sequence = 0

    def observe(self, milestone=None):
        self.console.connected(timeout=60000)
        name = milestone or "current"
        path = self.output / (name + ".png")
        self.console.grab(path)
        text = launcher.run(["tesseract", path, "stdout", "--psm", "6"]).decode(errors="replace")
        (self.output / (name + ".txt")).write_text(text)
        raw = launcher.run(["tesseract", path, "stdout", "--psm", "6", "tsv"]).decode(errors="replace")
        rows = []
        for line in raw.splitlines()[1:]:
            columns = line.split("\t", 11)
            if len(columns) != 12 or not columns[11].strip():
                continue
            try:
                rows.append({"text": columns[11].strip(), "x": int(columns[6]), "y": int(columns[7]),
                             "width": int(columns[8]), "height": int(columns[9]),
                             "line": tuple(columns[1:5])})
            except ValueError:
                continue
        return text, rows

    def click(self, phrase, rows):
        groups = {}
        for word in rows:
            groups.setdefault(word["line"], []).append(word)
        candidates = []
        for words in groups.values():
            text = " ".join(word["text"] for word in words)
            if re.search(phrase, text, re.IGNORECASE):
                candidates.append(words)
        launcher.require(candidates, "visible console control absent: " + phrase)
        # Screen text is evidence-derived; no hardcoded firstboot coordinates.
        words = min(candidates, key=lambda value: sum(word["width"] for word in value))
        x = sum(word["x"] + word["width"] / 2 for word in words) / len(words)
        y = sum(word["y"] + word["height"] / 2 for word in words) / len(words)
        canvas = self.console.page.locator("#vnc-canvas")
        box = canvas.bounding_box()
        width, height = self.console.size()
        self.console.page.mouse.click(box["x"] + x * box["width"] / width,
                                      box["y"] + y * box["height"] / height)
        self.console.wait(750)

    def text(self, value):
        self.console.page.keyboard.type(value, delay=15)
        self.console.wait(500)

    def key(self, chord, wait=750):
        self.console.key(chord, wait=wait)

    def wait_text(self, pattern, *, timeout=1800, milestone=None):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            text, rows = self.observe()
            if re.search(pattern, text, re.IGNORECASE):
                if milestone:
                    self.observe(milestone)
                return text, rows
            self.console.wait(2000)
        raise TimeoutError("visible guest state did not arrive: " + pattern)

    def markers(self, markers, *, timeout=120, milestone):
        wanted = [re.sub("[^A-Z0-9]", "", marker.upper()) for marker in markers]
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            text, _ = self.observe()
            actual = re.sub("[^A-Z0-9]", "", text.upper())
            if all(marker in actual for marker in wanted):
                self.observe(milestone)
                return
            self.console.wait(1500)
        raise TimeoutError("actual guest probe did not establish " + ", ".join(markers))

    def shell(self, script):
        # Expected result markers never appear in input echo: only the encoded
        # command is typed. A failed command cannot pass by photographing itself.
        encoded = base64.b64encode(script.encode()).decode()
        self.text("printf %s " + shlex.quote(encoded) + " | base64 -d | bash")
        self.key("Return")


def setup_account(screen, user, password, full_name):
    screen.wait_text(r"Welcome|Language|Start Setup", milestone="01-pristine-firstboot")
    for step in range(24):
        text, rows = screen.observe(f"setup-{step:02d}")
        lower = text.lower()
        if "start using" in lower:
            screen.click(r"Start Using", rows)
            return
        if "about you" in lower or "full name" in lower:
            screen.click(r"Full Name", rows)
            screen.key("Control+a")
            screen.text(full_name)
            _, rows = screen.observe()
            screen.click(r"Username|User Name", rows)
            screen.key("Control+a")
            screen.text(user)
            _, rows = screen.observe()
            screen.click(r"^Next$", rows)
        elif "confirm" in lower and "password" in lower:
            screen.click(r"^Password$", rows)
            screen.text(password)
            _, rows = screen.observe()
            screen.click(r"Confirm", rows)
            screen.text(password)
            _, rows = screen.observe()
            screen.click(r"^Next$", rows)
        elif "time zone" in lower or "timezone" in lower or "where are you" in lower:
            screen.click(r"Search|City", rows)
            screen.text("London")
            screen.console.wait(2000)
            _, rows = screen.observe()
            screen.click(r"London", rows)
            _, rows = screen.observe()
            screen.click(r"^Next$", rows)
        elif any(re.search(r"\bNext\b", row["text"], re.I) for row in rows):
            screen.click(r"^Next$", rows)
        elif any(re.search(r"\bSkip\b", row["text"], re.I) for row in rows):
            screen.click(r"^Skip$", rows)
        elif "start setup" in lower:
            screen.click(r"Start Setup", rows)
        else:
            raise RuntimeError("unrecognized real firstboot page; captured evidence retained")
        screen.console.wait(2000)
    raise TimeoutError("firstboot account setup did not complete")


def tty_login(screen, user, password, *, existing=False):
    screen.key("Control+Alt+F3", wait=5000)
    if not existing:
        screen.wait_text(r"login:", timeout=180)
        screen.text(user)
        screen.key("Return")
        screen.wait_text(r"Password:", timeout=60)
        screen.text(password)
        screen.key("Return", wait=3000)
    # Switch off input echo before probes. All pass markers are encoded in the
    # subsequent shell input and appear only as actual command output.
    screen.text("stty -echo; clear")
    screen.key("Return")


def session_probe(screen, user, package_version, nonce, *, persisted=False, rebooted=False):
    quote = shlex.quote
    marker = "".join(secrets.choice("ABCDEFGHJKLMNPQRSTUVWXYZ") for _ in range(12))
    script = "set -eu\n"
    script += f"test \"$(id -un)\" = {quote(user)}\n"
    script += "id -nG | tr ' ' '\\n' | grep -qx wheel\n"
    script += f"test \"$(pacman -Q tuna-desktop)\" = {quote('tuna-desktop ' + package_version)}\n"
    script += "session=\nfor candidate in $(loginctl show-user \"$USER\" -p Sessions --value); do\n"
    script += " case \"$(loginctl show-session \"$candidate\" -p Desktop --value)\" in [Tt]una) session=$candidate;; esac\ndone\n"
    script += "test -n \"$session\"\n"
    script += "found=0\nfor exe in /proc/[0-9]*/exe; do\n"
    script += " if test \"$(readlink \"$exe\" 2>/dev/null || :)\" = /usr/bin/tuna-compositor && test \"$(stat -c %u \"${exe%/exe}\")\" = \"$(id -u)\"; then tr '\\0' '\\n' < \"${exe%/exe}/environ\" | grep -qx XDG_SESSION_DESKTOP=tuna; found=1; fi\ndone\n"
    script += "test \"$found\" = 1\n"
    if persisted:
        script += f"test \"$(cat ~/tuna-launcher-user-data.txt)\" = {quote(nonce)}\n"
        comparison = "!=" if rebooted else "="
        script += f"test \"$(cat /proc/sys/kernel/random/boot_id)\" {comparison} \"$(cat ~/tuna-launcher-boot-id.txt)\"\n"
    else:
        script += f"printf '%s\\n' {quote(nonce)} > ~/tuna-launcher-user-data.txt\n"
        script += "cat /proc/sys/kernel/random/boot_id > ~/tuna-launcher-boot-id.txt\n"
    script += f"printf 'TUNA_SESSION_{marker}\\nWHEEL_{marker}\\nPACKAGE_{marker}\\n'\n"
    script += "sudo -k\ncommand -v sudo >/dev/null\nset +e\nmessage=$(sudo -n -v 2>&1)\nstatus=$?\nset -e\n"
    script += "test \"$status\" != 0\nprintf '%s' \"$message\" | grep -qi 'password is required'\n"
    script += f"printf 'PASSWORD_REQUIRED_{marker}\\n'\n"
    screen.shell(script)
    screen.markers([f"TUNA_SESSION_{marker}", f"WHEEL_{marker}", f"PACKAGE_{marker}", f"PASSWORD_REQUIRED_{marker}"],
                   milestone="session-probe-" + ("persistent" if persisted else "initial"))


def session_ui(screen, milestone):
    # Activate the real user's verified logind desktop, not a hardcoded VT.
    # The query is deliberately shorter than the expected application result:
    # a screenshot of typed input alone cannot satisfy this UI assertion.
    screen.shell("set -eu\nsession=\nfor candidate in $(loginctl show-user \"$USER\" -p Sessions --value); do\n"
                 " case \"$(loginctl show-session \"$candidate\" -p Desktop --value)\" in [Tt]una) session=$candidate;; esac\ndone\n"
                 "test -n \"$session\"\nloginctl activate \"$session\"\n")
    screen.wait_text(r"\b[0-2]?[0-9]:[0-5][0-9]\b", timeout=600, milestone=milestone + "-desktop")
    screen.key("Escape")
    screen.key("Meta")
    screen.text("sett")
    screen.wait_text(r"\bSettings\b", timeout=120, milestone=milestone + "-settings-result")
    screen.key("Escape")
    screen.observe(milestone + "-restored")
    screen.key("Control+Alt+F3", wait=3000)


def sudo_authenticate(screen, password):
    marker = "".join(secrets.choice("ABCDEFGHJKLMNPQRSTUVWXYZ") for _ in range(12))
    screen.shell("sudo -v && printf 'SUDO_AUTH_" + marker + "\\n'")
    screen.wait_text(r"password for", timeout=60)
    screen.text(password)
    screen.key("Return")
    screen.markers(["SUDO_AUTH_" + marker], milestone="sudo-password-authenticated")


def gdm_login(screen, user, password, full_name, milestone):
    _, rows = screen.wait_text(re.escape(full_name) + r"|Not listed|Username|User Name", milestone=milestone + "-greeter")
    text = " ".join(row["text"] for row in rows)
    if re.search(re.escape(full_name), text, re.I):
        screen.click(re.escape(full_name), rows)
    else:
        if re.search("Not listed", text, re.I):
            screen.click("Not listed", rows)
        screen.text(user)
        screen.key("Return")
    screen.wait_text("Password", timeout=120)
    screen.text(password)
    screen.key("Return", wait=5000)
    screen.observe(milestone + "-after-password")


def qualify(options):
    for key in launcher.PROOF_DEBUG_KEYS:
        os.environ.pop(key, None)
    from playwright.sync_api import sync_playwright
    def cancel(_signum, _frame):
        launcher.CANCELLED = True
    signal.signal(signal.SIGINT, cancel)
    signal.signal(signal.SIGTERM, cancel)
    signal.signal(signal.SIGHUP, cancel)
    launcher.require(launcher.host_platform() == options.platform, "qualification platform is not the actual native host")
    output = options.evidence_dir.resolve()
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    work = Path(tempfile.mkdtemp(prefix="tuna-native-", dir="/tmp"))
    state = launcher.private_directory(work / "state")
    checks = dict.fromkeys(launcher.CHECKS, False)
    receipt = {"schema": 1, "platform": options.platform, "manifestSha256": None, "diskSha256": None,
               "corralSha256": None, "producerRunId": None, "producerCommit": None,
               "host": {"system": platform.system(), "machine": platform.machine(), "qemuVersion": None,
                        "qemuSha256": None, "guestArchitecture": None, "accelerator": None, "firmwareCodeSha256": None},
               "checks": checks, "files": {}}
    child = None
    guard = None
    corral = None
    known_records = []
    failure = None
    admitted = None
    consumer_log = (output / "launcher.log").open("ab")
    def command(*args):
        return launcher.run([corral, "--backend", "qemu", *args], cancellable=False)
    console_urls = set()
    readiness_deadline = None
    def start(browser=True, opener=None):
        nonlocal child, readiness_deadline
        profile = launcher.private_directory(work / ("browser-" + secrets.token_hex(8)))
        env = dict(launcher.trusted_environment(), TUNA_VM_BROWSER_PROFILE=str(profile))
        args = [sys.executable, str(CONSUMER), "launch", "--manifest", str(options.artifact_dir / "manifest.json"),
                "--bundle", str(options.artifact_dir / "manifest.json.sigstore.json"),
                "--manifest-sha256", receipt["manifestSha256"], "--artifact-dir", str(options.artifact_dir),
                "--state-dir", str(state), "--browser"]
        args += opener if opener is not None else ([sys.executable, str(Path(__file__).resolve())] if browser else [shutil.which("false")])
        child = subprocess.Popen(args, env=env, stdout=subprocess.PIPE, stderr=consumer_log, start_new_session=True)
        endpoint = launcher.read_event(child, 900)
        readiness_deadline = time.monotonic() + 180
        launcher.require(endpoint.get("event") == "console-listening", "native CLI did not hand off its actual console")
        launcher.require(endpoint["url"] not in console_urls, "native CLI reused a previous console URL")
        console_urls.add(endpoint["url"])
        current_runtime(endpoint)
        return endpoint, profile
    def current_runtime(endpoint):
        value = json.loads((state / "receipt.json").read_text())
        launcher.require(value.get("console", {}).get("url") == endpoint["url"]
                         and value.get("manifestSha256") == receipt["manifestSha256"]
                         and value.get("cleanupComplete") is False,
                         "native readiness receipt does not belong to the current signed launch")
        return value
    def observe_until(predicate, deadline, message):
        # Consume only the existing human CLI output while observing owned
        # helper files/receipts; no synthetic readiness event or fixed sleep.
        while time.monotonic() < deadline:
            launcher.require(child.poll() is None, "native CLI exited while observing " + message)
            if predicate():
                return
            if select.select([child.stdout], [], [], min(.1, max(0, deadline - time.monotonic())))[0]:
                data = os.read(child.stdout.fileno(), 65536)
                launcher.require(data, "native CLI closed its output while observing " + message)
                consumer_log.write(data)
        raise TimeoutError(message)
    def browser_identity(profile, endpoint):
        identity = json.loads((profile.parent / (profile.name + "-identity.json")).read_text())
        launcher.require(identity.get("profile") == str(profile) and identity.get("url") == endpoint["url"]
                         and identity.get("system") == platform.system() and identity.get("machine") == platform.machine()
                         and type(identity.get("pid")) is int and identity["pid"] > 0,
                         "native Chromium identity is not from this profile/host/console")
        launcher.verify_host_binary(identity["executable"], options.platform)
        launcher.require(identity["sha256"] == launcher.sha256(identity["executable"]),
                         "native Chromium executable changed")
        return identity
    def active_browser(browser, console, profile, endpoint, identity):
        launcher.require(browser_identity(profile, endpoint) == identity, "current native Chromium identity changed")
        processes = browser.new_browser_cdp_session()
        try:
            actual = processes.send("SystemInfo.getProcessInfo")["processInfo"]
        finally:
            processes.detach()
        launcher.require([entry["id"] for entry in actual if entry["type"] == "browser"] == [identity["pid"]],
                         "CDP is not attached to the actual owned native Chromium process")
        launcher.require(console.page.url == endpoint["url"]
                         and console.page.evaluate('document.body.dataset.connected === "true" && !!document.querySelector("#vnc-canvas")'),
                         "actual current Chromium page has no RFB connection")
        value = current_runtime(endpoint)
        launcher.require(value.get("browserRfbConnected") is True, "current consumer receipt has no RFB connection")
        return value
    def connect_browser(driver, endpoint, profile, label):
        port_file = profile / "DevToolsActivePort"
        identity_file = profile.parent / (profile.name + "-identity.json")
        observe_until(lambda: port_file.is_file() and identity_file.is_file(),
                      min(readiness_deadline, time.monotonic() + 120), "actual native browser debugging endpoint absent")
        identity = browser_identity(profile, endpoint)
        launcher.require(Path(identity["executable"]).resolve() == Path(driver.chromium.executable_path).resolve(),
                         "owned native Chromium differs from the actual installed browser")
        port = int(port_file.read_text().splitlines()[0])
        browser = driver.chromium.connect_over_cdp(f"http://127.0.0.1:{port}",
                                                  timeout=max(1, int((readiness_deadline - time.monotonic()) * 1000)))
        pages = browser.contexts[0].pages
        launcher.require(len(pages) == 1 and pages[0].url == endpoint["url"], "browser did not open the owned noVNC URL")
        pages[0].set_viewport_size({"width": 1400, "height": 1000})
        Console = runpy.run_path(str(ROOT / "scripts/tuna-corral-journey"))["Console"]
        console = Console(driver, endpoint["url"], browser=browser, page=pages[0])
        console.connected(timeout=max(1, min(60000, int((readiness_deadline - time.monotonic()) * 1000))))
        observe_until(lambda: current_runtime(endpoint).get("browserRfbConnected") is True,
                      readiness_deadline, "current native RFB readiness receipt absent")
        value = active_browser(browser, console, profile, endpoint, identity)
        launcher.atomic_json(output / (label + "-identity.json"), identity)
        launcher.atomic_json(output / (label + "-connected.json"),
                             {"console": value["console"], "browserRfbConnected": True, "browserPid": identity["pid"],
                              "profile": str(profile), "nativeHeaderVerified": True, "version": browser.version})
        return browser, console, identity
    def browser_retired(profile, identity, label):
        exit_file = profile.parent / (profile.name + "-exit.json")
        launcher.require(exit_file.is_file(), "owned native Chromium helper did not report actual child retirement")
        value = json.loads(exit_file.read_text())
        launcher.require(value.get("pid") == identity["pid"] and value.get("ownedChildWaited") is True
                         and type(value.get("returncode")) is int,
                         "native Chromium retirement does not match its owned current process")
        launcher.atomic_json(output / (label + "-exit.json"), value)
        return value
    def finish(expected=0, cancel_signal=None, connected=None, early_opener=None):
        if cancel_signal is not None:
            launcher.require(connected is not None, "cancellation requires an actual connected native Chromium")
            browser, console, profile, endpoint, identity, label = connected
            active_browser(browser, console, profile, endpoint, identity)
            launcher.require(child.poll() is None, "native CLI exited before the cancellation signal")
            child.send_signal(cancel_signal)
        else:
            stop_started = time.monotonic()
            launcher.run([sys.executable, CONSUMER, "stop", "--state-dir", state], timeout=180)
            launcher.require(time.monotonic() - stop_started < 150, "actual stop did not acknowledge within its existing deadline")
        child.wait(timeout=180)
        launcher.require(child.returncode == expected if cancel_signal is None else child.returncode in (0, 1, 128 + cancel_signal), "native CLI returned unexpected lifecycle status")
        value = json.loads((state / "receipt.json").read_text())
        launcher.require(value["cleanupComplete"] is True and value["diskLockReleased"] is True
                         and value["userDataRetained"] is True and Path(value["userDataPath"]).is_file(), "actual lifecycle cleanup/data retention failed")
        if cancel_signal is not None:
            launcher.require(value.get("console", {}).get("url") == endpoint["url"] and value.get("browserRfbConnected") is True,
                             "cancellation cleanup receipt is not the connected current launch")
            browser_retired(profile, identity, label)
            value["qualificationCancellationSignal"] = signal.Signals(cancel_signal).name
            value["qualificationCliReturncode"] = child.returncode
        else:
            value["qualificationStopAcknowledged"] = True
        if early_opener is not None:
            launcher.require(value.get("console", {}).get("url") == early_opener["url"]
                             and value.get("browserRfbConnected") is False,
                             "early stop claimed RFB readiness or returned a stale cleanup receipt")
            value["qualificationEarlyStop"] = True
            value["qualificationOpener"] = early_opener
        known_records.append(value["vm"])
        launcher.atomic_json(output / ("lifecycle-" + str(len(known_records)) + ".json"), value)
        if guard:
            launcher.require(json.loads(command("qmp", guard, "query-status"))["running"] is True, "launcher stopped the unrelated guard VM")
        return value
    try:
        admitted = launcher.admit(options.artifact_dir / "manifest.json", options.artifact_dir / "manifest.json.sigstore.json", state, options.artifact_dir)
        manifest, manifest_sha, corral, pristine, tools = admitted
        launcher.require(os.environ.get("GITHUB_SHA") == manifest["producer"]["commit"], "native observer source differs from signed producer source")
        receipt.update(manifestSha256=manifest_sha, diskSha256=launcher.sha256(pristine), corralSha256=launcher.sha256(corral),
                       producerRunId=manifest["producer"]["runId"], producerCommit=manifest["producer"]["commit"])
        checks["signatureAdmission"] = True
        receipt["host"].update(qemuVersion=launcher.run([tools["qemu-system-x86_64"], "--version"]).decode().strip(),
                               qemuSha256=launcher.sha256(tools["qemu-system-x86_64"]), guestArchitecture=manifest["disk"]["architecture"])
        endpoint, profile = start()
        runtime = json.loads((state / "receipt.json").read_text())
        vm_record = runtime["vm"]
        known_records.append(vm_record)
        command_record = json.loads((Path(vm_record["directory"]) / "command.json").read_text())
        launcher.require(Path(command_record["binary"]).resolve() == Path(tools["qemu-system-x86_64"]).resolve(), "actual guest emulator differs")
        firmware = [arg for arg in command_record["args"] if "if=pflash" in arg and "readonly=on" in arg]
        launcher.require(len(firmware) == 1, "actual amd64 UEFI code drive missing")
        code = dict(part.split("=", 1) for part in firmware[0].split(","))["file"]
        receipt["host"].update(accelerator=runtime["runtime"]["acceleration"], firmwareCodeSha256=launcher.sha256(code))
        checks["declaredGuestArchitecture"] = (runtime["runtime"]["architecture"] == "amd64" and runtime["runtime"]["firmware"] == "uefi")
        launcher.require(json.loads(command("qmp", "qemu//local/" + vm_record["name"], "query-status"))["running"] is True, "native QEMU is not executing")
        target = json.loads(command("qmp", "qemu//local/" + vm_record["name"], "query-target"))
        kvm = json.loads(command("qmp", "qemu//local/" + vm_record["name"], "query-kvm"))
        selected = receipt["host"]["accelerator"]
        machines = [command_record["args"][index + 1] for index, value in enumerate(command_record["args"][:-1]) if value == "-machine"]
        launcher.require(target.get("arch") == "x86_64" and machines == ["q35,accel=" + selected]
                         and kvm.get("enabled") is (selected == "kvm"),
                         "actual guest target/accelerator differs from declared runtime")
        launcher.atomic_json(output / "native-runtime.json", {"target": target, "kvm": kvm, "command": command_record})
        checks["nativeExecution"] = True
        # Attach to the actual Chromium process opened by the real CLI, not a
        # second simulated browser. Reuse the existing journey console driver.
        nonce = "".join(secrets.choice("ABCDEFGHJKLMNPQRSTUVWXYZ") for _ in range(12))
        user, password, full_name = "tuna" + secrets.token_hex(4), secrets.token_urlsafe(24), "Tuna Launcher " + nonce
        with sync_playwright() as driver:
            browser, console, identity = connect_browser(driver, endpoint, profile, "browser-first")
            launcher.atomic_json(output / "browser-runtime.json", {"version": browser.version, "nativePlatform": options.platform})
            checks["browserRfbConnected"] = True
            screen = Screen(console, output)
            setup_account(screen, user, password, full_name)
            tty_login(screen, user, password)
            checks["pristineFirstbootAccountCreated"] = True
            session_probe(screen, user, manifest["package"]["version"], nonce)
            session_ui(screen, "02-first-tuna-session")
            checks["pristineTunaSession"] = True
            sudo_authenticate(screen, password)
            checks["adminWheelPasswordRequired"] = True
            # Real user-initiated logout through logind, followed by the actual
            # GDM password UI. No root SSH, fixture account or session override.
            screen.shell("for s in $(loginctl show-user \"$USER\" -p Sessions --value); do case \"$(loginctl show-session \"$s\" -p Desktop --value)\" in [Tt]una) loginctl terminate-session \"$s\";; esac; done\n")
            screen.key("Control+Alt+F1", wait=5000)
            gdm_login(screen, user, password, full_name, "logout-login")
            tty_login(screen, user, password, existing=True)
            session_probe(screen, user, manifest["package"]["version"], nonce, persisted=True)
            session_ui(screen, "03-tuna-after-password-login")
            checks["pristineLogoutLogin"] = True
            sudo_authenticate(screen, password)
            screen.shell("sudo -n systemctl reboot\n")
            console.wait(30000)
            gdm_login(screen, user, password, full_name, "reboot-login")
            tty_login(screen, user, password)
            session_probe(screen, user, manifest["package"]["version"], nonce, persisted=True, rebooted=True)
            session_ui(screen, "04-tuna-after-reboot")
            checks["rebootPersistence"] = True
            screen.observe("03-created-user-persisted")
            # CDP Browser.close targets the real Chromium process; the
            # Playwright browser.close() method would only detach from CDP.
            session = browser.new_browser_cdp_session()
            session.send("Browser.close")
            exit_file = profile.parent / (profile.name + "-exit.json")
            observe_until(exit_file.is_file, time.monotonic() + 60,
                          "actual Chromium process did not retire after browser close")
            browser_exit = browser_retired(profile, identity, "browser-first")
            launcher.require(browser_exit.get("returncode") == 0,
                             "actual native browser did not close successfully")
            launcher.atomic_json(output / "browser-close.json", browser_exit)
        # Closing a browser disconnects only; the VM/data must remain alive.
        launcher.require(json.loads(command("qmp", "qemu//local/" + vm_record["name"], "query-status"))["running"] is True, "browser close silently stopped the user VM")
        guard_name = "tuna-guard-" + secrets.token_hex(8)
        def display(name):
            value = 0
            for char in name:
                value = (value * 31 + ord(char)) % 100
            return value
        while display(guard_name) + 5900 == runtime["runtime"]["vnc_port"]:
            guard_name += "g"
        guard = "qemu//local/" + guard_name
        command("create", guard_name, "--loopback", "--arch", "amd64", "--disk", "64M", "--mem", "128M", "--cpu", "1")
        command("start", guard)
        finish()
        # A successful OS opener is not browser/RFB readiness. Stop must remain
        # serviceable while the real signed VM/console await a connection.
        endpoint, profile = start(opener=[sys.executable, str(Path(__file__).resolve()), "no-rfb-opener"])
        opener_exit = profile.parent / (profile.name + "-opener-exit.json")
        observe_until(opener_exit.is_file, readiness_deadline, "actual no-RFB OS opener did not exit")
        opener = json.loads(opener_exit.read_text())
        launcher.require(opener.get("executable") == "/usr/bin/true"
                         and opener.get("sha256") == launcher.sha256("/usr/bin/true")
                         and opener.get("profile") == str(profile) and opener.get("url") == endpoint["url"]
                         and type(opener.get("pid")) is int and opener["pid"] > 0
                         and opener.get("returncode") == 0 and opener.get("ownedChildWaited") is True,
                         "early stop did not observe its actual successful OS opener")
        early_runtime = current_runtime(endpoint)
        launcher.require(early_runtime.get("browserRfbConnected") is False,
                         "no-RFB OS opener unexpectedly established readiness")
        launcher.require(json.loads(command("qmp", "qemu//local/" + early_runtime["vm"]["name"], "query-status"))["running"] is True,
                         "early-stop negative did not run the actual VM")
        launcher.atomic_json(output / "early-stop-opener-exit.json", opener)
        launcher.atomic_json(output / "early-stop-awaiting-readiness.json", early_runtime)
        early_cleanup = finish(early_opener=opener)
        launcher.atomic_json(output / "early-stop-cleanup.json", early_cleanup)
        checks["stopCleanup"] = True
        checks["diskLockReleased"] = True
        for cancel_signal in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            endpoint, profile = start()
            label = "browser-cancel-" + signal.Signals(cancel_signal).name.lower()
            with sync_playwright() as driver:
                browser, console, identity = connect_browser(driver, endpoint, profile, label)
                finish(cancel_signal=cancel_signal, connected=(browser, console, profile, endpoint, identity, label))
        checks["cancellationCleanup"] = True
        start(browser=False)
        child.wait(timeout=180)
        launcher.require(child.returncode != 0, "failing browser command was accepted")
        failed = json.loads((state / "receipt.json").read_text())
        launcher.require(failed["cleanupComplete"] and failed["diskLockReleased"] and not failed["browserRfbConnected"], "browser failure leaked owned resources or claimed readiness")
        launcher.atomic_json(output / "browser-failure-cleanup.json", failed)
        checks["browserFailureCleanup"] = True
        launcher.require(json.loads(command("qmp", guard, "query-status"))["running"] is True, "failure stopped the unrelated guard VM")
        checks["ownershipIsolation"] = True
        invalid = subprocess.run([sys.executable, str(CONSUMER), "launch", "--manifest", str(options.artifact_dir / "manifest.json"),
                                  "--bundle", str(options.artifact_dir / "manifest.json.sigstore.json"), "--artifact-dir", str(options.artifact_dir),
                                  "--state-dir", str(state), "--memory", "0"], capture_output=True, timeout=900)
        (output / "preflight-failure.txt").write_bytes(invalid.stderr)
        launcher.require(invalid.returncode != 0 and b"unitful memory" in invalid.stderr, "invalid launch was not rejected at its real preflight boundary")
        launcher.require(json.loads(command("qmp", guard, "query-status"))["running"] is True, "preflight failure changed an unrelated VM")
        checks["failureCleanup"] = True
        checks["signatureAdmission"] = False
        for kind, blob, bundle, identity, source in (
                ("manifest", options.artifact_dir / "manifest.json", options.artifact_dir / "manifest.json.sigstore.json",
                 launcher.MANIFEST_IDENTITY, manifest["producer"]["commit"]),
                ("corral", corral, corral.with_name(corral.name + ".sigstore.json"),
                 manifest["corral"]["certificateIdentity"], manifest["corral"]["commit"])):
            wrong_source = "0" * 40 if source != "0" * 40 else "f" * 40
            try:
                launcher.verify_blob(tools["cosign"], blob, bundle, identity, wrong_source)
            except launcher.CommandFailure as error:
                (output / ("signature-source-rejection-" + kind + ".txt")).write_text(str(error) + "\n")
            else:
                raise RuntimeError("actual Sigstore verifier accepted a different " + kind + " source commit")
        tampered = work / "tampered.json"
        tampered.write_bytes((options.artifact_dir / "manifest.json").read_bytes() + b" ")
        try:
            launcher.admit(tampered, options.artifact_dir / "manifest.json.sigstore.json", state, options.artifact_dir,
                           hashlib.sha256(tampered.read_bytes()).hexdigest())
        except launcher.CommandFailure as error:
            (output / "signature-rejection.txt").write_text(str(error) + "\n")
        else:
            raise RuntimeError("actual Sigstore verifier accepted changed manifest bytes")
        launcher.require(launcher.sha256(pristine) == receipt["diskSha256"], "qualification changed pristine shipping disk")
        checks["signatureAdmission"] = True
        launcher.require(all(checks.values()), "native acceptance criteria incomplete")
    except Exception as caught:
        failure = caught
        (output / "failure.txt").write_text(f"{type(caught).__name__}: {caught}\n")
    finally:
        if child is not None and child.poll() is None:
            child.send_signal(signal.SIGTERM)
            try:
                child.wait(timeout=180)
            except subprocess.TimeoutExpired:
                # Do not unlink a still-active VM's state or claim cleanup.
                failure = failure or TimeoutError("native CLI cancellation did not retire")
        consumer_log.close()
        if corral is not None:
            if guard:
                try:
                    command("stop", guard)
                    command("delete", guard, "--force")
                except Exception as caught:
                    failure = failure or caught
            unique = {record["name"]: record for record in known_records}
            for name, record in unique.items():
                try:
                    directory = Path(record["directory"])
                    info = directory.lstat()
                    launcher.require([info.st_dev, info.st_ino] == record["directoryIdentity"], "qualification VM ownership changed")
                    command("stop", "qemu//local/" + name)
                    command("delete", "qemu//local/" + name, "--force")
                except Exception as caught:
                    failure = failure or caught
        if failure is not None:
            checks["stopCleanup"] = False
            (output / "failure.txt").write_text(f"{type(failure).__name__}: {failure}\n")
        for identity_file in list(work.glob("browser-*-identity.json")) + list(work.glob("browser-*-exit.json")):
            shutil.copyfile(identity_file, output / identity_file.name)
        receipt["files"] = {str(path.relative_to(output)): launcher.sha256(path) for path in sorted(output.rglob("*")) if path.is_file()}
        launcher.atomic_json(output / "qualification.json", receipt)
        if failure is None:
            shutil.rmtree(work)
    if failure is not None:
        raise failure


if __name__ == "__main__":
    no_rfb = len(sys.argv) == 3 and sys.argv[1] == "no-rfb-opener"
    launcher.require((len(sys.argv) == 2 or no_rfb) and sys.argv[-1].startswith("http://127.0.0.1:"),
                     "qualification browser requires the actual loopback console URL")
    if no_rfb:
        no_rfb_opener(sys.argv[-1])
    else:
        browser_process(sys.argv[-1])
