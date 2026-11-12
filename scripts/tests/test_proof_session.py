#!/usr/bin/env python3
"""scripts/lib/tuna-proof-session ends every process a proof started (#1082).

A fake session stands in for a nested proof: a real dbus-daemon that forks
(when installed), `sleep` daemons that detach (setsid and a double fork,
one with a scrubbed environment as ibus-daemon --daemonize could), a child
that ignores TERM, and the helper's own private bus. Whether the proof
passes, fails or is stopped with SIGTERM or SIGINT, nothing of it may
survive.
"""
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[2]
SESSION = ROOT / "scripts/lib/tuna-proof-session"
PREFLIGHT = ROOT / "scripts/lib/tuna-proof-preflight"
HAVE_DBUS = shutil.which("dbus-daemon") is not None

# $1 pids file, $2 token (a unique sleep length), $3 outcome.
FAKE = r"""
pids=$1 token=$2
note() { echo "$1" >>"$pids"; }
sleep "$token" & note $!
# Detached as daemons detach: their own session, parent gone (PID 1 would
# adopt them without the helper's subreaper).
setsid sh -c 'sleep "$1" & echo $! >>"$2"' sh "$token" "$pids"
env -i PATH="$PATH" setsid sh -c 'sleep "$1" & echo $! >>"$2"' sh "$token" "$pids"
sh -c 'trap "" TERM; exec sleep "$1"' sh "$token" & note $!
if command -v dbus-daemon >/dev/null 2>&1; then
    dbus-daemon --session --fork --print-pid=1 --print-address=0 >>"$pids"
fi
echo "$DBUS_SESSION_BUS_ADDRESS" >"$pids.bus"
echo "${TUNA_PROOF_SESSION_BUS_PID:-}" >"$pids.buspid"
echo "$TUNA_PROOF_SESSION" >"$pids.marker"
: >"$pids.ready"
case $3 in
    pass) exit 0 ;;
    fail) exit 3 ;;
    hang) trap 'echo cleaned >"$pids.trapped"; exit 1' TERM INT
          sleep "$token" & wait $! ;;
    check) "$4" --check; echo $? >"$pids.check"; exit 0 ;;
    stop) "$4" --stop sleep; echo $? >"$pids.stop"
          "$4" --check; echo $? >"$pids.check"; exit 0 ;;
esac
"""

STUBBORN = """
import importlib.machinery, importlib.util, os, sys
loader = importlib.machinery.SourceFileLoader("session", sys.argv[1])
spec = importlib.util.spec_from_loader("session", loader)
session = importlib.util.module_from_spec(spec)
loader.exec_module(session)
real = session.os_kill
def os_kill(pid, signum):
    # A process that refuses to die: signals to it are lost.
    try:
        with open(f"/proc/{pid}/cmdline", "rb") as handle:
            if sys.argv[2].encode() in handle.read():
                return
    except OSError:
        pass
    real(pid, signum)
session.os_kill = os_kill
sys.exit(session.main(sys.argv[3:]))
"""


def start_ticks(pid):
    try:
        raw = Path(f"/proc/{pid}/stat").read_text()
    except OSError:
        return None
    rest = raw[raw.rindex(")") + 2:].split()
    return None if rest[0] == "Z" else int(rest[19])


def with_token(token):
    """Live processes whose command line holds TOKEN."""
    found = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            if token.encode() in (entry / "cmdline").read_bytes() and start_ticks(int(entry.name)):
                found.append(int(entry.name))
        except OSError:
            pass
    return found


def with_marker(marker):
    found = []
    needle = f"\0TUNA_PROOF_SESSION={marker}\0".encode()
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            if needle in b"\0" + (entry / "environ").read_bytes() and start_ticks(int(entry.name)):
                found.append(int(entry.name))
        except OSError:
            pass
    return found


