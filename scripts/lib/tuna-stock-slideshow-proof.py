#!/usr/bin/python3
"""Actual GNOME51 chooser and naturally advancing XML wallpaper proof."""
import hashlib
import json
import math
import os
from pathlib import Path
import re
import stat
import subprocess
import time
import struct
import sys
import zlib
import xml.etree.ElementTree as ET
import pyatspi
import gi
gi.require_version('GnomeBG','4.0')
gi.require_version('GdkPixbuf','2.0')
from gi.repository import Gio, GLib, GnomeBG, GdkPixbuf
OUT=Path('/out')
STATE=OUT/'compositor-state.json'
BUS=Gio.bus_get_sync(Gio.BusType.SESSION,None)
NAME='org.gnome.Settings'
EXE=Path('/usr/bin/gnome-control-center')
PHASE=sys.argv[1]
COLORS={'light-a':(240,0,0),'light-b':(0,0,240),'dark-a':(240,240,0),'dark-b':(120,0,160),'variant':(0,200,0)}
XML={style:OUT/(style+'-timeline.xml') for style in ['light','dark']}

def bus_call(method, signature, args):
    return BUS.call_sync('org.freedesktop.DBus', '/org/freedesktop/DBus',
                         'org.freedesktop.DBus', method,
                         GLib.Variant(signature, args), None,
                         Gio.DBusCallFlags.NONE, 5000, None).unpack()[0]

def drain():
    while GLib.MainContext.default().pending():
        GLib.MainContext.default().iteration(False)

# Innermost active proof deadline (monotonic seconds), or None outside
# a bounded wait. Subprocess calls budget against it so no external
# command can outlive the wait that issued it.
_ACTIVE_END = None

def budget(label, cap=10):
    """Remaining seconds under the active deadline for one external
    call, capped per call. Raises when the deadline already passed."""
    if _ACTIVE_END is None:
        return cap
    remaining = _ACTIVE_END - time.monotonic()
    if remaining <= 0:
        raise RuntimeError(label + ': no deadline budget left')
    return min(remaining, cap)

def wait_for(predicate, label, seconds=15):
    global _ACTIVE_END
    end = time.monotonic() + seconds
    if _ACTIVE_END is not None:
        # A nested wait can only tighten its parent's deadline
        # (e.g. the chooser's 120 s traversal budget), never extend it.
        end = min(end, _ACTIVE_END)
    previous, _ACTIVE_END = _ACTIVE_END, end
    try:
        while time.monotonic() < end:
            drain()
            if predicate():
                # A slow predicate can return true after the
                # deadline; recheck before accepting, never pass late.
                if time.monotonic() >= end:
                    break
                return
            time.sleep(.1)
        raise RuntimeError(label)
    finally:
        _ACTIVE_END = previous

def scene():
    return json.loads(STATE.read_text())

def identity(process):
    owner = bus_call('GetNameOwner', '(s)', (NAME,))
    pid = bus_call('GetConnectionUnixProcessID', '(s)', (owner,))
    uid = bus_call('GetConnectionUnixUser', '(s)', (owner,))
    metadata = EXE.stat()
    if (pid != process.pid or uid != os.getuid() or uid != 1000
            or Path(f'/proc/{pid}/exe').resolve() != EXE.resolve()
            or metadata.st_uid != 0 or metadata.st_mode & 0o022
            or not stat.S_ISREG(metadata.st_mode)):
        raise RuntimeError('Settings identity is not the actual installed ordinary-user app')
    proc_stat = Path(f'/proc/{pid}/stat').read_text()
    start_ticks = proc_stat.rsplit(')', 1)[1].split()[19]
    return {'owner': owner, 'pid': pid, 'uid': uid, 'start_ticks': start_ticks,
            'executable': str(EXE.resolve()), 'bytes': metadata.st_size,
            'sha256': hashlib.sha256(EXE.read_bytes()).hexdigest()}

def guard(process, original):
    if process.poll() is not None or identity(process) != original:
        raise RuntimeError('original Settings identity changed during the journey')

