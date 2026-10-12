#!/usr/bin/env python3
"""A minimal CUPS/IPP stub for the printers proof gates.

Speaks just enough IPP-over-HTTP for the shell's client
(`crates/shell-gtk/src/printers.rs`): CUPS-Get-Printers, CUPS-Get-Default,
Get-Jobs, Cancel-Job and Print-Job. Tag, operation and status numbers are
those of cups/ipp.h in OpenPrinting/cups (RFC 8011), so the proof also
validates the client's wire compatibility.

Every operation is appended to the calls log, so the proof asserts the
shell really discovered state and drove actions instead of rendering
canned rows. The stub is a sensor, not a simulator: Cancel-Job is
recorded but the job keeps listing, exactly like the NetworkManager and
BlueZ stub logs the neighbouring gates assert.

Usage: tuna-cups-stub.py CALLS_LOG
Listens on 127.0.0.1 with an ephemeral port and prints "ready port=N".
"""
import socketserver
import struct
import sys

# Group tags (cups/ipp.h ipp_tag_t, RFC 8011).
G_OPERATION = 0x01
G_JOB = 0x02
G_END = 0x03
G_PRINTER = 0x04

# Value tags.
V_INTEGER = 0x21
V_BOOLEAN = 0x22
V_ENUM = 0x23
V_TEXT = 0x41
V_NAME = 0x42
V_KEYWORD = 0x44
V_URI = 0x45
V_CHARSET = 0x47
V_LANGUAGE = 0x48

# Operations (cups/ipp.h ipp_op_t).
OP_PRINT_JOB = 0x0002
OP_CANCEL_JOB = 0x0008
OP_GET_JOBS = 0x000A
OP_CUPS_GET_DEFAULT = 0x4001
OP_CUPS_GET_PRINTERS = 0x4002

OFFICEJET_URI = "ipp://tuna-proof/printers/OfficeJet-Pro"
LABEL_URI = "ipp://tuna-proof/printers/LabelPrinter"


def text(value):
    return value.encode("utf-8")


def integer(value):
    return struct.pack(">i", value)


def boolean(value):
    return b"\x01" if value else b"\x00"


def attr(tag, name, *values):
    """One attribute; further values repeat the tag with an empty name."""
    out = bytearray()
    name_bytes = name.encode("utf-8")
    for index, value in enumerate(values):
        out.append(tag)
        if index == 0:
            out += struct.pack(">H", len(name_bytes)) + name_bytes
        else:
            out += b"\x00\x00"
        out += struct.pack(">H", len(value)) + value
    return bytes(out)


def group(tag, *attrs):
    return bytes([tag]) + b"".join(attrs)


def response(status, request_id, *groups):
    return (
        b"\x01\x01"
        + struct.pack(">H", status)
        + struct.pack(">I", request_id)
        + b"".join(groups)
        + bytes([G_END])
    )


def op_base():
    return group(
        G_OPERATION,
        attr(V_CHARSET, "attributes-charset", text("utf-8")),
        attr(V_LANGUAGE, "attributes-natural-language", text("en")),
    )


def officejet_group():
    return group(
        G_PRINTER,
        attr(V_NAME, "printer-name", text("OfficeJet Pro")),
        attr(V_URI, "printer-uri-supported", text(OFFICEJET_URI)),
        attr(V_URI, "device-uri", text("usb://HP/OfficeJet%20Pro?serial=CN123")),
        attr(V_TEXT, "printer-info", text("Front office workhorse")),
        attr(V_TEXT, "printer-make-and-model", text("HP OfficeJet Pro 9020")),
        attr(V_TEXT, "printer-location", text("Front Office")),
        attr(V_ENUM, "printer-state", integer(3)),
        attr(V_KEYWORD, "printer-state-reasons", text("none")),
        attr(V_BOOLEAN, "printer-is-accepting-jobs", boolean(True)),
        attr(V_INTEGER, "queued-job-count", integer(1)),
        attr(V_NAME, "marker-names", text("Black"), text("Cyan"), text("Magenta"), text("Yellow")),
        attr(V_INTEGER, "marker-levels", integer(80), integer(60), integer(55), integer(40)),
        attr(
            V_KEYWORD,
            "marker-colors",
            text("#000000"),
            text("#00FFFF"),
            text("#FF00FF"),
            text("#FFFF00"),
        ),
        attr(V_KEYWORD, "marker-types", text("toner"), text("toner"), text("toner"), text("toner")),
        attr(V_KEYWORD, "printer-commands", text("Clean"), text("PrintSelfTestPage")),
    )


def label_group():
    return group(
        G_PRINTER,
        attr(V_NAME, "printer-name", text("LabelPrinter")),
        attr(V_URI, "printer-uri-supported", text(LABEL_URI)),
        attr(V_URI, "device-uri", text("usb://Dymo/LabelWriter?serial=LW9")),
        attr(V_TEXT, "printer-info", text("Shipping labels")),
        attr(V_TEXT, "printer-make-and-model", text("Dymo LabelWriter 550")),
        attr(V_TEXT, "printer-location", text("Shipping Bench")),
        attr(V_ENUM, "printer-state", integer(5)),
        attr(V_KEYWORD, "printer-state-reasons", text("media-empty-error")),
        attr(V_BOOLEAN, "printer-is-accepting-jobs", boolean(True)),
        attr(V_INTEGER, "queued-job-count", integer(0)),
        attr(V_KEYWORD, "printer-commands", text("PrintSelfTestPage")),
    )


