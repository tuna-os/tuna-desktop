#!/usr/bin/env python3
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import unittest

SCRIPT=Path(__file__).with_name('media-completion-fixture.py')


class Fixture(unittest.TestCase):
    def invoke(self, command, fail=False):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'volume').write_text('0.4200');(root/'muted').write_text('0')
            with socket.socket(socket.AF_UNIX) as server:
                server.settimeout(2);server.bind(str(root/'control.sock'));server.listen(1)
                child=subprocess.Popen(['/usr/bin/python3',str(SCRIPT),*command],env={**os.environ,'TUNA_MEDIA_FIXTURE_ROOT':str(root),'PYTHONDONTWRITEBYTECODE':'1'},stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
                try:
                    connection,_=server.accept()
                    with connection, connection.makefile('rwb',buffering=0) as wire:
                        enter=json.loads(wire.readline());self.assertEqual(enter['pid'],child.pid);self.assertEqual(enter['parent_pid'],os.getpid());self.assertEqual(enter['args'],command);self.assertGreater(enter['start_ticks'],0)
                        wire.write(json.dumps({'fail':True} if fail else {'go':True}).encode()+b'\n')
                        if not fail:
                            terminal=json.loads(wire.readline());self.assertEqual(terminal['stage'],'exit');self.assertEqual(terminal['pid'],enter['pid'])
                            self.assertIsNone(child.poll())
                            if command[0]=='set-volume':self.assertEqual((root/'volume').read_text(),command[2])
                            wire.write(b'{"go":true}\n')
                    out,err=child.communicate(timeout=2)
                    self.assertEqual(child.returncode,23 if fail else 0,err)
                    if fail:self.assertEqual((root/'volume').read_text(),'0.4200')
                    return out.strip()
                finally:
                    if child.poll() is None:child.kill();child.communicate(timeout=2)
    def test_actual_child_phase_blocks_terminal_exit(self):self.assertEqual(self.invoke(['set-volume','@DEFAULT_AUDIO_SINK@','1.0000']),'')
    def test_actual_owned_read(self):self.assertEqual(self.invoke(['get-volume','@DEFAULT_AUDIO_SINK@']),'Volume: 0.4200')
    def test_actual_owned_failure_refuses_mutation(self):self.invoke(['set-volume','@DEFAULT_AUDIO_SINK@','1.0000'],True)


if __name__=='__main__':unittest.main()
