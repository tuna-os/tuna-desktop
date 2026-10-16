#!/usr/bin/env python3
"""Deadline discipline + strict paint-receipt contract for the stock
XML slideshow proof (360).

wait_for/budget nest by tightening (a nested wait can never extend
its parent), slow-true predicates fail instead of passing late, and
every accepted paint record is typed, ranged and provenance-bound:
exact source URI, generation epoch prefix, live stat cross-check of
parsed FileIdentity dev/ino/size/mtime/ctime, duplicate-free source
references, and the exact 1280x800@1.0 producer viewport.

Runs without a compositor/gi/pyatspi: the pure functions are
AST-extracted from the proof script into a stub namespace.
"""
import ast
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROOF = ROOT / 'scripts' / 'lib' / 'tuna-stock-slideshow-proof.py'

WANT_FUNCS = {'wait_for', 'budget', 'check_paint', 'check_file_identity',
              'is_plain_int'}
WANT_GLOBALS = ('_ACTIVE_END', '_FILE_IDENTITY', 'SHOT_W', 'SHOT_H')


def load_proof(tmp):
    tree = ast.parse(PROOF.read_text())

    def keep(node):
        if isinstance(node, ast.FunctionDef):
            return node.name in WANT_FUNCS
        if isinstance(node, ast.Assign):
            names = set()
            for target in node.targets:
                if isinstance(target, ast.Name):
                    names.add(target.id)
                elif isinstance(target, ast.Tuple):
                    names.update(e.id for e in target.elts
                                 if isinstance(e, ast.Name))
            return bool(names & set(WANT_GLOBALS))
        return False

    code = compile(ast.Module(body=[n for n in tree.body if keep(n)],
                              type_ignores=[]), str(PROOF), 'exec')
    out = Path(tmp)
    ns = {'time': time, 'math': __import__('math'), 're': __import__('re'),
          'json': __import__('json'), 'subprocess': __import__('subprocess'),
          'Path': Path, 'OUT': out,
          'XML': {'light': out / 'light-timeline.xml',
                  'dark': out / 'dark-timeline.xml'},
          'COLORS': {'light-a': (240, 0, 0), 'light-b': (0, 0, 240)},
          'drain': lambda: None, 'PHASE': 'test'}
    exec(code, ns)  # noqa: S102 - local test fixture by design
    return ns


def identity_of(path):
    st = Path(path).stat()
    mt, ct = st.st_mtime_ns, st.st_ctime_ns
    return (f'Some(FileIdentity {{ path: "{path}", dev: {st.st_dev}, '
            f'ino: {st.st_ino}, bytes: {st.st_size}, '
            f'modified: ({mt // 1_000_000_000}, {mt % 1_000_000_000}), '
            f'changed: ({ct // 1_000_000_000}, {ct % 1_000_000_000}) }})')


def stale_identity_of(path):
    st = Path(path).stat()
    return (f'Some(FileIdentity {{ path: "{path}", dev: {st.st_dev}, '
            f'ino: {st.st_ino}, bytes: {st.st_size}, '
            f'modified: (1, 2), changed: (3, 4) }})')