def officejet_jobs_group():
    return group(
        G_JOB,
        attr(V_INTEGER, "job-id", integer(42)),
        attr(V_URI, "job-uri", text("ipp://tuna-proof/jobs/42")),
        attr(V_NAME, "job-name", text("Quarterly Report")),
        attr(V_NAME, "job-originating-user-name", text("proof-user")),
        attr(V_ENUM, "job-state", integer(5)),
        attr(V_TEXT, "job-state-message", text("")),
        attr(V_INTEGER, "job-media-sheets-completed", integer(2)),
    )


def parse_request(body):
    """Split an IPP request body into (op, request_id, attrs).

    attrs holds (name, raw value) pairs from every group; requests carry
    no collections, so a flat walk is enough. Malformed bodies return
    empty attrs rather than failing the exchange.
    """
    if len(body) < 8:
        return 0, 0, []
    op = struct.unpack(">H", body[2:4])[0]
    request_id = struct.unpack(">I", body[4:8])[0]
    attrs = []
    pos = 8
    try:
        while pos < len(body):
            tag = body[pos]
            pos += 1
            if tag == G_END:
                break
            if tag <= 0x0F:
                continue
            name_len = struct.unpack(">H", body[pos : pos + 2])[0]
            pos += 2
            name = body[pos : pos + name_len].decode("utf-8", "replace")
            pos += name_len
            value_len = struct.unpack(">H", body[pos : pos + 2])[0]
            pos += 2
            value = body[pos : pos + value_len]
            pos += value_len
            if name:
                attrs.append([name, value])
            elif attrs:
                attrs[-1][1] += b"\x00" + value
    except (IndexError, struct.error):
        pass
    return op, request_id, [(name, value) for name, value in attrs]


def attr_text(attrs, name):
    for key, value in attrs:
        if key == name:
            return value.split(b"\x00")[0].decode("utf-8", "replace")
    return ""


def attr_int(attrs, name):
    for key, value in attrs:
        if key == name and len(value) >= 4:
            return struct.unpack(">i", value[:4])[0]
    return 0


class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        calls = self.server.calls_log
        try:
            head = b""
            while b"\r\n\r\n" not in head and len(head) < 65536:
                chunk = self.request.recv(4096)
                if not chunk:
                    return
                head += chunk
            if b"\r\n\r\n" not in head:
                return
            head_text = head.decode("latin-1")
            length = 0
            for line in head_text.split("\r\n"):
                if line.lower().startswith("content-length:"):
                    try:
                        length = int(line.split(":", 1)[1].strip())
                    except ValueError:
                        length = 0
            body = head.split(b"\r\n\r\n", 1)[1]
            while len(body) < length:
                chunk = self.request.recv(min(65536, length - len(body)))
                if not chunk:
                    break
                body += chunk
            op, request_id, attrs = parse_request(body)
            if op == OP_CUPS_GET_PRINTERS:
                calls.write("CUPS-Get-Printers\n")
                payload = response(0, request_id, op_base(), officejet_group(), label_group())
            elif op == OP_CUPS_GET_DEFAULT:
                calls.write("CUPS-Get-Default\n")
                payload = response(
                    0,
                    request_id,
                    op_base(),
                    group(G_PRINTER, attr(V_NAME, "printer-name", text("OfficeJet Pro"))),
                )
            elif op == OP_GET_JOBS:
                uri = attr_text(attrs, "printer-uri")
                calls.write(f"Get-Jobs uri={uri}\n")
                if "OfficeJet" in uri:
                    payload = response(0, request_id, op_base(), officejet_jobs_group())
                else:
                    payload = response(0, request_id, op_base())
            elif op == OP_CANCEL_JOB:
                calls.write(f"Cancel-Job id={attr_int(attrs, 'job-id')}\n")
                payload = response(0, request_id, op_base())
            elif op == OP_PRINT_JOB:
                uri = attr_text(attrs, "printer-uri")
                calls.write(f"Print-Job uri={uri} bytes={len(body)}\n")
                payload = response(
                    0,
                    request_id,
                    op_base(),
                    group(G_JOB, attr(V_INTEGER, "job-id", integer(43))),
                )
            else:
                calls.write(f"op=0x{op:04x}\n")
                payload = response(0, request_id, op_base())
            calls.flush()
            reply = (
                f"HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\n"
                f"Content-Length: {len(payload)}\r\nConnection: close\r\n\r\n"
            ).encode("latin-1") + payload
            self.request.sendall(reply)
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            try:
                self.request.close()
            except OSError:
                pass


def main():
    calls_path = sys.argv[1]
    calls = open(calls_path, "a", buffering=1)

    class Server(socketserver.ThreadingTCPServer):
        allow_reuse_address = True
        daemon_threads = True

    server = Server(("127.0.0.1", 0), Handler)
    server.calls_log = calls
    port = server.server_address[1]
    print(f"ready port={port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
