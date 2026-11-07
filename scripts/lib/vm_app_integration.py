"""Host side of the marlin-vm app-integration boot (V-PICKER, V-INTROSPECT).

Drives real QEMU input and display changes against the guest fixtures in
packaging/marlin/vm-lane (tuna-vm-picker, tuna-vm-introspect-watch,
tuna-vm-sandbox-client and the root probe tuna-vm-app-integration, reached
through tuna-vm-lifecycle's app-* actions). The guest never synthesizes
input; every click and key below is a QMP input event on the VM's own
USB tablet and keyboard, and every output change is QEMU's own virtio-gpu
scanout hotplug through its D-Bus display (org.qemu.Display1).
"""
import json
import os
import secrets
import subprocess
import time

# Nautilus file chooser typed-location keys: QEMU qcodes for path characters.
QCODES = {"/": "slash", "-": "minus", ".": "dot", "_": ("shift", "minus")}
TOPLEVEL = (1280, 800)
SECOND = (1024, 768)
PICKER_CASES = (("OpenFile", "cancel"), ("OpenFile", "accept"), ("SaveFile", "cancel"), ("SaveFile", "accept"))


class AppError(RuntimeError):
    pass


def type_path(qmp, text):
    for ch in text:
        code = QCODES.get(ch, ch)
        if isinstance(code, tuple):
            qmp.keys(*code)
        else:
            qmp.keys(code)


def wait_for(predicate, timeout, message, interval=0.5):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = predicate()
        if last:
            return last
        time.sleep(interval)
    raise AppError(message)


def center(rect):
    return rect[0] + rect[2] / 2, rect[1] + rect[3] / 2


def inside(point, rect):
    return rect[0] <= point[0] < rect[0] + rect[2] and rect[1] <= point[1] < rect[1] + rect[3]


def exposed_point(parent, dialog, top=40):
    """A point on the parent not covered by the dialog or the top panel."""
    px, py, pw, ph = parent
    for x, y in ((px + 20, py + ph - 20), (px + pw - 20, py + ph - 20), (px + 20, max(py + 20, top)),
                 (px + pw - 20, max(py + 20, top)), (px + pw / 2, py + ph - 10)):
        if (inside((x, y), parent) and not inside((x, y), dialog) and y >= top
                and 0 <= x < TOPLEVEL[0] and 0 <= y < TOPLEVEL[1]):
            return int(x), int(y)
    return None


