"""Strict RPM provenance for packaged resources and generated Fontconfig caches."""
import ctypes
import os
from pathlib import Path
import re
import stat
import subprocess

CACHE_NAME = re.compile(r'[0-9a-f]{32}-(?:le|be)(?:32|64)\.cache-[1-9][0-9]{0,2}')


def fontconfig_cache_directories():
    """Read the actual native configuration; do not create/rebuild any caches."""
    native = ctypes.CDLL('libfontconfig.so.1')
    native.FcInitLoadConfig.argtypes = []
    native.FcInitLoadConfig.restype = ctypes.c_void_p
    native.FcConfigGetCacheDirs.argtypes = [ctypes.c_void_p]
    native.FcConfigGetCacheDirs.restype = ctypes.c_void_p
    native.FcStrListNext.argtypes = [ctypes.c_void_p]
    native.FcStrListNext.restype = ctypes.c_char_p
    native.FcStrListDone.argtypes = [ctypes.c_void_p]
    native.FcConfigDestroy.argtypes = [ctypes.c_void_p]
    config = native.FcInitLoadConfig()
    if not config:
        raise RuntimeError('actual Fontconfig configuration unavailable')
    try:
        listing = native.FcConfigGetCacheDirs(config)
        if not listing:
            raise RuntimeError('actual Fontconfig cache directories unavailable')
        try:
            directories = set()
            for _ in range(16384):
                value = native.FcStrListNext(listing)
                if value is None:
                    return directories
                path = Path(os.fsdecode(value))
                if not path.is_absolute():
                    raise RuntimeError('native cache directory is not absolute')
                directories.add(path)
            raise RuntimeError('native cache directory count bound failed')
        finally:
            native.FcStrListDone(listing)
    finally:
        native.FcConfigDestroy(config)


def rpm_owner(path):
    result = subprocess.run(['rpm', '-qf', '--qf', '%{NAME}', str(path)],
                            capture_output=True, text=True, timeout=10, env={**os.environ, 'LC_ALL': 'C'})
    if result.returncode == 0 and result.stdout and not result.stderr:
        return result.stdout
    if (result.returncode == 1 and not result.stderr
            and result.stdout.strip() == f'file {path} is not owned by any package'):
        return None
    raise RuntimeError('actual resource RPM ownership query failed')


def resource_package(path, resource, observed, cache_directories):
    """Return a package and explicit provenance; unknown unowned files fail."""
    package = rpm_owner(path)
    if package is not None:
        return package, {'kind': 'rpm-payload'}
    # Fontconfig generates these files from packaged fonts. They remain in the
    # helper's digest and FD/named-identity verification; they are not RPM payloads.
    # Never turn an arbitrary unowned file or user-writable cache into a pass.
    if (resource['kind'] != 'file' or not CACHE_NAME.fullmatch(path.name)
            or path.parent not in cache_directories
            or path.resolve(strict=True) != path
            or not stat.S_ISREG(observed.st_mode) or observed.st_uid != 0
            or observed.st_gid != 0 or observed.st_mode & 0o133):
        raise RuntimeError('unowned resource is not an immutable native Fontconfig cache')
    directory = path.parent.lstat()
    if (not stat.S_ISDIR(directory.st_mode) or directory.st_uid != 0
            or directory.st_gid != 0 or directory.st_mode & 0o022
            or rpm_owner(path.parent) != 'fontconfig'):
        raise RuntimeError('native cache directory lacks immutable Fontconfig package ownership')
    return 'fontconfig', {'kind': 'generated-fontconfig-cache',
                          'directory': str(path.parent),
                          'directory_rpm_owner': 'fontconfig'}
