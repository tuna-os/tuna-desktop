"""Native Core legs: the partition, each leg's own share, the join verdict and its CI wiring.

Pure host logic only. Nothing here boots a VM or qualifies an application.
"""
import copy
import importlib.machinery
import importlib.util
import io
import json
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts' / 'lib'))
sys.path.insert(0, str(ROOT / 'packaging/marlin/vm-lane/tuna-native-session'))
import core_apps
import core_apps_legs as legs

_spec = importlib.util.spec_from_file_location(
    'core_apps_legs_guest', ROOT / 'packaging/marlin/vm-lane/core-apps/guest.py')
guest = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(guest)

_ledger_loader = importlib.machinery.SourceFileLoader('core_apps_legs_ledger', str(ROOT / 'scripts/tuna-ledger'))
ledger = importlib.util.module_from_spec(importlib.util.spec_from_loader(_ledger_loader.name, _ledger_loader))
_ledger_loader.exec_module(ledger)

COMMIT, IMAGE, PACKAGE, RUN = 'a' * 40, 'sha256:' + 'b' * 64, 'c' * 64, '4242'
MATRIX_IDS = {'V-CORE-APPS', 'V-CORE-APPS-BASIC', 'V-CORE-APPS-DATA', 'V-CORE-APPS-MEDIA',
              'V-CORE-APPS-SYSTEM', 'V-CORE-APPS-WEB', 'V-CORE-APPS-AUTOTZ'}


def metadata(**changes):
    return {'schema': 1, 'commit': COMMIT, 'capture_commit': COMMIT, 'built_tree': 'd' * 40,
            'required_source_inputs_sha256': 'e' * 64, 'image_id': IMAGE,
            'base_image': 'ghcr.io/tuna-os/marlin:gnome@sha256:' + 'f' * 64,
            'package_sha256': PACKAGE, 'member_sha256': {'/usr/bin/tuna-compositor': '1' * 64},
            'installed_signed_release_qualified': False} | changes


def inventory(apps=None):
    apps = guest.APP_PACKAGES if apps is None else apps
    return {'apps': {app: {'app_id': app, 'package': package, 'version': '51.0-1',
                           'desktop_sha256': '2' * 64, 'desktop_name': app}
                     for app, package in apps.items()},
            'installation': {'source_commit': COMMIT, 'after': {}},
            'observer_sha256': {'/usr/libexec/tuna-core-apps/guest.py': '3' * 64}}


def leg_receipt(leg, *, source=None, installed=None, completed=None, timezone='default',
                assertions=None, run_id=RUN):
    installed = inventory() if installed is None else installed
    if completed is None:
        completed = list(legs.LEGS[leg]['apps'])
    if timezone == 'default':
        timezone = {'qualified': True} if leg == legs.TIMEZONE_LEG else None
    if assertions is None:
        assertions = {'V-GDM-LOGIN': True, 'V-PANEL': True, legs.LEG_ASSERTION: True, 'V-NOPANIC': True}
    return legs.receipt(leg, metadata() if source is None else source, installed, completed, timezone,
                        assertions, run={'run_id': run_id, 'run_attempt': '1'})


def all_receipts(**per_leg):
    return [leg_receipt(leg, **per_leg.get(leg, {})) for leg in legs.LEGS]


def join(rows, **changes):
    arguments = {'commit': COMMIT, 'image_id': IMAGE, 'package_sha256': PACKAGE, 'run_id': RUN,
                 'jobs': {'core-apps-image': 'success', 'core-apps-leg': 'success'}} | changes
    return legs.join(rows, **arguments)


def ledger_passes(verdict):
    """What scripts/tuna-ledger admits from the join's assertions file."""
    with tempfile.TemporaryDirectory() as scratch:
        path = Path(scratch) / 'assertions.txt'
        path.write_text('\n'.join(legs.assertion_lines(verdict)) + '\n')
        return ledger.load_assertions([str(path)])


