//! GNOME 51's monitor labels (`js/ui/osdMonitorLabeler.js`, called through
//! `org.gnome.Shell.ShowMonitorLabels` / `HideMonitorLabels` in
//! `shellDBus.js`): with more than one monitor, Settings › Displays asks
//! for a large number on each screen that matches its arrangement
//! (`cc-display-panel.c` `show_monitors_labels`).
//!
//! One client at a time owns the labels: its `ShowMonitorLabels` replaces
//! what is shown, a second client's is ignored until the first hides them
//! or leaves the bus (`_trackClient`). A change of monitors clears them
//! without freeing the client. Every monitor gets a label, with the text of
//! the connectors the call assigned to it (empty when none), sorted as
//! JavaScript's `Array.sort()` does and joined by a space; the label sits
//! in the monitor's work area, top-left (right-aligned for right-to-left
//! text), over everything, and takes no input.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::services::{bus_watch_name, NameWatch, WatchBus};

/// Which client holds the labels (`_trackClient`, `_untrackClient`).
#[derive(Debug, Default)]
pub struct Labeler {
    client: Option<String>,
}

impl Labeler {
    /// `client` may show labels: it holds them, or nobody does yet (and
    /// it now does).
    pub fn track(&mut self, client: &str) -> bool {
        match &self.client {
            Some(holder) => holder == client,
            None => {
                self.client = Some(client.to_owned());
                true
            }
        }
    }

