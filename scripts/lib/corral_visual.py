"""Installed fixture observations; never release or whole-capability qualification."""
import hashlib
import json
import re
import struct
import zlib


def candidate_intent(manifest, candidate, package_run):
    if not isinstance(candidate, str) or not re.fullmatch('[0-9a-f]{40}', candidate) or type(package_run) is not int or package_run <= 0:
        raise ValueError('exact candidate SHA and positive package run required')
    selection = manifest.get('package_selection', {})
    if (manifest.get('candidate_source_commit') != candidate
            or selection.get('head_sha') != candidate
            or selection.get('run_id') != package_run):
        raise ValueError('export differs from explicitly requested candidate/package run')


def png(width, height, pixels):
    if not (0 < width <= 4096 and 0 < height <= 4096 and len(pixels) == width*height*4):
        raise ValueError('bounded complete framebuffer required')
    rgb = b''.join(bytes((pixels[i+2], pixels[i+1], pixels[i])) for i in range(0, len(pixels), 4))
    def chunk(kind, data):
        return struct.pack('>I', len(data))+kind+data+struct.pack('>I', zlib.crc32(kind+data)&0xffffffff)
    raw = b''.join(b'\0'+rgb[y*width*3:(y+1)*width*3] for y in range(height))
    return (b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0))
            +chunk(b'IDAT', zlib.compress(raw))+chunk(b'IEND', b''))


class StateNotReady(ValueError):
    """Valid live consumer tree has not reached the requested UI state."""


STATES = ('desktop', 'overview', 'quick-settings', 'calendar', 'app-grid', 'desktop-return')
IDENTITY_KEYS = ('boot_id', 'shell_pid', 'shell_start_ticks', 'shell_sha256', 'session_id', 'native_backend')


def identity_pair(before, after):
    for key in IDENTITY_KEYS:
        if before.get(key) is None or before[key] != after.get(key):
            raise ValueError('consumer/boot/session identity changed across frame')


def validate_state(name, observation):
    if observation.get('locked') is not False or observation.get('wayland_session') is not True:
        raise ValueError('unlocked live Wayland session observation required')
    if not observation.get('nodes') or any(n.get('role') == 'error' for n in observation['nodes']):
        raise ValueError('complete nonempty accessibility observation required')
    showing = {n['name'] for n in observation['nodes'] if n['showing']}
    if 'Activities' not in showing or 'System' not in showing:
        raise ValueError('live panel controls absent')
    for first, second in (('Dark Style', 'Volume'), ('Previous month', 'Next month')):
        if (first in showing) != (second in showing):
            raise StateNotReady('partial consumer control pair: '+first+'/'+second)
    search = 'Search' in showing
    quick = 'Dark Style' in showing and 'Volume' in showing
    calendar = 'Previous month' in showing and 'Next month' in showing
    grid = 'App Grid' in showing
    expected = {'desktop': (False, False, False, False),
                'desktop-return': (False, False, False, False),
                'overview': (True, False, False, False),
                'quick-settings': (False, True, False, False),
                'calendar': (False, False, True, False),
                'app-grid': (True, False, False, True)}
    if name not in expected:raise ValueError('unknown capture state')
    if (search, quick, calendar, grid) != expected[name]:
        raise StateNotReady('named consumer state does not match visible accessibility controls')
    if grid:
        installed = {app['name'] for app in observation.get('installed_apps', []) if app.get('id')}
        buttons = [n for n in observation['nodes'] if n['showing'] and n['role'] in ('push button', 'button')
                   and 'App Grid' in n.get('ancestors', []) and n['name'] in installed]
        if not buttons:raise StateNotReady('app grid lacks a showing installed application launcher')
    return True


def observation_pair(name, before, after):
    identity_pair(before, after)
    validate_state(name, before)
    validate_state(name, after)


def wait_state(name, observe, anchor, timeout=12, clock=None, sleep=None):
    """Two fresh matching trees, each bounded by the remaining host deadline.

    No stale tree reuse; identity changes and missing consumers are fatal.
    Caller must give observe a transport timeout no greater than remaining.
    """
    import time
    clock = clock or time.monotonic
    sleep = sleep or time.sleep
    deadline = clock() + timeout
    previous = None
    while True:
        remaining = deadline - clock()
        if remaining <= 0:raise TimeoutError('installed state did not settle: '+name)
        current = observe(remaining)
        if clock() >= deadline:raise TimeoutError('installed observation exceeded deadline')
        identity_pair(anchor, current)
        try:
            validate_state(name, current)
        except StateNotReady:
            previous = None
        else:
            # Stability concerns visible consumer controls, not clock text or
            # daemon-backed list rows which may legitimately change.
            signature = sorted((n['name'], n['role']) for n in current['nodes']
                               if n['showing'] and n['name'] in
                               ('Activities', 'System', 'Search', 'Dark Style', 'Volume',
                                'Previous month', 'Next month', 'App Grid'))
            if signature == previous:return current
            previous = signature
        sleep(min(.1, max(0, deadline-clock())))


