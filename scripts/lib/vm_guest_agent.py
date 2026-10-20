"""Bounded CI guest-agent transport; only the fixed lifecycle probe is executed.

Protocol: https://www.qemu.org/docs/master/interop/qemu-ga-ref.html
"""
import base64
import binascii
import json
import socket
import time


FOCUS_STAGES = {"arguments", "reader-principal", "helper", "post-observation", "disks-evidence"}
FOCUS_OPERATIONS = {"arguments", "imports", "process-identity", "atspi-timeout",
                    "desktop-enumeration", "atspi-owner", "bus-connection", "bus-credentials",
                    "tree-cache", "tree-state", "tree-role", "tree-name", "tree-children",
                    "controlled-query", "catalog-receipt", "final-principal", "serialization",
                    "tree-state-flags", "tree-public-control"}


def valid_focus_error(value):
    types = {"RuntimeError", "ValueError", "OSError", "FileNotFoundError", "ProcessLookupError",
             "PermissionError", "KeyError", "JSONDecodeError", "UnicodeDecodeError", "RecursionError",
             "TypeError", "AttributeError", "CalledProcessError", "TimeoutExpired", "Error"}
    if not isinstance(value, dict):
        return False
    if set(value) == {"focus_error_type"}:
        staged = False
    elif set(value) in ({"focus_error_type", "focus_stage"},
                         {"focus_error_type", "focus_stage", "focus_operation"}):
        staged = True
    else:
        return False
    if not isinstance(value["focus_error_type"], str) or value["focus_error_type"] not in types:
        return False
    if "focus_operation" in value and (value["focus_stage"] != "helper"
            or type(value["focus_operation"]) is not str or value["focus_operation"] not in FOCUS_OPERATIONS):
        return False
    return not staged or (type(value["focus_stage"]) is str and value["focus_stage"] in FOCUS_STAGES)


def focus_failure_diagnostic(encoded):
    if not isinstance(encoded, str) or len(encoded) > 5464:
        return None
    try:
        raw = base64.b64decode(encoded, validate=True)
        if len(raw) > 4096:
            return None
        root = json.loads(raw)
        if valid_focus_error(root):
            return root
    except (binascii.Error, ValueError, UnicodeDecodeError):
        pass
    return None


def valid_orca_read_error(value):
    types = {"RuntimeError", "CalledProcessError", "FileNotFoundError", "ProcessLookupError",
             "PermissionError", "OSError", "ValueError", "JSONDecodeError", "UnicodeDecodeError",
             "TimeoutExpired", "StopIteration", "KeyError", "IndexError", "TypeError"}
    if not isinstance(value, dict) or not {"exception_type"} <= set(value) or not set(value) <= {"exception_type", "errno", "returncode", "dbus_method", "stderr"}:
        return False
    if not isinstance(value["exception_type"], str) or value["exception_type"] not in types:
        return False
    for key, bound in (("errno", 4095), ("returncode", 255)):
        if key in value and (type(value[key]) is not int or not -bound <= value[key] <= bound):
            return False
    if "returncode" in value and value["exception_type"] != "CalledProcessError":
        return False
    if "errno" in value and value["exception_type"] not in {"OSError", "FileNotFoundError", "ProcessLookupError", "PermissionError"}:
        return False
    if ("dbus_method" in value) != ("stderr" in value):
        return False
    if "dbus_method" in value:
        if value["exception_type"] != "CalledProcessError" or "returncode" not in value:
            return False
        if not isinstance(value["dbus_method"], str) or value["dbus_method"] not in {"GetNameOwner", "GetConnectionUnixProcessID", "GetConnectionUnixUser", "GetVersion"}:
            return False
        message = value["stderr"]
        if not isinstance(message, str) or len(message) > 512 or any(not c.isprintable() and c not in "\n\t" for c in message):
            return False
    return True


def valid_strip_stop_error(value):
    types = {"RuntimeError", "TimeoutError", "ValueError", "KeyError", "TypeError",
             "OSError", "FileNotFoundError", "ProcessLookupError", "PermissionError",
             "ChildProcessError", "JSONDecodeError", "UnicodeDecodeError"}
    if not isinstance(value, dict) or not {"exception_type", "message"} <= set(value) or not set(value) <= {"exception_type", "message", "errno", "stage"}:
        return False
    if not isinstance(value["exception_type"], str) or value["exception_type"] not in types:
        return False
    message = value["message"]
    if not isinstance(message, str) or len(message) > 512 or any(not c.isprintable() for c in message):
        return False
    if "errno" in value and (type(value["errno"]) is not int or not 0 <= value["errno"] <= 4095
                             or value["exception_type"] not in {"OSError", "FileNotFoundError", "ProcessLookupError", "PermissionError", "ChildProcessError", "TimeoutError"}):
        return False
    if "stage" in value and value["stage"] not in {"client", "launcher"}:
        return False
    return True