class Partition(unittest.TestCase):
    def test_every_installed_application_lands_in_exactly_one_leg(self):
        shares = legs.partition(guest.APP_PACKAGES)
        self.assertEqual(list(shares), list(legs.LEGS))
        placed = [app for share in shares.values() for app in share]
        self.assertEqual(len(placed), 28)
        self.assertEqual(sorted(placed), sorted(guest.APP_PACKAGES))
        self.assertEqual(len(set(placed)), len(placed))
        for leg, share in shares.items():
            self.assertEqual(share, sorted(legs.LEGS[leg]['apps']))
        self.assertEqual({leg: len(share) for leg, share in shares.items()},
                         {'basic': 5, 'data': 5, 'media': 5, 'system': 6, 'web': 7})

    def test_unknown_installed_application_fails(self):
        with self.assertRaisesRegex(ValueError, 'no native Core leg: org.gnome.Unknown'):
            legs.partition([*guest.APP_PACKAGES, 'org.gnome.Unknown'])

    def test_declared_application_that_is_not_installed_fails(self):
        installed = [app for app in guest.APP_PACKAGES if app != 'org.gnome.Maps']
        with self.assertRaisesRegex(ValueError, 'not installed: org.gnome.Maps'):
            legs.partition(installed)

    def test_repeated_installed_application_fails(self):
        with self.assertRaisesRegex(ValueError, 'repeats an application: org.gnome.Loupe'):
            legs.partition([*guest.APP_PACKAGES, 'org.gnome.Loupe'])

    def test_table_must_itself_be_a_partition(self):
        table = copy.deepcopy(legs.LEGS)
        table['data']['apps'] += ('org.gnome.Loupe',)
        with self.assertRaisesRegex(ValueError, 'two legs: org.gnome.Loupe'):
            legs.owners(table)
        table = copy.deepcopy(legs.LEGS)
        del table['media']
        with self.assertRaisesRegex(ValueError, 'every native Core domain'):
            legs.owners(table)
        table = copy.deepcopy(legs.LEGS)
        table['web']['domains'] = ('web',)
        table['basic']['domains'] = ('basic', 'autotz')
        with self.assertRaisesRegex(ValueError, 'one GeoClue fixture'):
            legs.owners(table)

    def test_each_leg_declares_what_its_domain_modules_actually_launch(self):
        # A leg's applications are the ones its own host domain modules start;
        # no domain module is left without a leg.
        for leg, spec in legs.LEGS.items():
            source = ''.join((ROOT / 'scripts/lib' / ('core_apps_' + domain + '.py')).read_text()
                             for domain in spec['domains'])
            for app in spec['apps']:
                with self.subTest(leg=leg, app=app):
                    self.assertRegex(source, r'\.start\(\s*["\']' + re.escape(app) + r'["\']')
        modules = {path.stem.removeprefix('core_apps_')
                   for path in (ROOT / 'scripts/lib').glob('core_apps_*.py')} - {'legs'}
        self.assertEqual(modules, set(legs.DOMAINS))


