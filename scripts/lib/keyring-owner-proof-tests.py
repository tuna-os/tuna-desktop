#!/usr/bin/env python3
"""Execute the production readiness predicate with mocked bus replies and real procfs."""
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]

class KeyringOwner(unittest.TestCase):
    def test_owner_pid_and_control_directory_must_all_match(self):
        script = (ROOT / 'scripts/tuna-gtk-shell-proof').read_text()
        guard = re.search(r'(?m)^keyring_owns_secrets\(\) \{\n.*?^\}', script, re.S).group()
        with tempfile.TemporaryDirectory() as directory:
            expected = str(Path(directory) / 'keyring-control')
            peer = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)', '--control-directory=' + expected])
            try:
                mock = '''
gdbus() {
 case "$*" in
 *NameHasOwner*) printf '(%%s,)\\n' "$OWNER_PRESENT";;
 *GetNameOwner*) printf "(':fixture.owner',)\\n";;
 *GetConnectionUnixProcessID*)
  [ "${!#}" = ':fixture.owner' ] || return 1
  printf '(uint32 %%s,)\\n' "$OWNER_PID";;
 *) return 1;;
 esac
}
'''.replace('%%', '%')
                for present, pid, artifacts, accepted in [
                    ('true', str(peer.pid), directory, True),
                    ('false', str(peer.pid), directory, False),
                    ('true', str(peer.pid), directory + '/wrong', False),
                    ('true', '2147483647', directory, False),
                ]:
                    with self.subTest(present=present, pid=pid, artifacts=artifacts):
                        env = {**os.environ, 'OWNER_PRESENT': present, 'OWNER_PID': pid, 'ARTIFACTS': artifacts}
                        result = subprocess.run(['bash', '-c', mock + guard + '\nkeyring_owns_secrets'], env=env, capture_output=True, text=True)
                        self.assertEqual(result.returncode == 0, accepted, result.stderr)
            finally:
                peer.terminate()
                peer.wait(timeout=5)

    def test_guard_runs_after_unlock_and_before_first_enterprise_request(self):
        script = (ROOT / 'scripts/tuna-gtk-shell-proof').read_text()
        self.assertLess(script.index('gnome-keyring-daemon --unlock'), script.index('keyring_owns_secrets()'))
        self.assertLess(script.index('keyring_owns_secrets || fail'), script.index('TUNA_NM_VPN_PLUGIN_DIR="$ARTIFACTS/vpn-plugins"', script.index('gnome-keyring-daemon --unlock')))

if __name__ == '__main__':
    unittest.main()