def controls(process, original, label):
    guard(process,original)
    nodes, found=[],[]
    def walk(node,depth):
        if depth>32 or len(nodes)>=4096: raise RuntimeError('bounded accessible tree exceeded')
        node.clear_cache()
        state=node.getState()
        showing=state.contains(pyatspi.STATE_SHOWING)
        record={'name':node.name or '', 'role':node.getRoleName(),'showing':showing,
                'checked':state.contains(pyatspi.STATE_CHECKED),'depth':depth}
        nodes.append(record)
        if state.contains(pyatspi.STATE_VISIBLE) and record['role']=='toggle button': found.append(node)
        for index in range(node.childCount):
            child=node.getChildAtIndex(index)
            if child is not None: walk(child,depth+1)
    desktop=pyatspi.Registry.getDesktop(0)
    for index in range(desktop.childCount):
        app=desktop.getChildAtIndex(index)
        if app is not None and app.get_process_id()==original['pid']: walk(app,0)
    (OUT/f'{label}-a11y.json').write_text(json.dumps(nodes,indent=2))
    return found

def session_identity():
    owner=bus_call('GetNameOwner','(s)',('org.gnome.Shell',))
    pid=bus_call('GetConnectionUnixProcessID','(s)',(owner,))
    exe=Path(f'/proc/{pid}/exe').resolve()
    if exe!=Path('/candidate/usr/bin/tuna-shell-gtk').resolve():raise RuntimeError('not actual candidate GTK shell')
    stat_text=Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()
    return {'owner':owner,'pid':pid,'start_time':stat_text[19],'executable':str(exe),'sha256':hashlib.sha256(exe.read_bytes()).hexdigest(),'compositor_pid':int(stat_text[1])}