class ProofSession(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="proof-session-test."))
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)
        self.fake = self.dir / "fake-proof"
        self.fake.write_text("#!/bin/sh\n" + FAKE)
        self.fake.chmod(0o755)
        self.pids = self.dir / "pids"
        # A unique sleep length marks this test's stand-in daemons.
        self.token = f"{4000 + os.getpid() % 1000}.{uuid.uuid4().int % 10**9:09d}"
        self.addCleanup(self.kill_leftovers)
        self.env = dict(os.environ, TUNA_PROOF_SESSION_GRACE="2", TUNA_PROOF_INOTIFY_NEED="0")
        self.env.pop("TUNA_PROOF_SESSION", None)

    def kill_leftovers(self):
        for pid in with_token(self.token):
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass

    def run_session(self, outcome, *options, extra=(), wrapper=None):
        argv = [*(wrapper or [str(SESSION)]), *options, "--", str(self.fake),
                str(self.pids), self.token, outcome, *extra]
        # A file, not a pipe: a survivor holding a pipe would hang the test.
        with (self.dir / "output").open("w") as output:
            process = subprocess.Popen(argv, env=self.env, stdin=subprocess.DEVNULL,
                                       stdout=output, stderr=subprocess.STDOUT)
        self.addCleanup(process.wait)
        self.addCleanup(process.kill)
        return process

    def output(self):
        return (self.dir / "output").read_text()

    def wait_ready(self, process):
        end = time.monotonic() + 30
        while not (self.dir / "pids.ready").exists():
            self.assertIsNone(process.poll(), self.output())
            self.assertLess(time.monotonic(), end, "the fake session never got ready")
            time.sleep(0.05)
        time.sleep(0.3)        # let the double forks land

    def started(self):
        return [int(line) for line in self.pids.read_text().split() if line.isdigit()]

    def assert_nothing_survives(self, result):
        started = self.started()
        self.assertGreaterEqual(len(started), 4 + HAVE_DBUS, started)
        marker = (self.dir / "pids.marker").read_text().strip()
        self.assertTrue(marker)
        alive = [pid for pid in started if start_ticks(pid)]
        self.assertEqual(alive, [], f"started processes survived: {alive}\n{result}")
        self.assertEqual(with_token(self.token), [], result)
        self.assertEqual(with_marker(marker), [], result)
        bus = (self.dir / "pids.buspid").read_text().strip()
        if bus:
            self.assertIsNone(start_ticks(int(bus)), "the private session bus survived")

    def finish(self, process, timeout=60):
        process.wait(timeout=timeout)
        return process.returncode, self.output()

    @unittest.skipUnless(HAVE_DBUS, "dbus-daemon is not installed")
    def test_pass_ends_everything_and_the_private_bus(self):
        status, result = self.finish(self.run_session("pass", "--bus"))
        self.assertEqual(status, 0, result)
        address = (self.dir / "pids.bus").read_text().strip()
        self.assertTrue(address.startswith("unix:"), address)
        self.assertNotEqual(address, os.environ.get("DBUS_SESSION_BUS_ADDRESS"))
        self.assertIn("left behind", result)
        self.assertIn("no process of run", result)
        self.assert_nothing_survives(result)

    def test_pass_without_bus(self):
        status, result = self.finish(self.run_session("pass"))
        self.assertEqual(status, 0, result)
        self.assert_nothing_survives(result)

    def test_failing_gate_keeps_its_status_and_ends_everything(self):
        status, result = self.finish(self.run_session("fail", *(["--bus"] if HAVE_DBUS else [])))
        self.assertEqual(status, 3, result)
        self.assert_nothing_survives(result)

    def stopped_mid_run(self, signum):
        process = self.run_session("hang", *(["--bus"] if HAVE_DBUS else []))
        self.wait_ready(process)
        process.send_signal(signum)
        status, result = self.finish(process)
        self.assertEqual(status, 128 + signum, result)
        # The proof itself was told and ran its own cleanup first.
        self.assertEqual((self.dir / "pids.trapped").read_text().strip(), "cleaned", result)
        self.assert_nothing_survives(result)

    def test_sigterm_mid_run_ends_everything(self):
        self.stopped_mid_run(signal.SIGTERM)

    def test_sigint_mid_run_ends_everything(self):
        self.stopped_mid_run(signal.SIGINT)

    def test_a_killed_session_still_tells_the_proof_and_preflight_reports_the_rest(self):
        process = self.run_session("hang", *(["--bus"] if HAVE_DBUS else []))
        self.wait_ready(process)
        process.kill()                     # no teardown: only the parent-death signal
        process.wait(timeout=10)
        end = time.monotonic() + 10
        while not (self.dir / "pids.trapped").exists():
            self.assertLess(time.monotonic(), end, "the proof never heard its session die")
            time.sleep(0.05)
        marker = (self.dir / "pids.marker").read_text().strip()
        self.addCleanup(lambda: [os.kill(pid, signal.SIGKILL) for pid in with_marker(marker)])
        left = with_marker(marker)
        self.assertTrue(left, "the detached stand-in daemons should outlive a KILLed session")
        result = subprocess.run([str(PREFLIGHT)], capture_output=True, text=True,
                                env=dict(os.environ, TUNA_PROOF_INOTIFY_NEED="0"))
        self.assertEqual(result.returncode, 0, result.stderr)
        for pid in left:
            self.assertIn(f"pid {pid} ", result.stderr)
        self.assertIn(f"run {marker}, which has ended", result.stderr)

    def test_a_process_that_refuses_to_die_fails_the_run(self):
        stubborn = self.token + "1"
        self.fake.write_text(self.fake.read_text().replace(
            "case $3 in", f'sleep {stubborn} & note $!\ncase $3 in'))
        process = self.run_session(
            "pass", wrapper=[sys.executable, "-c", STUBBORN, str(SESSION), stubborn])
        status, result = self.finish(process)
        self.assertNotEqual(status, 0, result)
        self.assertIn("still alive after KILL", result)
        self.assertIn(stubborn, result)
        self.kill_leftovers()

    def test_check_inside_the_session_names_escaped_processes(self):
        status, result = self.finish(self.run_session("check", "--bus" if HAVE_DBUS else "--label", *(
            [] if HAVE_DBUS else ["fake"]), extra=[str(SESSION)]))
        self.assertEqual(status, 0, result)
        self.assertEqual((self.dir / "pids.check").read_text().strip(), "1", result)
        self.assertIn("escaped the proof's cleanup", result)
        self.assertIn(self.token, result)
        self.assert_nothing_survives(result)

    def test_stop_by_name_then_check_is_clean(self):
        self.fake.write_text(self.fake.read_text().replace(
            'if command -v dbus-daemon', 'if false && command -v dbus-daemon'))
        status, result = self.finish(self.run_session("stop", "--bus" if HAVE_DBUS else "--label", *(
            [] if HAVE_DBUS else ["fake"]), extra=[str(SESSION)]))
        self.assertEqual(status, 0, result)
        self.assertEqual((self.dir / "pids.stop").read_text().strip(), "0", result)
        self.assertEqual((self.dir / "pids.check").read_text().strip(), "0", result)
        self.assertNotIn("left behind", result)
        self.assertEqual(with_token(self.token), [], result)

    @unittest.skipUnless(HAVE_DBUS and shutil.which("gdbus"), "dbus-daemon or gdbus is not installed")
    def test_check_leaves_services_of_the_private_bus_to_the_session(self):
        services = self.dir / "share/dbus-1/services"
        services.mkdir(parents=True)
        # Never claims its name: activation spawns it and it keeps running.
        (services / "org.tuna.ProofSessionTest.service").write_text(
            f"[D-BUS Service]\nName=org.tuna.ProofSessionTest\nExec=/bin/sleep {self.token}\n")
        self.env["XDG_DATA_DIRS"] = f"{self.dir / 'share'}:{os.environ.get('XDG_DATA_DIRS', '/usr/share')}"
        self.fake.write_text(
            "#!/bin/sh\n"
            "gdbus call --session --timeout 2 --dest org.tuna.ProofSessionTest --object-path / "
            "--method org.freedesktop.DBus.Peer.Ping >/dev/null 2>&1\n"
            'pgrep -f "sleep $2" >"$1.activated"\n'
            '"$4" --check; echo $? >"$1.check"\n')
        status, result = self.finish(self.run_session("check", "--bus", extra=[str(SESSION)]))
        self.assertEqual(status, 0, result)
        self.assertTrue((self.dir / "pids.activated").read_text().split(), "the service was never activated")
        self.assertEqual((self.dir / "pids.check").read_text().strip(), "0", result)
        self.assertIn("left behind: sleep(", result)
        self.assertEqual(with_token(self.token), [], result)

    def test_check_outside_a_session_refuses(self):
        result = subprocess.run([str(SESSION), "--check"], env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2, result.stderr)

    def test_preflight_refusal_runs_nothing(self):
        self.env["TUNA_PROOF_INOTIFY_NEED"] = str(10**9)
        status, result = self.finish(self.run_session("pass"))
        self.assertEqual(status, 75, result)
        self.assertIn("fs.inotify.max_user_instances", result)
        self.assertFalse(self.pids.exists())