class LegRun(unittest.TestCase):
    """Desktop.run drives only its leg's domains and owes exactly its share."""

    def desktop(self, leg, complete):
        desktop = core_apps.Desktop.__new__(core_apps.Desktop)
        desktop.leg, desktop.assigned, desktop.timezone = leg, None, None
        desktop.metadata = {'commit': COMMIT, 'member_sha256': {'/usr/bin/tuna-compositor': '1' * 64,
                                                                '/usr/bin/tuna-shell-gtk': '4' * 64}}
        desktop._observer_members = {'/usr/libexec/tuna-core-apps/guest.py': '3' * 64}
        desktop._results, desktop._actors, desktop._uncertain = [], {}, None
        desktop.retained, desktop.calls, desktop.ran = [], [], []
        installed = inventory()
        installed |= {'fixture': '/home/tuna-test/tuna-core-apps/x', 'observer_sha256': desktop._observer_members,
                      'installation': {'source_commit': COMMIT, 'observer_sha256': desktop._observer_members,
                                       'after': {row['package']: row['version']
                                                 for row in installed['apps'].values()}},
                      'identity': {'compositor': {'exe': '/usr/bin/tuna-compositor', 'exe_sha256': '1' * 64},
                                   'shell': {'exe': '/usr/bin/tuna-shell-gtk', 'exe_sha256': '4' * 64}},
                      'content_controls': {'web': {'prepared': {'marker': 'm'}, 'endpoint': {'url': 'u'}}}}

        def guest_call(domain, operation, **arguments):
            desktop.calls.append((domain, operation))
            return installed if (domain, operation) == ('desktop', 'prepare') else {'prepared': True}
        desktop.guest = guest_call
        desktop.retain = lambda label, value: desktop.retained.append(label)

        def module(name):
            domain = name.removeprefix('core_apps_')

            def run(current, **options):
                desktop.ran.append(domain)
                if domain == 'autotz':
                    return {'qualified': True}
                for app in complete(domain):
                    current._results.append({'app': {'app_id': app}})
            return SimpleNamespace(run=run)
        self.modules = patch.object(core_apps.importlib, 'import_module', side_effect=module)
        return desktop

    @staticmethod
    def own(domain):
        return next(spec['apps'] for spec in legs.LEGS.values() if spec['domains'][0] == domain)

    def test_each_leg_runs_only_its_domains_and_completes_its_share(self):
        for leg, spec in legs.LEGS.items():
            with self.subTest(leg=leg):
                desktop = self.desktop(leg, self.own)
                with self.modules:
                    result = desktop.run()
                self.assertEqual(desktop.ran, list(spec['domains']))
                self.assertEqual(result['leg'], leg)
                self.assertEqual(result['assigned_apps'], sorted(spec['apps']))
                self.assertEqual([row['app']['app_id'] for row in result['apps']], list(spec['apps']))
                self.assertEqual(result['automatic_timezone'],
                                 {'qualified': True} if leg == 'web' else None)
                # The shared GeoClue source is owned only by the web+timezone boot.
                self.assertEqual(('autotz', 'fixture-prepare') in desktop.calls, leg == 'web')
                self.assertEqual(('autotz', 'restore') in desktop.calls, leg == 'web')
                self.assertEqual(desktop.calls[-1], ('desktop', 'finish'))

    def test_leg_missing_one_of_its_applications_fails(self):
        desktop = self.desktop('system', lambda domain: self.own(domain)[:-1])
        with self.modules, self.assertRaisesRegex(RuntimeError, 'Core matrix incomplete'):
            desktop.run()
        self.assertIn('incomplete-core-apps-matrix', desktop.retained)
        self.assertNotIn('core-apps-component-results', desktop.retained)

    def test_leg_completing_an_application_twice_or_outside_its_share_fails(self):
        for complete in (lambda domain: (*self.own(domain), self.own(domain)[0]),
                         lambda domain: (*self.own(domain), 'org.gnome.Maps')):
            desktop = self.desktop('basic', complete)
            with self.modules, self.assertRaisesRegex(RuntimeError, 'Core matrix incomplete'):
                desktop.run()

    def test_desktop_requires_a_declared_leg(self):
        for leg in (None, 'everything'):
            with self.assertRaisesRegex(ValueError, 'declared native Core leg required'):
                core_apps.Desktop(None, None, '/nonexistent', metadata(), None, leg=leg)
        with self.assertRaises(TypeError):
            core_apps.Desktop(None, None, '/nonexistent', metadata(), None)