class Lane:
    def __init__(self, qmp, connect, out, record, display_bus):
        self.qmp, self.connect, self.out, self.record = qmp, connect, out, record
        self.display_bus = display_bus
        self.agent = connect()
        self.evidence = os.path.join(out, "app-integration")
        os.makedirs(self.evidence, exist_ok=True)
        self.log = []

    def close(self):
        if self.agent is not None:
            self.agent.close()
        with open(os.path.join(self.evidence, "steps.json"), "w") as stream:
            json.dump(self.log, stream, indent=2)

    def app(self, action, *args, timeout=60):
        try:
            value = self.agent.run(action, *args, timeout=timeout)
        except (OSError, RuntimeError, ValueError) as error:
            # A timed-out guest-exec poisons the channel; reconnect once.
            try:
                self.agent.close()
            except (OSError, RuntimeError):
                pass
            self.agent = self.connect()
            raise AppError(f"{action}: {error}") from error
        self.log.append({"action": action, "args": list(args), "result": value, "t": time.time()})
        if isinstance(value, dict) and "app_error" in value:
            raise AppError(f"{action}: {value['app_error']}")
        return value

    def save(self, name, value):
        with open(os.path.join(self.evidence, name), "w") as stream:
            json.dump(value, stream, indent=2)

    def frame(self, name):
        self.qmp.frame(os.path.join(self.evidence, name))

    # ---- QEMU D-Bus display: virtio-gpu scanout hotplug -----------------
    def set_head(self, head, width, height, x=0, y=0):
        """org.qemu.Display1.Console.SetUIInfo: what a real display client
        sends when a monitor window appears/resizes; zero size disconnects."""
        mm = (int(width * 0.264), int(height * 0.264)) if width else (0, 0)
        result = subprocess.run(
            ["gdbus", "call", "--address", self.display_bus, "--dest", "org.qemu",
             "--object-path", f"/org/qemu/Display1/Console_{head}",
             "--method", "org.qemu.Display1.Console.SetUIInfo",
             str(mm[0]), str(mm[1]), str(x), str(y), str(width), str(height)],
            capture_output=True, text=True, timeout=10)
        self.log.append({"action": "qemu-set-ui-info", "head": head, "size": [width, height],
                         "returncode": result.returncode, "stderr": result.stderr[-400:], "t": time.time()})
        if result.returncode:
            raise AppError(f"SetUIInfo head {head}: {result.stderr.strip()[-300:]}")

    # ---- V-PICKER ------------------------------------------------------
    def picker_state(self, role, key):
        return self.app("app-picker-state", role, key)

    def run_picker(self, role):
        """Four Nautilus outcomes for one caller role; returns failures list."""
        failures = []
        bystander_key, caller_key = secrets.token_hex(8), secrets.token_hex(8)
        bystander = self.app("app-picker-start", "bystander", bystander_key, timeout=90)
        caller = self.app("app-picker-start", role, caller_key, timeout=90)
        evidence = {"role": role, "bystander_start": bystander, "caller_start": caller, "cases": []}
        try:
            evidence["requested"] = [1100, 700]
            # GTK publishes its allocation on its next 50 ms tick after map.
            settled = wait_for(lambda: (lambda s: s if s["client"]["allocation"] != [0, 0] else None)(
                self.picker_state(role, caller_key)), 10, f"{role} caller never allocated")
            client, window = settled["client"], settled["window"]
            evidence["settled"] = settled
            size_ok = window["rect"][2:] == [1100, 700] and client["allocation"] == [1100, 700]
            if not size_ok:
                failures.append(f"{role} caller size: managed {window['rect'][2:]}, GTK {client['allocation']}")
            for method, outcome in PICKER_CASES:
                case = self.picker_case(role, caller_key, bystander_key, method, outcome)
                evidence["cases"].append(case)
                if case["failures"]:
                    failures.extend(f"{role} {method} {outcome}: {f}" for f in case["failures"])
        finally:
            for r, key in ((role, caller_key), ("bystander", bystander_key)):
                try:
                    self.app("app-picker-stop", r, key)
                except AppError as error:
                    failures.append(f"{r} stop: {error}")
            self.save(f"picker-{role}.json", evidence)
        return failures

    def picker_case(self, role, key, bystander_key, method, outcome):
        token = secrets.token_hex(6)
        case = {"method": method, "outcome": outcome, "token": token, "failures": []}
        fail = case["failures"].append
        before = self.picker_state(role, key)
        quiet = self.picker_state("bystander", bystander_key)["client"]
        parent = before["window"]
        if parent is None or before["scene"]["focused"] != parent["id"]:
            fail("caller not focused before the case")
            return case
        armed = self.app("app-picker-arm", role, key, method, token)
        case["armed"] = armed
        px, py = (int(v) for v in center(parent["rect"]))
        self.qmp.click(px, py)

        def dialog():
            state = self.picker_state(role, key)
            if state["client"]["clicks"] != before["client"]["clicks"] + 1:
                return None
            children = [w for w in state["scene"]["windows"]
                        if w.get("parent_window_id") == parent["id"] and w["id"] != parent["id"]]
            if len(children) == 1 and state["scene"]["focused"] == children[0]["id"]:
                return state, children[0]
            return None
        try:
            opened, child = wait_for(dialog, 30, "Nautilus dialog did not map as the focused child of the caller")
        except AppError as error:
            fail(str(error))
            case["failure_state"] = self.picker_state(role, key)
            self.frame(f"picker-{role}-{method}-{outcome}-nodialog.png")
            self.qmp.keys("esc")
            return case
        time.sleep(1.5)  # let Nautilus finish its first layout before input
        settled = self.picker_state(role, key)
        child = next((w for w in settled["scene"]["windows"] if w["id"] == child["id"]), child)
        case["dialog"] = child
        self.frame(f"picker-{role}-{method}-{outcome}-dialog.png")
        pc, dc = center(parent["rect"]), center(child["rect"])
        case["center_offset"] = [dc[0] - pc[0], dc[1] - pc[1]]
        if abs(dc[0] - pc[0]) > 2:
            fail(f"dialog not horizontally centred on its parent: offset {case['center_offset']}")
        point = exposed_point(parent["rect"], child["rect"])
        case["exposed_parent_point"] = point
        if point is not None:
            self.qmp.click(*point)
            time.sleep(1)
            blocked = self.picker_state(role, key)
            if blocked["client"]["clicks"] != opened["client"]["clicks"]:
                fail("click on the exposed parent reached the caller under its modal dialog")
            if blocked["scene"]["focused"] != child["id"]:
                fail("click on the exposed parent took focus from the modal dialog")
        if outcome == "cancel":
            self.qmp.keys("esc")
        elif method == "OpenFile":
            # Nautilus type-ahead selects the file in the current (home)
            # folder; Enter then activates Select.
            type_path(self.qmp, os.path.basename(armed["destination"]))
            time.sleep(1.5)
            self.qmp.keys("ret")
        else:
            self.qmp.keys("ret")

        def responded():
            state = self.picker_state(role, key)
            if len(state["client"]["responses"]) > len(before["client"]["responses"]):
                return state
            return None
        try:
            after = wait_for(responded, 30, "portal Response never reached the caller")
        except AppError as error:
            fail(str(error))
            self.frame(f"picker-{role}-{method}-{outcome}-noresponse.png")
            self.qmp.keys("esc")
            return case
        response = after["client"]["responses"][-1]
        case["response"] = response
        name = os.path.basename(armed["destination"])
        if outcome == "cancel":
            if response["response"] != 1 or response["uris"]:
                fail(f"cancel answered {response['response']} with {response['uris']}")
        else:
            if response["response"] != 0 or len(response["uris"]) != 1 or not response["uris"][0].endswith("/" + name):
                fail(f"accept answered {response['response']} with {response['uris']}")
            if method == "SaveFile":
                written = self.app("app-picker-file", role, key, token)
                case["saved"] = written
                if not written["exists"] or written["content"] != f"tuna-vm-picker {key} {token}\n":
                    fail("the granted SaveFile destination was not written by the caller")

        def restored():
            state = self.picker_state(role, key)
            if (state["scene"]["focused"] == parent["id"]
                    and all(w["id"] != child["id"] for w in state["scene"]["windows"])):
                return state
            return None
        try:
            back = wait_for(restored, 15, "focus did not return to the caller after the dialog closed")
        except AppError as error:
            fail(str(error))
            return case
        self.qmp.keys("a")
        self.qmp.click(px, py)
        time.sleep(1)
        final = self.picker_state(role, key)
        quiet_after = self.picker_state("bystander", bystander_key)["client"]
        case["keys"] = [back["client"]["keys"], final["client"]["keys"]]
        case["clicks"] = [back["client"]["clicks"], final["client"]["clicks"]]
        if final["client"]["keys"] != back["client"]["keys"] + 1 or final["client"]["clicks"] != back["client"]["clicks"] + 1:
            fail("keyboard/pointer input did not reach the caller after the dialog closed")
        if (quiet_after["clicks"], quiet_after["keys"]) != (quiet["clicks"], quiet["keys"]):
            fail("the unrelated bystander received input during the case")
        if final["client"]["errors"]:
            fail(f"caller errors: {final['client']['errors']}")
        return case

    # ---- V-INTROSPECT ScreenSize ----------------------------------------
    @staticmethod
    def layout_size(layout):
        """Mutter 51 meta_monitor_manager whole-display size: the union of
        logical monitor rectangles (logical layout mode)."""
        boxes = []
        for lm in layout["logical"]:
            if not lm["mode"]:
                continue
            w, h = lm["mode"]
            if lm["transform"] % 2:
                w, h = h, w
            boxes.append((lm["x"], lm["y"], lm["x"] + round(w / lm["scale"]), lm["y"] + round(h / lm["scale"])))
        if not boxes:
            return [0, 0]
        return [max(b[2] for b in boxes) - min(b[0] for b in boxes), max(b[3] for b in boxes) - min(b[1] for b in boxes)]

    def run_screen_size(self):
        failures = []
        key = secrets.token_hex(8)
        watch = self.app("app-introspect-start", key)
        initial = watch["initial"]
        steps = {"initial": initial}
        if initial["size"] != self.layout_size(initial["layout"]) or initial["size"] != list(TOPLEVEL):
            failures.append(f"initial ScreenSize {initial['size']} vs layout {self.layout_size(initial['layout'])}")
        seen = 0

        def change(predicate, message):
            nonlocal seen

            def arrived():
                value = self.app("app-introspect-state", key)
                fresh = value["changes"][seen:]
                hit = [c for c in fresh if c.get("layout") and predicate(c)]
                return (value, hit[-1]) if hit else None
            value, record = wait_for(arrived, 45, message, interval=1)
            seen = len(value["changes"])
            return record
        try:
            steps["drm_before"] = self.app("app-drm-state")
            self.app("app-drm-watch")
            self.set_head(1, *SECOND, x=TOPLEVEL[0], y=0)
            plugged = change(lambda c: len(c["layout"]["logical"]) == 2,
                             "no ScreenSize change for the hotplugged second output")
            steps["drm_plugged"] = self.app("app-drm-state")
            steps["hotplug"] = plugged
            self.frame("introspect-hotplug.png")
            if plugged["size"] != self.layout_size(plugged["layout"]):
                failures.append(f"hotplug ScreenSize {plugged['size']} vs union {self.layout_size(plugged['layout'])}")
            primary = [lm for lm in plugged["layout"]["logical"] if lm["primary"]]
            if plugged["size"] == (primary[0]["mode"] if primary else None):
                failures.append("hotplug ScreenSize is the primary monitor, not the whole display")
            second = next(lm for lm in plugged["layout"]["logical"] if lm["mode"] == list(SECOND))
            connector = second["monitors"][0]
            rotated_value = self.app("app-rotate", connector, 1)
            steps["rotate_applied"] = rotated_value
            rotated = change(lambda c: any(lm["transform"] == 1 for lm in c["layout"]["logical"]),
                             "no ScreenSize change for rotating the second output")
            steps["rotate"] = rotated
            self.frame("introspect-rotated.png")
            if rotated["size"] != self.layout_size(rotated["layout"]) or rotated["size"] == plugged["size"]:
                failures.append(f"rotated ScreenSize {rotated['size']} vs union {self.layout_size(rotated['layout'])}")
            self.set_head(1, 0, 0)
            unplugged = change(lambda c: len(c["layout"]["logical"]) == 1,
                               "no ScreenSize change for unplugging the second output")
            steps["unplug"] = unplugged
            if unplugged["size"] != list(TOPLEVEL):
                failures.append(f"unplugged ScreenSize {unplugged['size']}")
            final = self.app("app-introspect-state", key)
            steps["notifications"] = [c["size"] for c in final["changes"]]
            sizes = [initial["size"], *steps["notifications"]]
            repeats = [i for i in range(1, len(sizes)) if sizes[i] == sizes[i - 1]]
            if repeats:
                failures.append(f"PropertiesChanged repeated an unchanged ScreenSize: {sizes}")
            if final["errors"]:
                failures.append(f"watcher errors: {final['errors']}")
        except (AppError, StopIteration, KeyError) as error:
            failures.append(f"{type(error).__name__}: {error}")
            try:
                steps["failure_state"] = self.app("app-introspect-state", key)
            except AppError:
                pass
            # Guest kernel's view: did the virtio-gpu event light the
            # connector and raise a DRM HOTPLUG uevent the compositor missed?
            try:
                steps["drm_failure"] = drm = self.app("app-drm-state")
                lit = sorted(name for name, c in drm["connectors"].items() if c["status"] == "connected")
                failures.append(f"guest DRM: connected {lit}, {drm['hotplug_uevents']} HOTPLUG uevents")
            except (AppError, KeyError, TypeError):
                pass
        finally:
            try:
                self.app("app-introspect-stop", key)
            except AppError as error:
                failures.append(f"watcher stop: {error}")
            self.save("introspect-screen-size.json", steps)
        return failures

    # ---- V-INTROSPECT sandbox identity --------------------------------
    def run_sandbox(self):
        failures = []
        app = "org.tuna.VmSandbox"
        steps = {}
        try:
            steps["install"] = self.app("app-flatpak-install", timeout=400)
            steps["baseline"] = self.app("app-background")
            if app in steps["baseline"]["apps"]:
                failures.append("sandbox app already reported before launch")
            steps["forged_launch"] = self.app("app-flatpak-forged")
            wait_for(lambda: any(not c["flatpak_info"] for c in self.app("app-flatpak-processes")["clients"]),
                     20, "forged unsandboxed client did not start")
            time.sleep(3)
            steps["forged"] = self.app("app-background")
            steps["forged_scene"] = self.app("app-scene")
            if not any(w.get("app_id") == app for w in steps["forged_scene"]["windows"]):
                failures.append("forged client claiming the app ID never mapped")
            if app in steps["forged"]["apps"]:
                failures.append("Background.GetAppState trusted a forged, unsandboxed app ID")
            steps["sandboxed_launch"] = self.app("app-flatpak-sandboxed")
            wait_for(lambda: any(c["flatpak_info"] for c in self.app("app-flatpak-processes")["clients"]),
                     60, "Flatpak-sandboxed client did not start")
            steps["sandboxed"] = wait_for(lambda: (lambda v: v if app in v["apps"] else None)(self.app("app-background")),
                                          30, "Background.GetAppState never reported the sandboxed app")
            if steps["sandboxed"]["apps"][app] not in (1, 2):
                failures.append(f"sandboxed app state {steps['sandboxed']['apps'][app]} is not running/active")
            steps["stop"] = self.app("app-flatpak-stop")
            steps["stopped"] = wait_for(lambda: (lambda v: v if app not in v["apps"] else None)(self.app("app-background")),
                                        30, "sandboxed app still reported after it exited")
        except AppError as error:
            failures.append(str(error))
        finally:
            try:
                self.app("app-flatpak-stop")
            except AppError:
                pass
            self.save("introspect-sandbox.json", steps)
        return failures


