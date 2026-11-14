//! GNOME's keyboard backlight toggle (gnome-shell 51
//! `js/ui/status/backlight.js`: `KeyboardBrightnessToggle`, `SliderItem`
//! and `DiscreteItem`), backed by gnome-settings-daemon's power plugin
//! on the session bus (`org.gnome.SettingsDaemon.Power`, interface
//! `org.gnome.SettingsDaemon.Power.Keyboard`).
//!
//! The "Keyboard" toggle shows while `Brightness` is a non-negative
//! integer and is checked while it is above zero; a click writes
//! `Brightness = checked ? 0 : 100`. Its menu (no header, as 51 sets
//! none) holds a "Keyboard Brightness" slider when `Steps >= 4`, else
//! Off / Low / High level buttons for `Steps` levels; either writes
//! `Brightness` (a percentage) as the user moves it. No
//! `org.gnome.desktop.lockdown` key gates it in GNOME 51, and the
//! toggle has no panel indicator.
//!
//! Recorded divergences: a slider position is written rounded to a
//! whole percentage (the D-Bus property is an `i`); with a single level
//! the Off button writes 0 where `_levelToValue` divides by zero; and
//! every property is re-read when the daemon returns to the bus (GNOME's
//! proxy only re-syncs on the next `PropertiesChanged`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::services::{self, Tile};

const BUS_NAME: &str = "org.gnome.SettingsDaemon.Power";
const OBJECT_PATH: &str = "/org/gnome/SettingsDaemon/Power";
const IFACE: &str = "org.gnome.SettingsDaemon.Power.Keyboard";
const PROPS: &str = "org.freedesktop.DBus.Properties";
/// Bound on every D-Bus round trip (ms), as in `services`.
const CALL_TIMEOUT_MS: i32 = 2000;

/// `KeyboardBrightnessToggle`'s title, icon and menu-button name.
pub const TITLE: &str = "Keyboard";
pub const ICON: &str = "display-brightness-symbolic";
pub const MENU_BUTTON_NAME: &str = "Open keyboard brightness menu";
/// `SliderItem`'s accessible name.
pub const SLIDER_NAME: &str = "Keyboard Brightness";

/// The keyboard properties the 51 toggle reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyboardState {
    /// `Brightness` (percent; negative means no backlight), `None`
    /// while the daemon reports none.
    pub brightness: Option<i32>,
    /// `Steps`, 0 while unreported.
    pub steps: i32,
}

impl KeyboardState {
    /// Read the state from a `GetAll` reply.
    pub fn from_props(props: &glib::VariantDict) -> Self {
        let int = |key: &str| props.lookup_value(key, None).and_then(|v| v.get::<i32>());
        Self {
            brightness: int("Brightness"),
            steps: int("Steps").unwrap_or(0),
        }
    }

    /// `_sync`: visible while `Brightness` is an integer `>= 0`.
    pub fn visible(self) -> bool {
        self.brightness.is_some_and(|b| b >= 0)
    }

    /// `_sync`: checked while the backlight is lit.
    pub fn checked(self) -> bool {
        self.brightness.is_some_and(|b| b > 0)
    }

    /// `_sync`: the slider for four or more steps, else level buttons.
    pub fn uses_slider(self) -> bool {
        self.steps >= 4
    }

    /// `DiscreteItem.nLevels` for these steps. The property's range is
    /// 1..=3, so GObject refuses anything below and the item keeps its
    /// initial single level; four and more use the slider instead.
    pub fn levels(self) -> i32 {
        self.steps.clamp(1, 3)
    }
}

/// The `Brightness` a toggle click writes: off when lit, full when off.
pub fn click_brightness(checked: bool) -> i32 {
    if checked {
        0
    } else {
        100
    }
}

/// The `Brightness` a slider position (0..=100) writes.
pub fn slider_brightness(value: f64) -> i32 {
    value.round().clamp(0.0, 100.0) as i32
}

/// One of `DiscreteItem`'s level buttons, in the order it adds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Off,
    Low,
    High,
}

