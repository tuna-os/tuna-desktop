"""Bounded CI guest-agent transport; only the fixed lifecycle probe is executed.

Protocol: https://www.qemu.org/docs/master/interop/qemu-ga-ref.html
"""
import base64
import binascii
import json
import socket
import time


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
                    if action == "orca-read" and not status.get("out-truncated"):
                        try:
                            encoded = status.get("out-data", "")
                            if not isinstance(encoded, str) or len(encoded) > 5464:
                                raise ValueError("read diagnostic exceeds transport bound")
                            output = base64.b64decode(encoded, validate=True)
                            if len(output) <= 4096:
                                diagnostic = json.loads(output).get("orca_read_error")
                            else:
                                diagnostic = None
                        except (binascii.Error, ValueError, UnicodeDecodeError, AttributeError, TypeError):
                            diagnostic = None
                        if valid_orca_read_error(diagnostic):
                            raise RuntimeError(f"guest lifecycle orca-read failed (exit={status.get('exitcode')}): {json.dumps(diagnostic)}")
                    raise RuntimeError(f"guest lifecycle {action} failed (exit={status.get('exitcode')})")
                return json.loads(base64.b64decode(status.get("out-data", ""), validate=True))
            time.sleep(min(0.1, max(0, deadline - time.monotonic())))
        self._poison()
        raise TimeoutError(f"guest lifecycle {action} timed out; child retirement unconfirmed")

    def close(self):
        self._poison()
