#!/usr/bin/env python3
"""Admit the two-file transition from original .deb archives before extraction.

This is package-content policy, not evidence of a display-manager login,
installed upgrade, PAM authentication, or Arch/Marlin activation.
"""
import importlib.util
import io
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tarfile

_spec = importlib.util.spec_from_file_location(
    'release_deb_ownership', Path(__file__).with_name('release-deb-ownership.py'))
_ownership = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_ownership)

LEGACY_SESSION = 'usr/share/wayland-sessions/roost.desktop'  # tuna-rename: keep
LEGACY_PAM = 'etc/pam.d/roost-lock'  # tuna-rename: keep
SESSION = 'usr/share/wayland-sessions/tuna.desktop'
PAM = 'etc/pam.d/tuna-lock'
BRIDGES = frozenset((LEGACY_SESSION, LEGACY_PAM))
MAX_TEXT = 16 * 1024
MAX_MEMBERS = 4096
DESKTOP = {
    'Name': 'Tuna Desktop',
    'Comment': 'Tuna Desktop (GNOME-style shell with optional scrollable tiling)',
    'Exec': 'tuna-session',
    'TryExec': 'tuna-session',
    'Type': 'Application',
    'DesktopNames': 'Tuna;GNOME;',
    'X-GDM-SessionRegisters': 'true',
}


def canonical_path(name, directory=False):
    """Permit tar's one leading ./, never normalize ambiguous input away."""
    if not name or '\\' in name or any(ord(c) < 32 or ord(c) == 127 for c in name):
        raise ValueError('archive-path-ambiguous')
    if name in ('.', './'):
        if not directory:
            raise ValueError('archive-root-not-directory')
        return '.'
    if name.startswith('./'):
        name = name[2:]
    if directory and name.endswith('/'):
        name = name[:-1]
    if any(part in ('', '.', '..') for part in name.split('/')):
        raise ValueError('archive-path-ambiguous')
    return name


def archive_index(raw, kind):
    if kind not in ('data', 'control') or not 0 < len(raw) <= _ownership.MAX_TAR:
        raise ValueError('archive-bounds')
    entries, text = {}, {}
    with tarfile.open(fileobj=io.BytesIO(raw), mode='r:') as archive:
        for member in archive:
            if len(entries) >= MAX_MEMBERS:
                raise ValueError('archive-entry-bound')
            name = canonical_path(member.name, member.isdir())
            if name in entries:
                raise ValueError('archive-path-duplicate')
            if member.type not in (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE, tarfile.SYMTYPE) or member.sparse is not None:
                raise ValueError('archive-entry-type')
            # tarfile strips directory suffix slashes; inspect the original
            # final header too so redundant slashes cannot be normalized away.
            header = raw[member.offset_data - 512:member.offset_data]
            header_name = header[:100].split(b'\0', 1)[0].decode('utf-8', errors='strict')
            if header[257:263] == b'ustar\0':
                prefix = header[345:500].split(b'\0', 1)[0].decode('utf-8', errors='strict')
                if prefix:
                    header_name = prefix + '/' + header_name
            canonical_path(header_name, member.isdir())
            entries[name] = member
            if kind == 'data' and 'roost' in name.lower() and name not in BRIDGES:  # tuna-rename: keep
                raise ValueError('forbidden-legacy-path')
            if kind == 'control' and name in ('preinst', 'postinst', 'prerm', 'postrm', 'config', 'triggers'):
                raise ValueError('transition-lifecycle-hook')
            if name in (SESSION, LEGACY_SESSION, PAM, LEGACY_PAM, 'conffiles'):
                if not member.isfile() or member.mode != 0o644:
                    raise ValueError('transition-regular-mode')
                if not 0 < member.size <= MAX_TEXT:
                    raise ValueError('transition-text-bound')
                stream = archive.extractfile(member)
                if stream is None:
                    raise ValueError('transition-text-missing')
                text[name] = stream.read(MAX_TEXT + 1).decode('utf-8', errors='strict')
    if not entries:
        raise ValueError('empty-archive')
    # Never allow extraction through a symlink/non-directory ancestor, even
    # when the offending entry follows its children in the original archive.
    for name in entries:
        for parent in PurePosixPath(name).parents:
            ancestor = entries.get(str(parent))
            if ancestor is not None and not ancestor.isdir():
                raise ValueError('archive-parent-not-directory')
            if (kind == 'data' and name in (SESSION, LEGACY_SESSION, PAM, LEGACY_PAM)
                    and ancestor is not None and ancestor.mode & 0o022):
                raise ValueError('transition-parent-writable')
    # Asset symlinks are allowed only when they resolve inside this archive.
    # Transition files themselves were required to be regular above.
    for name, member in entries.items():
        if member.issym():
            target = canonical_path(member.linkname)
            target = str(PurePosixPath(name).parent / target)
            seen = {name}
            while True:
                if target in seen or target not in entries:
                    raise ValueError('archive-link-dangling-or-cycle')
                seen.add(target)
                linked = entries[target]
                if not linked.issym():
                    break
                target = str(PurePosixPath(target).parent / canonical_path(linked.linkname))
    return entries, text


def desktop_fields(text):
    fields = {}
    group = False
    for line in text.splitlines():
        if not line or line.startswith('#'):
            continue
        if line == '[Desktop Entry]' and not group and not fields:
            group = True
            continue
        if not group or '=' not in line:
            raise ValueError('transition-desktop-format')
        key, value = line.split('=', 1)
        if not key or key in fields:
            raise ValueError('transition-desktop-duplicate')
        fields[key] = value
    return fields


def pam_rules(text):
    return [line.split() for line in text.splitlines() if line.strip() and not line.lstrip().startswith('#')]


def validate(data, control):
    _, payload = archive_index(data, 'data')
    _, metadata = archive_index(control, 'control')
    if not {SESSION, LEGACY_SESSION, PAM, LEGACY_PAM}.issubset(payload):
        raise ValueError('transition-file-missing')
    if desktop_fields(payload[SESSION]) != DESKTOP:
        raise ValueError('transition-current-desktop-content')
    if desktop_fields(payload[LEGACY_SESSION]) != dict(DESKTOP, NoDisplay='true'):
        raise ValueError('transition-legacy-desktop-content')
    for path in (PAM, LEGACY_PAM):
        if pam_rules(payload[path]) != [['auth', 'include', 'login'], ['account', 'include', 'login']]:
            raise ValueError('transition-pam-content')
    conffiles = metadata.get('conffiles', '').splitlines()
    if len(conffiles) != 2 or set(conffiles) != {'/' + PAM, '/' + LEGACY_PAM}:
        raise ValueError('transition-conffiles')


def check(package):
    validate(_ownership.original_tar(package, 'data'), _ownership.original_tar(package, 'control'))


if __name__ == '__main__':
    try:
        if len(sys.argv) != 2:
            raise ValueError('fixed-arguments')
        check(Path(sys.argv[1]))
    except (OSError, UnicodeError, ValueError, tarfile.TarError, subprocess.SubprocessError) as error:
        print(f'transition-package-policy: original archive rejected ({error})', file=sys.stderr)
        raise SystemExit(1) from None