/// The level buttons in `DiscreteItem`'s key order.
pub const LEVELS: [Level; 3] = [Level::Off, Level::Low, Level::High];

impl Level {
    fn index(self) -> i32 {
        match self {
            Level::Off => 0,
            Level::Low => 1,
            Level::High => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Level::Off => "Off",
            Level::Low => "Low",
            Level::High => "High",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Level::Off => "keyboard-brightness-off-symbolic",
            Level::Low => "keyboard-brightness-medium-symbolic",
            Level::High => "keyboard-brightness-high-symbolic",
        }
    }

    /// `_syncLevels`: Off from one level, High from two, Low from three.
    pub fn visible(self, levels: i32) -> bool {
        match self {
            Level::Off => levels > 0,
            Level::High => levels > 1,
            Level::Low => levels > 2,
        }
    }
}

/// `_valueToLevel`: the checked button for a brightness. The top index
/// is always High (so two levels read Off / High); an index past the
/// keys checks nothing.
pub fn level_for_brightness(value: i32, levels: i32) -> Option<Level> {
    // Math.round: halves round up.
    let index = (f64::from(value) * f64::from(levels - 1) / 100.0 + 0.5).floor() as i32;
    if index == levels - 1 {
        return Some(Level::High);
    }
    usize::try_from(index)
        .ok()
        .and_then(|i| LEVELS.get(i).copied())
}

/// `_levelToValue`: the brightness a level button writes, its key index
/// capped at the top level and spread over 0..=100.
pub fn brightness_for_level(level: Level, levels: i32) -> i32 {
    if levels <= 1 {
        return 0;
    }
    let index = level.index().min(levels - 1);
    (100.0 * f64::from(index) / f64::from(levels - 1)).round() as i32
}

/// The Keyboard tile, its brightness menu and the daemon behind them.
pub struct KeyboardBacklight {
    tile: Tile,
    slider_row: gtk::Box,
    slider: gtk::Scale,
    levels_row: gtk::Box,
    level_buttons: Vec<(Level, gtk::Box, gtk::ToggleButton)>,
    /// The daemon's last report.
    state: Cell<KeyboardState>,
    /// `DiscreteItem.nLevels`.
    levels: Cell<i32>,
    /// Set while the shell updates the menu from daemon state, so its
    /// handlers do not write it back.
    syncing: Cell<bool>,
    conn: RefCell<Option<gio::DBusConnection>>,
    /// Bumped on every read, so a slower earlier one cannot win.
    generation: Cell<u64>,
}

impl KeyboardBacklight {
    /// The menu's slider and level rows go into `section`.
    pub fn new(tile: Tile, section: &gtk::Box) -> Rc<Self> {
        tile.present(false);
        let slider_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        slider_row.add_css_class("keyboard-brightness-item");
        let slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        slider.set_hexpand(true);
        slider.update_property(&[gtk::accessible::Property::Label(SLIDER_NAME)]);
        slider_row.append(&slider);
        section.append(&slider_row);
        let levels_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        levels_row.set_homogeneous(true);
        levels_row.add_css_class("keyboard-brightness-levels");
        let level_buttons: Vec<_> = LEVELS
            .into_iter()
            .map(|level| {
                let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
                column.add_css_class("keyboard-brightness-level");
                column.set_hexpand(true);
                let button = gtk::ToggleButton::new();
                button.set_icon_name(level.icon());
                button.set_halign(gtk::Align::Center);
                button.add_css_class("qs-round");
                button.update_property(&[gtk::accessible::Property::Label(level.label())]);
                column.append(&button);
                column.append(&gtk::Label::new(Some(level.label())));
                levels_row.append(&column);
                (level, column, button)
            })
            .collect();
        section.append(&levels_row);
        let this = Rc::new(Self {
            tile,
            slider_row,
            slider,
            levels_row,
            level_buttons,
            state: Cell::new(KeyboardState::default()),
            levels: Cell::new(1),
            syncing: Cell::new(false),
            conn: RefCell::default(),
            generation: Cell::new(0),
        });
        this.sync_levels(0);
        {
            let weak = Rc::downgrade(&this);
            this.tile.on_user_toggle(move |_| {
                let Some(this) = weak.upgrade() else { return };
                let checked = this.state.get().checked();
                // The tile shows the daemon's state, not the click.
                this.tile.show_state(checked, None);
                this.write(click_brightness(checked));
            });
        }
        {
            let weak = Rc::downgrade(&this);
            this.slider.connect_value_changed(move |slider| {
                let Some(this) = weak.upgrade() else { return };
                if !this.syncing.get() && this.slider_row.is_visible() {
                    this.write(slider_brightness(slider.value()));
                }
            });
        }
        for (level, _, button) in &this.level_buttons {
            let (weak, level) = (Rc::downgrade(&this), *level);
            button.connect_toggled(move |_| {
                let Some(this) = weak.upgrade() else { return };
                if this.syncing.get() {
                    return;
                }
                let value = brightness_for_level(level, this.levels.get());
                this.syncing.set(true);
                this.sync_levels(value);
                this.syncing.set(false);
                if this.levels_row.is_visible() {
                    this.write(value);
                }
            });
        }
        this
    }