def valid_delayed_auth_error(value):
    """Only fixed observer failure codes and bounded identity comparisons."""
    types = {"RuntimeError", "ValueError", "TypeError", "KeyError", "IndexError", "AttributeError",
             "OSError", "FileNotFoundError", "PermissionError", "ProcessLookupError",
             "JSONDecodeError", "UnicodeDecodeError", "OtherError"}
    stages = {"arguments", "process-snapshot", "desktop-identity", "sleep-identity",
              "delay-ownership", "owner-read", "owner-parse", "serialization"}
    codes = {"process-metadata-invalid": "process-snapshot",
             "process-identity-changed": "process-snapshot",
             "desktop-not-unique": "desktop-identity",
             "desktop-identity-mismatch": "desktop-identity"}
    base = {"stage", "exception_type", "code"}
    if type(value) is not dict or not base <= set(value):
        return False
    if (any(type(value[key]) is not str for key in base)
            or value["exception_type"] not in types or value["stage"] not in stages):
        return False
    code = value["code"]
    if code == "observation-exception":
        return set(value) == base
    if value["exception_type"] != "RuntimeError" or codes.get(code) != value["stage"]:
        return False
    if code != "desktop-not-unique":
        return set(value) == base
    if set(value) != base | {"root_count", "expected_desktop_present"}:
        return False
    count, present = value["root_count"], value["expected_desktop_present"]
    return (type(count) is int and 0 <= count <= 65535 and count != 1
            and type(present) is bool and not (count == 0 and present))


# Lifecycle actions whose failure prints one bounded, validated record on
# stdout; any other action's output (and every stderr) stays private.
FAILURE_RECORDS = {"orca-read": ("orca_read_error", valid_orca_read_error),
                   "strip-stop": ("strip_stop_error", valid_strip_stop_error),
                   "delayed-auth-state": ("delayed_auth_error", valid_delayed_auth_error)}
def speech_failure_diagnostic(encoded):
    if not isinstance(encoded, str) or len(encoded) > 5464:
        return None
    try:
        raw = base64.b64decode(encoded, validate=True)
        if len(raw) > 4096:
            return None
        root = json.loads(raw)
        if not isinstance(root, dict) or set(root) != {"orca_speech_error"}:
            return None
        value = root["orca_speech_error"]
        types = {"RuntimeError", "CalledProcessError", "FileNotFoundError", "ProcessLookupError",
                 "PermissionError", "ValueError", "JSONDecodeError", "OSError", "TimeoutExpired", "OtherError"}
        allowed = ({"exception_type", "cleanup"}, {"exception_type", "cleanup", "returncode"},
                   {"exception_type", "cleanup", "stage"}, {"exception_type", "cleanup", "returncode", "stage"})
        if not isinstance(value, dict) or set(value) not in allowed or value["exception_type"] not in types:
            return None
        if "stage" in value and (type(value["stage"]) is not str or value["stage"] not in {"token", "selection", "graph", "sink-link"}):
            return None
        if "returncode" in value and (type(value["returncode"]) is not int or not -255 <= value["returncode"] <= 255):
            return None
        cleanup = value["cleanup"]
        if not isinstance(cleanup, dict) or set(cleanup) not in ({"unit_departed", "presenter_closed", "errors"},
                                                               {"unit_departed", "presenter_closed", "errors", "unit_stop"}):
            return None
        if "unit_stop" in cleanup:
            stop = cleanup["unit_stop"]
            if not isinstance(stop, dict) or set(stop) != {"stop_returncode", "show_returncode", "properties", "original_recorder_departed"}:
                return None
            if any(type(stop[k]) is not int or not -255 <= stop[k] <= 255 for k in ("stop_returncode", "show_returncode")):
                return None
            if stop["original_recorder_departed"] is not None and type(stop["original_recorder_departed"]) is not bool:
                return None
            props = stop["properties"]
            if not isinstance(props, dict) or set(props) != {"LoadState", "ActiveState", "MainPID"}:
                return None
            if props["LoadState"] not in {"loaded", "not-found"} or props["ActiveState"] != "inactive" or props["MainPID"] != "0":
                return None
        if type(cleanup["unit_departed"]) is not bool or type(cleanup["presenter_closed"]) is not bool:
            return None
        if not isinstance(cleanup["errors"], list) or len(cleanup["errors"]) > 3:
            return None
        for error in cleanup["errors"]:
            if not isinstance(error, dict) or not {"stage", "exception_type"} <= set(error) or not set(error) <= {"stage", "exception_type", "returncode", "stop_returncode", "show_returncode"}:
                return None
            if error["stage"] not in {"owned_unit_stop", "owned_presenter_close", "owned_state_release"} or error["exception_type"] not in types:
                return None
            for key in ("returncode", "stop_returncode", "show_returncode"):
                if key in error and (type(error[key]) is not int or not -255 <= error[key] <= 255):
                    return None
            if error["stage"] != "owned_unit_stop" and ({"stop_returncode", "show_returncode"} & set(error)):
                return None
        return value
    except (binascii.Error, ValueError, UnicodeDecodeError, TypeError):
        return None



