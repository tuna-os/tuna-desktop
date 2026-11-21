#!/usr/bin/env python3
"""Parallel legs of the native Core consumer, and the join that states the matrix.

One installed-session boot cannot drive all 28 applications inside the VM
job's limit, so the consumer runs one boot per leg, every leg on the same
admitted image. A leg runs whole domains with every per-application
requirement unchanged, and qualifies only its own applications. Only `join`
may state the complete matrix: it recomputes completeness from the legs'
receipts and never trusts a leg's own claim about another leg.

Usage:
  core_apps_legs.py join --receipts DIR --out DIR --commit SHA --image-id ID
      --package-sha256 HEX --run-id N [--job NAME=RESULT ...]
"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import sys

# Leg -> the host domain modules it runs (core_apps_<domain>.py, in this
# order) and the applications those domains complete. `autotz` shares the
# web domain's GeoClue static source, so both stay in one boot.
LEGS = {
    'basic': {'domains': ('basic',),
              'apps': ('org.gnome.font-viewer', 'org.gnome.Calculator', 'org.gnome.Loupe',
                       'org.gnome.Papers', 'org.gnome.Settings')},
    'data': {'domains': ('data',),
             'apps': ('org.gnome.Nautilus', 'org.gnome.TextEditor', 'org.gnome.Console',
                      'org.gnome.Calendar', 'org.gnome.Contacts')},
    'media': {'domains': ('media',),
              'apps': ('org.gnome.Decibels', 'org.gnome.Music', 'org.gnome.Showtime',
                       'org.gnome.Snapshot', 'org.gnome.SimpleScan')},
    'system': {'domains': ('system',),
               'apps': ('org.gnome.Connections', 'org.gnome.Software', 'org.gnome.DiskUtility',
                        'org.gnome.SystemMonitor', 'org.gnome.Logs', 'org.gnome.baobab')},
    'web': {'domains': ('web', 'autotz'),
            'apps': ('org.gnome.Epiphany', 'org.gnome.Characters', 'org.gnome.Maps',
                     'org.gnome.Weather', 'org.gnome.clocks', 'org.gnome.Tour', 'org.gnome.Yelp')},
}
DOMAINS = ('basic', 'data', 'media', 'system', 'web', 'autotz')
RECEIPT_NAME = 'core-apps-leg.json'
RECEIPT_KIND = 'native-core-apps-leg'
JOIN_KIND = 'native-core-apps-join'
# A leg's own lane assertion. The matrix ids below are written by the join only.
LEG_ASSERTION = 'V-CORE-APPS-LEG'
MATRIX_ASSERTION = 'V-CORE-APPS'
TIMEZONE_ASSERTION = 'V-CORE-APPS-AUTOTZ'
SOURCE_FIELDS = ('commit', 'capture_commit', 'built_tree', 'required_source_inputs_sha256',
                 'image_id', 'base_image', 'package_sha256')
APP_FIELDS = ('package', 'version', 'desktop_sha256')
MAX_RECEIPT = 1048576


def owners(legs=None):
    """Application -> leg, refusing a table that is not a partition."""
    legs = LEGS if legs is None else legs
    domains = [domain for spec in legs.values() for domain in spec['domains']]
    if sorted(domains) != sorted(DOMAINS):
        raise ValueError('every native Core domain must run in exactly one leg')
    result = {}
    for leg, spec in legs.items():
        if ('web' in spec['domains']) != ('autotz' in spec['domains']):
            raise ValueError('web and automatic timezone share one GeoClue fixture and one boot')
        for app in spec['apps']:
            if app in result:
                raise ValueError('application declared in two legs: ' + app)
            result[app] = leg
    return result


OWNER = owners()
TIMEZONE_LEG = next(leg for leg, spec in LEGS.items() if 'autotz' in spec['domains'])


def partition(installed):
    """Assign every installed application to exactly one leg.

    The installed inventory is the guest's own observation. An installed
    application no leg declares, a declared application that is not
    installed, and a repeated application all fail: none may be dropped.
    """
    installed = list(installed)
    repeated = sorted(app for app, count in Counter(installed).items() if count > 1)
    if repeated:
        raise ValueError('installed inventory repeats an application: ' + ', '.join(repeated))
    unknown = sorted(set(installed) - set(OWNER))
    if unknown:
        raise ValueError('installed application has no native Core leg: ' + ', '.join(unknown))
    absent = sorted(set(OWNER) - set(installed))
    if absent:
        raise ValueError('declared native Core application is not installed: ' + ', '.join(absent))
    return {leg: sorted(app for app in installed if OWNER[app] == leg) for leg in LEGS}


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'),
                                     ensure_ascii=False).encode()).hexdigest()


def identity(metadata, inventory):
    """What a leg ran on: admitted source, package and image, and the installed inventory."""
    result = {name: metadata.get(name) for name in SOURCE_FIELDS}
    result['member_sha256'] = digest(metadata.get('member_sha256'))
    installed = inventory is not None
    result['installed_apps'] = ({app: {name: row[name] for name in APP_FIELDS}
                                 for app, row in sorted(inventory['apps'].items())}
                                if installed else None)
    result['installation_sha256'] = digest(inventory['installation']) if installed else None
    result['observer_sha256'] = digest(inventory['observer_sha256']) if installed else None
    return result


def receipt(leg, metadata, inventory, completed, timezone, assertions, run=None):
    """One leg's result bound to its identity and exact application list.

    Written for a failed leg too; `passed` requires every lane assertion of
    the boot, not only the Core consumer's.
    """
    spec = LEGS[leg]
    assigned = None
    if inventory is not None:
        try:
            assigned = partition(inventory['apps'])[leg]
        except ValueError:
            assigned = None
    return {'schema': 1, 'kind': RECEIPT_KIND, 'leg': leg, 'domains': list(spec['domains']),
            'identity': identity(metadata, inventory),
            'assigned_apps': assigned, 'completed_apps': list(completed),
            'automatic_timezone': {
                'required': 'autotz' in spec['domains'],
                'qualified': None if timezone is None else timezone.get('qualified') is True},
            'assertions': dict(assertions),
            'passed': assertions.get(LEG_ASSERTION) is True and all(
                value is True for value in assertions.values()),
            'run': dict(run or {}),
            'installed_signed_release_qualified': False,
            'scope': 'one leg of the native Core consumer; only the join of every leg states the matrix'}


def _image(value):
    return value.removeprefix('sha256:') if isinstance(value, str) else value


def _leg_problems(leg, row):
    if row is None:
        return ['receipt missing']
    problems = []
    spec = LEGS[leg]
    if row.get('domains') != list(spec['domains']):
        problems.append('receipt domains differ from the declared leg')
    if row.get('installed_signed_release_qualified') is not False:
        problems.append('receipt claims signed-release qualification')
    assertions = row.get('assertions')
    if not isinstance(assertions, dict) or assertions.get(LEG_ASSERTION) is not True:
        problems.append(LEG_ASSERTION + ' did not pass')
    else:
        problems += ['lane assertion ' + name + ' did not pass'
                     for name, value in sorted(assertions.items()) if value is not True]
    if row.get('passed') is not True:
        problems.append('leg did not pass')
    assigned, completed = row.get('assigned_apps'), row.get('completed_apps')
    if (not isinstance(assigned, list) or not isinstance(completed, list)
            or not all(isinstance(app, str) for app in assigned + completed)):
        problems.append('application lists missing')
    else:
        repeated = sorted(app for app, count in Counter(completed).items() if count > 1)
        if repeated:
            problems.append('application completed twice: ' + ', '.join(repeated))
        missing, extra = sorted(set(assigned) - set(completed)), sorted(set(completed) - set(assigned))
        if missing:
            problems.append('assigned application not completed: ' + ', '.join(missing))
        if extra:
            problems.append('completed application outside the assignment: ' + ', '.join(extra))
    timezone = row.get('automatic_timezone')
    required = 'autotz' in spec['domains']
    if (not isinstance(timezone, dict) or timezone.get('required') is not required
            or timezone.get('qualified') is not (True if required else None)):
        problems.append('automatic timezone not qualified by its leg' if required
                        else 'automatic timezone claimed outside its leg')
    subject = row.get('identity')
    if (not isinstance(subject, dict)
            or not isinstance(subject.get('commit'), str) or not re.fullmatch('[0-9a-f]{40}', subject['commit'])
            or not isinstance(subject.get('package_sha256'), str)
            or not re.fullmatch('[0-9a-f]{64}', subject['package_sha256'])
            or not isinstance(subject.get('image_id'), str)
            or not re.fullmatch('(sha256:)?[0-9a-f]{64}', subject['image_id'])
            or not isinstance(subject.get('base_image'), str)
            or not isinstance(subject.get('installed_apps'), dict) or not subject['installed_apps']
            or not all(isinstance(subject.get(name), str)
                       for name in ('member_sha256', 'installation_sha256', 'observer_sha256'))):
        problems.append('source, package, image or installed-inventory identity incomplete')
    return problems


def join(receipts, *, commit, image_id, package_sha256, run_id, jobs):
    """Recompute the complete native Core verdict from the legs' receipts.

    Passes only when every declared leg has exactly one passing receipt from
    this run, every receipt names the same source, package, image and
    installed inventory, that identity is the run's own (`commit`,
    `image_id`, `package_sha256`), and the legs' completed applications are
    the installed inventory exactly: none missing, none completed by two
    legs, none outside it. `jobs` maps the producing jobs to their CI
    results; anything but success fails.
    """
    errors = ['job ' + name + ' result is ' + (str(result) or 'absent') + ', not success'
              for name, result in sorted(jobs.items()) if result != 'success']
    by_leg = {}
    for row in receipts:
        if not isinstance(row, dict) or row.get('schema') != 1 or row.get('kind') != RECEIPT_KIND:
            errors.append('malformed leg receipt')
        elif row.get('leg') not in LEGS:
            errors.append('receipt for an undeclared leg: ' + str(row.get('leg')))
        elif row['leg'] in by_leg:
            errors.append('more than one receipt for leg ' + row['leg'])
        else:
            by_leg[row['leg']] = row
    legs = {}
    for leg in LEGS:
        row = by_leg.get(leg)
        problems = _leg_problems(leg, row)
        completed = row.get('completed_apps') if row is not None else None
        legs[leg] = {'present': row is not None, 'passed': not problems, 'errors': problems,
                     'apps': [app for app in completed if isinstance(app, str)]
                             if isinstance(completed, list) else []}
        errors += [leg + ': ' + problem for problem in problems]

    subjects = {leg: row['identity'] for leg, row in by_leg.items() if isinstance(row.get('identity'), dict)}
    common = None
    if subjects:
        first = next(leg for leg in LEGS if leg in subjects)
        common = subjects[first]
        for leg, subject in subjects.items():
            differing = sorted(name for name in set(common) | set(subject)
                               if common.get(name) != subject.get(name))
            if differing:
                errors.append('identity differs between legs ' + first + ' and ' + leg + ': '
                              + ', '.join(differing))
        for name, expected, observed in (
                ('commit', commit, common.get('commit')),
                ('image_id', _image(image_id), _image(common.get('image_id'))),
                ('package_sha256', package_sha256, common.get('package_sha256'))):
            if not expected or observed != expected:
                errors.append('legs ran on a different ' + name + ' than this run admitted')
    errors += [leg + ': receipt belongs to another run' for leg, row in by_leg.items()
               if not run_id or not isinstance(row.get('run'), dict)
               or str(row['run'].get('run_id')) != str(run_id)]

    installed = common.get('installed_apps') if common is not None else None
    holders = {}
    for leg in LEGS:
        for app in set(legs[leg]['apps']):
            holders.setdefault(app, []).append(leg)
    if not isinstance(installed, dict) or not installed:
        errors.append('no installed application inventory to join against')
    else:
        try:
            expected = partition(installed)
        except ValueError as error:
            errors.append(str(error))
        else:
            errors += [leg + ': assignment differs from the partition of the installed inventory'
                       for leg, row in by_leg.items() if row.get('assigned_apps') != expected[leg]]
        for app in sorted(app for app, owning in holders.items() if len(owning) > 1):
            errors.append('application completed by more than one leg: ' + app
                          + ' (' + ', '.join(holders[app]) + ')')
        missing, extra = sorted(set(installed) - set(holders)), sorted(set(holders) - set(installed))
        if missing:
            errors.append('installed application completed by no leg: ' + ', '.join(missing))
        if extra:
            errors.append('completed application is not in the installed inventory: ' + ', '.join(extra))
    return {'schema': 1, 'kind': JOIN_KIND, 'passed': not errors, 'errors': errors, 'legs': legs,
            'identity': common, 'apps': sorted(holders), 'jobs': dict(jobs),
            'installed_signed_release_qualified': False,
            'scope': 'join of the native Core legs on one source, package and image; '
                     'not signed-release qualification'}


def assertion_lines(verdict):
    """`<ID> pass|fail <description>` rows; every id passes only with the whole join."""
    passed = verdict['passed']
    state = 'pass' if passed else 'fail'
    errors = verdict['errors']
    reason = '' if passed else ('join failed: ' + errors[0]
                                + (' (+%d more)' % (len(errors) - 1) if len(errors) > 1 else ''))
    subject = verdict['identity'] or {}
    lines = [(MATRIX_ASSERTION, '%d installed applications and automatic timezone across %d legs on '
              'source %s image %s; not signed-release qualification'
              % (len(verdict['apps']), len(LEGS), str(subject.get('commit'))[:12],
                 str(_image(subject.get('image_id')))[:12]) if passed else reason)]
    for leg in LEGS:
        row = verdict['legs'][leg]
        own = ('%d applications: %s' % (len(row['apps']), ', '.join(row['apps']))
               if row['passed'] else 'leg failed: ' + '; '.join(row['errors']))
        lines.append((MATRIX_ASSERTION + '-' + leg.upper(), own if passed else own + '; ' + reason))
    timezone = verdict['legs'][TIMEZONE_LEG]
    lines.append((TIMEZONE_ASSERTION, 'automatic timezone domain in leg ' + TIMEZONE_LEG
                  if passed else ('leg ' + TIMEZONE_LEG + (' passed' if timezone['passed'] else ' failed')
                                  + '; ' + reason)))
    return ['%s %s %s' % (name, state, ' '.join(description.split())) for name, description in lines]


def read_receipts(directory):
    """Every leg receipt below `directory`; an unreadable one stays a malformed row.

    No directory at all (no leg uploaded anything) is no receipt, which the
    join reports as every leg missing.
    """
    rows = []
    root = Path(directory)
    for path in sorted(root.rglob(RECEIPT_NAME)) if root.is_dir() else ():
        try:
            if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_RECEIPT:
                raise ValueError('receipt type or size')
            rows.append(json.loads(path.read_bytes()))
        except (OSError, ValueError):
            rows.append(None)
    return rows


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest='command', required=True)
    joiner = commands.add_parser('join', help='state the complete matrix from every leg receipt')
    joiner.add_argument('--receipts', required=True, help='directory holding each leg\'s ' + RECEIPT_NAME)
    joiner.add_argument('--out', required=True, help='directory for join.json and assertions.txt')
    joiner.add_argument('--commit', required=True, help='source commit this run admitted')
    joiner.add_argument('--image-id', required=True, help='image this run built for every leg')
    joiner.add_argument('--package-sha256', required=True, help='package this run built the image from')
    joiner.add_argument('--run-id', required=True, help='CI run every receipt must belong to')
    joiner.add_argument('--job', action='append', default=[], metavar='NAME=RESULT',
                        help='CI result of a producing job; anything but success fails the join')
    args = parser.parse_args(argv)
    jobs = dict(value.split('=', 1) if '=' in value else (value, '') for value in args.job)
    verdict = join(read_receipts(args.receipts), commit=args.commit, image_id=args.image_id,
                   package_sha256=args.package_sha256, run_id=args.run_id, jobs=jobs)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / 'join.json').write_text(json.dumps(verdict, indent=2, ensure_ascii=False) + '\n')
    lines = assertion_lines(verdict)
    (out / 'assertions.txt').write_text('\n'.join(lines) + '\n')
    print('\n'.join(lines))
    for error in verdict['errors']:
        print('core-apps join: ' + error, file=sys.stderr)
    return 0 if verdict['passed'] else 1


if __name__ == '__main__':
    sys.exit(main())