class Receipt(unittest.TestCase):
    def test_receipt_binds_identity_and_exact_application_list(self):
        row = leg_receipt('system')
        self.assertTrue(row['passed'])
        self.assertEqual(row['domains'], ['system'])
        self.assertEqual(row['assigned_apps'], sorted(legs.LEGS['system']['apps']))
        self.assertEqual(row['completed_apps'], list(legs.LEGS['system']['apps']))
        self.assertEqual(row['automatic_timezone'], {'required': False, 'qualified': None})
        subject = row['identity']
        self.assertEqual((subject['commit'], subject['image_id'], subject['package_sha256']),
                         (COMMIT, IMAGE, PACKAGE))
        self.assertEqual(set(subject['installed_apps']), set(guest.APP_PACKAGES))
        self.assertEqual(subject['installed_apps']['org.gnome.Loupe'],
                         {'package': 'loupe', 'version': '51.0-1', 'desktop_sha256': '2' * 64})
        self.assertFalse(row['installed_signed_release_qualified'])

    def test_any_failed_lane_assertion_fails_the_leg(self):
        for assertions in ({legs.LEG_ASSERTION: False, 'V-PANEL': True},
                           {legs.LEG_ASSERTION: True, 'V-NOPANIC': False},
                           {'V-PANEL': True}, {}):
            with self.subTest(assertions=assertions):
                self.assertFalse(leg_receipt('basic', assertions=assertions)['passed'])

    def test_failed_boot_without_inventory_still_names_its_identity(self):
        row = legs.receipt('web', metadata(), None, [], None, {legs.LEG_ASSERTION: False})
        self.assertEqual((row['passed'], row['assigned_apps'], row['completed_apps']), (False, None, []))
        self.assertEqual(row['identity']['image_id'], IMAGE)
        self.assertIsNone(row['identity']['installed_apps'])