def run(qmp, connect, out, record, display_bus):
    lane = Lane(qmp, connect, out, record, display_bus)
    try:
        lane.save("env.json", lane.app("app-env"))
        failures = []
        for role in ("x11", "wayland"):
            try:
                failures.extend(lane.run_picker(role))
            except AppError as error:
                failures.append(f"{role}: {error}")
        record("V-PICKER", not failures,
               "; ".join(failures)[:900] if failures else
               "X11 and native Wayland GTK callers: 1100x700 allocation, Nautilus OpenFile/SaveFile "
               "cancel/accept centred modal children, exposed-parent clicks blocked, focus and input "
               "returned to the caller, bystander untouched (app-integration/picker-*.json)")
        size = lane.run_screen_size()
        sandbox = lane.run_sandbox()
        record("V-INTROSPECT-SCREEN", not size,
               "; ".join(size)[:900] if size else
               "Introspect ScreenSize equals the union of Mutter logical monitors and notifies once per "
               "change across virtio-gpu hotplug, rotation and unplug (app-integration/introspect-screen-size.json)")
        record("V-INTROSPECT-SANDBOX", not sandbox,
               "; ".join(sandbox)[:900] if sandbox else
               "xdg-desktop-portal-gnome Background.GetAppState reports the Flatpak-sandboxed app, not an "
               "unsandboxed client claiming its app ID, and drops it on exit (app-integration/introspect-sandbox.json)")
    finally:
        lane.close()
