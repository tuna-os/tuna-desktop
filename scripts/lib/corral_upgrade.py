"""Pure logic of the upgrade/rollback/recovery proof in `tuna-corral-journey --upgrade-to`.

Everything here is deterministic and runs without a VM: the user-state fixture,
the guest snippets that create and inventory it, bootc status projection, and
the comparisons that decide whether state survived a deployment switch, a
rollback or a crash. The journey script owns the VM, the console and the
receipts. See docs/upgrade-recovery.md.
"""
import base64
import hashlib
import json
import re
import shlex

FIXTURE_DIR = 'Documents/tuna-upgrade-proof'
BINARY_SIZE = 1 << 20
# Wellbeing history retention (`HISTORY_THRESHOLD_SECONDS`, shell-gtk wellbeing.rs).
HISTORY_THRESHOLD_SECONDS = 14 * 7 * 24 * 60 * 60
NOTIFICATIONS = '.local/state/tuna-shell/notifications.json'
WELLBEING_HISTORY = '.local/share/gnome-shell/session-active-history.json'
WINDOW_SESSIONS = '.local/share/tuna/window-sessions.json'
SETTINGS_STORES = ('.config/dconf/user', '.config/glib-2.0/settings/keyfile')
APP_FOLDER = 'TunaUpgradeProof'
APP_FOLDER_PATH = f'/org/gnome/desktop/app-folders/folders/{APP_FOLDER}/'
DIGEST_REF = re.compile(r'[^@\s]+@sha256:[0-9a-f]{64}')


def fixture_bytes(token, size=BINARY_SIZE):
    """Deterministic binary content both host and guest derive from the token."""
    return hashlib.shake_256(f'tuna-upgrade-proof:{token}'.encode()).digest(size)


def fixture_files(token):
    """Relative path under $HOME -> (kind, mode, content or link target)."""
    base = FIXTURE_DIR
    return {
        f'{base}/notes.txt': ('file', 0o644, f'Tuna Desktop upgrade proof {token}\n'.encode()),
        f'{base}/nested/deeper/data.bin': ('file', 0o644, fixture_bytes(token)),
        f'{base}/private.txt': ('file', 0o600, f'owner-only {token}\n'.encode()),
        f'{base}/latest': ('symlink', None, 'notes.txt'),
        f'{base}/empty': ('dir', 0o755, None),
    }


def fixture_expectations(token):
    """Manifest entries the fixture must keep at every checkpoint."""
    expected = {}
    for path, (kind, mode, content) in fixture_files(token).items():
        entry = {'type': kind}
        if kind == 'file':
            entry.update(mode=oct(mode), size=len(content), sha256=hashlib.sha256(content).hexdigest())
        elif kind == 'symlink':
            entry['target'] = content
        else:
            entry['mode'] = oct(mode)
        expected[path] = entry
    return expected


def gsettings_fixture(token):
    """(schema[:path], key, value) set through gsettings in the session.

    Covers appearance, the shell clock, the dash favorites and an app-grid
    folder (the saved app layout) and wellbeing break reminders."""
    del token  # values are fixed so later releases can compare them verbatim
    return [
        ('org.gnome.desktop.interface', 'color-scheme', "'prefer-dark'"),
        ('org.gnome.desktop.interface', 'clock-show-seconds', 'true'),
        ('org.gnome.shell', 'favorite-apps',
         "['org.gnome.Nautilus.desktop', 'org.gnome.Settings.desktop', 'org.gnome.Console.desktop']"),
        ('org.gnome.desktop.app-folders', 'folder-children', f"['{APP_FOLDER}']"),
        (f'org.gnome.desktop.app-folders.folder:{APP_FOLDER_PATH}', 'name', "'Tuna Upgrade Proof'"),
        (f'org.gnome.desktop.app-folders.folder:{APP_FOLDER_PATH}', 'apps',
         "['org.gnome.Nautilus.desktop', 'org.gnome.Settings.desktop']"),
        ('org.gnome.desktop.break-reminders', 'selected-breaks', "['movement']"),
    ]


