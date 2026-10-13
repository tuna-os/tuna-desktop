#!/usr/bin/env python3
"""Focused suite for scripts/tuna-corral-desktop.

Drives the consumer shell script as a subprocess against local fixtures
only: a stub corral binary served over file://, stub QEMU tools on PATH,
and test-seam coordinates. No network, no VM, no container, no build.
"""
import hashlib
import json
import os
import shutil
import signal
import stat
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / 'tuna-corral-desktop'
SH = shutil.which('sh') or '/bin/sh'

# The exact `corral launch` flag surface this consumer may emit, cited from
# the reviewed C4 cmd/launch.go flag registrations (--manifest/--image are
# the input pair; the consumer always uses --manifest).
C4_LAUNCH_FLAGS = frozenset({
    '--manifest', '--name', '--mem', '--cpu', '--disk', '--web-port',
    '--vm-timeout', '--web-timeout', '--stop-vm', '--force',
    '--allow-emulation', '--allow-cross-arch',
})

STUB_VERSION = '0.0.0-fixture'
STUB_CONSOLE = 'http://127.0.0.1:5190/vnc.html?path=websockify&token=fixture'

# Admitted (non-PENDING) qualification fixtures: every positive flow
# exercises these, so PENDING can never reach launch in a passing test.
FIX_SOURCE = 'git:fixtures/tuna-src@deadbeefcafedeadbeefcafedeadbeefcafedeadbeef'
FIX_BUILD = 'fixture-build-0042'
FIX_PACKAGE = 'tuna-desktop=1.0-fixture-1'
FIX_DISK = 'disk-record-fixture-7'

STUB_CORRAL = """#!/bin/sh
# Fixture stub emulating ONLY the reviewed corral surface this consumer
# coordinates with: the `version` line shape and `launch` argv capture.
case "$1" in
    version)
        printf 'corral %s (commit deadbeefcafe, built 2026-01-01, go1.24.0)\\n' "$STUB_VERSION"
        ;;
    launch)
        printf '%s\\n' "$@" >"$ARGV_LOG"
        if [ -n "${HOME_LOG:-}" ]; then printf '%s\\n' "$HOME" >"$HOME_LOG"; fi
        shift
        if [ "${1:-}" = '--help' ]; then exit 0; fi
        printf 'Console: %s\\n' "$STUB_CONSOLE" >&2
        exit 0
        ;;
    *)
        printf 'stub: unknown command %s\\n' "$1" >&2
        exit 1
        ;;
esac
"""

VERSION_ONLY_CORRAL = """#!/bin/sh
case "$1" in
    version) printf 'corral %s (commit deadbeefcafe, built 2026-01-01, go1.24.0)\\n' "$STUB_VERSION" ;;
    *) printf 'stub: unknown command %s\\n' "$1" >&2; exit 1 ;;
esac
"""

# Silent hung stub: emits nothing, never exits (bounded-probe control).
HANG_CORRAL = """#!/bin/sh
sleep 30
"""

# Hung stub that first spawns a lingering grandchild (descendant control):
# the grandchild records its pid, then sleeps past any probe deadline.
GRANDCHILD_CORRAL = """#!/bin/sh
printf 'spawned\\n' >>"$SPAWN_LOG"
sleep 30 &
printf '%s\\n' "$!" >"$GRANDCHILD_PID"
sleep 30
"""

# Valid version line but a failing exit status (exact-retirement control).
NONZERO_VERSION_CORRAL = """#!/bin/sh
case "$1" in
    version)
        printf 'corral %s (commit deadbeefcafe, built 2026-01-01, go1.24.0)\\n' "$STUB_VERSION"
        exit 3
        ;;
    launch)
        shift
        if [ "${1:-}" = '--help' ]; then exit 0; fi
        printf 'Console: %s\\n' "$STUB_CONSOLE" >&2
        exit 0
        ;;
    *)
        printf 'stub: unknown command %s\\n' "$1" >&2
        exit 1
        ;;
esac
"""

# Interrupt control: records its own pid, then hangs (replaced by sleep
# via exec, so the recorded pid is the sleeper itself).
PIDFILE_HANG_CORRAL = """#!/bin/sh
printf '%s\\n' "$$" >"$PIDFILE"
exec sleep 30
"""

# External tools the script may invoke on the fixture paths below.
TOOL_NAMES = ('curl', 'sha256sum', 'df', 'awk', 'head', 'tail', 'cat',
              'cp', 'chmod', 'mv', 'rm', 'mkdir', 'mktemp',
              'sleep', 'stat', 'id')


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