class Join(unittest.TestCase):
    def failed(self, rows, pattern, **changes):
        verdict = join(rows, **changes)
        self.assertFalse(verdict['passed'])
        self.assertTrue(any(re.search(pattern, error) for error in verdict['errors']), verdict['errors'])
        # No matrix or per-leg id is ever written as pass by a failed join.
        self.assertEqual(ledger_passes(verdict), set())
        self.assertTrue(all(line.split()[1] == 'fail' for line in legs.assertion_lines(verdict)))
        return verdict

    def test_complete_matrix_passes_and_writes_every_gate_id(self):
        verdict = join(all_receipts())
        self.assertEqual(verdict['errors'], [])
        self.assertTrue(verdict['passed'])
        self.assertEqual(verdict['apps'], sorted(guest.APP_PACKAGES))
        self.assertEqual(len(verdict['apps']), 28)
        self.assertEqual(verdict['identity']['image_id'], IMAGE)
        self.assertEqual(ledger_passes(verdict), MATRIX_IDS)
        self.assertFalse(verdict['installed_signed_release_qualified'])
        # Receipt order and a re-run leg's later attempt do not matter.
        rows = list(reversed(all_receipts()))
        rows[0]['run']['run_attempt'] = '2'
        self.assertTrue(join(rows)['passed'])
        # Podman prints the same image with or without its digest prefix.
        self.assertTrue(join(all_receipts(), image_id='b' * 64)['passed'])

    def test_missing_leg_fails(self):
        for absent in legs.LEGS:
            with self.subTest(absent=absent):
                rows = [row for row in all_receipts() if row['leg'] != absent]
                verdict = self.failed(rows, '^' + absent + ': receipt missing$')
                self.assertFalse(verdict['legs'][absent]['present'])
        self.failed([], 'basic: receipt missing')
        self.failed([], 'no installed application inventory')

    def test_failed_leg_fails(self):
        rows = all_receipts(media={'assertions': {legs.LEG_ASSERTION: False, 'V-PANEL': True}})
        verdict = self.failed(rows, '^media: V-CORE-APPS-LEG did not pass$')
        self.assertEqual([leg for leg, row in verdict['legs'].items() if not row['passed']], ['media'])
        rows = all_receipts(data={'assertions': {legs.LEG_ASSERTION: True, 'V-NOPANIC': False}})
        self.failed(rows, '^data: lane assertion V-NOPANIC did not pass$')
        rows = all_receipts()
        rows[0]['passed'] = False
        self.failed(rows, '^basic: leg did not pass$')
        rows = all_receipts(system={'completed': list(legs.LEGS['system']['apps'][:-1])})
        self.failed(rows, 'system: assigned application not completed: org.gnome.baobab')
        self.failed(rows, 'installed application completed by no leg: org.gnome.baobab')

    def test_duplicate_application_across_legs_fails(self):
        rows = all_receipts(data={'completed': [*legs.LEGS['data']['apps'], 'org.gnome.Loupe']})
        self.failed(rows, r'completed by more than one leg: org.gnome.Loupe \(basic, data\)')
        rows = all_receipts(basic={'completed': [*legs.LEGS['basic']['apps'], 'org.gnome.Loupe']})
        self.failed(rows, 'basic: application completed twice: org.gnome.Loupe')

    def test_extra_application_fails(self):
        rows = all_receipts(web={'completed': [*legs.LEGS['web']['apps'], 'org.gnome.Unknown']})
        self.failed(rows, 'not in the installed inventory: org.gnome.Unknown')
        self.failed(rows, 'web: completed application outside the assignment: org.gnome.Unknown')
        # Every leg agrees on an inventory holding an application no leg owns.
        installed = inventory(guest.APP_PACKAGES | {'org.gnome.Unknown': 'unknown'})
        rows = all_receipts(**{leg: {'installed': installed} for leg in legs.LEGS})
        self.failed(rows, 'installed application has no native Core leg: org.gnome.Unknown')
        installed = inventory({app: package for app, package in guest.APP_PACKAGES.items()
                               if app != 'org.gnome.Tour'})
        rows = all_receipts(**{leg: {'installed': installed} for leg in legs.LEGS})
        self.failed(rows, 'declared native Core application is not installed: org.gnome.Tour')

    def test_identity_mismatch_fails(self):
        for field, value in (('image_id', 'sha256:' + '9' * 64), ('commit', '9' * 40),
                             ('package_sha256', '9' * 64), ('base_image', 'other@sha256:' + '9' * 64),
                             ('member_sha256', {'/usr/bin/tuna-compositor': '9' * 64}),
                             ('built_tree', '9' * 40)):
            with self.subTest(field=field):
                rows = all_receipts(media={'source': metadata(**{field: value})})
                self.failed(rows, 'identity differs between legs basic and media: ' + field)
        changed = inventory()
        changed['apps']['org.gnome.Music']['version'] = '51.1-1'
        self.failed(all_receipts(media={'installed': changed}),
                    'identity differs between legs basic and media: installed_apps')
        changed = inventory()
        changed['installation']['after'] = {'glibc': '2.43'}
        self.failed(all_receipts(web={'installed': changed}),
                    'identity differs between legs basic and web: installation_sha256')
        changed = inventory()
        changed['observer_sha256'] = {'/usr/libexec/tuna-core-apps/guest.py': '9' * 64}
        self.failed(all_receipts(data={'installed': changed}),
                    'identity differs between legs basic and data: observer_sha256')

    def test_legs_must_run_on_what_this_run_admitted(self):
        self.failed(all_receipts(), 'different commit than this run admitted', commit='9' * 40)
        self.failed(all_receipts(), 'different image_id than this run admitted', image_id='sha256:' + '9' * 64)
        self.failed(all_receipts(), 'different package_sha256', package_sha256='9' * 64)
        # The image job failed or never ran: its outputs are empty.
        self.failed(all_receipts(), 'different image_id than this run admitted', image_id='')
        self.failed(all_receipts(), 'different package_sha256', package_sha256='')
        self.failed(all_receipts(media={'run_id': '4241'}), '^media: receipt belongs to another run$')
        self.failed(all_receipts(), 'receipt belongs to another run', run_id='')
        rows = all_receipts()
        rows[2]['identity']['commit'] = None
        self.failed(rows, 'media: source, package, image or installed-inventory identity incomplete')

    def test_absent_skipped_cancelled_or_failed_jobs_fail(self):
        for result in ('failure', 'cancelled', 'skipped', ''):
            for job in ('core-apps-image', 'core-apps-leg'):
                with self.subTest(job=job, result=result):
                    jobs = {'core-apps-image': 'success', 'core-apps-leg': 'success'} | {job: result}
                    self.failed(all_receipts(), 'job ' + job + ' result is ' + (result or 'absent'), jobs=jobs)

    def test_malformed_repeated_and_undeclared_receipts_fail(self):
        self.failed([*all_receipts(), leg_receipt('basic')], 'more than one receipt for leg basic')
        self.failed([*all_receipts(), None], 'malformed leg receipt')
        self.failed([*all_receipts(), {'schema': 2, 'kind': legs.RECEIPT_KIND, 'leg': 'basic'}],
                    'malformed leg receipt')
        extra = leg_receipt('basic') | {'leg': 'everything'}
        self.failed([*all_receipts(), extra], 'receipt for an undeclared leg: everything')
        rows = all_receipts()
        rows[1]['installed_signed_release_qualified'] = True
        self.failed(rows, 'data: receipt claims signed-release qualification')
        rows = all_receipts()
        rows[1]['assigned_apps'] = rows[0]['assigned_apps']
        rows[1]['completed_apps'] = rows[0]['completed_apps']
        self.failed(rows, 'data: assignment differs from the partition of the installed inventory')

    def test_automatic_timezone_is_owed_by_exactly_its_leg(self):
        self.failed(all_receipts(web={'timezone': None}), 'web: automatic timezone not qualified by its leg')
        self.failed(all_receipts(web={'timezone': {'qualified': False}}),
                    'web: automatic timezone not qualified by its leg')
        self.failed(all_receipts(basic={'timezone': {'qualified': True}}),
                    'basic: automatic timezone claimed outside its leg')

    def test_failed_join_reports_each_leg_without_passing_it(self):
        verdict = join([row for row in all_receipts() if row['leg'] != 'web'])
        lines = {line.split()[0]: line for line in legs.assertion_lines(verdict)}
        self.assertEqual(set(lines), MATRIX_IDS)
        self.assertRegex(lines['V-CORE-APPS-BASIC'], r'^V-CORE-APPS-BASIC fail 5 applications: .*join failed')
        self.assertRegex(lines['V-CORE-APPS-WEB'], r'^V-CORE-APPS-WEB fail leg failed: receipt missing')
        self.assertRegex(lines['V-CORE-APPS-AUTOTZ'], r'^V-CORE-APPS-AUTOTZ fail leg web failed')
        self.assertRegex(lines['V-CORE-APPS'], r'^V-CORE-APPS fail join failed: web: receipt missing')


