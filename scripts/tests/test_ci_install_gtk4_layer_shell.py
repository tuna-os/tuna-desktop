#!/usr/bin/env python3
"""Keep the CI gtk4-layer-shell checkout tied to one upstream revision."""
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
INSTALLER = (ROOT / "scripts/ci-install-gtk4-layer-shell").read_text()
REVISION = "4867d7b85cdf1e829fc1fd6f1d5f04c42cc99389"


class Gtk4LayerShellInstallerContracts(unittest.TestCase):
    def test_checkout_is_verified_before_build(self):
        self.assertIn(f"REVISION='{REVISION}'", INSTALLER)
        self.assertNotIn('VERSION="${1:-v1.1.1}"', INSTALLER)
        clone = INSTALLER.index("git clone")
        verify = INSTALLER.index('actual="$(git -C "$work/src" rev-parse HEAD)"')
        reject = INSTALLER.index('if [ "$actual" != "$REVISION" ]')
        build = INSTALLER.index('meson setup "$work/build"')
        self.assertLess(clone, verify)
        self.assertLess(verify, reject)
        self.assertLess(reject, build)
        self.assertIn('exit 1', INSTALLER[reject:build])

    def test_build_and_install_are_after_the_integrity_gate(self):
        gate_end = INSTALLER.index('meson setup')
        self.assertGreater(INSTALLER.index('ninja -C'), gate_end)
        self.assertGreater(INSTALLER.index('sudo ninja -C'), gate_end)


if __name__ == "__main__":
    unittest.main()