def image(name,color):
    def chunk(kind,data):
        return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
    rows=b''.join(b'\x00'+bytes(color)*64 for _ in range(32))
    (OUT/(name+'.png')).write_bytes(b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',64,32,8,2,0,0,0))+chunk(b'IDAT',zlib.compress(rows))+chunk(b'IEND',b''))


def timeline(style,reverse=False):
    root=ET.Element('background')
    start=ET.SubElement(root,'starttime')
    for tag,value in [('year','2020'),('month','1'),('day','1'),('hour','0'),('minute','0'),('second','0')]:
        ET.SubElement(start,tag).text=value
    a,b=style+'-a',style+'-b'
    if reverse:a,b=b,a
    def files(node,tag,name):
        parent=ET.SubElement(node,tag)
        ET.SubElement(parent,'size',{'width':'1280','height':'800'}).text=str(OUT/(name+'.png'))
        ET.SubElement(parent,'size',{'width':'640','height':'400'}).text=str(OUT/'variant.png')
    for fixed,first,last,duration in [(True,a,None,4),(False,a,b,8),(True,b,None,4),(False,b,a,8)]:
        node=ET.SubElement(root,'static' if fixed else 'transition')
        ET.SubElement(node,'duration').text=str(duration)
        files(node,'file' if fixed else 'from',first)
        if last:files(node,'to',last)
    # Genuine XML source replacement; no setting write or time substitution.
    temporary=XML[style].with_suffix('.tmp')
    ET.ElementTree(root).write(temporary,encoding='utf-8',xml_declaration=True)
    temporary.replace(XML[style])


def fixture():
    for name,color in COLORS.items():image(name,color)
    for style in XML:timeline(style)
    directory=OUT/'data/gnome-background-properties';directory.mkdir(parents=True)
    root=ET.Element('wallpapers');item=ET.SubElement(root,'wallpaper',{'deleted':'false'})
    for tag,value in [('name','Tuna Desktop dynamic proof'),('filename',str(XML['light'])),('filename-dark',str(XML['dark'])),('options','stretched'),('shade_type','solid'),('pcolor','#00c800'),('scolor','#0000c8')]:
        ET.SubElement(item,tag).text=value
    ET.ElementTree(root).write(directory/'tuna-dynamic.xml',encoding='utf-8',xml_declaration=True)


def activate_chooser(find_chooser, process, original, label):
    global _ACTIVE_END
    # GNOME's chooser is GtkFlowBoxChild with an overridden accessible role.
    # Its generic Action indices enumerate muxer actions, not child::activate.
    # Retain that real inventory and activate the documented Enter keybinding.
    # These chooser accessibles do not honor programmatic focus grabs, so
    # focus is acquired with genuine Tab key events through the verified
    # keyboard host instead. Traversal sends only real Tabs, activation is
    # exactly one genuine Return, and every resolved accessible is re-read
    # live because the thumbnail panel rebuilds children asynchronously.
    hosts = subprocess.check_output(['xdotool', 'search', '--onlyvisible', '--name', '^Smithay'], text=True, timeout=budget('xdotool search Smithay host')).split()
    if len(hosts) != 1:
        raise RuntimeError('expected exactly one actual Smithay keyboard host')
    host = hosts[0]
    pid = int(subprocess.check_output(['xdotool', 'getwindowpid', host], text=True, timeout=budget('xdotool Smithay host pid')))
    executable = Path(f'/proc/{pid}/exe').resolve(strict=True)
    start = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19]
    actual = {'pid': pid, 'executable': str(executable), 'start_ticks': start,
              'sha256': hashlib.sha256(executable.read_bytes()).hexdigest()}
    if actual != ORIGINAL_HOST:
        raise RuntimeError('keyboard host identity changed from the original candidate compositor')
    subprocess.run(['xdotool', 'windowfocus', '--sync', host], check=True, timeout=min(5, budget('xdotool focus Smithay host')))
    control = None
    actions = None
    seen_showing = False
    tabs = 0
    end = time.monotonic() + 120
    # Traversal and the single key share this deadline; nested
    # wait_for calls save and restore it. Any raise aborts the
    # proof, so the restore below only serves the success path.
    previous_active, _ACTIVE_END = _ACTIVE_END, end
    while True:
        guard(process, original)
        if scene().get('focused_app_id') != NAME:
            raise RuntimeError('actual compositor focus left Settings during traversal')
        try:
            candidate = find_chooser()
        except GLib.GError:
            candidate = None
        if candidate is not None:
            try:
                candidate.clear_cache()
                if not candidate.getState().contains(pyatspi.STATE_SHOWING):
                    candidate.queryComponent().scrollTo(pyatspi.SCROLL_ANYWHERE)
                    drain()
                    candidate.clear_cache()
                if candidate.getState().contains(pyatspi.STATE_SHOWING):
                    seen_showing = True
                    action = candidate.queryAction()
                    names = [action.getName(i) for i in range(action.nActions)]
                    candidate.clear_cache()
                    if candidate.getState().contains(pyatspi.STATE_FOCUSED):
                        control, actions = candidate, names
                        break
            except GLib.GError:
                pass
        tabs += 1
        if tabs > 150 or time.monotonic() >= end:
            if not seen_showing:
                raise RuntimeError('actual chooser item not showing after scroll')
            raise RuntimeError('actual chooser target did not acquire focus')
        subprocess.run(['xdotool', 'key', 'Tab'], check=True, timeout=min(5, budget('xdotool traversal Tab')))
        guard(process, original)
        drain()
        time.sleep(.2)
    def focused():
        guard(process, original)
        try:
            control.clear_cache()
            state = control.getState()
        except GLib.GError:
            return False
        return (state.contains(pyatspi.STATE_FOCUSED)
                and scene().get('focused_app_id') == NAME)
    wait_for(focused, 'actual chooser target did not acquire focus')
    actual_focus = int(subprocess.check_output(['xdotool', 'getwindowfocus'], text=True, timeout=budget('xdotool focus check')))
    actual_focus = int(subprocess.check_output(['xdotool', 'getwindowfocus'], text=True, timeout=budget('xdotool focus recheck')))
    if actual_focus != int(host) or not focused():
        raise RuntimeError('actual host/chooser focus changed before the key')
    (OUT/f'{label}-keyboard-host.json').write_text(json.dumps({
        'host': actual, 'window': int(host), 'actual_x_focus': actual_focus,
        'target': control.name, 'actions': actions, 'target_focused': True,
        'scene_before_key': scene(), 'settings_identity': original}, indent=2))
    # One genuine event, without SendEvent, direct setters, retries or replay.
    subprocess.run(['xdotool', 'key', 'Return'], check=True, timeout=budget('xdotool activation Return'))
    guard(process, original)
    if Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19] != start:
        raise RuntimeError('keyboard host changed during the key')
    _ACTIVE_END = previous_active