class JoinCommand(unittest.TestCase):
    def run_join(self, rows, *extra, image=IMAGE):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            for index, row in enumerate(rows):
                directory = root / 'legs' / ('native-core-apps-receipt-%d' % index)
                directory.mkdir(parents=True)
                (directory / legs.RECEIPT_NAME).write_text(row if isinstance(row, str) else json.dumps(row))
            argv = ['join', '--receipts', str(root / 'legs'), '--out', str(root / 'out'),
                    '--commit', COMMIT, '--image-id', image, '--package-sha256', PACKAGE, '--run-id', RUN,
                    '--job', 'core-apps-image=success', *extra]
            with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                code = legs.main(argv)
            verdict = json.loads((root / 'out/join.json').read_text())
            return code, verdict, ledger.load_assertions([str(root / 'out/assertions.txt')])

    def test_command_writes_the_verdict_and_assertions_the_ledger_reads(self):
        code, verdict, passed = self.run_join(all_receipts(), '--job', 'core-apps-leg=success')
        self.assertEqual((code, verdict['passed'], passed), (0, True, MATRIX_IDS))

    def test_command_fails_closed(self):
        for rows, extra, image in (
                (all_receipts()[:-1], ['--job', 'core-apps-leg=success'], IMAGE),
                ([], ['--job', 'core-apps-leg=skipped'], IMAGE),
                (all_receipts(), ['--job', 'core-apps-leg=failure'], IMAGE),
                (all_receipts(), ['--job', 'core-apps-leg='], IMAGE),
                (all_receipts(), ['--job', 'core-apps-leg=success'], ''),
                ([*all_receipts()[:-1], '{truncated'], ['--job', 'core-apps-leg=success'], IMAGE)):
            with self.subTest(extra=extra, receipts=len(rows), image=image):
                code, verdict, passed = self.run_join(rows, *extra, image=image)
                self.assertEqual((code, verdict['passed'], passed), (1, False, set()))