# Nested proofs that start a private bus or session daemons on the host.
OWNED_PROOFS = [
    "tuna-gtk-shell-proof", "tuna-lock-auth-proof", "tuna-magnifier-proof", "tuna-orca-speech-proof",
    "tuna-osk-proof", "tuna-parity-capture", "tuna-scale-proof", "tuna-visual-capture",
    "tuna-gtk-accessibility-proof", "tuna-slow-portal-proof", "tuna-journey", "tuna-session-sync-proof",
    "tuna-capture", "tuna-minimize-proof", "tuna-modal-dim-proof", "tuna-window-lifecycle-proof",
    "tuna-workspace-topology-proof", "tuna-wellbeing-proof", "tuna-session-restore-proof",
    "tuna-screenshot-flash-proof", "tuna-motion-barriers-proof", "tuna-stress",
    "tuna-media-completion-proof", "lib/tuna-sound-consumers-proof.py",
]
# dbus-run-session stays only where something else owns every process:
# inside a podman container, or under auth_proof_process.bounded_command's
# subreaper retirement, or in workflow-owned CI tooling.
DBUS_RUN_SESSION_ALLOWED = {
    "tuna-niri-compare", "tuna-niri-reference", "tuna-gnome-wayland-info",
    "tuna-auth-no-replay-gtk-proof", "gtk-atspi-root-proof",
}