class DeadlineDiscipline(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.mkdtemp(prefix='w360-proof-ci-')
        cls.ns = load_proof(cls.tmp)

    def tearDown(self):
        self.ns['_ACTIVE_END'] = None

    def test_slow_true_fails_instead_of_passing_late(self):
        calls = []

        def slow_true():
            calls.append(1)
            if len(calls) == 1:
                time.sleep(0.25)
            return True

        with self.assertRaises(RuntimeError):
            self.ns['wait_for'](slow_true, 'late', seconds=0.1)

    def test_immediate_true_passes(self):
        self.ns['wait_for'](lambda: True, 'now', seconds=5)

    def test_never_true_raises(self):
        with self.assertRaises(RuntimeError):
            self.ns['wait_for'](lambda: False, 'never', seconds=0.15)

    def test_nested_wait_tightens_parent(self):
        self.ns['_ACTIVE_END'] = time.monotonic() + 0.2
        seen = {}

        def outer_pred():
            seen['before'] = self.ns['_ACTIVE_END']
            with self.assertRaises(RuntimeError):
                self.ns['wait_for'](lambda: False, 'inner', seconds=30)
            seen['raised'] = True
            seen['after'] = self.ns['_ACTIVE_END']
            return True

        with self.assertRaises(RuntimeError):
            self.ns['wait_for'](outer_pred, 'outer', seconds=30)
        self.assertTrue(seen['raised'])
        self.assertEqual(seen['before'], seen['after'])

    def test_nested_wait_restores_parent(self):
        parent = time.monotonic() + 30
        self.ns['_ACTIVE_END'] = parent
        self.ns['wait_for'](lambda: True, 'r', seconds=1)
        self.assertEqual(self.ns['_ACTIVE_END'], parent)

    def test_budget_default_cap(self):
        self.assertEqual(self.ns['budget']('x'), 10)
        self.assertEqual(self.ns['budget']('x', cap=3), 3)

    def test_budget_honors_active_deadline(self):
        self.ns['_ACTIVE_END'] = time.monotonic() + 30
        remaining = self.ns['budget']('y', cap=10)
        self.assertGreater(remaining, 0)
        self.assertLessEqual(remaining, 10)

    def test_budget_expired_raises(self):
        self.ns['_ACTIVE_END'] = time.monotonic() - 1
        with self.assertRaises(RuntimeError):
            self.ns['budget']('z')


class PaintReceipt(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.mkdtemp(prefix='w360-paint-ci-')
        cls.ns = load_proof(cls.tmp)
        out = Path(cls.tmp)
        cls.out = out
        cls.xml = out / 'light-timeline.xml'
        cls.xml.write_bytes(b'<background/>')
        cls.frames = {}
        for stem in ('light-a', 'light-b'):
            path = out / (stem + '.png')
            path.write_bytes(b'P' * (100 + len(stem)))
            cls.frames[stem] = str(path)

    def good_paint(self, fixed=True):
        frm, to = self.frames['light-a'], self.frames['light-b']
        uri = self.xml.as_uri()
        return {'sample_wall': time.time(),
                'progress': 0.0 if fixed else 0.5,
                'interval': 4.0, 'from': frm,
                'to': None if fixed else to, 'slide': 0,
                'generation': uri + '\nPictureSettings\nGeometry\nabc123',
                'source_uri': uri,
                'root_identity': identity_of(self.xml),
                'source_identities': [
                    {'path': frm, 'identity': identity_of(frm)},
                    {'path': to, 'identity': identity_of(to)}],
                'geometry': {'physical': [1280, 800], 'origin': [0, 0],
                             'desktop': [0, 0, 1280, 800], 'scale': 1.0}}

    def check(self, paint, fixed=True, first=None, last=None):
        first = first if first is not None else self.frames['light-a']
        self.ns['check_paint'](paint, 'light', fixed, first, last)

    def mutate(self, **kw):
        paint = self.good_paint()
        paint.update(kw)
        return paint

    def test_valid_static_and_blend(self):
        self.check(self.good_paint(), fixed=True)
        blend = self.good_paint(fixed=False)
        self.check(blend, fixed=False, last=self.frames['light-b'])

    def test_non_dict_and_missing_key(self):
        with self.assertRaises(RuntimeError):
            self.check([], fixed=True)
        paint = self.good_paint()
        del paint['interval']
        with self.assertRaises(RuntimeError):
            self.check(paint, fixed=True)

    def test_wall_progress_interval_slide_types(self):
        for key, bads in (
                ('sample_wall', (0.0, -1.0, float('nan'), float('inf'), 'x', True)),
                ('progress', (-0.1, 1.1, float('nan'), 'x', True)),
                ('interval', (0, -4.0, 1e12, float('nan'), 'x', True)),
                ('slide', (-1, 1.5, 'x', True))):
            for bad in bads:
                with self.assertRaises(RuntimeError, msg=f'{key}={bad!r:.20}'):
                    self.check(self.mutate(**{key: bad}), fixed=True)

    def test_source_and_generation_binding(self):
        with self.assertRaises(RuntimeError):
            self.check(self.mutate(source_uri='file:///evil.xml'), fixed=True)
        with self.assertRaises(RuntimeError):
            self.check(self.mutate(generation='file:///evil.xml\n…'), fixed=True)
        with self.assertRaises(RuntimeError):
            self.check(self.mutate(generation=None), fixed=True)

    def test_frame_paths_and_reference_agreement(self):
        with self.assertRaises(RuntimeError):  # static must not blend
            self.check(self.mutate(to=self.frames['light-b']), fixed=True)
        with self.assertRaises(RuntimeError):  # outside OUT
            self.check(self.mutate(**{'from': '/etc/hostname'}), fixed=True)
        missing = str(Path(self.tmp) / 'nope.png')
        with self.assertRaises(RuntimeError):
            self.check(self.mutate(**{'from': missing}), fixed=True)
        with self.assertRaises(RuntimeError):  # diverges from reference
            self.check(self.good_paint(), fixed=True,
                       first=self.frames['light-b'])

    def test_root_identity_bound_to_expected_xml(self):
        other = Path(self.tmp) / 'other-timeline.xml'
        other.write_bytes(b'<background/>')
        paint = self.good_paint()
        paint['root_identity'] = identity_of(other)
        with self.assertRaises(RuntimeError):
            self.check(paint, fixed=True)
        paint = self.good_paint()
        paint['root_identity'] = 'None'
        with self.assertRaises(RuntimeError):
            self.check(paint, fixed=True)

    def test_stale_timestamps_rejected(self):
        paint = self.good_paint()
        paint['root_identity'] = stale_identity_of(self.xml)
        with self.assertRaises(RuntimeError):
            self.check(paint, fixed=True)
        frm = self.frames['light-a']
        paint = self.good_paint()
        paint['source_identities'] = [
            {'path': frm, 'identity': stale_identity_of(frm)},
            {'path': self.frames['light-b'],
             'identity': identity_of(self.frames['light-b'])}]
        with self.assertRaises(RuntimeError):
            self.check(paint, fixed=True)
        paint = self.good_paint()
        tampered = identity_of(self.frames['light-a']).replace(
            f'ino: {Path(self.frames["light-a"]).stat().st_ino}',
            'ino: 999999999')
        paint['source_identities'] = [
            {'path': frm, 'identity': tampered},
            {'path': self.frames['light-b'],
             'identity': identity_of(self.frames['light-b'])}]
        with self.assertRaises(RuntimeError):
            self.check(paint, fixed=True)

    def test_reference_list_shape_and_duplicates(self):
        with self.assertRaises(RuntimeError):
            self.check(self.mutate(source_identities=[]), fixed=True)
        frm = self.frames['light-a']
        with self.assertRaises(RuntimeError):
            self.check(self.mutate(source_identities=[{'path': frm}]),
                       fixed=True)
        paint = self.good_paint()
        paint['source_identities'] = [
            {'path': frm, 'identity': identity_of(frm)},
            {'path': frm, 'identity': identity_of(frm)}]
        with self.assertRaises(RuntimeError):
            self.check(paint, fixed=True)
        paint = self.good_paint()
        paint['source_identities'] = [
            {'path': self.frames['light-b'],
             'identity': identity_of(self.frames['light-b'])}]
        with self.assertRaises(RuntimeError):
            self.check(paint, fixed=True)
        paint = self.good_paint()
        paint['source_identities'] = [{'path': self.frames['light-b'],
                                       'identity': identity_of(frm)}]
        with self.assertRaises(RuntimeError):
            self.check(paint, fixed=True)

    def test_geometry_exact_producer_match(self):
        for bad in ({'physical': [1280, 800], 'origin': [10, 0],
                     'desktop': [0, 0, 1280, 800], 'scale': 1.0},
                    {'physical': [1280, 800], 'origin': [0, 0],
                     'desktop': [0, 0, 1280, 800], 'scale': 2.0},
                    {'physical': [1280, 800], 'origin': [0, 0],
                     'desktop': [0, 0, 640, 400], 'scale': 1.0},
                    {'physical': [1280, 800], 'origin': [0, 0],
                     'desktop': [0, 0, 0, 800], 'scale': 1.0}):
            with self.assertRaises(RuntimeError, msg=bad):
                self.check(self.mutate(geometry=bad), fixed=True)

    def test_geometry_rejects_bool_and_misshaped_numbers(self):
        # True == 1.0 and [False, False] == [0, 0] in Python: bare
        # equality would admit typed-wrong geometry, so the
        # validator types first and compares second.
        good = {'physical': [1280, 800], 'origin': [0, 0],
                'desktop': [0, 0, 1280, 800], 'scale': 1.0}
        bads = dict(good)
        bads['scale'] = True
        with self.assertRaises(RuntimeError, msg='bool scale'):
            self.check(self.mutate(geometry=dict(bads)), fixed=True)
        bads = dict(good)
        bads['origin'] = [False, False]
        with self.assertRaises(RuntimeError, msg='bool origin'):
            self.check(self.mutate(geometry=dict(bads)), fixed=True)
        bads = dict(good)
        bads['physical'] = [1280.0, 800]
        with self.assertRaises(RuntimeError, msg='float physical'):
            self.check(self.mutate(geometry=dict(bads)), fixed=True)
        bads = dict(good)
        bads['desktop'] = [0, 0, 1280]
        with self.assertRaises(RuntimeError, msg='short desktop'):
            self.check(self.mutate(geometry=dict(bads)), fixed=True)
        bads = dict(good)
        bads['desktop'] = [0, 0, 1280, 800, 0]
        with self.assertRaises(RuntimeError, msg='long desktop'):
            self.check(self.mutate(geometry=dict(bads)), fixed=True)
        bads = dict(good)
        bads['scale'] = float('nan')
        with self.assertRaises(RuntimeError, msg='nan scale'):
            self.check(self.mutate(geometry=dict(bads)), fixed=True)
        bads = dict(good)
        bads['scale'] = '1.0'
        with self.assertRaises(RuntimeError, msg='string scale'):
            self.check(self.mutate(geometry=dict(bads)), fixed=True)
        with self.assertRaises(RuntimeError, msg='non-dict geometry'):
            self.check(self.mutate(geometry=[1280, 800]), fixed=True)


if __name__ == '__main__':
    unittest.main()