    /// Follow gsd-power on the session bus. Without a session bus the
    /// tile stays hidden.
    pub fn attach(self: &Rc<Self>) {
        let this = self.clone();
        gio::bus_get(
            gio::BusType::Session,
            None::<&gio::Cancellable>,
            move |conn| match conn {
                Ok(conn) => this.watch(&conn),
                Err(e) => eprintln!("tuna-shell-gtk: no session bus, Keyboard hidden: {e}"),
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
                this.show(KeyboardState::default());
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
                        KeyboardState::from_props(&glib::VariantDict::new(Some(&v.child_value(0))))
                    })
                    .unwrap_or_default();
                this.show(state);
            },
        );
    }

    /// `_sync`: the tile, and the slider or level buttons, from the
    /// daemon's state.
    fn show(&self, state: KeyboardState) {
        self.state.set(state);
        let visible = state.visible();
        self.tile.present(visible);
        if !visible {
            return;
        }
        let brightness = state.brightness.unwrap_or(0);
        self.tile.show_state(state.checked(), None);
        let slider = state.uses_slider();
        self.syncing.set(true);
        self.slider_row.set_visible(slider);
        self.levels_row.set_visible(!slider);
        self.slider.set_value(f64::from(brightness));
        if !slider {
            self.levels.set(state.levels());
        }
        self.sync_levels(brightness);
        self.syncing.set(false);
    }

    /// `_syncLevels` and `_syncChecked` for a brightness.
    fn sync_levels(&self, brightness: i32) {
        let levels = self.levels.get();
        let checked = level_for_brightness(brightness, levels);
        for (level, column, button) in &self.level_buttons {
            column.set_visible(level.visible(levels));
            button.set_active(Some(*level) == checked);
        }
    }

    /// Write `Brightness`; a refused write re-reads the daemon.
    fn write(self: &Rc<Self>, brightness: i32) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let this = self.clone();
        conn.call(
            Some(BUS_NAME),
            OBJECT_PATH,
            PROPS,
            "Set",
            Some(&(IFACE, "Brightness", brightness.to_variant()).to_variant()),
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

    fn state(brightness: Option<i32>, steps: Option<i32>) -> KeyboardState {
        let dict = glib::VariantDict::new(None);
        if let Some(b) = brightness {
            dict.insert_value("Brightness", &b.to_variant());
        }
        if let Some(s) = steps {
            dict.insert_value("Steps", &s.to_variant());
        }
        KeyboardState::from_props(&dict)
    }

    #[test]
    fn keyboard_toggle_hidden_without_a_backlight() {
        // gsd-power reports -1 when there is no keyboard backlight.
        assert!(!state(Some(-1), Some(3)).visible());
        // An absent daemon or property hides it too.
        assert!(!state(None, None).visible());
        assert!(!KeyboardState::default().visible());
        // A wrongly typed Brightness is not an integer.
        let dict = glib::VariantDict::new(None);
        dict.insert_value("Brightness", &50u32.to_variant());
        assert!(!KeyboardState::from_props(&dict).visible());
        // Zero is a backlight that is off: shown, unchecked.
        let off = state(Some(0), Some(3));
        assert!(off.visible());
        assert!(!off.checked());
        assert!(state(Some(1), Some(3)).checked());
    }

    #[test]
    fn keyboard_click_turns_off_or_full() {
        assert_eq!(click_brightness(state(Some(40), Some(3)).checked()), 0);
        assert_eq!(click_brightness(state(Some(0), Some(3)).checked()), 100);
    }

    #[test]
    fn keyboard_levels_follow_gnome_steps() {
        // Four steps and more draw the slider.
        assert!(state(Some(10), Some(4)).uses_slider());
        assert!(state(Some(10), Some(101)).uses_slider());
        assert!(!state(Some(10), Some(3)).uses_slider());
        assert!(!state(Some(10), Some(2)).uses_slider());
        // Three steps: Off, Low, High.
        let three = state(Some(0), Some(3)).levels();
        assert_eq!(three, 3);
        assert!(LEVELS.iter().all(|l| l.visible(three)));
        // Two steps: Off and High only.
        let two = state(Some(0), Some(2)).levels();
        assert_eq!(two, 2);
        assert!(Level::Off.visible(two));
        assert!(!Level::Low.visible(two));
        assert!(Level::High.visible(two));
        // One step (or an out-of-range report): Off alone.
        for steps in [1, 0, -3] {
            let one = state(Some(0), Some(steps)).levels();
            assert_eq!(one, 1);
            assert!(Level::Off.visible(one));
            assert!(!Level::Low.visible(one));
            assert!(!Level::High.visible(one));
        }
    }

    #[test]
    fn level_buttons_write_spread_percentages() {
        assert_eq!(brightness_for_level(Level::Off, 3), 0);
        assert_eq!(brightness_for_level(Level::Low, 3), 50);
        assert_eq!(brightness_for_level(Level::High, 3), 100);
        // Two levels: High's index caps at the top level.
        assert_eq!(brightness_for_level(Level::Off, 2), 0);
        assert_eq!(brightness_for_level(Level::High, 2), 100);
        assert_eq!(brightness_for_level(Level::Low, 2), 100);
        assert_eq!(brightness_for_level(Level::Off, 1), 0);
    }

    #[test]
    fn brightness_checks_the_nearest_level() {
        assert_eq!(level_for_brightness(0, 3), Some(Level::Off));
        assert_eq!(level_for_brightness(24, 3), Some(Level::Off));
        assert_eq!(level_for_brightness(25, 3), Some(Level::Low));
        assert_eq!(level_for_brightness(50, 3), Some(Level::Low));
        assert_eq!(level_for_brightness(74, 3), Some(Level::Low));
        // Math.round rounds the half up, to the top level.
        assert_eq!(level_for_brightness(75, 3), Some(Level::High));
        assert_eq!(level_for_brightness(100, 3), Some(Level::High));
        // Two levels: anything from half up is High.
        assert_eq!(level_for_brightness(49, 2), Some(Level::Off));
        assert_eq!(level_for_brightness(50, 2), Some(Level::High));
        // One level: index 0 is the top, so GNOME checks the hidden High.
        assert_eq!(level_for_brightness(0, 1), Some(Level::High));
        // Each level's own write reads back as that level.
        for levels in [2, 3] {
            for level in LEVELS.into_iter().filter(|l| l.visible(levels)) {
                assert_eq!(
                    level_for_brightness(brightness_for_level(level, levels), levels),
                    Some(level)
                );
            }
        }
    }

    #[test]
    fn slider_positions_write_whole_percentages() {
        assert_eq!(slider_brightness(0.0), 0);
        assert_eq!(slider_brightness(37.4), 37);
        assert_eq!(slider_brightness(37.5), 38);
        assert_eq!(slider_brightness(100.0), 100);
        assert_eq!(slider_brightness(120.0), 100);
    }
}
