#!/usr/bin/python3
"""Source admission only; does not claim GTK callbacks were executed."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]

class NotifyCallbackContract(unittest.TestCase):
    def test_production_constructor_keeps_persisted_layer_surface(self):
        source = (ROOT / 'crates/shell-gtk/src/notify.rs').read_text()
        constructor = source.split('pub fn new(app:', 1)[1].split('// Tests use', 1)[0]
        self.assertIn('NotificationCenter::load_system()', constructor)
        self.assertIn('Self::build(app, center, true)', constructor)
        self.assertIn('set_policy(Arc::new(GioPolicyStore::new()))', constructor)
        self.assertIn('if layered {', source)
        self.assertIn('banner_window.init_layer_shell()', source)

    def test_gtk_fixture_captures_connected_buttons_and_real_terminal_paths(self):
        source = (ROOT / 'crates/shell-gtk/src/notify.rs').read_text()
        fixture = source.split('mod gtk_action_lifecycle_tests {', 1)[1]
        for required in ('gtk::init()', 'NotifyUi::build(&app, center.clone(), false)',
                         'NotificationCenter::new()', 'take.emit_clicked()',
                         'delay.emit_clicked()', 'ui.clear.emit_clicked()',
                         'button(&ui.banner_box, "close").emit_clicked()',
                         'button(&ui.list, "close").emit_clicked()',
                         'ui.update()', 'ui.dismiss_notification(id)',
                         'ui.drop_notification_actions(id)',
                         'TUNA_REQUIRE_NOTIFY_UI', 'DBUS_SESSION_BUS_ADDRESS',
                         'WAYLAND_DISPLAY', 'WAYLAND_SOCKET'):
            self.assertIn(required, fixture)
        self.assertNotIn('dispatch_local_action(', fixture)
        self.assertNotIn('drop_notice(', fixture)
        self.assertNotIn('thread::sleep', fixture)
        self.assertIn('fired.get(),\n                0,', fixture)
        self.assertIn('new_fired.get(),', fixture)

if __name__ == '__main__':
    unittest.main()