    /// `client` hides its labels: true when it held them.
    pub fn untrack(&mut self, client: &str) -> bool {
        if self.client.as_deref() == Some(client) {
            self.client = None;
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    pub fn client(&self) -> Option<&str> {
        self.client.as_deref()
    }
}

/// The text each monitor shows for a `ShowMonitorLabels` call that named
/// `labels` as `(connector, label)`: every connector in `monitors` gets
/// an entry, in order, holding the labels of the connectors that are it,
/// sorted and joined by a space; connectors the call names that are not
/// monitors (`get_monitor_for_connector` is -1) are dropped.
pub fn texts(monitors: &[String], labels: &[(String, String)]) -> Vec<(String, String)> {
    monitors
        .iter()
        .map(|monitor| {
            let mut mine: Vec<&str> = labels
                .iter()
                .filter(|(connector, _)| connector == monitor)
                .map(|(_, label)| label.as_str())
                .collect();
            // `Array.sort()` compares UTF-16 code units.
            mine.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            (monitor.clone(), mine.join(" "))
        })
        .collect()
}

/// Unpack `ShowMonitorLabels`' `a{sv}` (connector → label, which
/// Settings sends as the monitor's number): `deepUnpack()`ed and
/// stringified, in the order sent.
pub fn parse_labels(params: &gtk::glib::Variant) -> Vec<(String, String)> {
    let Some(dict) = params.try_child_value(0) else {
        return Vec::new();
    };
    (0..dict.n_children())
        .filter_map(|i| {
            let entry = dict.child_value(i);
            let connector = entry.child_value(0).str()?.to_owned();
            let value = entry.child_value(1);
            let value = value.as_variant().unwrap_or(value);
            let label = match value.str() {
                Some(text) => text.to_owned(),
                None => value.print(false).to_string(),
            };
            Some((connector, label))
        })
        .collect()
}

/// The labels on screen.
pub struct MonitorLabels {
    app: gtk::Application,
    state: RefCell<Labeler>,
    windows: RefCell<Vec<(String, gtk::Window, gtk::Label)>>,
    /// `Gio.bus_watch_name` on the client: it leaving hides the labels.
    watch: RefCell<Option<NameWatch>>,
}

impl MonitorLabels {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let labels = Rc::new(Self {
            app: app.clone(),
            state: RefCell::default(),
            windows: RefCell::default(),
            watch: RefCell::default(),
        });
        // `layoutManager` `monitors-changed`: `_reset` frees the labels.
        if let Some(display) = gtk::gdk::Display::default() {
            let weak = Rc::downgrade(&labels);
            display.monitors().connect_items_changed(move |_, _, _, _| {
                if let Some(labels) = weak.upgrade() {
                    labels.reset();
                }
            });
        }
        labels
    }

    /// The connectors of the monitors there are.
    fn monitors() -> Vec<(String, gtk::gdk::Monitor)> {
        let Some(display) = gtk::gdk::Display::default() else {
            return Vec::new();
        };
        let list = display.monitors();
        (0..list.n_items())
            .filter_map(|i| list.item(i)?.downcast::<gtk::gdk::Monitor>().ok())
            .filter_map(|monitor| Some((monitor.connector()?.to_string(), monitor)))
            .collect()
    }

    /// `ShowMonitorLabels` from `client`.
    pub fn show(self: &Rc<Self>, client: &str, labels: &[(String, String)]) {
        if !self.state.borrow_mut().track(client) {
            return;
        }
        if self.watch.borrow().is_none() {
            let weak = Rc::downgrade(self);
            let name = client.to_owned();
            *self.watch.borrow_mut() = Some(bus_watch_name(
                WatchBus::Type(gtk::gio::BusType::Session),
                client,
                gtk::gio::BusNameWatcherFlags::NONE,
                || {},
                move || {
                    if let Some(labels) = weak.upgrade() {
                        labels.hide(&name);
                    }
                },
            ));
        }
        self.reset();
        let monitors = Self::monitors();
        let connectors: Vec<String> = monitors.iter().map(|(name, _)| name.clone()).collect();
        for (connector, text) in texts(&connectors, labels) {
            let monitor = monitors
                .iter()
                .find(|(name, _)| *name == connector)
                .map(|(_, monitor)| monitor);
            let (window, label) = self.build(&text, monitor);
            self.windows.borrow_mut().push((connector, window, label));
        }
    }

    /// `HideMonitorLabels` from `client` (or it left the bus).
    pub fn hide(&self, client: &str) {
        if !self.state.borrow_mut().untrack(client) {
            return;
        }
        if let Some(watch) = self.watch.borrow_mut().take() {
            watch.unwatch();
        }
        self.reset();
    }

    /// Free every label, keeping the client.
    fn reset(&self) {
        for (_, window, _) in self.windows.borrow_mut().drain(..) {
            window.destroy();
        }
    }

    /// `OsdMonitorLabel`: the number in its accent-colored box at the
    /// top-left of the monitor's work area.
    fn build(&self, text: &str, monitor: Option<&gtk::gdk::Monitor>) -> (gtk::Window, gtk::Label) {
        let window = gtk::Window::new();
        window.set_application(Some(&self.app));
        window.set_title(Some("Monitor Label"));
        window.add_css_class("tuna-monitor-label");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::MONITOR_LABEL_NAMESPACE));
        window.set_monitor(monitor);
        // The work area starts under the top bar, which every monitor has.
        window.set_margin(Edge::Top, tuna_shell_control::PANEL_HEIGHT as i32);
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::None);
        let rtl = gtk::Widget::default_direction() == gtk::TextDirection::Rtl;
        window.set_anchor(Edge::Top, true);
        window.set_anchor(if rtl { Edge::Right } else { Edge::Left }, true);
        let label = gtk::Label::new(Some(text));
        label.add_css_class("osd-monitor-label");
        window.set_child(Some(&label));
        window.present();
        // Labels are looked at, never clicked.
        if let Some(surface) = window.surface() {
            surface.set_input_region(Some(&gtk::cairo::Region::create()));
        }
        (window, label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitors(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    fn labels(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(c, l)| ((*c).to_owned(), (*l).to_owned()))
            .collect()
    }

    #[test]
    fn one_client_holds_the_labels_until_it_hides_them() {
        let mut labeler = Labeler::default();
        assert!(labeler.track(":1.5"), "the first client takes them");
        assert!(labeler.track(":1.5"), "and may replace them");
        assert!(!labeler.track(":1.9"), "a second client is ignored");
        assert!(!labeler.untrack(":1.9"), "and cannot hide them");
        assert_eq!(labeler.client(), Some(":1.5"));
        assert!(labeler.untrack(":1.5"));
        assert!(!labeler.untrack(":1.5"), "hiding twice is a no-op");
        assert!(labeler.track(":1.9"), "now another client may show");
    }

    #[test]
    fn every_monitor_gets_a_label_with_its_connectors_numbers() {
        let shown = texts(
            &monitors(&["DP-1", "HDMI-1", "eDP-1"]),
            &labels(&[("HDMI-1", "2"), ("DP-1", "1")]),
        );
        assert_eq!(
            shown,
            [
                ("DP-1".to_owned(), "1".to_owned()),
                ("HDMI-1".to_owned(), "2".to_owned()),
                ("eDP-1".to_owned(), String::new()),
            ]
        );
    }

    #[test]
    fn unknown_connectors_are_dropped_and_mirrored_ones_join_sorted() {
        // Mirrored monitors are one logical monitor: their numbers share
        // a label, as `Array.sort()` orders strings ("10" before "9").
        let shown = texts(
            &monitors(&["DP-1"]),
            &labels(&[("DP-1", "9"), ("DP-1", "10"), ("DP-1", "1"), ("gone", "3")]),
        );
        assert_eq!(shown, [("DP-1".to_owned(), "1 10 9".to_owned())]);
    }

    #[test]
    fn settings_numbers_unpack_whatever_integer_type_carries_them() {
        use gtk::glib::Variant;
        let dict = Variant::array_from_iter_with_type(
            gtk::glib::VariantTy::new("{sv}").unwrap(),
            [
                gtk::glib::Variant::from_dict_entry(
                    &"DP-1".to_variant(),
                    &Variant::from_variant(&2_i32.to_variant()),
                ),
                gtk::glib::Variant::from_dict_entry(
                    &"HDMI-1".to_variant(),
                    &Variant::from_variant(&3_u32.to_variant()),
                ),
                gtk::glib::Variant::from_dict_entry(
                    &"DP-2".to_variant(),
                    &Variant::from_variant(&"A".to_variant()),
                ),
            ],
        );
        let params = Variant::tuple_from_iter([dict]);
        assert_eq!(
            parse_labels(&params),
            labels(&[("DP-1", "2"), ("HDMI-1", "3"), ("DP-2", "A")])
        );
        assert!(parse_labels(&().to_variant()).is_empty());
    }
}