class GuestAgent:
    """One owned QGA stream; deadline failure poisons it instead of accepting stale replies.

    Transport retirement is not guest-exec child retirement. A timed-out guest PID
    stays recorded for separate exact-owner cleanup by the installed proof runner.
    """
    MAX_REPLY = 1024 * 1024

    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.buffer = bytearray()
        self.poisoned = False
        self.pending_guest_pid = None
        self.guest_exec_state = "none"
        self.command_nonce = f"tuna-qga-{time.time_ns()}"
        self.command_sequence = 0
        deadline = time.monotonic() + 5
        try:
            self.socket.settimeout(self._remaining(deadline))
            self.socket.connect(path)
            nonce = time.time_ns() % (1 << 62)
            self._send(b"\xff" + json.dumps({"execute": "guest-sync-delimited",
                         "arguments": {"id": nonce}}).encode() + b"\n", deadline)
            for _ in range(256):
                try:
                    response = self._read(deadline)
                except json.JSONDecodeError:
                    continue
                if isinstance(response, dict) and response.get("return") == nonce:
                    break
            else:
                raise RuntimeError("guest-agent synchronization response bound")
        except BaseException:
            self._poison()
            raise

    @staticmethod
    def _remaining(deadline):
        value = deadline - time.monotonic()
        if value <= 0:
            raise TimeoutError("absolute guest-agent deadline")
        return value

    def _poison(self):
        self.poisoned = True
        stream = getattr(self, "socket", None)
        if stream is not None:
            try:
                stream.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            stream.close()
        if hasattr(self, "buffer"):
            self.buffer.clear()

    def _send(self, data, deadline):
        if self.poisoned:
            raise RuntimeError("guest-agent transport poisoned")
        self.socket.settimeout(self._remaining(deadline))
        self.socket.sendall(data)

    def _read(self, deadline=None):
        deadline = deadline if deadline is not None else time.monotonic() + 5
        received = 0
        while True:
            self._remaining(deadline)
            # QGA's delimiter resets the lexer even in the middle of stale
            # partial JSON, not merely at the beginning of a framed line.
            sentinel = self.buffer.rfind(b"\xff")
            if sentinel >= 0:
                del self.buffer[:sentinel + 1]
            newline = self.buffer.find(b"\n")
            if newline >= 0:
                if newline + 1 > self.MAX_REPLY:
                    raise RuntimeError("oversized guest-agent response")
                line = bytes(self.buffer[:newline + 1])
                del self.buffer[:newline + 1]
                return json.loads(line)
            if len(self.buffer) >= self.MAX_REPLY:
                raise RuntimeError("missing or oversized guest-agent response")
            self.socket.settimeout(self._remaining(deadline))
            data = self.socket.recv(min(65536, self.MAX_REPLY - len(self.buffer)))
            if not data:
                raise RuntimeError("missing guest-agent response")
            received += len(data)
            if received > self.MAX_REPLY:
                raise RuntimeError("oversized guest-agent response stream")
            self.buffer.extend(data)

    def command(self, name, *, deadline=None, **arguments):
        deadline = deadline if deadline is not None else time.monotonic() + 5
        if getattr(self, "poisoned", False):
            raise RuntimeError("guest-agent transport poisoned")
        try:
            self.command_sequence = getattr(self, "command_sequence", 0) + 1
            request_id = f"{getattr(self, 'command_nonce', 'tuna-qga-' + str(id(self)))}:{self.command_sequence}"
            request = json.dumps({"execute": name, "arguments": arguments, "id": request_id}).encode() + b"\n"
            if len(request) > 65536:
                raise ValueError("guest-agent request exceeds bound")
            self._send(request, deadline)
            result = self._read(deadline)
            if not isinstance(result, dict) or not ("return" in result or "error" in result):
                raise RuntimeError("invalid guest-agent response envelope")
            if result.get("id") != request_id:
                raise RuntimeError("guest-agent response correlation mismatch")
            if ("return" in result) == ("error" in result):
                raise RuntimeError("ambiguous guest-agent response envelope")
        except BaseException:
            self._poison()
            raise
        if "error" in result:
            raise RuntimeError(f"guest-agent command {name} rejected")
        return result["return"]

    def run(self, action, *arguments, timeout=30):
        deadline = time.monotonic() + timeout
        if getattr(self, "guest_exec_state", "none") != "none":
            raise RuntimeError("previous guest-exec retirement unconfirmed")
        # Set uncertainty before sending: a lost initial acknowledgement
        # can leave a running child whose PID we never received.
        self.guest_exec_state = "unknown"
        try:
            pid = self.command("guest-exec", deadline=deadline, path="/usr/libexec/tuna-vm-lifecycle",
                               arg=[action, *map(str, arguments)], **{"capture-output": True})["pid"]
        except BaseException:
            self._poison()
            raise
        if type(pid) is not int or pid <= 0:
            self._poison()
            raise RuntimeError("invalid owned guest-exec PID")
        self.pending_guest_pid = pid
        self.guest_exec_state = "known"
        while time.monotonic() < deadline:
            try:
                status = self.command("guest-exec-status", pid=pid, deadline=deadline)
            except BaseException:
                self._poison()
                raise
            if not isinstance(status, dict) or type(status.get("exited")) is not bool:
                self._poison()
                raise RuntimeError("invalid guest-exec status")
            if status["exited"]:
                self.pending_guest_pid = None
                self.guest_exec_state = "none"
                if status.get("exitcode") != 0 or status.get("out-truncated") or status.get("err-truncated"):
                    if action in {"orca-speech-start", "orca-speech-end"} and not status.get("out-truncated"):
                        diagnostic = speech_failure_diagnostic(status.get("out-data", ""))
                        if diagnostic is not None:
                            raise RuntimeError(f"guest lifecycle {action} failed: {json.dumps(diagnostic)}")
                    if action in {"orca-shell-focus", "orca-disks-focus"} and not status.get("out-truncated"):
                        diagnostic = focus_failure_diagnostic(status.get("out-data", ""))
                        if diagnostic is not None:
                            raise RuntimeError(f"guest lifecycle {action} failed (exit={status.get('exitcode')}): {json.dumps(diagnostic)}")
                    if action in FAILURE_RECORDS and not status.get("out-truncated"):
                        key, valid = FAILURE_RECORDS[action]
                        try:
                            encoded = status.get("out-data", "")
                            if not isinstance(encoded, str) or len(encoded) > 5464:
                                raise ValueError("failure record exceeds transport bound")
                            output = base64.b64decode(encoded, validate=True)
                            if len(output) <= 4096:
                                root = json.loads(output)
                                if action == "delayed-auth-state" and (type(root) is not dict or set(root) != {key}):
                                    diagnostic = None
                                else:
                                    diagnostic = root.get(key)
                            else:
                                diagnostic = None
                        except (binascii.Error, ValueError, UnicodeDecodeError, AttributeError, TypeError):
                            diagnostic = None
                        if valid(diagnostic):
                            if action == "delayed-auth-state":
                                # Preserve the sanitized reason before the lane's
                                # restore-unlock finally block can itself fail.
                                print("tuna-vm-lane: delayed-auth-state failure " + json.dumps(diagnostic), flush=True)
                            raise RuntimeError(f"guest lifecycle {action} failed (exit={status.get('exitcode')}): {json.dumps(diagnostic)}")
                    raise RuntimeError(f"guest lifecycle {action} failed (exit={status.get('exitcode')})")
                return json.loads(base64.b64decode(status.get("out-data", ""), validate=True))
            time.sleep(min(0.1, max(0, deadline - time.monotonic())))
        self._poison()
        raise TimeoutError(f"guest lifecycle {action} timed out; child retirement unconfirmed")

    def close(self):
        self._poison()
