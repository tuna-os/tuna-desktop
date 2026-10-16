"""Bind workspace menu proof to its originally launched native Alpha fixture.

This is a harness launch association, not an application authorization claim.
The original compositor native IDs survive overview focus changes; an X11
client or replacement same-name window is never accepted as that fixture.
"""
import json
from pathlib import Path
import sys

APP_ID = "tuna-test-alpha"


def process_identity(pid):
    root = Path('/proc') / str(pid)
    stat = (root / 'stat').read_text().rsplit(')', 1)[1].split()
    return {'pid': pid, 'uid': root.stat().st_uid, 'start_ticks': int(stat[19])}


def owner(scene, identity):
    windows = [w for w in scene['windows'] if w['app_id'] == APP_ID]
    if len(windows) != 1 or windows[0].get('x11_window_id') is not None:
        raise RuntimeError('original Alpha is not one unique native window')
    w = windows[0]
    return {**identity, 'id': w['id'], 'application_window_id': w['application_window_id'], 'app_id': APP_ID}


def target(scene, expected, live):
    if any(expected[k] != live[k] for k in ('pid', 'uid', 'start_ticks')):
        raise RuntimeError('original Alpha process lifetime changed')
    windows = [w for w in scene['windows'] if w['app_id'] == expected['app_id']]
    if len(windows) != 1:
        raise RuntimeError('original Alpha native identity is ambiguous or missing')
    w = windows[0]
    if (w.get('x11_window_id') is not None or w['id'] != expected['id']
            or w['application_window_id'] != expected['application_window_id']):
        raise RuntimeError('original Alpha native identity changed')
    if (scene.get('overview_open') or scene.get('overview_progress') != 0
            or scene['focused'] != w['id'] or scene['focused_app_id'] != expected['app_id']
            or scene['focused_rect'] != w['rect'] or scene['active_workspace'] != w['workspace']):
        raise RuntimeError('original Alpha is not focused and mapped on the active workspace')
    x, y, width, height = w['rect']
    if width <= 60 or height <= 20:
        raise RuntimeError('Alpha header geometry is not ready')
    return w['id'], w['workspace'], x + 60, y + 20


def main():
    mode, scene_path, owner_path, *args = sys.argv[1:]
    scene = json.loads(Path(scene_path).read_text())
    if mode == 'record':
        identity = process_identity(int(args[0]))
        Path(owner_path).write_text(json.dumps(owner(scene, identity), indent=2))
    elif mode == 'target':
        expected = json.loads(Path(owner_path).read_text())
        print(*target(scene, expected, process_identity(expected['pid'])))
    else:
        raise ValueError('unknown fixture operation')


if __name__ == '__main__':
    main()