def select(style,choose=False):
    log=(OUT/f'{PHASE}-{style}-settings.log').open('w')
    process=subprocess.Popen([str(EXE),'background'],stdout=log,stderr=log)
    try:
        wait_for(lambda:bus_call('NameHasOwner','(s)',(NAME,)),'actual Settings bus missing')
        original=identity(process)
        wait_for(lambda:scene().get('focused_app_id')==NAME,'Settings did not receive actual focus')
        (OUT/f'{PHASE}-{style}-identity.json').write_text(json.dumps(original,indent=2))
        def find(target):
            matches=[c for c in controls(process,original,PHASE+'-'+style) if c.name==target]
            if len(matches)>1:raise RuntimeError('ambiguous actual Settings control')
            return matches[0] if matches else None
        def activate(target):
            wait_for(lambda:find(target) is not None,'actual Settings control missing: '+target,30)
            control=find(target)
            control.clear_cache()
            if not control.getState().contains(pyatspi.STATE_SHOWING):
                control.queryComponent().scrollTo(pyatspi.SCROLL_ANYWHERE);drain();control.clear_cache()
            if not control.getState().contains(pyatspi.STATE_SHOWING):raise RuntimeError('actual control not showing')
            if target=='Tuna Desktop dynamic proof':
                activate_chooser(lambda:find('Tuna Desktop dynamic proof'),process,original,PHASE+'-'+style)
            else:
                action=control.queryAction()
                if action.nActions<1 or not action.doAction(0):raise RuntimeError('actual control action failed')
        if choose:activate('Tuna Desktop dynamic proof')
        activate('Dark' if style=='dark' else 'Default')
        def saved():
            keys={key:subprocess.check_output(['gsettings','get','org.gnome.desktop.background',key],text=True,timeout=budget('gsettings background '+key)).strip() for key in ['picture-uri','picture-uri-dark','picture-options']}
            scheme=subprocess.check_output(['gsettings','get','org.gnome.desktop.interface','color-scheme'],text=True,timeout=budget('gsettings color-scheme')).strip()
            (OUT/f'{PHASE}-{style}-keys.json').write_text(json.dumps({'background':keys,'color-scheme':scheme},indent=2))
            return keys['picture-uri']==repr(XML['light'].as_uri()) and keys['picture-uri-dark']==repr(XML['dark'].as_uri()) and keys['picture-options']==repr('stretched') and scheme==repr('prefer-dark' if style=='dark' else 'default')
        wait_for(saved,'real chooser/style did not persist XML and color scheme')
        guard(process,original)
        if not find('Tuna Desktop dynamic proof').getState().contains(pyatspi.STATE_CHECKED):raise RuntimeError('actual XML chooser item not selected')
        subprocess.run(['scrot','-o',str(OUT/f'{PHASE}-{style}-settings.png')],check=True,timeout=budget('scrot settings capture'))
    finally:
        process.terminate();process.wait(timeout=10);log.close()
    wait_for(lambda:not bus_call('NameHasOwner','(s)',(NAME,)),'Settings did not exit')
    wait_for(lambda:not scene()['windows'],'unobstructed desktop contains app')


def reference(style):
    show=GnomeBG.BGSlideShow.new(str(XML[style]))
    if not show.load():raise RuntimeError('actual GNOME51 slideshow reference failed to load')
    return show


SHOT_W, SHOT_H = 1280, 800

def frame_owners():
    """Shell bus owner/pid plus compositor pid/exe/start, re-read live."""
    owner = bus_call('GetNameOwner', '(s)', ('org.gnome.Shell',))
    pid = bus_call('GetConnectionUnixProcessID', '(s)', (owner,))
    compid = ORIGINAL_HOST['pid']
    comp_stat = Path(f'/proc/{compid}/stat').read_text().rsplit(')', 1)[1].split()
    return {'shell_owner': owner, 'shell_pid': pid, 'compositor_pid': compid,
            'compositor_exe': str(Path(f'/proc/{compid}/exe').resolve()),
            'compositor_start': comp_stat[19]}

def check_frame_owners(owners, label):
    if (owners['shell_owner'] != current['owner']
            or owners['shell_pid'] != current['pid']
            or owners['compositor_pid'] != ORIGINAL_HOST['pid']
            or owners['compositor_exe'] != ORIGINAL_HOST['executable']
            or owners['compositor_start'] != ORIGINAL_HOST['start_ticks']):
        raise RuntimeError(f'frame owners changed during {label}: {owners}')

_FILE_IDENTITY = re.compile(r'Some\(FileIdentity \{ path: "([^"]+)", dev: (\d+), ino: (\d+), bytes: (\d+), modified: \((-?\d+), (-?\d+)\), changed: \((-?\d+), (-?\d+)\) \}\)')