def native_identity(value):
    if value.get('gnome_shell_pids') or value.get('backend_request') not in ('auto', 'drm', 'kms'):
        raise ValueError('nested/foreign compositor refused')
    if not value.get('same_session_scope') or not value.get('shell_descends_from_compositor'):
        raise ValueError('shell and compositor must belong to the same native login session')
    if not value.get('primary_drm_fds') or not value.get('drm_output_journal'):
        raise ValueError('live primary DRM device and current-process DRM output receipt required')
    if not value.get('compositor_pid') or not value.get('compositor_start_ticks'):
        raise ValueError('live compositor process identity required')
    return True


# Read the actual GTK application by PID on its own accessibility bus, never a
# shared test bus. The parent runs this code as the already logged-in user.
USER_OBSERVE = r'''
import json,os,sys,subprocess
import gi
gi.require_version('Atspi','2.0')
from gi.repository import Atspi,Gio,GLib
argument=json.loads(sys.argv[1]);pid=argument['pid'];nodes=[];targets=[]
def walk(node,depth,ancestors):
 if depth>32 or len(nodes)>=4096:raise ValueError('accessibility tree bound')
 node.clear_cache();showing=node.get_state_set().contains(Atspi.StateType.SHOWING)
 name=node.get_name() or '';role=node.get_role_name()
 nodes.append({'name':name,'role':role,'showing':showing,'ancestors':ancestors})
 if argument.get('press') and name==argument['press'] and showing and role!='label':targets.append(node)
 for i in range(node.get_child_count()):
  child=node.get_child_at_index(i)
  if child is not None:walk(child,depth+1,ancestors+[name])
desktop=Atspi.get_desktop(0)
if desktop is None:raise ValueError('live accessibility desktop unavailable')
apps=[desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
apps=[app for app in apps if app is not None and app.get_process_id()==pid]
if len(apps)!=1:raise ValueError('unique live GTK accessibility application required')
walk(apps[0],0,[])
if argument.get('press'):
 if len(targets)!=1:raise ValueError('unique showing action required')
 action=targets[0].get_action_iface()
 if action is None or action.get_n_actions()<1 or not action.do_action(0):raise ValueError('accessible action failed')
bus=Gio.bus_get_sync(Gio.BusType.SESSION,None)
locked=bus.call_sync('org.gnome.ScreenSaver','/org/gnome/ScreenSaver','org.gnome.ScreenSaver','GetActive',None,None,Gio.DBusCallFlags.NONE,5000,None).unpack()[0]
installed=[{'id':app.get_id(),'name':app.get_display_name()} for app in Gio.AppInfo.get_all() if app.should_show() and app.get_id()]
if len(installed)>4096:raise ValueError('installed app inventory bound')
print(json.dumps({'nodes':nodes,'locked':locked,'installed_apps':installed}))
'''


