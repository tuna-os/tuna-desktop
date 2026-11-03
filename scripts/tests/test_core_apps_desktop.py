"""Host Files-cell matching regressions; no VM qualification."""
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts' / 'lib'))
import core_apps


class OpenDefaultCellSpellings(unittest.TestCase):
    def desktop(self, nodes):
        tree = {'nodes': nodes}
        calls = {}
        desktop = core_apps.Desktop.__new__(core_apps.Desktop)
        desktop._actors = {}
        desktop.guest = lambda domain, operation, **k: (
            {'pid': 9} if operation == 'discover'
            else {'default_handler': 'org.gnome.font-viewer.desktop'})
        desktop.start = lambda *a: {'pid': 1}
        desktop.keys = lambda *a: None
        desktop.type_ascii = lambda *a: None
        desktop.tree = lambda actor: tree

        def click_path(actor, path):
            calls['click'] = path
            for node in nodes:
                if node['path'] == path:
                    node['selected'] = True
        desktop.click_path = click_path
        desktop.observe = lambda *a: {}

        def await_state(label, read, predicate, timeout=10):
            value = read()
            if predicate(value):
                return value
            raise TimeoutError('not ready: ' + label)
        desktop.await_state = await_state
        return desktop, calls

    def test_suffixed_file_cell_opens(self):
        nodes = [{'showing': True, 'name': 'NotoSansCJKjp-Regular.otf. File',
                  'role': 'table cell', 'path': ['row', 1], 'selected': False}]
        desktop, calls = self.desktop(nodes)
        result = desktop.open_default('/fixture/NotoSansCJKjp-Regular.otf')
        self.assertEqual(calls['click'], ['row', 1])
        self.assertEqual(result['actor']['pid'], 9)

    def test_bare_cell_name_still_opens(self):
        nodes = [{'showing': True, 'name': 'seed.txt', 'role': 'list item',
                  'path': ['row', 0], 'selected': False}]
        desktop, calls = self.desktop(nodes)
        desktop.open_default('/fixture/seed.txt')
        self.assertEqual(calls['click'], ['row', 0])

    def test_suffixed_folder_cell_opens(self):
        nodes = [{'showing': True, 'name': 'web. Folder', 'role': 'table cell',
                  'path': ['row', 2], 'selected': False}]
        desktop, calls = self.desktop(nodes)
        desktop.open_default('/fixture/web')
        self.assertEqual(calls['click'], ['row', 2])


if __name__ == '__main__':
    unittest.main()
