#!/usr/bin/python3
"""Typed standalone clock consumer on the original native GDM session.

No application catalog, Core matrix, private service bus, login substitute,
account elevation or source-provider override is admitted here.
"""
import json
import os
from pathlib import Path
import re
import stat
import traceback

from native_session import NativeSession, digest

MEMBERS = ('native_session.py', 'autotz_guest.py', 'autotz.py', 'geoclue_fixture.py')


def installed_members():
    files = [Path(__file__).resolve().parent / name for name in MEMBERS]
    files.extend(Path('/usr/libexec') / name for name in ('tuna-geoclue-agent-policy', 'tuna-vm-lifecycle'))
    observed = {}
    for file in files:
        metadata = file.stat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != 0 or metadata.st_mode & 0o022:
            raise RuntimeError('standalone native observer is not original immutable installed code')
        observed[str(file)] = digest(file)
    return observed


def desktop(operation, arguments, context):
    if operation == 'prepare':
        if context.run_state.exists() or context.fixture.exists():
            raise RuntimeError('standalone clock run already exists; refusing receipt reuse')
        identity = context.identity()
        if context.uid != 1000:
            raise RuntimeError('original native clock fixture UID differs')
        context.fixture.mkdir(mode=0o700, parents=True)
        for directory in (context.root, context.fixture):
            os.chown(directory, context.uid, context.owner.pw_gid)
        descriptor = os.open(context.run_state, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(descriptor, 'w') as stream:
            json.dump({'nonce': context.nonce, 'identity': identity}, stream)
        groups = context.command(['id', '-nG', context.owner.pw_name]).stdout.split()
        return {'identity': identity, 'fixture': str(context.fixture),
                'observer_sha256': installed_members(),
                'account': {'name': context.owner.pw_name, 'uid': context.uid, 'groups': groups,
                            'role_changed_by_observer': False},
                'installed_signed_release_qualified': False,
                'scope': 'standalone-native-clock-no-Core-application-inventory'}
    if operation == 'state':
        return context.state()
    if operation == 'shell-actor':
        return context.shell_actor()
    if operation == 'tree':
        actor = arguments['actor']
        original = context.identity()['shell']
        if (actor.get('kind') != 'shell' or actor.get('id') != 'tuna-shell-gtk'
                or any(actor.get(key) != value for key, value in original.items())):
            raise RuntimeError('standalone clock tree is not the original native shell')
        return context.tree(actor)
    if operation == 'finish':
        return {'identity': context.identity(), 'owned_original_session_preserved': True}
    raise ValueError('unknown standalone native clock observation')


def execute(arguments, owner, inventory, state):
    if len(arguments) != 3:
        raise ValueError('autotz requires domain, operation and bounded JSON arguments')
    domain, operation, encoded = arguments
    if domain not in ('autotz', 'desktop') or len(encoded) > 65536:
        raise ValueError('bounded standalone clock domain required')
    values = json.loads(encoded)
    if not isinstance(values, dict):
        raise ValueError('typed standalone clock arguments required')
    values = dict(values)
    context = NativeSession(owner, inventory, state, values.pop('_nonce'), fixture_root='tuna-autotz')
    if domain == 'desktop':
        return desktop(operation, values, context)
    context.identity()
    import autotz
    return autotz.dispatch(operation, values, context)


def error_receipt(error, arguments):
    domain = arguments[0] if arguments and arguments[0] in ('autotz', 'desktop') else 'arguments'
    operation = arguments[1] if len(arguments) > 1 and re.fullmatch(r'[a-z][a-z0-9-]{0,63}', arguments[1]) else 'arguments'
    directory = Path(__file__).resolve().parent
    sites = [{'module': Path(frame.filename).name, 'line': frame.lineno, 'function': frame.name}
             for frame in traceback.extract_tb(error.__traceback__)
             if Path(frame.filename).resolve().parent == directory][-4:]
    cause = getattr(error, 'autotz_cause_sites', None)
    if type(cause) is list:
        # The causal origin (a caught liveness failure) displaces generic
        # outer frames; validated to the same bounded shape as live frames.
        for site in cause:
            if (type(site) is dict and set(site) == {'module', 'line', 'function'}
                    and site['module'] in MEMBERS and type(site['line']) is int
                    and 1 <= site['line'] <= 100000 and type(site['function']) is str
                    and re.fullmatch(r'(?:[A-Za-z_][A-Za-z0-9_]{0,127}|<module>|<listcomp>|<dictcomp>|<genexpr>)',
                                     site['function'])):
                sites.append(site)
        sites = sites[-4:]
    result = {'domain': domain, 'operation': operation, 'exception_type': type(error).__name__, 'sites': sites}
    for name in ('errno', 'returncode'):
        value = getattr(error, name, None)
        if type(value) is int and -4095 <= value <= 4095:
            result[name] = value
    return result