def guest_source(members):
    # No guest packages are installed and no session environment is modified.
    return r'''
import hashlib,json,os,pathlib,pwd,stat,subprocess,sys
argument=json.loads(sys.argv[1]);boot=pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()
members=MEMBERS
for name,expected in members.items():
 path=pathlib.Path('/'+name)
 if hashlib.sha256(path.read_bytes()).hexdigest()!=expected:raise ValueError('live native package member changed: '+name)
processes=[]
for p in pathlib.Path('/proc').iterdir():
 if p.name.isdigit():
  try:
   if os.readlink(p/'exe')=='/usr/bin/tuna-shell-gtk':processes.append(p)
  except OSError:pass
if len(processes)!=1:raise ValueError('unique installed Tuna GTK process required')
p=processes[0];pid=int(p.name);uid=p.stat().st_uid;user=pwd.getpwuid(uid).pw_name
if uid==0:raise ValueError('ordinary shell user required')
if hashlib.sha256(p.joinpath('exe').read_bytes()).hexdigest()!=members['usr/bin/tuna-shell-gtk']:raise ValueError('running GTK bytes differ from original native package')
start=p.joinpath('stat').read_text().rsplit(')',1)[1].split()[19]
bus=pathlib.Path('/run/user')/str(uid)/'bus';info=bus.stat()
if not stat.S_ISSOCK(info.st_mode) or info.st_uid!=uid:raise ValueError('owned live session bus required')
sessions=json.loads(subprocess.check_output(['loginctl','list-sessions','--json=short'],timeout=5))
ids=[]
for s in sessions:
 sid=str(s['session']);props=subprocess.check_output(['loginctl','show-session',sid,'-p','User','-p','Type','-p','Active'],timeout=5).decode()
 values=dict(line.split('=',1) for line in props.splitlines() if '=' in line)
 if values.get('User')==str(uid) and values.get('Type')=='wayland' and values.get('Active')=='yes':ids.append(sid)
if len(ids)!=1:raise ValueError('unique active Wayland session required')
compositors=[];foreign=[]
for item in pathlib.Path('/proc').iterdir():
 if not item.name.isdigit():continue
 try:
  if item.stat().st_uid!=uid:continue
  exe=os.readlink(item/'exe')
  if exe=='/usr/bin/tuna-compositor':compositors.append(item)
  if exe=='/usr/bin/gnome-shell':foreign.append(int(item.name))
 except (FileNotFoundError,ProcessLookupError):continue
if len(compositors)!=1 or foreign:raise ValueError('unique native Tuna compositor and no same-user GNOME required')
c=compositors[0];cpid=int(c.name);cstart=c.joinpath('stat').read_text().rsplit(')',1)[1].split()[19]
if hashlib.sha256(c.joinpath('exe').read_bytes()).hexdigest()!=members['usr/bin/tuna-compositor']:raise ValueError('running compositor bytes differ from original native package')
cg=c.joinpath('cgroup').read_text();sg=p.joinpath('cgroup').read_text();scope='session-'+ids[0]+'.scope'
ancestry=[];cursor=p
for _ in range(32):
 fields=cursor.joinpath('stat').read_text().rsplit(')',1)[1].split();parent=int(fields[1])
 if parent<=1:break
 ancestry.append(parent)
 if parent==cpid:break
 cursor=pathlib.Path('/proc')/str(parent)
argv=[v.decode() for v in c.joinpath('cmdline').read_bytes().split(b'\0') if v];backend='auto'
for index,arg in enumerate(argv):
 if arg=='--backend':backend=argv[index+1]
primary=[]
for fd in c.joinpath('fd').iterdir():
 try:
  target=os.readlink(fd);info=fd.stat()
  if target.startswith('/dev/dri/card') and stat.S_ISCHR(info.st_mode) and os.major(info.st_rdev)==226 and os.minor(info.st_rdev)<64:
   primary.append({'fd':int(fd.name),'path':target,'device_major':226,'device_minor':os.minor(info.st_rdev)})
 except FileNotFoundError:continue
logs=subprocess.check_output(['journalctl','--no-pager','-b','_PID='+str(cpid),'_UID='+str(uid),'-o','json','--lines=2000'],timeout=10)
if len(logs)>4*1024*1024:raise ValueError('DRM journal receipt bound')
output=[];since=int(cstart)*1000000//os.sysconf('SC_CLK_TCK')
for row in logs.splitlines():
 record=json.loads(row);message=record.get('MESSAGE','')
 if isinstance(message,str) and message.startswith('tuna-compositor: drm: output ') and record.get('_BOOT_ID')==boot.replace('-','') and int(record.get('__MONOTONIC_TIMESTAMP',0))>=since:
  output.append({'message':message,'monotonic_us':record['__MONOTONIC_TIMESTAMP'],'pid':record['_PID'],'boot_id':record['_BOOT_ID']})
native={'compositor_pid':cpid,'compositor_start_ticks':cstart,'compositor_sha256':members['usr/bin/tuna-compositor'],
 'compositor_cgroup':cg,'shell_cgroup':sg,'same_session_scope':cg==sg and scope in [part.strip() for part in cg.split('/')],
 'shell_descends_from_compositor':cpid in ancestry,'backend_request':backend,'gnome_shell_pids':foreign,
 'primary_drm_fds':sorted(primary,key=lambda fd:fd['fd']),'drm_output_journal':output,'backend':'drm'}
NATIVE_VALIDATE(native)
env={'PATH':'/usr/bin:/bin','HOME':pwd.getpwuid(uid).pw_dir,'XDG_RUNTIME_DIR':str(bus.parent),'DBUS_SESSION_BUS_ADDRESS':'unix:path='+str(bus)}
result=subprocess.run(['runuser','-u',user,'--','python3','-c',USER_SOURCE,json.dumps({'pid':pid,'press':argument.get('press')})],env=env,capture_output=True,timeout=15)
if result.returncode:raise ValueError('live accessibility observation unavailable: '+result.stderr.decode()[-1200:])
value=json.loads(result.stdout)
if p.joinpath('stat').read_text().rsplit(')',1)[1].split()[19]!=start or c.joinpath('stat').read_text().rsplit(')',1)[1].split()[19]!=cstart or pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()!=boot:raise ValueError('consumer or boot changed during observation')
value.update(boot_id=boot,shell_pid=pid,shell_start_ticks=start,shell_sha256=members['usr/bin/tuna-shell-gtk'],session_id=ids[0],wayland_session=True,native_backend=native)
print(json.dumps(value))
'''.replace('MEMBERS', repr(members)).replace('USER_SOURCE', repr(USER_OBSERVE)).replace('NATIVE_VALIDATE(native)', __import__('inspect').getsource(native_identity)+'\nnative_identity(native)')


def sha(data):
    return hashlib.sha256(data).hexdigest()