def create_fixture_script(token):
    """Python for the guest, run as the user, that writes the file fixture.

    The 1 MiB binary is derived in the guest from the token, never sent over SSH."""
    derived = fixture_bytes(token)

    def encode(kind, content):
        if kind == 'file':
            return {'derive': len(content)} if content == derived else base64.b64encode(content).decode()
        return content if kind == 'symlink' else None
    plan = {path: [kind, mode, encode(kind, content)] for path, (kind, mode, content) in fixture_files(token).items()}
    program = f'''import base64,hashlib,json,os
plan=json.loads({json.dumps(json.dumps(plan))});token={json.dumps(token)};home=os.path.expanduser('~')
for rel,(kind,mode,data) in plan.items():
 path=os.path.join(home,rel);os.makedirs(os.path.dirname(path),exist_ok=True)
 if kind=='dir':os.makedirs(path,exist_ok=True);os.chmod(path,mode);continue
 if os.path.lexists(path):os.remove(path)
 if kind=='symlink':os.symlink(data,path);continue
 body=hashlib.shake_256(('tuna-upgrade-proof:'+token).encode()).digest(data['derive']) if isinstance(data,dict) else base64.b64decode(data)
 fd=os.open(path,os.O_WRONLY|os.O_CREAT|os.O_TRUNC,mode)
 with os.fdopen(fd,'wb') as out:out.write(body);out.flush();os.fsync(out.fileno())
 os.chmod(path,mode)
print('ok')
'''
    return 'python3 -c ' + shlex.quote(program)


# Inventory of a whole home directory: type, mode, owner, size, SHA-256 and
# symlink target for every entry. Runs as root so nothing is skipped.
MANIFEST_PROGRAM = r'''import hashlib,json,os,stat,sys
root=sys.argv[1];entries={}
for directory,dirs,files in os.walk(root):
 dirs.sort()
 for name in sorted(dirs+files):
  path=os.path.join(directory,name);st=os.lstat(path);rel=os.path.relpath(path,root)
  entry={'mode':oct(stat.S_IMODE(st.st_mode)),'uid':st.st_uid,'gid':st.st_gid}
  if stat.S_ISLNK(st.st_mode):entry.update(type='symlink',target=os.readlink(path))
  elif stat.S_ISREG(st.st_mode):
   digest=hashlib.sha256()
   with open(path,'rb') as data:
    for block in iter(lambda:data.read(1<<20),b''):digest.update(block)
   entry.update(type='file',size=st.st_size,sha256=digest.hexdigest())
  elif stat.S_ISDIR(st.st_mode):entry['type']='dir'
  else:entry['type']='other'
  entries[rel]=entry
print(json.dumps({'root':root,'entries':entries},sort_keys=True))
'''


def manifest_script(home):
    return f'python3 -c {shlex.quote(MANIFEST_PROGRAM)} {shlex.quote(home)}'


def compare_manifests(before, after):
    """Entries missing or changed between two home inventories, plus additions.

    Directory entries compare type, mode and owner only; their timestamps are
    not part of the inventory."""
    old, new = before['entries'], after['entries']
    missing = sorted(set(old) - set(new))
    added = sorted(set(new) - set(old))
    changed = [{'path': path, 'before': old[path], 'after': new[path]}
               for path in sorted(set(old) & set(new)) if old[path] != new[path]]
    return {'missing': missing, 'changed': changed, 'added': added,
            'compared': len(old), 'preserved': not missing and not changed}


def fixture_mismatches(manifest, token):
    """Fixture paths whose inventory differs from what the fixture wrote."""
    entries = manifest['entries']
    problems = []
    for path, expected in fixture_expectations(token).items():
        actual = entries.get(path)
        if actual is None:
            problems.append({'path': path, 'expected': expected, 'actual': None})
            continue
        projected = {key: actual.get(key) for key in expected}
        if projected != expected:
            problems.append({'path': path, 'expected': expected, 'actual': actual})
    return problems


def deployment(status, role):
    """Project `bootc status --format json` for role booted/staged/rollback."""
    entry = ((status or {}).get('status') or {}).get(role)
    if not entry:
        return None
    image = entry.get('image') or {}
    return {'ref': ((image.get('image') or {}).get('image')), 'digest': image.get('imageDigest'),
            'version': image.get('version'), 'timestamp': image.get('timestamp'),
            'pinned': entry.get('pinned'), 'ostree': (entry.get('ostree') or {}).get('checksum')}


