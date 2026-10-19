#!/usr/bin/env python3
"""Negative controls and shipped metadata/startup contracts; no GUI required."""
import importlib.util
from pathlib import Path
import unittest
import tempfile

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('proof', ROOT / 'scripts/lib/gtk-privacy-portal-proof.py')
proof = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proof)


class PortalProofTests(unittest.TestCase):
    def test_only_real_request_handle_is_admission(self):
        self.assertEqual(proof.classify_camera(('/org/freedesktop/portal/desktop/request/1/token',), None),
                         'request-admitted')
        for result in (None, (), (1, '/bogus'), ('/bogus',), (True,)):
            with self.assertRaises(RuntimeError):
                proof.classify_camera(result, None)

    def test_timeout_missing_interface_and_generic_denial_are_failures(self):
        for error in ('Timeout', 'UnknownMethod', 'Permission denied',
                      'NotAllowed: camera unavailable', 'Camera access disabled',
                      'org.other.Error.NotAllowed: Camera access disabled',
                      'org.freedesktop.portal.Error.NotAllowed: Camera access disabled unexpectedly'):
            with self.assertRaises(RuntimeError):
                proof.classify_camera(None, error)
        self.assertEqual(proof.classify_camera(None,
                         'org.freedesktop.portal.Error.NotAllowed: Camera access disabled'),
                         'lockdown-denied')
        self.assertEqual(proof.classify_camera(None,
                         'g-dbus-error-quark: GDBus.Error:org.freedesktop.portal.Error.NotAllowed: Camera access disabled (19)'),
                         'lockdown-denied')

    def test_publisher_is_bound_to_expected_binary_and_digest(self):
        with tempfile.TemporaryDirectory() as directory:
            shell = Path(directory) / 'tuna-shell-gtk'
            shell.write_bytes(b'fixture binary bytes, not executed')
            identity = {'sha256': proof.digest(shell)}
            self.assertEqual(proof.validate_publisher(shell, shell, identity), shell.resolve())
            impostor = Path(directory) / 'other/tuna-shell-gtk'
            impostor.parent.mkdir()
            impostor.write_bytes(shell.read_bytes())
            for executable, digest in ((impostor, identity),
                                       (shell, {'sha256': '0' * 64})):
                with self.assertRaises(RuntimeError):
                    proof.validate_publisher(executable, shell, digest)
            host = Path(directory) / 'tuna-shell-host'
            host.write_bytes(shell.read_bytes())
            with self.assertRaises(RuntimeError):
                proof.validate_publisher(host, host, identity)

    def test_only_access_is_overridden(self):
        portal = (ROOT / 'share/xdg-desktop-portal/portals/tuna.portal').read_text()
        config = (ROOT / 'packaging/marlin/tuna-portals.conf').read_text()
        fixture = proof.metadata(portal, config)
        self.assertEqual(fixture.replace('Access=proof-access;', 'Access=gtk;'), config)
        for bad_portal, bad_config in ((portal.replace('Lockdown', 'Camera'), config),
                                       (portal, config.replace('Lockdown=tuna', 'Lockdown=gnome')),
                                       (portal, config.replace('Access=gtk', 'Access=tuna'))):
            with self.assertRaises(RuntimeError):
                proof.metadata(bad_portal, bad_config)

    def test_all_package_routes_install_exact_metadata(self):
        for name in ('packaging/arch/PKGBUILD', 'scripts/tuna-release'):
            body = (ROOT / name).read_text()
            self.assertIn('share/xdg-desktop-portal/portals/tuna.portal', body)
            self.assertIn('packaging/marlin/tuna-portals.conf', body)
        image = (ROOT / 'packaging/marlin/Containerfile').read_text()
        self.assertNotIn('COPY packaging/marlin/tuna-portals.conf', image)
        self.assertIn('pacman -Qqo /usr/share/xdg-desktop-portal/tuna-portals.conf', image)
        self.assertIn('pacman -Qqo /usr/share/xdg-desktop-portal/portals/tuna.portal', image)

    def test_gtk_publisher_starts_once_and_seeds_before_name_acquisition(self):
        main = (ROOT / 'crates/shell-gtk/src/main.rs').read_text()
        self.assertEqual(main.count('privacy_portal::start'), 1)
        self.assertLess(main.index('privacy_portal::start'), main.index('app.connect_activate(build)'))
        worker = (ROOT / 'crates/shell-gtk/src/privacy_portal.rs').read_text()
        self.assertLess(worker.index('publisher.poll(&GioBackend)'), worker.index('publisher.ensure()'))
        self.assertIn('connect_shutdown', worker)
        self.assertIn('with_thread_default', worker)


if __name__ == '__main__':
    unittest.main()
