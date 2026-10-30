"""Bounded-retry GitHub REST GET for CI gates.

Retries transient failures (HTTP 5xx, 429, connection errors/timeouts) with
exponential backoff and jitter, honouring Retry-After. Any 4xx other than 429
fails at once; exhausted retries re-raise the last error. Callers never get a
fake answer: either the API's response or an exception.
"""
import email.utils
import json
import os
import random
import sys
import time
import urllib.error
import urllib.request

API_ROOT = "https://api.github.com"
MAX_BYTES = 4 * 1024 * 1024
TRIES = 6
BASE_DELAY = 4.0
MAX_DELAY = 45.0


def _retry_after(error):
    value = error.headers.get("Retry-After") if error.headers else None
    if not value:
        return None
    try:
        return max(0.0, float(value))
    except ValueError:
        parsed = email.utils.parsedate_to_datetime(value)
        return max(0.0, parsed.timestamp() - time.time()) if parsed else None


def transient(error):
    if isinstance(error, urllib.error.HTTPError):
        return error.code == 429 or 500 <= error.code < 600
    return isinstance(error, (urllib.error.URLError, ConnectionError, TimeoutError))


def get(url, headers, *, tries=TRIES, base_delay=BASE_DELAY, max_delay=MAX_DELAY,
        timeout=20, sleep=time.sleep, max_bytes=MAX_BYTES):
    """Return the raw body of a GET, retrying transient failures only."""
    for attempt in range(1, tries + 1):
        request = urllib.request.Request(url, headers={k: v for k, v in headers.items() if k != "Authorization"})
        if "Authorization" in headers:
            # Never forward the token to redirect targets (artifact blob storage).
            request.add_unredirected_header("Authorization", headers["Authorization"])
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                data = response.read(max_bytes + 1)
            if len(data) > max_bytes:
                raise ValueError("GitHub API response exceeds bounded admission size")
            return data
        except (urllib.error.URLError, ConnectionError, TimeoutError) as error:
            if not transient(error) or attempt == tries:
                raise
            delay = min(max_delay, base_delay * 2 ** (attempt - 1)) * random.uniform(0.5, 1.0)
            if isinstance(error, urllib.error.HTTPError):
                hinted = _retry_after(error)
                if hinted is not None:
                    delay = min(max_delay, max(delay, hinted))
            print(f"GitHub API transient failure ({error}); retry {attempt}/{tries - 1} in {delay:.1f}s",
                  file=sys.stderr)
            sleep(delay)
    raise AssertionError("unreachable")


def headers():
    return {"Authorization": f"Bearer {os.environ['GH_TOKEN']}", "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28"}


def repo_json(repo, path, **kwargs):
    """GET /repos/{repo}/{path} with GH_TOKEN and return decoded JSON."""
    return json.loads(get(f"{API_ROOT}/repos/{repo}/{path}", headers(), **kwargs))


def main(argv):
    """CLI: github_api.py API_PATH [--max-bytes N]; writes the raw body to stdout."""
    import argparse
    parser = argparse.ArgumentParser(description=main.__doc__)
    parser.add_argument("path", help="path below https://api.github.com/, e.g. repos/OWNER/REPO/actions/runs/1")
    parser.add_argument("--max-bytes", type=int, default=MAX_BYTES)
    args = parser.parse_args(argv)
    root = os.environ.get("GITHUB_API_URL", API_ROOT).rstrip("/")
    sys.stdout.buffer.write(get(f"{root}/{args.path.lstrip('/')}", headers(), max_bytes=args.max_bytes))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
