#!/usr/bin/env python3
"""github_api retry helper against a local fake HTTP server."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import threading
import unittest
import urllib.error

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("github_api", ROOT / "scripts/lib/github_api.py")
gh = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gh)


class Fake(BaseHTTPRequestHandler):
    script = []  # list of (status, headers, body); last entry repeats
    hits = []

    def do_GET(self):
        Fake.hits.append((self.path, self.headers.get("Authorization")))
        status, headers, body = Fake.script[min(len(Fake.hits), len(Fake.script)) - 1]
        self.send_response(status)
        for key, value in headers.items():
            self.send_header(key, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class GithubApiTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Fake)
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()
        cls.url = f"http://127.0.0.1:{cls.server.server_address[1]}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()

    def setUp(self):
        Fake.hits = []
        self.sleeps = []

    def call(self, **kwargs):
        return gh.get(self.url + "/repos/x/y/z", {"Authorization": "Bearer t"},
                      sleep=self.sleeps.append, **kwargs)

    def test_retries_transient_then_succeeds(self):
        Fake.script = [(502, {}, b"bad gateway"), (429, {"Retry-After": "7"}, b""),
                       (503, {}, b"egress over limit"), (200, {}, b'{"ok": true}')]
        self.assertEqual(self.call(), b'{"ok": true}')
        self.assertEqual(len(Fake.hits), 4)
        self.assertEqual(len(self.sleeps), 3)
        self.assertGreaterEqual(self.sleeps[1], 7.0)  # Retry-After honoured
        self.assertTrue(all(0 < s <= gh.MAX_DELAY for s in self.sleeps))
        self.assertTrue(all(auth == "Bearer t" for _, auth in Fake.hits))

    def test_client_error_fails_at_once(self):
        Fake.script = [(404, {}, b"not found"), (200, {}, b"{}")]
        with self.assertRaises(urllib.error.HTTPError) as caught:
            self.call()
        self.assertEqual(caught.exception.code, 404)
        self.assertEqual(len(Fake.hits), 1)
        self.assertEqual(self.sleeps, [])

    def test_exhaustion_fails(self):
        Fake.script = [(502, {}, b"bad gateway")]
        with self.assertRaises(urllib.error.HTTPError) as caught:
            self.call()
        self.assertEqual(caught.exception.code, 502)
        self.assertEqual(len(Fake.hits), gh.TRIES)
        self.assertEqual(len(self.sleeps), gh.TRIES - 1)
        # Bounded total backoff (~2 minutes at worst).
        self.assertLessEqual(sum(self.sleeps), 120)

    def test_connection_error_retried_then_fails(self):
        with self.assertRaises(urllib.error.URLError):
            gh.get("http://127.0.0.1:1/", {}, tries=3, sleep=self.sleeps.append)
        self.assertEqual(len(self.sleeps), 2)

    def test_oversized_body_fails(self):
        Fake.script = [(200, {}, b"x" * 10)]
        with self.assertRaises(ValueError):
            self.call(max_bytes=5)

    def test_cli_fails_nonzero_on_client_error_and_prints_body_on_success(self):
        env = dict(os.environ, GH_TOKEN="t", GITHUB_API_URL=self.url)
        Fake.script = [(200, {}, b"payload")]
        out = subprocess.run([sys.executable, str(ROOT / "scripts/lib/github_api.py"), "repos/x/y"],
                             env=env, capture_output=True, check=True)
        self.assertEqual(out.stdout, b"payload")
        Fake.hits = []
        Fake.script = [(403, {}, b"forbidden")]
        out = subprocess.run([sys.executable, str(ROOT / "scripts/lib/github_api.py"), "repos/x/y"],
                             env=env, capture_output=True)
        self.assertNotEqual(out.returncode, 0)
        self.assertEqual(out.stdout, b"")


if __name__ == "__main__":
    unittest.main()
