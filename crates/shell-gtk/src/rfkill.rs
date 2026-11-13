//! GNOME's Airplane Mode toggle (gnome-shell 51 `js/ui/status/rfkill.js`:
//! `RfkillManager`, `RfkillToggle` and `Indicator`), backed by
//! gnome-settings-daemon's rfkill plugin on the session bus
//! (`org.gnome.SettingsDaemon.Rfkill`).
//!
//! The toggle shows while `HasAirplaneMode && ShouldShowAirplaneMode`
//! (`show-airplane-mode`), is checked with `AirplaneMode`, and a click
//! writes `AirplaneMode = !AirplaneMode`. The panel's airplane icon shows
//! while the toggle is both visible and checked. No
//! `org.gnome.desktop.lockdown` key gates it in GNOME 51.
//!
//! GNOME 51's toggle never reads `HardwareAirplaneMode`: the manager
//! exposes it as `hw-airplane-mode`, but nothing in the 51 shell binds
//! it, so a hardware kill switch leaves the tile clickable and without
//! a subtitle. Tuna matches that and does not read the property either.
//!
//! Recorded divergence: GNOME's `_changed` notifies only for keys in the
//! changed dict, so a gsd-rfkill that exits leaves its toggle on screen
//! and one that restarts is not re-read until a property changes. Here
//! the tile hides when the daemon leaves the bus and every property is
//! re-read when it returns, as the other service tiles do.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::services::{self, Tile};

const BUS_NAME: &str = "org.gnome.SettingsDaemon.Rfkill";
const OBJECT_PATH: &str = "/org/gnome/SettingsDaemon/Rfkill";
const IFACE: &str = "org.gnome.SettingsDaemon.Rfkill";
const PROPS: &str = "org.freedesktop.DBus.Properties";
/// Bound on every D-Bus round trip (ms), as in `services`.
const CALL_TIMEOUT_MS: i32 = 2000;

/// `RfkillToggle`'s title and icon; the panel indicator uses the icon too.
pub const TITLE: &str = "Airplane Mode";
pub const ICON: &str = "airplane-mode-symbolic";

/// The rfkill properties the 51 toggle reads. A property the daemon
/// does not report reads `false`, as an unset GDBusProxy property is
/// falsy in GNOME's bindings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RfkillState {
    pub airplane_mode: bool,
    pub has_airplane_mode: bool,
    pub should_show_airplane_mode: bool,
}

impl RfkillState {
    /// Read the state from a `GetAll` reply.
    pub fn from_props(props: &glib::VariantDict) -> Self {
        let flag = |key: &str| {
            props
                .lookup_value(key, None)
                .and_then(|v| v.get::<bool>())
                .unwrap_or(false)
        };
        Self {
            airplane_mode: flag("AirplaneMode"),
            has_airplane_mode: flag("HasAirplaneMode"),
            should_show_airplane_mode: flag("ShouldShowAirplaneMode"),
        }
    }

    /// `show-airplane-mode`, bound to the toggle's `visible`.
    pub fn toggle_visible(self) -> bool {
        self.has_airplane_mode && self.should_show_airplane_mode
    }

    /// `airplane-mode`, bound to the toggle's `checked`.
    pub fn toggle_checked(self) -> bool {
        self.airplane_mode
    }

    /// `Indicator._sync`: the panel icon only while airplane mode is on
    /// and the toggle is shown.
    pub fn indicator_visible(self) -> bool {
        self.toggle_visible() && self.toggle_checked()
    }

    /// The `AirplaneMode` value a click writes.
    pub fn after_click(self) -> bool {
        !self.airplane_mode
    }
}

/// The Airplane Mode tile and panel indicator, following gsd-rfkill.
pub struct Airplane {
    tile: Tile,
    indicator: gtk::Image,
    /// The daemon's last report, with a click's write cached at once as
    /// gjs's proxy setter does.
    state: Cell<RfkillState>,
    conn: RefCell<Option<gio::DBusConnection>>,
    /// Bumped on every read, so a slower earlier one cannot win.
    generation: Cell<u64>,
}

impl Airplane {
    /// Both widgets start hidden until the daemon says otherwise.
    pub fn new(tile: Tile, indicator: gtk::Image) -> Rc<Self> {
        tile.present(false);
        indicator.set_visible(false);
        let this = Rc::new(Self {
            tile,
            indicator,
            state: Cell::new(RfkillState::default()),
            conn: RefCell::default(),
            generation: Cell::new(0),
        });
        let weak = Rc::downgrade(&this);
        this.tile.on_user_toggle(move |_| {
            if let Some(this) = weak.upgrade() {
                this.clicked();
            }
        });
        this
    }

    /// Follow gsd-rfkill on the session bus. Without a session bus the
    /// tile stays hidden.
    pub fn attach(self: &Rc<Self>) {
        let this = self.clone();
        gio::bus_get(
            gio::BusType::Session,
            None::<&gio::Cancellable>,
            move |conn| match conn {
                Ok(conn) => this.watch(&conn),
                Err(e) => eprintln!("tuna-shell-gtk: no session bus, Airplane Mode hidden: {e}"),
            },
        );
    }

