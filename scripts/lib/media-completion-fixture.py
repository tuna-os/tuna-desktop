#!/usr/bin/python3
"""Only for the isolated real MediaKeys CI test: parent controls child phases."""
import json
import os
from pathlib import Path
import socket
import sys


def main():
    root = Path(os.environ['TUNA_MEDIA_FIXTURE_ROOT'])
    identity = (Path('/proc/self/stat').read_text().rsplit(')', 1)[1].split())
    with socket.socket(socket.AF_UNIX) as peer:
        peer.settimeout(8)
        peer.connect(str(root / 'control.sock'))
        with peer.makefile('rwb', buffering=0) as wire:
            row = {'pid': os.getpid(), 'parent_pid': os.getppid(), 'start_ticks': int(identity[19]), 'args': sys.argv[1:]}
            wire.write((json.dumps({**row, 'stage':'enter'})+'\n').encode())
            command = json.loads(wire.readline())
            if command.get('fail'):
                return 23
            args = sys.argv[1:]
            output = ''
            if args[0] == 'get-volume':
                output = 'Volume: ' + (root/'volume').read_text().strip()
                if (root/'muted').read_text().strip() == '1':
                    output += ' [MUTED]'
            elif args[0] == 'set-volume':
                (root/'volume').write_text(args[2])
            elif args[0] == 'set-mute':
                (root/'muted').write_text(args[2])
            else:
                raise ValueError('unexpected production command')
            wire.write((json.dumps({**row,'stage':'exit'})+'\n').encode())
            if json.loads(wire.readline()) != {'go':True}:
                raise ValueError('terminal release required')
            print(output)
    return 0


if __name__ == '__main__':
    sys.exit(main())