class EveryProofIsOwned(unittest.TestCase):
    def test_proofs_with_session_daemons_run_in_the_helper(self):
        for name in OWNED_PROOFS:
            with self.subTest(proof=name):
                text = (ROOT / "scripts" / name).read_text()
                self.assertIn("tuna-proof-session", text)
                self.assertNotIn("dbus-run-session --", text)
                self.assertNotRegex(text, r"['\"]dbus-run-session['\"]")

    def test_no_other_script_starts_a_bare_dbus_run_session(self):
        for path in sorted((ROOT / "scripts").glob("*")) + sorted((ROOT / "scripts/lib").glob("*")):
            if not path.is_file() or path.name in DBUS_RUN_SESSION_ALLOWED:
                continue
            text = path.read_text(errors="replace")
            with self.subTest(script=str(path.relative_to(ROOT))):
                self.assertIsNone(
                    re.search(r"dbus-run-session['\"]?,? *['\"]?--|exec dbus-run-session", text),
                    "start the session with scripts/lib/tuna-proof-session --bus instead")


class Preflight(unittest.TestCase):
    def test_reports_a_leftover_of_an_ended_run_and_headroom(self):
        token = f"5{os.getpid() % 1000}.{uuid.uuid4().int % 10**9:09d}"
        # A run marker whose session (pid 1 at boot time 0) is not running.
        left = subprocess.Popen(["sleep", token], env=dict(os.environ, TUNA_PROOF_SESSION="gone.1.0"),
                                start_new_session=True)
        self.addCleanup(left.wait)
        self.addCleanup(left.kill)
        time.sleep(0.2)
        result = subprocess.run([str(PREFLIGHT), "--label", "test"], capture_output=True, text=True,
                                env=dict(os.environ, TUNA_PROOF_INOTIFY_NEED="0"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"leftover: sleep pid {left.pid}", result.stderr)
        self.assertIn("run gone.1.0, which has ended", result.stderr)
        self.assertRegex(result.stderr, r"inotify instances: \d+ of \d+ in use")

    def test_too_little_headroom_fails_clearly(self):
        result = subprocess.run([str(PREFLIGHT)], capture_output=True, text=True,
                                env=dict(os.environ, TUNA_PROOF_INOTIFY_NEED=str(10**9)))
        self.assertEqual(result.returncode, 1)
        self.assertIn("FAIL", result.stderr)
        self.assertIn("fs.inotify.max_user_instances", result.stderr)


if __name__ == "__main__":
    unittest.main()