class Workflow(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import yaml
        cls.jobs = yaml.safe_load((ROOT / '.github/workflows/ci.yml').read_text())['jobs']

    @staticmethod
    def step(job, name):
        return next(step for step in job['steps'] if step.get('name') == name)

    def test_one_leg_job_per_declared_leg_inside_the_unchanged_job_limit(self):
        leg = self.jobs['core-apps-leg']
        self.assertEqual(leg['strategy']['matrix'], {'leg': list(legs.LEGS)})
        self.assertIs(leg['strategy']['fail-fast'], False)
        self.assertEqual(leg['timeout-minutes'], 60)
        self.assertEqual(self.jobs['marlin-vm']['timeout-minutes'], 60)
        run = self.step(leg, "Complete this leg's original native Core applications")
        self.assertEqual(run['env'], {'CORE_LEG': '${{ matrix.leg }}'})
        self.assertIn('--profile core-apps --core-leg "$CORE_LEG" --require-kvm --boots 1', run['run'])
        self.assertIn('--strip-image vm-lane-artifacts/strip-image.json', run['run'])

    def test_every_leg_boots_the_one_image_admitted_exactly_as_the_lifecycle_lane(self):
        image, leg, vm = (self.jobs[name] for name in ('core-apps-image', 'core-apps-leg', 'marlin-vm'))
        # The image is built once; a leg loads it and never builds or pulls.
        self.assertFalse(any('podman build' in step.get('run', '') or 'podman pull' in step.get('run', '')
                             for step in leg['steps']))
        load = self.step(leg, 'Load the one admitted image and require its exact identity')['run']
        self.assertIn('expected=$(cat /mnt/vm/core-image/image-id)', load)
        self.assertIn('test "$(scripts/tuna-vm-strip-image --canonical-image-id "$loaded")" = "$expected"', load)
        self.assertIn('cp /mnt/vm/core-image/base-digest /mnt/vm/base-digest', load)
        bind = 'Bind installed acceptance to the original package and immutable final image'
        install = 'Install the image to a raw disk with bootc'
        for job in (image, leg):
            self.assertEqual(self.step(job, bind)['run'], self.step(vm, bind)['run'])
            self.assertEqual(self.step(job, bind)['env'], {'IMAGE_FIXTURE': 'core-apps'})
        self.assertEqual(self.step(leg, install)['run'], self.step(vm, install)['run'])
        self.assertEqual(self.step(leg, 'Enable KVM')['run'], self.step(vm, 'Enable KVM')['run'])
        for name in ('Build the Marlin Tuna Desktop image and the VM test layer',
                     'Overlay the reference GDM 51 on the final fixture image',
                     'Inspect the actual final native image without rebuilding it'):
            self.assertEqual(self.step(image, name)['run'], self.step(vm, name)['run'])
        names = [step.get('name') for step in image['steps']]
        self.assertLess(names.index('Add the native Core application layer'),
                        names.index('Overlay the reference GDM 51 on the final fixture image'))
        self.assertEqual(image['outputs'], {'image_id': '${{ steps.image.outputs.image_id }}',
                                            'package_sha256': '${{ steps.image.outputs.package_sha256 }}'})
        # The ordinary lifecycle matrix no longer carries a Core member.
        selector = next(step for step in self.jobs['select-lanes']['steps'] if step.get('id') == 'lanes')['run']
        self.assertNotIn('core-apps', selector.split('primary=', 1)[1])
        self.assertFalse(any('core-apps' in str(step.get('if', '')) for step in vm['steps']))

    def test_whole_consumer_is_selected_together(self):
        selected = "needs.select-lanes.outputs.core_apps == 'true'"
        self.assertEqual(self.jobs['core-apps-image']['if'], selected)
        self.assertEqual(self.jobs['core-apps-leg']['if'], selected)
        self.assertEqual(self.jobs['native-core-apps']['if'], '${{ always() && ' + selected + ' }}')
        self.assertEqual(self.jobs['select-lanes']['outputs']['core_apps'],
                         '${{ steps.lanes.outputs.core_apps }}')
        self.assertIn('core-apps-image', self.jobs['core-apps-leg']['needs'])

    def test_join_runs_after_any_leg_outcome_and_receives_every_result(self):
        job = self.jobs['native-core-apps']
        self.assertEqual(set(job['needs']), {'select-lanes', 'core-apps-image', 'core-apps-leg'})
        step = self.step(job, 'Join every leg into the complete native Core verdict')
        self.assertEqual(step['if'], 'always()')
        self.assertEqual(step['env'], {
            'IMAGE_ID': '${{ needs.core-apps-image.outputs.image_id }}',
            'PACKAGE_SHA256': '${{ needs.core-apps-image.outputs.package_sha256 }}',
            'IMAGE_RESULT': '${{ needs.core-apps-image.result }}',
            'LEGS_RESULT': '${{ needs.core-apps-leg.result }}'})
        for argument in ('core_apps_legs.py join', '--commit "$GITHUB_SHA"', '--image-id "$IMAGE_ID"',
                         '--package-sha256 "$PACKAGE_SHA256"', '--run-id "$GITHUB_RUN_ID"',
                         '--job "core-apps-image=$IMAGE_RESULT"', '--job "core-apps-leg=$LEGS_RESULT"'):
            self.assertIn(argument, step['run'])
        upload = job['steps'][-1]
        self.assertEqual((upload['if'], upload['with']['name']), ('always()', 'native-core-apps'))
        receipts = [step for step in self.jobs['core-apps-leg']['steps']
                    if step.get('with', {}).get('name') == 'native-core-apps-receipt-${{ matrix.leg }}']
        self.assertEqual([(step['if'], step['with']['path']) for step in receipts],
                         [('always()', 'vm-lane-artifacts/' + legs.RECEIPT_NAME)])
        download = next(step['with'] for step in job['steps'] if 'pattern' in step.get('with', {}))
        self.assertEqual(download['pattern'], 'native-core-apps-receipt-*')

    def test_ledger_reads_only_the_join_and_requires_it_when_selected(self):
        job = self.jobs['parity-ledger']
        self.assertIn('native-core-apps', job['needs'])
        self.assertIn("needs.native-core-apps.result == (needs.select-lanes.outputs.core_apps == 'true' "
                      "&& 'success' || 'skipped')", job['if'])
        for producer in job['needs']:
            self.assertIn('needs.' + producer + '.result', job['if'])
        check = next(step['run'] for step in job['steps'] if 'tuna-ledger check' in step.get('run', ''))
        self.assertIn('core=(--assertions ledger-in/native-core-apps/assertions.txt)', check)
        self.assertNotIn('receipt', check)

    @unittest.skipUnless(shutil.which('jq'), 'the gate expression runs under jq')
    def test_gate_rejects_a_selected_consumer_whose_join_did_not_succeed(self):
        gate = self.jobs['ci-gate']
        self.assertEqual(gate['if'], 'always()')
        expression = re.search(r"jq -e '([^']+)'", gate['steps'][0]['run']).group(1)

        def passes(core, join_result, **others):
            needs = {name: {'result': 'success', 'outputs': {}} for name in gate['needs']}
            needs['select-lanes']['outputs'] = {'core_apps': core}
            needs['native-core-apps']['result'] = join_result
            for name, result in others.items():
                needs[name.replace('_', '-')]['result'] = result
            return subprocess.run(['jq', '-e', expression], input=json.dumps(needs), text=True,
                                  capture_output=True).returncode == 0
        self.assertTrue(passes('true', 'success'))
        self.assertTrue(passes('false', 'skipped', core_apps_image='skipped', core_apps_leg='skipped'))
        for result in ('skipped', 'failure', 'cancelled'):
            self.assertFalse(passes('true', result), result)
        self.assertFalse(passes('true', 'success', core_apps_leg='failure'))
        self.assertFalse(passes('true', 'success', core_apps_image='cancelled'))


if __name__ == '__main__':
    unittest.main()