    fn watch(self: &Rc<Self>, conn: &gio::DBusConnection) {
        *self.conn.borrow_mut() = Some(conn.clone());
        let this = self.clone();
        let sub = conn.subscribe_to_signal(
            Some(BUS_NAME),
            Some(PROPS),
            Some("PropertiesChanged"),
            Some(OBJECT_PATH),
            Some(IFACE),
            gio::DBusSignalFlags::NONE,
            move |_| this.refresh(),
        );
        // The subscription lives as long as the shell.
        std::mem::forget(sub);
        let this = self.clone();
        services::watch_name(conn, BUS_NAME, move |owned| {
            if owned {
                this.refresh();
            } else {
                this.generation.set(this.generation.get() + 1);
                this.show(RfkillState::default());
            }
        });
    }

    /// Re-read every property; a failed read hides the tile.
    fn refresh(self: &Rc<Self>) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let this = self.clone();
        conn.call(
            Some(BUS_NAME),
            OBJECT_PATH,
            PROPS,
            "GetAll",
            Some(&(IFACE,).to_variant()),
            glib::VariantTy::new("(a{sv})").ok(),
            gio::DBusCallFlags::NO_AUTO_START,
            CALL_TIMEOUT_MS,
            None::<&gio::Cancellable>,
            move |reply| {
                if this.generation.get() != generation {
                    return;
                }
                let state = reply
                    .ok()
                    .map(|v| {
                        RfkillState::from_props(&glib::VariantDict::new(Some(&v.child_value(0))))
                    })
                    .unwrap_or_default();
                this.show(state);
            },
        );
    }

    fn show(&self, state: RfkillState) {
        self.state.set(state);
        self.tile.present(state.toggle_visible());
        self.tile.show_state(state.toggle_checked(), None);
        self.indicator.set_visible(state.indicator_visible());
    }

    /// `RfkillToggle`'s click: write the flipped `AirplaneMode`. The
    /// tile keeps the daemon's last word until its `PropertiesChanged`
    /// confirms; a refused write re-reads the daemon.
    fn clicked(self: &Rc<Self>) {
        let mut state = self.state.get();
        let shown = state.toggle_checked();
        let next = state.after_click();
        state.airplane_mode = next;
        self.state.set(state);
        self.tile.show_state(shown, None);
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let this = self.clone();
        conn.call(
            Some(BUS_NAME),
            OBJECT_PATH,
            PROPS,
            "Set",
            Some(&(IFACE, "AirplaneMode", next.to_variant()).to_variant()),
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            CALL_TIMEOUT_MS,
            None::<&gio::Cancellable>,
            move |reply| {
                if reply.is_err() {
                    this.refresh();
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(entries: &[(&str, bool)]) -> glib::VariantDict {
        let dict = glib::VariantDict::new(None);
        for (key, value) in entries {
            dict.insert_value(key, &value.to_variant());
        }
        dict
    }

    #[test]
    fn airplane_toggle_hidden_without_should_show() {
        let state = RfkillState::from_props(&props(&[
            ("HasAirplaneMode", true),
            ("ShouldShowAirplaneMode", false),
            ("AirplaneMode", true),
        ]));
        assert!(!state.toggle_visible());
        // The indicator needs a visible toggle, even with airplane mode on.
        assert!(!state.indicator_visible());
        let no_radios = RfkillState::from_props(&props(&[
            ("HasAirplaneMode", false),
            ("ShouldShowAirplaneMode", true),
        ]));
        assert!(!no_radios.toggle_visible());
        let shown = RfkillState::from_props(&props(&[
            ("HasAirplaneMode", true),
            ("ShouldShowAirplaneMode", true),
        ]));
        assert!(shown.toggle_visible());
    }

    #[test]
    fn missing_daemon_properties_hide_the_toggle() {
        let state = RfkillState::from_props(&props(&[]));
        assert_eq!(state, RfkillState::default());
        assert!(!state.toggle_visible());
        assert!(!state.indicator_visible());
    }

    #[test]
    fn airplane_toggle_checks_with_airplane_mode_and_flips_it() {
        let on = RfkillState::from_props(&props(&[
            ("HasAirplaneMode", true),
            ("ShouldShowAirplaneMode", true),
            ("AirplaneMode", true),
        ]));
        assert!(on.toggle_checked());
        assert!(on.indicator_visible());
        assert!(!on.after_click());
        let off = RfkillState {
            airplane_mode: false,
            ..on
        };
        assert!(!off.toggle_checked());
        assert!(!off.indicator_visible());
        assert!(off.after_click());
    }

    #[test]
    fn hardware_airplane_mode_leaves_the_toggle_as_gnome_51_does() {
        // GNOME 51's RfkillToggle binds only show-airplane-mode and
        // airplane-mode; a hardware kill switch changes neither the
        // toggle's visibility nor what a click writes.
        let state = RfkillState::from_props(&props(&[
            ("HasAirplaneMode", true),
            ("ShouldShowAirplaneMode", true),
            ("AirplaneMode", true),
            ("HardwareAirplaneMode", true),
        ]));
        let soft_only = RfkillState::from_props(&props(&[
            ("HasAirplaneMode", true),
            ("ShouldShowAirplaneMode", true),
            ("AirplaneMode", true),
        ]));
        assert_eq!(state, soft_only);
        assert!(state.toggle_visible());
        assert!(!state.after_click());
    }

    #[test]
    fn wrongly_typed_properties_read_as_unset() {
        let dict = glib::VariantDict::new(None);
        dict.insert_value("HasAirplaneMode", &1u32.to_variant());
        dict.insert_value("ShouldShowAirplaneMode", &true.to_variant());
        assert!(!RfkillState::from_props(&dict).toggle_visible());
    }
}