class ConsumerCase(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix='corral-desktop-consumer-'))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.home = self.tmp / 'home'
        self.bindir = self.tmp / 'bin'
        self.bindir.mkdir()
        for name in ('qemu-system-x86_64', 'qemu-system-aarch64', 'qemu-img', 'xdg-open'):
            stub = self.bindir / name
            stub.write_text('#!/bin/sh\nexit 0\n')
            stub.chmod(0o755)
        self.argv_log = self.tmp / 'argv.log'
        self.home_log = self.tmp / 'home.log'
        # Fixture "corral binary": the stub, served over file:// with a real digest.
        self.corral_src = self.tmp / 'corral-fixture'
        self.corral_src.write_text(STUB_CORRAL)
        self.corral_src.chmod(0o755)
        self.corral_sha = sha256_bytes(self.corral_src.read_bytes())
        # Fixture image bytes exist only to give the manifest honest-looking
        # coordinates; the consumer never downloads the image (corral does).
        self.image_bytes = b'fixture-qcow2-bytes' * 64
        self.image_sha = sha256_bytes(self.image_bytes)

    def base_env(self, extra=None, farm=None):
        env = dict(os.environ)
        env.pop('TUNA_CORRAL_DESKTOP_HOME', None)
        for key in list(env):
            if key.startswith('CORRAL_OVMF_'):
                env.pop(key)
        env['TUNA_CORRAL_DESKTOP_TEST_SEAMS'] = '1'
        env['TUNA_CORRAL_DESKTOP_UNAME_S'] = 'Linux'
        env['TUNA_CORRAL_DESKTOP_UNAME_M'] = 'x86_64'
        env['TUNA_CORRAL_DESKTOP_DF_KB'] = str(50 * 1024 * 1024)
        env['TUNA_CORRAL_DESKTOP_FIXTURE_CORRAL_VERSION'] = STUB_VERSION
        env['TUNA_CORRAL_DESKTOP_FIXTURE_CORRAL_URL'] = self.corral_src.as_uri()
        env['TUNA_CORRAL_DESKTOP_FIXTURE_CORRAL_SHA256'] = self.corral_sha
        env['TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_URL'] = 'https://images.example.invalid/tuna-v1-amd64.qcow2'
        env['TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_SHA256'] = self.image_sha
        env['TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_SIZE'] = '1048576'
        env['TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_FIRMWARE'] = 'uefi'
        env['TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_FORMAT'] = 'qcow2'
        env['TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_VIRTUAL_SIZE'] = '2147483648'
        env['TUNA_CORRAL_DESKTOP_FIXTURE_SOURCE'] = FIX_SOURCE
        env['TUNA_CORRAL_DESKTOP_FIXTURE_BUILD'] = FIX_BUILD
        env['TUNA_CORRAL_DESKTOP_FIXTURE_PACKAGE'] = FIX_PACKAGE
        env['TUNA_CORRAL_DESKTOP_FIXTURE_DISK'] = FIX_DISK
        env['STUB_VERSION'] = STUB_VERSION
        env['STUB_CONSOLE'] = STUB_CONSOLE
        env['ARGV_LOG'] = str(self.argv_log)
        env['HOME_LOG'] = str(self.home_log)
        if farm is not None:
            env['PATH'] = str(farm)
        else:
            env['PATH'] = str(self.bindir) + os.pathsep + env['PATH']
        if extra:
            env.update(extra)
        return env

    def make_farm(self, exclude=(), with_qemu=True):
        farm = self.tmp / 'farm'
        if farm.exists():
            shutil.rmtree(farm)
        farm.mkdir()
        for name in TOOL_NAMES:
            if name in exclude:
                continue
            src = shutil.which(name)
            self.assertTrue(src, f'need real {name} to build the tool farm')
            os.symlink(src, farm / name)
        if with_qemu:
            for name in ('qemu-system-x86_64', 'qemu-system-aarch64', 'qemu-img'):
                stub = farm / name
                stub.write_text('#!/bin/sh\nexit 0\n')
                stub.chmod(0o755)
        return farm

    def run_script(self, *args, env=None):
        if env is None:
            env = self.base_env()
        return subprocess.run([SH, str(SCRIPT), '--home', str(self.home), *args],
                              env=env, capture_output=True, text=True, timeout=120)

    def run_script_with_home(self, home, *args, env=None, cwd=None, timeout=120):
        if env is None:
            env = self.base_env()
        return subprocess.run([SH, str(SCRIPT), '--home', str(home), *args],
                              env=env, capture_output=True, text=True,
                              cwd=cwd, timeout=timeout)

    def write_stub(self, name, body):
        path = self.tmp / name
        path.write_text(body)
        path.chmod(0o755)
        return path

    def own_state_skeleton(self, home):
        # A strictly-owned (exact marker, real child dirs) state dir.
        for child in ('bin', 'manifests', 'tmp'):
            (home / child).mkdir(parents=True, exist_ok=True)
        (home / 'tuna-corral-desktop.state').write_text(
            'tuna-corral-desktop-state 1\n')
        return home

    @staticmethod
    def poll_dead(pid, timeout=10.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return True
            except PermissionError:
                return False
            time.sleep(0.2)
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        return False

    @staticmethod
    def poll_exists(path, timeout=15.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if path.exists():
                return True
            time.sleep(0.2)
        return path.exists()

    # --- static contract ---

    def test_script_is_executable_posix_sh(self):
        self.assertTrue(os.access(SCRIPT, os.X_OK))
        first = SCRIPT.read_text().splitlines()[0]
        self.assertEqual(first, '#!/bin/sh')
        probed = subprocess.run([SH, '-n', str(SCRIPT)], capture_output=True, text=True)
        self.assertEqual(probed.returncode, 0, probed.stderr)

    def test_no_rolling_labels_or_invented_sources(self):
        text = SCRIPT.read_text()
        self.assertNotIn('releases/download/', text)
        self.assertNotIn('latest', text)
        self.assertNotIn('CURRENT', text)
        self.assertNotIn('http://', text)

    def test_contract_names_both_pending_gates(self):
        proc = self.run_script('--print-contract')
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn('gate.corral: PENDING', proc.stdout)
        self.assertIn('gate.tuna-image: PENDING', proc.stdout)
        for key in ('corral.version', 'corral.linux/amd64',
                    'tuna.amd64.manifest', 'tuna.amd64.source', 'tuna.amd64.build',
                    'tuna.amd64.image', 'tuna.amd64.package', 'tuna.amd64.disk',
                    'tuna.arm64.image'):
            self.assertIn(key, proc.stdout)
        pending_lines = [line for line in proc.stdout.splitlines() if 'PENDING' in line]
        # Exact tripwire: 1 version + 4 platform + 2 arches x (source, build,
        # image, package, disk) + 2 gate lines. Admitting a real value must
        # update this count deliberately.
        self.assertEqual(len(pending_lines), 1 + 4 + 2 * 5 + 2)

    def test_help_and_option_errors(self):
        proc = self.run_script('--help')
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn('Usage:', proc.stdout)
        self.assertIn('--allow-cross-arch', proc.stdout)
        bad = self.run_script('--bogus-flag')
        self.assertNotEqual(bad.returncode, 0)
        self.assertIn('unknown option', bad.stderr)
        missing = self.run_script('--cpu')
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn('needs a value', missing.stderr)

    # --- fail-closed without fixtures ---

    def test_default_flow_fails_closed_on_tuna_gate_before_state(self):
        # Admission precedes state and network: the Tuna gate fails first
        # and no ownership marker is minted by the failed startup.
        env = self.base_env()
        for key in list(env):
            if key.startswith('TUNA_CORRAL_DESKTOP_FIXTURE_'):
                env.pop(key)
        proc = self.run_script(env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('no release-qualified Tuna VM image', proc.stderr)
        self.assertFalse((self.home / 'bin' / 'corral').exists())
        self.assertFalse((self.home / 'tuna-corral-desktop.state').exists())

    def test_default_flow_fails_closed_on_corral_gate(self):
        # With the Tuna record admitted but no Corral pin, the Corral
        # gate still fails closed and refuses the rolling tag.
        env = self.base_env()
        for key in list(env):
            if key.startswith('TUNA_CORRAL_DESKTOP_FIXTURE_CORRAL_'):
                env.pop(key)
        proc = self.run_script(env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('no immutable Corral', proc.stderr)
        self.assertIn('rolling tag', proc.stderr)
        self.assertFalse((self.home / 'bin' / 'corral').exists())

    def test_fixture_vars_ignored_without_seams(self):
        env = self.base_env()
        env.pop('TUNA_CORRAL_DESKTOP_TEST_SEAMS')
        proc = self.run_script(env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('no release-qualified Tuna VM image', proc.stderr)

    def test_print_manifest_fails_closed(self):
        env = self.base_env()
        for key in list(env):
            if key.startswith('TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_'):
                env.pop(key)
        proc = self.run_script('--print-manifest', env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('no release-qualified Tuna VM image', proc.stderr)

    def test_unsupported_host_matrix_refused(self):
        env = self.base_env(extra={'TUNA_CORRAL_DESKTOP_UNAME_S': 'FreeBSD'})
        proc = self.run_script('--check-prereqs', env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('Linux/macOS only', proc.stderr)
        env = self.base_env(extra={'TUNA_CORRAL_DESKTOP_UNAME_M': 'sparc'})
        proc = self.run_script('--check-prereqs', env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('amd64/arm64 only', proc.stderr)

    def test_qemu_missing_refused_with_action(self):
        for path in ('/usr/bin/qemu-system-x86_64', '/usr/local/bin/qemu-system-x86_64',
                     '/opt/homebrew/bin/qemu-system-x86_64'):
            if os.path.exists(path):
                self.skipTest(f'host carries real QEMU at {path}; absence untestable here')
        farm = self.make_farm(with_qemu=False)
        proc = self.run_script('--check-prereqs', env=self.base_env(farm=farm))
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('QEMU for amd64 not found', proc.stderr)
        self.assertIn('qemu-system', proc.stderr)

    def test_qemu_emulator_missing_arm64_refused(self):
        # Same refusal branch as above, via the absent aarch64 emulator, so
        # the actionable-error path executes even on hosts carrying x86 QEMU.
        candidates = ['/opt/homebrew/bin/qemu-system-aarch64',
                      '/usr/local/bin/qemu-system-aarch64',
                      '/usr/bin/qemu-system-aarch64',
                      '/home/linuxbrew/.linuxbrew/bin/qemu-system-aarch64']
        if shutil.which('qemu-system-aarch64') or any(os.path.exists(path) for path in candidates):
            self.skipTest('host carries an aarch64 emulator; absence untestable here')
        farm = self.make_farm(with_qemu=False)
        proc = self.run_script('--arch', 'arm64', '--allow-cross-arch', '--check-prereqs',
                               env=self.base_env(farm=farm))
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('QEMU for arm64 not found', proc.stderr)
        self.assertIn('qemu-system-aarch64', proc.stderr)

    def test_cross_arch_needs_flag(self):
        proc = self.run_script('--arch', 'arm64', '--check-prereqs')
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('--allow-cross-arch', proc.stderr)
        proc = self.run_script('--arch', 'arm64', '--allow-cross-arch', '--check-prereqs')
        self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_bad_cpu_refused(self):
        proc = self.run_script('--cpu', '2x', '--check-prereqs')
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('--cpu', proc.stderr)

    # --- manifest rendering and validation ---

    def expected_manifest(self, arch='amd64', firmware='uefi', fmt='qcow2'):
        return {
            'version': '1',
            'name': 'tuna-v1',
            'image': {
                'url': 'https://images.example.invalid/tuna-v1-amd64.qcow2',
                'sha256': self.image_sha,
                'size': 1048576,
                'arch': arch,
                'firmware': firmware,
                'format': fmt,
                'virtual_size': 2147483648,
            },
        }

    def test_print_manifest_fixture_exact(self):
        proc = self.run_script('--print-manifest')
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(json.loads(proc.stdout), self.expected_manifest())
        on_disk = self.home / 'manifests' / 'tuna-amd64.json'
        self.assertEqual(json.loads(on_disk.read_text()), self.expected_manifest())

    def test_manifest_refuses_http_url(self):
        env = self.base_env(extra={'TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_URL': 'http://plain.example/tuna.qcow2'})
        proc = self.run_script('--print-manifest', env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('https:// or file://', proc.stderr)

    def test_manifest_refuses_bad_digest(self):
        for bad in ('ABCDEF' * 10 + 'ABCD', '0' * 63, 'g' * 64):
            env = self.base_env(extra={'TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_SHA256': bad})
            proc = self.run_script('--print-manifest', env=env)
            self.assertNotEqual(proc.returncode, 0, bad)
            self.assertIn('sha256', proc.stderr)

    def test_manifest_refuses_bad_size(self):
        for bad in ('0', '-5', '12x'):
            env = self.base_env(extra={'TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_SIZE': bad})
            proc = self.run_script('--print-manifest', env=env)
            self.assertNotEqual(proc.returncode, 0, bad)
            self.assertIn('size', proc.stderr)

    def test_manifest_refuses_arm64_bios(self):
        env = self.base_env(extra={'TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_FIRMWARE': 'bios'})
        proc = self.run_script('--arch', 'arm64', '--allow-cross-arch', '--print-manifest', env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('UEFI', proc.stderr)

    def test_manifest_refuses_bad_format(self):
        env = self.base_env(extra={'TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_FORMAT': 'vmdk'})
        proc = self.run_script('--print-manifest', env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('format', proc.stderr)

    def test_manifest_requires_virtual_size(self):
        env = self.base_env(extra={'TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_VIRTUAL_SIZE': '0'})
        proc = self.run_script('--print-manifest', env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('virtual_size', proc.stderr)

    # --- full fixture flow ---

    def expected_argv(self, manifest):
        return ['launch', '--manifest', manifest, '--name', 'tuna-v1',
                '--mem', '4G', '--cpu', '2', '--disk', '20G',
                '--web-port', '0', '--vm-timeout', '300', '--web-timeout', '30']

    def test_full_flow_argv_and_owned_state(self):
        proc = self.run_script()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn('digest verified', proc.stderr)
        self.assertIn(f'Console: {STUB_CONSOLE}', proc.stderr)
        manifest = str(self.home / 'manifests' / 'tuna-amd64.json')
        self.assertEqual(self.argv_log.read_text().splitlines(), self.expected_argv(manifest))
        cached = self.home / 'bin' / 'corral'
        self.assertTrue(cached.exists())
        self.assertTrue(os.access(cached, os.X_OK))
        self.assertEqual((self.home / 'bin' / 'corral.sha256').read_text().strip(), self.corral_sha)
        version_line = (self.home / 'bin' / 'corral.version').read_text().strip()
        self.assertTrue(version_line.startswith(f'corral {STUB_VERSION} ('), version_line)
        self.assertEqual(json.loads(Path(manifest).read_text()), self.expected_manifest())
        leftovers = [path for path in (self.home / 'tmp').iterdir()]
        self.assertEqual(leftovers, [])
        self.assertEqual((self.home / 'tuna-corral-desktop.state').read_text().strip(),
                         'tuna-corral-desktop-state 1')

    def test_flow_argv_uses_only_reviewed_c4_flags(self):
        proc = self.run_script('--stop-vm', '--force', '--allow-emulation')
        self.assertEqual(proc.returncode, 0, proc.stderr)
        argv = self.argv_log.read_text().splitlines()
        self.assertEqual(argv[0], 'launch')
        self.assertIn('--stop-vm', argv)
        self.assertIn('--force', argv)
        self.assertIn('--allow-emulation', argv)
        for token in argv[1:]:
            if token.startswith('--'):
                self.assertIn(token, C4_LAUNCH_FLAGS, token)

    def test_flow_custom_options_pass_through(self):
        proc = self.run_script('--name', 'demo', '--mem', '8G', '--cpu', '4',
                               '--disk', '40G', '--web-port', '8080',
                               '--vm-timeout', '60', '--web-timeout', '10')
        self.assertEqual(proc.returncode, 0, proc.stderr)
        argv = self.argv_log.read_text().splitlines()
        for flag, value in (('--name', 'demo'), ('--mem', '8G'), ('--cpu', '4'),
                            ('--disk', '40G'), ('--web-port', '8080'),
                            ('--vm-timeout', '60'), ('--web-timeout', '10')):
            self.assertIn(flag, argv)
            self.assertEqual(argv[argv.index(flag) + 1], value)

    def test_corral_checksum_mismatch_refused(self):
        env = self.base_env(extra={'TUNA_CORRAL_DESKTOP_FIXTURE_CORRAL_SHA256': '0' * 64})
        proc = self.run_script(env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('checksum mismatch', proc.stderr)
        self.assertFalse((self.home / 'bin' / 'corral').exists())

    def test_corral_version_identity_enforced(self):
        env = self.base_env(extra={'STUB_VERSION': '9.9.9-drift'})
        proc = self.run_script(env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('identity mismatch', proc.stderr)

    def test_corral_without_launch_refused(self):
        only_version = self.tmp / 'corral-version-only'
        only_version.write_text(VERSION_ONLY_CORRAL)
        only_version.chmod(0o755)
        env = self.base_env(extra={
            'TUNA_CORRAL_DESKTOP_FIXTURE_CORRAL_URL': only_version.as_uri(),
            'TUNA_CORRAL_DESKTOP_FIXTURE_CORRAL_SHA256': sha256_bytes(only_version.read_bytes()),
        })
        proc = self.run_script(env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("no working 'launch' command", proc.stderr)

    def test_headroom_refused(self):
        env = self.base_env(extra={'TUNA_CORRAL_DESKTOP_DF_KB': '100'})
        proc = self.run_script(env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('need 4.3 GB free', proc.stderr)
        self.assertIn('have 0.0 GB', proc.stderr)
        self.assertIn(str(self.home / '.local' / 'share' / 'corral' / 'vms'),
                      proc.stderr)

    def test_opener_absent_still_succeeds_soft(self):
        farm = self.make_farm()
        self.assertFalse((farm / 'xdg-open').exists())
        proc = self.run_script(env=self.base_env(farm=farm))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn('xdg-open', proc.stderr)
        self.assertIn('manually', proc.stderr)
        self.assertIn(f'Console: {STUB_CONSOLE}', proc.stderr)

    def test_cached_corral_reused(self):
        first = self.run_script()
        self.assertEqual(first.returncode, 0, first.stderr)
        self.corral_src.unlink()
        second = self.run_script()
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertIn('reusing cached corral', second.stderr)

    # --- operator-supplied binary and lifecycle ---

    def test_corral_bin_unpinned_needs_ack(self):
        env = self.base_env()
        for key in list(env):
            if key.startswith('TUNA_CORRAL_DESKTOP_FIXTURE_CORRAL_'):
                env.pop(key)
        proc = self.run_script('--corral-bin', str(self.corral_src), env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('--allow-unpinned-corral', proc.stderr)
        proc = self.run_script('--corral-bin', str(self.corral_src),
                               '--allow-unpinned-corral', env=env)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn('UNPINNED corral', proc.stderr)
        self.assertIn('no pin metadata recorded', proc.stderr)
        self.assertTrue((self.home / 'bin' / 'corral').exists())
        self.assertFalse((self.home / 'bin' / 'corral.version').exists())
        self.assertFalse((self.home / 'bin' / 'corral.sha256').exists())
        self.assertFalse((self.home / 'bin' / 'corral.provenance').exists())

    def test_corral_bin_missing_refused(self):
        proc = self.run_script('--corral-bin', str(self.tmp / 'no-such-file'),
                               '--allow-unpinned-corral')
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('not an executable file', proc.stderr)

    def test_clean_removes_owned_state_only(self):
        proc = self.run_script()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        cleaned = self.run_script('--clean')
        self.assertEqual(cleaned.returncode, 0, cleaned.stderr)
        self.assertFalse((self.home / 'bin').exists())
        self.assertFalse((self.home / 'manifests').exists())
        self.assertFalse((self.home / 'tmp').exists())
        self.assertFalse((self.home / 'tuna-corral-desktop.state').exists())
        # The scoped Corral VM/cache home is never clean inventory: its
        # skeleton survives with a preservation notice.
        self.assertIn('preserved', cleaned.stderr)
        self.assertTrue((self.home / '.local' / 'share' / 'corral' / 'vms').is_dir())
        stray = self.tmp / 'stray'
        stray.mkdir()
        env = self.base_env()
        refused = subprocess.run([SH, str(SCRIPT), '--home', str(stray), '--clean'],
                                 env=env, capture_output=True, text=True, timeout=60)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn('state marker', refused.stderr)
        self.assertTrue(stray.exists())


    # --- state ownership: never adopt, never over-clean (review P1) ---

    def test_unmarked_dir_failed_flow_mints_no_marker_and_clean_refuses(self):
        claimed = self.tmp / 'claimed'
        claimed.mkdir()
        sentinel = claimed / 'precious.txt'
        sentinel.write_text('do not touch\n')
        env = self.base_env()
        for key in list(env):
            if key.startswith('TUNA_CORRAL_DESKTOP_FIXTURE_'):
                env.pop(key)
        proc = self.run_script_with_home(claimed, env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('no release-qualified Tuna VM image', proc.stderr)
        self.assertFalse((claimed / 'tuna-corral-desktop.state').exists())
        self.assertFalse((claimed / 'bin').exists())
        self.assertTrue(sentinel.exists())
        refused = self.run_script_with_home(claimed, '--clean', env=env)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn('state marker', refused.stderr)
        self.assertTrue(sentinel.exists())

    def test_wrong_marker_refused_before_writes(self):
        foreign = self.tmp / 'foreign'
        (foreign / 'sub').mkdir(parents=True)
        (foreign / 'tuna-corral-desktop.state').write_text('bogus-state 9\n')
        sentinel = foreign / 'keep.txt'
        sentinel.write_text('x\n')
        proc = self.run_script_with_home(foreign, '--print-manifest')
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('marker mismatch', proc.stderr)
        self.assertFalse((foreign / 'bin').exists())
        self.assertFalse((foreign / 'manifests').exists())
        self.assertTrue(sentinel.exists())

    def test_symlink_boundaries_refused(self):
        real = self.tmp / 'realhome'
        real.mkdir()
        link = self.tmp / 'linkhome'
        link.symlink_to(real)
        proc = self.run_script_with_home(link, '--print-manifest')
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('symlink', proc.stderr)
        owned = self.tmp / 'owned'
        self.own_state_skeleton(owned)
        outside = self.tmp / 'outside'
        outside.mkdir()
        shutil.rmtree(owned / 'bin')
        (owned / 'bin').symlink_to(outside)
        proc = self.run_script_with_home(owned, '--print-manifest')
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('symlink', proc.stderr)
        self.assertEqual(list(outside.iterdir()), [])

    def test_home_and_relative_paths_refused(self):
        scratch_home = self.tmp / 'scratchhome'
        scratch_home.mkdir()
        env = self.base_env(extra={'HOME': str(scratch_home)})
        for bad in (str(scratch_home), str(scratch_home) + '/',
                    str(scratch_home) + '/./'):
            with self.subTest(home=bad):
                proc = self.run_script_with_home(bad, '--print-manifest', env=env)
                self.assertNotEqual(proc.returncode, 0, bad)
                self.assertIn('HOME', proc.stderr)
        self.assertFalse((scratch_home / 'tuna-corral-desktop.state').exists())
        self.assertFalse((scratch_home / 'bin').exists())
        proc = self.run_script_with_home('rel/home', '--print-manifest',
                                         cwd=self.tmp)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('absolute', proc.stderr)
        self.assertFalse((self.tmp / 'rel').exists())

    def test_world_writable_home_refused(self):
        wide = self.tmp / 'wide'
        self.own_state_skeleton(wide)
        wide.chmod(0o777)
        proc = self.run_script_with_home(wide, '--print-manifest')
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('world-writable', proc.stderr)

    def test_clean_preserves_foreign_and_vm_state(self):
        proc = self.run_script()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        foreign = self.home / 'operator-notes.txt'
        foreign.write_text('keep\n')
        vmhome = self.home / '.local' / 'share' / 'corral' / 'vms'
        artifact = vmhome / 'guest-artifact.bin'
        artifact.write_bytes(b'x' * 64)
        cleaned = self.run_script('--clean')
        self.assertEqual(cleaned.returncode, 0, cleaned.stderr)
        self.assertIn('preserved', cleaned.stderr)
        self.assertFalse((self.home / 'bin').exists())
        self.assertFalse((self.home / 'manifests').exists())
        self.assertFalse((self.home / 'tmp').exists())
        self.assertFalse((self.home / 'tuna-corral-desktop.state').exists())
        self.assertTrue(foreign.exists())
        self.assertTrue(artifact.exists())
        self.assertTrue(self.home.exists())

    def test_clean_refuses_active_lock_and_recovers_stale(self):
        proc = self.run_script()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        lockdir = self.home / '.lock'
        sleeper = subprocess.Popen(['sleep', '60'])
        try:
            lockdir.mkdir()
            (lockdir / 'owner').write_text(f'{sleeper.pid}\n')
            refused = self.run_script('--clean')
            self.assertNotEqual(refused.returncode, 0)
            self.assertIn('state in use', refused.stderr)
            self.assertIn(str(sleeper.pid), refused.stderr)
            self.assertTrue(self.home.exists())
        finally:
            sleeper.terminate()
            sleeper.wait()
        cleaned = self.run_script('--clean')
        self.assertEqual(cleaned.returncode, 0, cleaned.stderr)

    # --- qualification binding: PENDING never reaches launch (review P1) ---

    def test_each_pending_qual_identity_refuses_manifest(self):
        cases = (('TUNA_CORRAL_DESKTOP_FIXTURE_SOURCE', 'source'),
                 ('TUNA_CORRAL_DESKTOP_FIXTURE_BUILD', 'build'),
                 ('TUNA_CORRAL_DESKTOP_FIXTURE_PACKAGE', 'package'),
                 ('TUNA_CORRAL_DESKTOP_FIXTURE_DISK', 'disk'))
        for key, field in cases:
            with self.subTest(field=field):
                env = self.base_env()
                env.pop(key)
                proc = self.run_script('--print-manifest', env=env)
                self.assertNotEqual(proc.returncode, 0, field)
                self.assertIn('no release-qualified Tuna VM image', proc.stderr)
                self.assertIn('qualification', proc.stderr)

    def test_flow_refuses_pending_qual_before_state_or_network(self):
        env = self.base_env()
        env.pop('TUNA_CORRAL_DESKTOP_FIXTURE_SOURCE')
        proc = self.run_script(env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('qualification', proc.stderr)
        self.assertFalse(self.home.exists())

    def test_malformed_qual_refused(self):
        for bad in ('line1\nline2', 'has\ttab', 'bad\x07bell', ''):
            with self.subTest(value=repr(bad)):
                env = self.base_env(
                    extra={'TUNA_CORRAL_DESKTOP_FIXTURE_BUILD': bad})
                proc = self.run_script('--print-manifest', env=env)
                self.assertNotEqual(proc.returncode, 0, repr(bad))
                self.assertIn('build', proc.stderr)

    def test_qualification_sidecar_binds_admitted_bytes(self):
        proc = self.run_script()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        sidecar = self.home / 'manifests' / 'tuna-amd64.qualification'
        text = sidecar.read_text()
        self.assertIn('provenance=fixture', text)
        self.assertIn(f'image.sha256={self.image_sha}', text)
        self.assertIn(f'source={FIX_SOURCE}', text)
        self.assertIn(f'build={FIX_BUILD}', text)
        self.assertIn(f'package={FIX_PACKAGE}', text)
        self.assertIn(f'disk={FIX_DISK}', text)
        other_sha = sha256_bytes(b'other-image-bytes')
        env = self.base_env(
            extra={'TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_SHA256': other_sha})
        proc = self.run_script(env=env)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        rebound = sidecar.read_text()
        self.assertIn(f'image.sha256={other_sha}', rebound)
        self.assertNotIn(self.image_sha, rebound)

    # --- operator binary: digest before execution (review P1) ---

    def test_corral_bin_same_version_wrong_digest_never_executes(self):
        marker = self.tmp / 'executed.marker'
        lines = STUB_CORRAL.splitlines(keepends=True)
        impostor = self.write_stub(
            'corral-impostor',
            lines[0] + f'printf "executed %s\\n" "$1" >>"{marker}"\n'
            + ''.join(lines[1:]) + '# different bytes, same version\n')
        self.assertNotEqual(sha256_bytes(impostor.read_bytes()), self.corral_sha)
        proc = self.run_script('--corral-bin', str(impostor))
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('digest mismatch', proc.stderr)
        self.assertIn('refusing to execute', proc.stderr)
        self.assertFalse(marker.exists())
        self.assertFalse((self.home / 'bin' / 'corral').exists())

    def test_corral_bin_admitted_writes_full_receipts(self):
        proc = self.run_script('--corral-bin', str(self.corral_src))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn('digest verified before execution', proc.stderr)
        cached = self.home / 'bin' / 'corral'
        self.assertTrue(cached.exists())
        self.assertEqual((self.home / 'bin' / 'corral.sha256').read_text().strip(),
                         self.corral_sha)
        version_line = (self.home / 'bin' / 'corral.version').read_text().strip()
        self.assertTrue(version_line.startswith(f'corral {STUB_VERSION} ('),
                        version_line)
        self.assertEqual(
            (self.home / 'bin' / 'corral.provenance').read_text().strip(),
            'fixture')

    def test_corral_bin_version_mismatch_refused(self):
        env = self.base_env(extra={'STUB_VERSION': '9.9.9-drift'})
        proc = self.run_script('--corral-bin', str(self.corral_src), env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('identity mismatch', proc.stderr)

    def test_cache_provenance_mismatch_forces_reacquire(self):
        first = self.run_script()
        self.assertEqual(first.returncode, 0, first.stderr)
        (self.home / 'bin' / 'corral.provenance').write_text('release-pin\n')
        self.corral_src.unlink()
        second = self.run_script()
        self.assertNotEqual(second.returncode, 0)
        self.assertNotIn('reusing cached corral', second.stderr)

    def test_cache_tampered_metadata_forces_reacquire(self):
        first = self.run_script()
        self.assertEqual(first.returncode, 0, first.stderr)
        (self.home / 'bin' / 'corral.sha256').write_text('0' * 64 + '\n')
        second = self.run_script()
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertNotIn('reusing cached corral', second.stderr)
        self.assertIn('acquired corral', second.stderr)

    # --- concurrency: serialized install/cleanup (review P2) ---

    def test_installer_waits_for_held_lock(self):
        self.own_state_skeleton(self.home)
        sleeper = subprocess.Popen(['sleep', '4'])
        # Reap the holder promptly once it exits: until waited-on it
        # lingers as a zombie, and kill -0 (the only portable liveness
        # probe) still succeeds on zombies, so the lock would read live.
        threading.Thread(target=sleeper.wait, daemon=True).start()
        try:
            lockdir = self.home / '.lock'
            lockdir.mkdir()
            (lockdir / 'owner').write_text(f'{sleeper.pid}\n')
            start = time.monotonic()
            proc = self.run_script()
            elapsed = time.monotonic() - start
        finally:
            sleeper.wait()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertGreaterEqual(elapsed, 3.0)

    def test_lock_wait_exhaustion_refuses(self):
        self.own_state_skeleton(self.home)
        sleeper = subprocess.Popen(['sleep', '30'])
        try:
            lockdir = self.home / '.lock'
            lockdir.mkdir()
            (lockdir / 'owner').write_text(f'{sleeper.pid}\n')
            env = self.base_env(
                extra={'TUNA_CORRAL_DESKTOP_FIXTURE_LOCK_WAIT': '2'})
            proc = self.run_script(env=env)
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn('cannot acquire state lock', proc.stderr)
            self.assertFalse((self.home / 'bin' / 'corral').exists())
        finally:
            sleeper.terminate()
            sleeper.wait()

    def test_concurrent_installers_consistent(self):
        results = {}
        env_a = self.base_env(extra={'ARGV_LOG': str(self.tmp / 'argv-a.log')})
        env_b = self.base_env(extra={'ARGV_LOG': str(self.tmp / 'argv-b.log')})

        def one(name, env):
            results[name] = subprocess.run(
                [SH, str(SCRIPT), '--home', str(self.home)], env=env,
                capture_output=True, text=True, timeout=120)

        threads = [threading.Thread(target=one, args=('a', env_a)),
                   threading.Thread(target=one, args=('b', env_b))]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        self.assertEqual(results['a'].returncode, 0, results['a'].stderr)
        self.assertEqual(results['b'].returncode, 0, results['b'].stderr)
        cached = self.home / 'bin' / 'corral'
        self.assertEqual(sha256_bytes(cached.read_bytes()), self.corral_sha)
        manifest = json.loads(
            (self.home / 'manifests' / 'tuna-amd64.json').read_text())
        self.assertEqual(manifest['image']['sha256'], self.image_sha)
        self.assertFalse((self.home / '.lock').exists())

    def test_stale_lock_recovered(self):
        self.own_state_skeleton(self.home)
        sleeper = subprocess.Popen(['sleep', '30'])
        dead_pid = sleeper.pid
        sleeper.terminate()
        sleeper.wait()
        lockdir = self.home / '.lock'
        lockdir.mkdir()
        (lockdir / 'owner').write_text(f'{dead_pid}\n')
        proc = self.run_script()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertFalse((self.home / '.lock').exists())

    # --- bounded probes: deadline, retirement, cancel (review P2) ---

    def stub_corral_env(self, body, name, extra=None):
        stub = self.write_stub(name, body)
        merged = {'TUNA_CORRAL_DESKTOP_FIXTURE_CORRAL_URL': stub.as_uri(),
                  'TUNA_CORRAL_DESKTOP_FIXTURE_CORRAL_SHA256':
                      sha256_bytes(stub.read_bytes())}
        if extra:
            merged.update(extra)
        return self.base_env(extra=merged)

    def test_hung_version_probe_times_out(self):
        env = self.stub_corral_env(HANG_CORRAL, 'corral-hang',
                                   {'TUNA_CORRAL_DESKTOP_FIXTURE_PROBE_TIMEOUT': '2'})
        start = time.monotonic()
        proc = self.run_script(env=env)
        elapsed = time.monotonic() - start
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('timed out', proc.stderr)
        self.assertLess(elapsed, 20.0)

    def test_hung_probe_kills_descendants(self):
        spawn_log = self.tmp / 'spawn.log'
        grandchild_pid = self.tmp / 'grandchild.pid'
        env = self.stub_corral_env(
            GRANDCHILD_CORRAL, 'corral-grandchild',
            {'TUNA_CORRAL_DESKTOP_FIXTURE_PROBE_TIMEOUT': '2',
             'SPAWN_LOG': str(spawn_log),
             'GRANDCHILD_PID': str(grandchild_pid)})
        proc = self.run_script(env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('timed out', proc.stderr)
        self.assertTrue(spawn_log.exists())
        if shutil.which('setsid') is None:
            self.skipTest('no setsid: grouped descendant sweep unavailable; '
                          'timeout refusal above is the portable guarantee')
        grandpid = int(grandchild_pid.read_text().strip())
        try:
            self.assertTrue(self.poll_dead(grandpid, timeout=10.0),
                            f'descendant {grandpid} survived the probe timeout')
        finally:
            try:
                os.kill(grandpid, signal.SIGKILL)
            except OSError:
                pass

    def test_nonzero_version_exit_refused(self):
        env = self.stub_corral_env(NONZERO_VERSION_CORRAL, 'corral-nonzero')
        proc = self.run_script(env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('failed with status 3', proc.stderr)

    def test_interrupt_kills_probe_and_exits(self):
        pidfile = self.tmp / 'probe.pid'
        stub = self.write_stub('corral-int', PIDFILE_HANG_CORRAL)
        env = self.base_env(extra={
            'TUNA_CORRAL_DESKTOP_FIXTURE_CORRAL_URL': stub.as_uri(),
            'TUNA_CORRAL_DESKTOP_FIXTURE_CORRAL_SHA256':
                sha256_bytes(stub.read_bytes()),
            'TUNA_CORRAL_DESKTOP_FIXTURE_PROBE_TIMEOUT': '30',
            'PIDFILE': str(pidfile)})
        proc = subprocess.Popen([SH, str(SCRIPT), '--home', str(self.home)],
                                env=env, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True)
        try:
            self.assertTrue(self.poll_exists(pidfile),
                            'probe never started; cannot interrupt it')
            probepid = int(pidfile.read_text().strip())
            start = time.monotonic()
            proc.send_signal(signal.SIGTERM)
            _, stderr = proc.communicate(timeout=15)
            elapsed = time.monotonic() - start
            self.assertEqual(proc.returncode, 143, stderr)
            self.assertLess(elapsed, 12.0)
            self.assertTrue(self.poll_dead(probepid, timeout=10.0),
                            f'probe child {probepid} survived SIGTERM')
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait()
            if pidfile.exists():
                try:
                    os.kill(int(pidfile.read_text().strip()), signal.SIGKILL)
                except OSError:
                    pass

    # --- VM home scoping (review nonblocking: real storage target) ---

    def test_launch_child_home_scoped_under_home(self):
        proc = self.run_script()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(self.home_log.read_text().splitlines(), [str(self.home)])

    def test_vm_home_created_under_home(self):
        proc = self.run_script()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(
            (self.home / '.local' / 'share' / 'corral' / 'vms').is_dir())

    # --- canonical/bounded numerics (review nonblocking) ---

    def test_leading_zero_size_refused(self):
        env = self.base_env(
            extra={'TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_SIZE': '01048576'})
        proc = self.run_script('--print-manifest', env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('canonical', proc.stderr)
        self.assertIn('size', proc.stderr)

    def test_manifest_size_bounds(self):
        int64max = '9223372036854775807'
        env = self.base_env(
            extra={'TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_SIZE': int64max,
                   'TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_VIRTUAL_SIZE': int64max})
        proc = self.run_script('--print-manifest', env=env)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(json.loads(proc.stdout)['image']['size'], int(int64max))
        for bad in ('9223372036854775808', '9' * 40, '0', '1.5', '0x10'):
            with self.subTest(value=bad):
                env = self.base_env(
                    extra={'TUNA_CORRAL_DESKTOP_FIXTURE_IMAGE_SIZE': bad})
                proc = self.run_script('--print-manifest', env=env)
                self.assertNotEqual(proc.returncode, 0, bad)
                self.assertIn('size', proc.stderr)

    def test_cli_numerics_canonical_and_bounded(self):
        bad_cases = (('--cpu', '02'), ('--cpu', '99999999999999999999999'),
                     ('--web-port', '00'), ('--web-port', '65536'),
                     ('--vm-timeout', '01'), ('--web-timeout', '3.5'))
        for flag, value in bad_cases:
            with self.subTest(flag=flag, value=value):
                proc = self.run_script(flag, value, '--check-prereqs')
                self.assertNotEqual(proc.returncode, 0, value)
                self.assertIn(flag, proc.stderr)
        ok_cases = (('--cpu', '0'), ('--web-port', '0'),
                    ('--web-port', '65535'), ('--vm-timeout', '300'))
        for flag, value in ok_cases:
            with self.subTest(flag=flag, value=value):
                proc = self.run_script(flag, value, '--check-prereqs')
                self.assertEqual(proc.returncode, 0, proc.stderr)

    # --- seams visibly non-release; gates use one matcher (review) ---

    def test_fixture_mode_visible_and_receipts_marked(self):
        proc = self.run_script()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn('FIXTURE MODE', proc.stderr)
        self.assertEqual(
            (self.home / 'bin' / 'corral.provenance').read_text().strip(),
            'fixture')
        sidecar = self.home / 'manifests' / 'tuna-amd64.qualification'
        self.assertIn('provenance=fixture', sidecar.read_text())
        contract = self.run_script('--print-contract')
        self.assertIn('mode: fixture', contract.stdout)
        env = self.base_env()
        env.pop('TUNA_CORRAL_DESKTOP_TEST_SEAMS')
        release = self.run_script('--print-contract', env=env)
        self.assertIn('mode: release coordinates', release.stdout)

    def test_contract_gates_use_release_constants(self):
        proc = self.run_script()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        contract = self.run_script('--print-contract')
        self.assertIn('gate.corral: PENDING', contract.stdout)
        self.assertIn('gate.tuna-image: PENDING', contract.stdout)


if __name__ == '__main__':
    unittest.main()