def deployments(status):
    projection = {role: deployment(status, role) for role in ('booted', 'staged', 'rollback')}
    projection['spec'] = ((status or {}).get('spec') or {}).get('image')
    projection['rollbackQueued'] = ((status or {}).get('status') or {}).get('rollbackQueued')
    return projection


def booted_is(status, ref):
    booted = deployment(status, 'booted')
    return bool(booted) and booted['ref'] == ref


def validate_refs(source, target):
    """Both refs digest-pinned and distinct: bootc does nothing for an unchanged spec."""
    for name, ref in (('--image', source), ('--upgrade-to', target)):
        if not DIGEST_REF.fullmatch(ref or ''):
            return f'{name} must be pinned by digest (REPO@sha256:...)'
    if source == target:
        return ('--upgrade-to equals --image; `bootc switch` reports "Image specification is unchanged" '
                'and stages nothing, so no switch or rollback could be proven')
    return None


def notification_tokens(text, token):
    """Whether the shell's persisted notification store names the proof token."""
    try:
        data = json.loads(text)
    except (TypeError, ValueError):
        return {'parsed': False, 'present': False}
    found = token in json.dumps(data)
    return {'parsed': True, 'present': found, 'version': data.get('version') if isinstance(data, dict) else None}


def history_retained(before, after, now_secs):
    """Wellbeing history before a checkpoint must survive into the next one.

    Entries may only disappear through the shell's own retention: older than
    14 weeks (except the last entry), or an equal-time on/off pair that the
    store collapses (shell-gtk wellbeing.rs `collapse_equal_time_pairs`)."""
    def parse(text):
        if text is None:
            return []
        value = json.loads(text)
        if not isinstance(value, list):
            raise ValueError('history is not an array')
        return [(e['oldState'], e['newState'], e['wallTimeSecs']) for e in value]
    old, new = parse(before), parse(after)
    present = set(new)
    dropped = []
    for index, entry in enumerate(old):
        if entry in present:
            continue
        last = index + 1 == len(old)
        if not last and entry[2] < now_secs - HISTORY_THRESHOLD_SECONDS:
            continue
        collapsed = any(other[2] == entry[2] and other[1] == entry[0] for other in new + old[index + 1:])
        if collapsed:
            continue
        dropped.append({'oldState': entry[0], 'newState': entry[1], 'wallTimeSecs': entry[2]})
    return {'before': len(old), 'after': len(new), 'unexplained_drops': dropped, 'retained': not dropped}


SUPERVISION = re.compile(r'shell supervision: (ShellExited|RestartScheduled|BudgetExhausted|ShellStarted)\b(.*)')


def supervision_events(lines):
    """`tuna-compositor: shell supervision: …` journal events, in order."""
    events = []
    for line in lines:
        match = SUPERVISION.search(line)
        if match:
            events.append({'event': match.group(1), 'detail': match.group(2).strip()})
    return events


def shell_restart_observed(events):
    """A shell exit followed by a scheduled restart, and no exhausted budget."""
    names = [event['event'] for event in events]
    if 'BudgetExhausted' in names or 'ShellExited' not in names:
        return False
    return 'RestartScheduled' in names[names.index('ShellExited'):]


def gsettings_mismatches(expected, observed):
    """Fixture keys whose observed `gsettings get` output differs."""
    problems = []
    for schema, key, value in expected:
        actual = observed.get(f'{schema} {key}')
        if actual != value:
            problems.append({'schema': schema, 'key': key, 'expected': value, 'observed': actual})
    return problems


def lock_matches(state, locked):
    """Whether a lock probe shows the wanted state.

    `active` is the shell's `org.gnome.ScreenSaver.GetActive` reply. logind's
    LockedHint must agree when the compositor sets it (from #849, beta.3);
    beta.2's compositor never does, so there it is recorded, not required."""
    if state.get('active') != ('(true,)' if locked else '(false,)'):
        return False
    if state.get('locked_hint_supported'):
        return state.get('locked_hint') == ('yes' if locked else 'no')
    return True