def check_file_identity(rendered, label, expect_path=None):
    """The compositor's Debug-rendered FileIdentity must parse and its
    path/dev/ino/size/mtime/ctime must match a live stat: the frame's
    bytes are the claimed file's bytes. Timestamps are bound too, so
    a same-inode same-size in-place mutation cannot pass. When
    `expect_path` is given the parsed path must equal it exactly."""
    match = _FILE_IDENTITY.fullmatch(rendered)
    if not match:
        raise RuntimeError(f'{label} identity is not a complete FileIdentity: {rendered!r:.200}')
    path = match.group(1)
    if expect_path is not None and path != expect_path:
        raise RuntimeError(f'{label} identity is for {path!r:.100}, expected {expect_path!r:.100}')
    dev, ino, size = int(match.group(2)), int(match.group(3)), int(match.group(4))
    mtime, mtime_nsec = int(match.group(5)), int(match.group(6))
    ctime, ctime_nsec = int(match.group(7)), int(match.group(8))
    try:
        actual = Path(path).stat()
    except OSError:
        raise RuntimeError(f'{label} identity path is not statable: {path}')
    live_mtime, live_ctime = actual.st_mtime_ns, actual.st_ctime_ns
    if (actual.st_ino != ino or actual.st_size != size or actual.st_dev != dev
            or (live_mtime // 1_000_000_000, live_mtime % 1_000_000_000) != (mtime, mtime_nsec)
            or (live_ctime // 1_000_000_000, live_ctime % 1_000_000_000) != (ctime, ctime_nsec)):
        raise RuntimeError(f'{label} identity does not match live file: {rendered!r:.200}')
    return path

def is_plain_int(value):
    return isinstance(value, int) and not isinstance(value, bool)

def check_paint(paint, style, fixed, first, last):
    """Strict per-frame receipt against the compositor's actual
    paint_record schema (wallpaper.rs:444-449). Raises on any
    missing/mistyped/ranged-out field or provenance mismatch, so an
    accepted frame ties actual pixels to the expected XML source,
    generation, viewport and owner — never to bare construction
    metadata alone."""
    if not isinstance(paint, dict):
        raise RuntimeError(f'paint receipt is not an object: {paint!r:.200}')
    for key in ('sample_wall', 'progress', 'interval', 'from', 'to', 'slide',
                'generation', 'source_uri', 'root_identity', 'source_identities', 'geometry'):
        if key not in paint:
            raise RuntimeError(f'paint receipt lacks {key}: {sorted(paint)}')
    wall = paint['sample_wall']
    if isinstance(wall, bool) or not isinstance(wall, (int, float)) or not math.isfinite(wall) or wall <= 0:
        raise RuntimeError(f'paint sample_wall is not a positive finite time: {wall!r:.100}')
    progress = paint['progress']
    if isinstance(progress, bool) or not isinstance(progress, (int, float)) or not math.isfinite(progress) or not 0.0 <= progress <= 1.0:
        raise RuntimeError(f'paint progress is not finite in 0..=1: {progress!r:.100}')
    interval = paint['interval']
    if isinstance(interval, bool) or not isinstance(interval, (int, float)) or not math.isfinite(interval) or not 0 < interval <= 86400:
        raise RuntimeError(f'paint interval is not a sane finite duration: {interval!r:.100}')
    if not is_plain_int(paint['slide']) or paint['slide'] < 0:
        raise RuntimeError(f'paint slide is not a nonnegative int: {paint["slide"]!r:.100}')
    expected_uri = XML[style].as_uri()
    if paint['source_uri'] != expected_uri:
        raise RuntimeError(f'paint source is not the expected XML: {paint["source_uri"]!r:.200} vs {expected_uri}')
    generation = paint['generation']
    if not isinstance(generation, str) or not generation.startswith(expected_uri + '\n'):
        raise RuntimeError(f'paint generation does not carry the expected source epoch: {generation!r:.200}')
    for name, value, want_none in (('from', paint['from'], False), ('to', paint['to'], fixed)):
        if want_none:
            if value is not None:
                raise RuntimeError(f'static paint carries a blend target: {value!r:.200}')
            continue
        if not isinstance(value, str) or not value.startswith(str(OUT) + '/'):
            raise RuntimeError(f'paint {name} is not an owned frame path: {value!r:.200}')
        stem = Path(value).stem
        if stem not in COLORS or not Path(value).exists():
            raise RuntimeError(f'paint {name} is not a live proof frame: {value!r:.200}')
    if paint['from'] != first or paint['to'] != (last or None):
        raise RuntimeError(f'paint frames diverge from the reference slide: {paint["from"]!r:.100} -> {paint["to"]!r:.100}')
    # The root is the proof's own XML: bind its exact path, not just
    # whatever file the record happens to describe.
    check_file_identity(paint['root_identity'], 'root', expect_path=str(XML[style]))
    refs = paint['source_identities']
    if not isinstance(refs, list) or not refs:
        raise RuntimeError('paint source identities are not a nonempty list')
    ref_paths = set()
    for ref in refs:
        if not isinstance(ref, dict) or not isinstance(ref.get('path'), str) or not isinstance(ref.get('identity'), str):
            raise RuntimeError(f'source identity entry is malformed: {ref!r:.200}')
        if ref['path'] in ref_paths:
            raise RuntimeError(f'duplicate source identity path: {ref["path"]!r:.100}')
        live = check_file_identity(ref['identity'], 'reference ' + ref['path'][-40:])
        if live != ref['path']:
            raise RuntimeError(f'reference path disagrees with its identity: {ref["path"]!r:.100} vs {live!r:.100}')
        ref_paths.add(ref['path'])
    if paint['from'] not in ref_paths or (paint['to'] is not None and paint['to'] not in ref_paths):
        raise RuntimeError('paint frames are missing from the source identities')
    # Exact producer match for this proof's single winit output
    # (runtime background_geometry: offset (0,0), desktop from the
    # 1280x800 window at scale 1.0), not merely sane values. Types
    # first: bare == lets True pass as 1.0 and [False, False] as
    # [0, 0], so arrays must be plain ints and scale a non-bool
    # finite number before the exact comparison runs.
    geometry = paint['geometry']
    if not isinstance(geometry, dict):
        raise RuntimeError(f'paint geometry is not an object: {geometry!r:.200}')
    for key, length in (('physical', 2), ('origin', 2), ('desktop', 4)):
        values = geometry.get(key)
        if (not isinstance(values, list) or len(values) != length
                or not all(is_plain_int(v) for v in values)):
            raise RuntimeError(f'paint geometry {key} is not a plain-int array: {values!r:.100}')
    scale = geometry.get('scale')
    if isinstance(scale, bool) or not isinstance(scale, (int, float)) or not math.isfinite(scale):
        raise RuntimeError(f'paint scale is not a finite number: {scale!r:.100}')
    if (geometry['physical'] != [SHOT_W, SHOT_H]
            or geometry['origin'] != [0, 0]
            or geometry['desktop'] != [0, 0, SHOT_W, SHOT_H]
            or scale != 1.0):
        raise RuntimeError(f'paint geometry is not the expected {SHOT_W}x{SHOT_H}@1.0 output: {geometry!r:.200}')


def observe(style,label,required=None):
    global _ACTIVE_END
    show=reference(style)
    seen={};samples=[];wrapped=False;deadline=time.monotonic()+85
    previous_active, _ACTIVE_END = _ACTIVE_END, deadline
    try:
        while time.monotonic()<deadline:
            owners_before=frame_owners()
            paint_before=scene().get('background_paint')
            before=time.time()
            progress,duration,fixed,first,last=show.get_current_slide(SHOT_W,SHOT_H)
            reference_after=time.time()
            path=OUT/f'{PHASE}-{label}-latest.png'
            capture_before=time.time()
            subprocess.run(['scrot','-o',str(path)],check=True,timeout=budget('scrot observation capture'))
            after=time.time()
            check_frame_owners(frame_owners(),'observation capture')
            check_frame_owners(owners_before,'observation capture')
            # Post-guard recheck: the bus/stat calls above consume an
            # unbounded slice of the budget; never spend the expensive
            # decode on a deadline that already passed.
            if time.monotonic()>=deadline:
                break
            paint_after=scene().get('background_paint')
            pb=GdkPixbuf.Pixbuf.new_from_file(str(path))
            if pb.get_width()!=SHOT_W or pb.get_height()!=SHOT_H:
                raise RuntimeError(f'screenshot is not the {SHOT_W}x{SHOT_H} viewport: {pb.get_width()}x{pb.get_height()}')
            channels=pb.get_n_channels()
            if channels not in (3,4):
                raise RuntimeError(f'screenshot has unexpected channels: {channels}')
            data=pb.get_pixels()
            if len(data)!=pb.get_height()*pb.get_rowstride():
                raise RuntimeError('screenshot pixel buffer does not match rowstride geometry')
            offset=400*pb.get_rowstride()+640*channels
            actual=tuple(data[offset:offset+3])
            if len(actual)!=3:
                raise RuntimeError('center sample fell outside the pixel buffer')
            first_name=Path(first).stem
            last_name=Path(last).stem if last else None
            ca=COLORS[first_name];cb=COLORS[last_name] if last else ca
            stable=paint_before is not None and paint_before==paint_after
            sampled_progress=progress
            age=None;progress_low=None;progress_high=None
            time_qualified=False
            receipt_ok=False
            receipt_error=None
            if stable:
                # The strict receipt runs before any timing math: a
                # malformed record can never qualify on pixels alone.
                # A bad sample disqualifies and is recorded; only
                # owner changes abort the observation outright.
                try:
                    check_paint(paint_before,style,fixed,first,last)
                except RuntimeError as bad:
                    receipt_error=str(bad)
                else:
                    receipt_ok=True
                if receipt_ok:
                    sample_wall=paint_before['sample_wall']
                    age=after-sample_wall
                    same_paths=paint_before['from']==first and paint_before['to']==(last or None)
                    if fixed:
                        sampled_progress=0.0
                        progress_qualified=paint_before['progress']==0.0
                    else:
                        progress_low=progress-(reference_after-sample_wall)/duration
                        progress_high=progress-(before-sample_wall)/duration
                        sampled_progress=(progress_low+progress_high)/2
                        progress_qualified=progress_low-.002<=paint_before['progress']<=progress_high+.002
                    time_qualified=(same_paths and progress_qualified and 0<=age<=paint_before['interval']+.5
                                    and after-capture_before<=.5)
            want=tuple(round(a*(1-sampled_progress)+b*sampled_progress) for a,b in zip(ca,cb))
            entry={'reference_before_wall':before,'reference_after_wall':reference_after,
                   'capture_before_wall':capture_before,'capture_after_wall':after,
                   'before_wall':before,'after_wall':after,'paint_before':paint_before,'paint_after':paint_after,
                   'actual_sample_age':age,'reference_progress_at_sample_bounds':[progress_low,progress_high],
                   'time_qualified':time_qualified,'receipt_qualified':receipt_ok,
                   'receipt_error':receipt_error,
                   'owners_before_capture':owners_before,
                   'reference':{'progress':progress,'duration':duration,'fixed':fixed,'from':first,'to':last},
                   'actual':actual,'expected_reference_at_actual_sample':want}
            samples.append(entry)
            # The final observation is expensive (reference + capture +
            # decode); recheck the absolute deadline before accepting
            # anything it produced. A late pass is a failure, not a
            # qualification, and the claimed bound is not extended.
            if time.monotonic()>=deadline:
                break
            # Exact pixels correspond to the independently bounded GNOME reference
            # progress at the candidate's retained real sample wall time. A stale
            # frame, wrong phase, source epoch mismatch or slow capture cannot pass.
            correct=receipt_ok and time_qualified and max(abs(a-b) for a,b in zip(actual,want))<=2
            state=('static-'+first_name) if fixed else ('blend-'+first_name+'-'+last_name if .35<=progress<=.65 else None)
            if correct and state:
                if state not in seen:
                    seen[state]=entry
                    path.replace(OUT/f'{PHASE}-{label}-{state}.png')
                elif fixed and before-seen[state]['before_wall']>=22:
                    wrapped=True
                    path.replace(OUT/f'{PHASE}-{label}-cycle-wrap.png')
            (OUT/f'{PHASE}-{label}-natural-clock-pixels.json').write_text(json.dumps({'samples':samples,'qualified_states':seen,'natural_cycle_wrap':wrapped},indent=2))
            # Final acceptance gate: recheck after the bookkeeping
            # write, immediately before any return can fire.
            if time.monotonic()>=deadline:
                break
            if required:
                if required in seen:return
            elif wrapped and (style+'-a') in ''.join(k for k in seen if k.startswith('static-')) and (style+'-b') in ''.join(k for k in seen if k.startswith('static-')) and sum(k.startswith('blend-') for k in seen)>=2:
                return
            time.sleep(.15)
    finally:
        _ACTIVE_END = previous_active
    raise RuntimeError('naturally advancing actual pixels did not qualify required reference phases')


wait_for(lambda:bus_call('NameHasOwner','(s)',('org.gnome.Shell',)),'actual GTK shell missing',30)
current=session_identity()
compositor_pid=current['compositor_pid']
compositor_exe=Path(f'/proc/{compositor_pid}/exe').resolve(strict=True)
if compositor_exe != Path('/candidate/usr/bin/tuna-compositor').resolve(strict=True):
    raise RuntimeError('original host is not the actual candidate compositor')
ORIGINAL_HOST={'pid':compositor_pid,'executable':str(compositor_exe),
               'start_ticks':Path(f'/proc/{compositor_pid}/stat').read_text().rsplit(')',1)[1].split()[19],
               'sha256':hashlib.sha256(compositor_exe.read_bytes()).hexdigest()}
if PHASE=='initial':
    fixture()
    select('light',True)
    observe('light','light-cycle')
    select('dark')
    observe('dark','dark-cycle')
    old=hashlib.sha256((OUT/'dark-a.png').read_bytes()).hexdigest()
    COLORS['dark-a']=(0,240,240);image('dark-a',COLORS['dark-a'])
    observe('dark','same-image-uri','static-dark-a')
    (OUT/'image-replacement.json').write_text(json.dumps({'same_uri':(OUT/'dark-a.png').as_uri(),'before_sha256':old,'after_sha256':hashlib.sha256((OUT/'dark-a.png').read_bytes()).hexdigest()},indent=2))
    good=XML['dark'].read_bytes()
    for negative,payload in [('forbidden-entity',b"<!DOCTYPE background [<!ENTITY a 'x'>]><background><static><duration>4</duration><file>&a;</file></static></background>"),('oversized',b' '* (1024*1024+1))]:
        XML['dark'].write_bytes(payload)
        path=OUT/f'{PHASE}-{negative}.png'
        def rejected():
            subprocess.run(['scrot','-o',str(path)],check=True,timeout=budget('scrot negative capture'))
            pb=GdkPixbuf.Pixbuf.new_from_file(str(path))
            if pb.get_width()!=SHOT_W or pb.get_height()!=SHOT_H or pb.get_n_channels() not in (3,4):
                return False
            data=pb.get_pixels()
            if len(data)!=pb.get_height()*pb.get_rowstride():
                return False
            offset=400*pb.get_rowstride()+640*pb.get_n_channels()
            actual=tuple(data[offset:offset+3])
            if len(actual)!=3:
                return False
            (OUT/f'{PHASE}-{negative}.json').write_text(json.dumps({'actual':actual,'expected_solid_fallback':[0,200,0],'xml_bytes':len(payload),'xml_sha256':hashlib.sha256(payload).hexdigest()},indent=2))
            return actual==(0,200,0)
        wait_for(rejected,'unsafe XML was not rejected to bounded fallback',30)
    XML['dark'].write_bytes(good)
    old=hashlib.sha256(XML['dark'].read_bytes()).hexdigest();timeline('dark',True)
    observe('dark','same-xml-uri')
    (OUT/'xml-replacement.json').write_text(json.dumps({'same_uri':XML['dark'].as_uri(),'before_sha256':old,'after_sha256':hashlib.sha256(XML['dark'].read_bytes()).hexdigest()},indent=2))
    (OUT/'initial-session.json').write_text(json.dumps(current,indent=2))
elif PHASE=='restarted':
    previous=json.loads((OUT/'initial-session.json').read_text())
    if current['pid']==previous['pid'] or current['owner']==previous['owner'] or current['compositor_pid']==previous['compositor_pid'] or current['sha256']!=previous['sha256']:raise RuntimeError('candidate session did not genuinely restart')
    COLORS['dark-a']=(0,240,240)
    # Observe the persisted XML before launching Settings after genuine restart.
    observe('dark','persisted-restart')
    select('dark')
    (OUT/'restarted-session.json').write_text(json.dumps(current,indent=2))
else:raise RuntimeError('invalid proof phase')
