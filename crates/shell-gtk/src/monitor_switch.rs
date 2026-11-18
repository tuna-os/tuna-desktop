//! GNOME's switch-monitor popup (#1195): gnome-shell 51 `js/ui/switchMonitor.js`
//! on `switcherPopup.js`, bound to mutter's `org.gnome.mutter.keybindings`
//! `switch-monitor` (Super+P, XF86Display).
//!
//! Mirror and Join Displays always; External Only and Built-in Only too
//! when there is a built-in panel. The popup opens with the choice after
//! the one in effect selected (`_initialSelection`); the binding again,
//! Left and Right step, Escape cancels, space and Enter pick, a click
//! picks, and releasing the binding's modifier picks. A binding without a
//! modifier (XF86Display) waits `NO_MODS_TIMEOUT` for a further press. The
//! compositor applies the choice (mutter's `switch_config`), so what it may
//! switch to is whatever it last said ([`Message::MonitorSwitch`]): with
//! fewer than two monitors the binding does nothing (`can_switch_config`).
//!
//! [`Message::MonitorSwitch`]: tuna_shell_control::Message::MonitorSwitch

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};
use tuna_shell_control::MonitorSwitchConfig;

use crate::input_sources::{primary_modifier, released, NO_MODS_TIMEOUT, POPUP_DELAY};

/// Layer-shell namespace of the popup's surface.
pub const NAMESPACE: &str = "tuna-monitor-switch";
/// `APP_ICON_SIZE`.
const ICON_SIZE: i32 = 96;
/// `Meta.MonitorSwitchConfigType.UNKNOWN`, after the four choices.
const UNKNOWN_CONFIG: usize = 4;

/// One choice of the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    pub icon: &'static str,
    pub label: &'static str,
    pub config: MonitorSwitchConfig,
}

/// `SwitchMonitorPopup._init`: Mirror and Join Displays, plus External
/// Only and Built-in Only with a built-in panel.
pub fn items(has_builtin: bool) -> Vec<Item> {
    let mut items = vec![
        Item {
            icon: "shell-display-mirror-symbolic",
            label: "Mirror",
            config: MonitorSwitchConfig::AllMirror,
        },
        Item {
            icon: "shell-display-extend-all-symbolic",
            label: "Join Displays",
            config: MonitorSwitchConfig::AllLinear,
        },
    ];
    if has_builtin {
        items.push(Item {
            icon: "shell-display-external-only-symbolic",
            label: "External Only",
            config: MonitorSwitchConfig::External,
        });
        items.push(Item {
            icon: "shell-display-built-in-only-symbolic",
            label: "Built-in Only",
            config: MonitorSwitchConfig::Builtin,
        });
    }
    items
}

/// `_initialSelection`: `(currentConfig + 1) % items.length`, with the
/// choice's `Meta.MonitorSwitchConfigType` value as the index and UNKNOWN
/// (any other configuration in effect) after the four.
pub fn initial_selection(current: Option<MonitorSwitchConfig>, len: usize) -> usize {
    let current = match current {
        Some(MonitorSwitchConfig::AllMirror) => 0,
        Some(MonitorSwitchConfig::AllLinear) => 1,
        Some(MonitorSwitchConfig::External) => 2,
        Some(MonitorSwitchConfig::Builtin) => 3,
        None => UNKNOWN_CONFIG,
    };
    (current + 1) % len.max(1)
}

/// `SwitcherPopup._next` and `_previous`: wrap around.
pub fn step(selected: usize, len: usize, backward: bool) -> usize {
    if backward {
        (selected + len - 1) % len
    } else {
        (selected + 1) % len
    }
}

/// What the compositor last said the popup reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Monitors {
    can_switch: bool,
    has_builtin: bool,
    current: Option<MonitorSwitchConfig>,
}

/// The popup.
pub struct MonitorSwitchPopup {
    window: gtk::ApplicationWindow,
    row: gtk::Box,
    items: RefCell<Vec<(Item, gtk::Box)>>,
    selected: Cell<usize>,
    mask: Cell<gtk::gdk::ModifierType>,
    open: Cell<bool>,
    monitors: Cell<Monitors>,
    /// Applies the picked choice (the compositor's `SwitchMonitor`).
    choose: Rc<dyn Fn(MonitorSwitchConfig)>,
    /// Pending timers; each clears its own slot when it fires.
    delay: Rc<RefCell<Option<glib::SourceId>>>,
    no_mods: Rc<RefCell<Option<glib::SourceId>>>,
}

impl MonitorSwitchPopup {
    pub fn new(app: &gtk::Application, choose: Rc<dyn Fn(MonitorSwitchConfig)>) -> Rc<Self> {
        let window = gtk::ApplicationWindow::new(app);
        window.add_css_class("tuna-switcher");
        window.add_css_class("tuna-monitor-switcher");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(NAMESPACE));
        // A modal popup holds the keyboard to see the modifier released.
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        window.set_exclusive_zone(-1);
        window.set_title(Some("Switch Monitor"));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("switcher-list");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_halign(gtk::Align::Center);
        list.append(&row);
        window.set_child(Some(&list));
        let ui = Rc::new(Self {
            window,
            row,
            items: RefCell::new(Vec::new()),
            selected: Cell::new(0),
            mask: Cell::new(gtk::gdk::ModifierType::empty()),
            open: Cell::new(false),
            monitors: Cell::new(Monitors::default()),
            choose,
            delay: Rc::new(RefCell::new(None)),
            no_mods: Rc::new(RefCell::new(None)),
        });
        let keys = gtk::EventControllerKey::new();
        {
            let weak = Rc::downgrade(&ui);
            keys.connect_key_pressed(move |_, key, _, _| {
                let Some(ui) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                use gtk::gdk::Key;
                match key {
                    Key::Left | Key::KP_Left => ui.step(true),
                    Key::Right | Key::KP_Right => ui.step(false),
                    Key::Escape | Key::Tab => ui.close(None),
                    Key::space | Key::Return | Key::KP_Enter | Key::ISO_Enter => ui.finish(),
                    _ => {}
                }
                glib::Propagation::Stop
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            keys.connect_modifiers(move |_, state| {
                if let Some(ui) = weak.upgrade() {
                    ui.modifiers_changed(state);
                }
                glib::Propagation::Proceed
            });
        }
        {
            // A key event carries the state from before it: read the seat's
            // once the compositor's modifiers event landed.
            let weak = Rc::downgrade(&ui);
            keys.connect_key_released(move |_, _, _, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.check_seat_modifiers();
                }
            });
        }
        ui.window.add_controller(keys);
        {
            // `show`'s race check: a modifier released before the popup
            // held the keyboard is never reported, so look each time it
            // takes the keyboard.
            let focus = gtk::EventControllerLegacy::new();
            let weak = Rc::downgrade(&ui);
            focus.connect_event(move |_, event| {
                let focused = event
                    .downcast_ref::<gtk::gdk::FocusEvent>()
                    .is_some_and(|focus| focus.is_in());
                if focused {
                    if let Some(ui) = weak.upgrade() {
                        ui.check_seat_modifiers();
                    }
                }
                glib::Propagation::Proceed
            });
            ui.window.add_controller(focus);
        }
        ui.install_actions();
        ui
    }

    /// The compositor's latest `MonitorSwitch`.
    pub fn set_monitors(
        &self,
        can_switch: bool,
        has_builtin: bool,
        current: Option<MonitorSwitchConfig>,
    ) {
        self.monitors.set(Monitors {
            can_switch,
            has_builtin,
            current,
        });
        if !can_switch {
            // The monitor that made it possible went away.
            self.close(None);
        }
    }

    /// `switch-monitor` fired from `accelerator`: open the popup, or step
    /// it when open (`_keyPressHandler`'s `SWITCH_MONITOR` action).
    pub fn binding(self: &Rc<Self>, accelerator: &str) {
        if self.open.get() {
            self.step(false);
            return;
        }
        let monitors = self.monitors.get();
        if !monitors.can_switch {
            return;
        }
        let mask = gtk::accelerator_parse(accelerator)
            .map(|(_, mods)| mods.bits())
            .unwrap_or(0);
        self.show(monitors, primary_modifier(mask));
    }

    fn show(self: &Rc<Self>, monitors: Monitors, mask: u32) {
        while let Some(child) = self.row.first_child() {
            self.row.remove(&child);
        }
        let choices = items(monitors.has_builtin);
        let mut shown = Vec::new();
        for (index, item) in choices.into_iter().enumerate() {
            let tile = gtk::Box::new(gtk::Orientation::Vertical, 6);
            tile.add_css_class("item-box");
            let icon = gtk::Image::from_icon_name(item.icon);
            icon.set_pixel_size(ICON_SIZE);
            let text = gtk::Label::new(Some(item.label));
            text.set_halign(gtk::Align::Center);
            tile.append(&icon);
            tile.append(&text);
            tile.update_property(&[gtk::accessible::Property::Label(item.label)]);
            let click = gtk::GestureClick::new();
            let weak = Rc::downgrade(self);
            click.connect_released(move |_, _, _, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.select(index);
                    ui.finish();
                }
            });
            tile.add_controller(click);
            self.row.append(&tile);
            shown.push((item, tile));
        }
        let selected = initial_selection(monitors.current, shown.len());
        *self.items.borrow_mut() = shown;
        self.open.set(true);
        self.mask
            .set(gtk::gdk::ModifierType::from_bits_truncate(mask));
        self.select(selected);
        // Held for the keyboard but invisible for POPUP_DELAY, so a quick
        // tap never flashes it.
        self.window.set_opacity(0.0);
        self.window.present();
        if let Some(old) = self.delay.borrow_mut().take() {
            old.remove();
        }
        let window = self.window.downgrade();
        let slot = self.delay.clone();
        let id = glib::timeout_add_local_once(POPUP_DELAY, move || {
            slot.borrow_mut().take();
            if let Some(window) = window.upgrade() {
                window.set_opacity(1.0);
            }
        });
        *self.delay.borrow_mut() = Some(id);
        if mask == 0 {
            self.reset_no_mods();
        }
    }

    fn reset_no_mods(&self) {
        if let Some(old) = self.no_mods.borrow_mut().take() {
            old.remove();
        }
        let window = self.window.downgrade();
        let slot = self.no_mods.clone();
        let id = glib::timeout_add_local_once(NO_MODS_TIMEOUT, move || {
            slot.borrow_mut().take();
            if let Some(window) = window.upgrade() {
                let _ = WidgetExt::activate_action(&window, "monitor-switch.finish", None);
            }
        });
        *self.no_mods.borrow_mut() = Some(id);
    }

    fn select(&self, index: usize) {
        self.selected.set(index);
        for (i, (_, tile)) in self.items.borrow().iter().enumerate() {
            if i == index {
                tile.add_css_class("selected");
            } else {
                tile.remove_css_class("selected");
            }
        }
    }

    fn step(&self, backward: bool) {
        let len = self.items.borrow().len();
        if !self.open.get() || len == 0 {
            return;
        }
        self.select(step(self.selected.get(), len, backward));
        // A key press shows the popup at once (`_showImmediately`).
        self.window.set_opacity(1.0);
        if self.mask.get().is_empty() {
            self.reset_no_mods();
        }
    }

    /// Read the seat keyboard's modifiers a moment later (after the
    /// compositor's `wl_keyboard.modifiers` for the latest key).
    fn check_seat_modifiers(&self) {
        let window = self.window.downgrade();
        glib::timeout_add_local_once(Duration::from_millis(30), move || {
            let Some(window) = window.upgrade() else {
                return;
            };
            let state = gtk::gdk::Display::default()
                .and_then(|d| d.default_seat())
                .and_then(|s| s.keyboard())
                .map(|k| k.modifier_state())
                .unwrap_or_else(gtk::gdk::ModifierType::empty);
            let _ = WidgetExt::activate_action(
                &window,
                "monitor-switch.modifiers",
                Some(&state.bits().to_variant()),
            );
        });
    }

    fn modifiers_changed(&self, state: gtk::gdk::ModifierType) {
        if !self.open.get() {
            return;
        }
        let mask = self.mask.get();
        if mask.is_empty() {
            self.reset_no_mods();
        } else if released(mask.bits(), state.bits()) {
            self.finish();
        }
    }

    /// `_finish`: apply the selected choice.
    fn finish(&self) {
        let chosen = self
            .items
            .borrow()
            .get(self.selected.get())
            .map(|(item, _)| item.config);
        self.close(chosen);
    }

    fn close(&self, chosen: Option<MonitorSwitchConfig>) {
        if !self.open.replace(false) {
            return;
        }
        for slot in [&self.delay, &self.no_mods] {
            if let Some(id) = slot.borrow_mut().take() {
                id.remove();
            }
        }
        self.window.set_visible(false);
        if let Some(config) = chosen {
            (self.choose)(config);
        }
    }

    /// The popup's finish and modifier check as window actions, for timers
    /// that hold only the window.
    fn install_actions(self: &Rc<Self>) {
        let finish = gio::SimpleAction::new("finish", None);
        let weak = Rc::downgrade(self);
        finish.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                ui.finish();
            }
        });
        let modifiers = gio::SimpleAction::new("modifiers", Some(glib::VariantTy::UINT32));
        let weak = Rc::downgrade(self);
        modifiers.connect_activate(move |_, state| {
            let state = state.and_then(|v| v.get::<u32>()).unwrap_or(0);
            if let Some(ui) = weak.upgrade() {
                ui.modifiers_changed(gtk::gdk::ModifierType::from_bits_truncate(state));
            }
        });
        let group = gio::SimpleActionGroup::new();
        group.add_action(&finish);
        group.add_action(&modifiers);
        self.window
            .insert_action_group("monitor-switch", Some(&group));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(has_builtin: bool) -> Vec<&'static str> {
        items(has_builtin).iter().map(|item| item.label).collect()
    }

    #[test]
    fn the_popup_offers_gnomes_choices() {
        // switchMonitor.js: External Only and Built-in Only need a panel.
        assert_eq!(labels(false), ["Mirror", "Join Displays"]);
        assert_eq!(
            labels(true),
            ["Mirror", "Join Displays", "External Only", "Built-in Only"]
        );
        let all = items(true);
        assert_eq!(all[0].config, MonitorSwitchConfig::AllMirror);
        assert_eq!(all[1].config, MonitorSwitchConfig::AllLinear);
        assert_eq!(all[2].config, MonitorSwitchConfig::External);
        assert_eq!(all[3].config, MonitorSwitchConfig::Builtin);
        assert_eq!(all[2].icon, "shell-display-external-only-symbolic");
    }

    #[test]
    fn the_choice_after_the_one_in_effect_starts_selected() {
        // `(get_switch_config() + 1) % items.length`, UNKNOWN being 4.
        assert_eq!(initial_selection(None, 4), 1);
        assert_eq!(
            initial_selection(Some(MonitorSwitchConfig::AllMirror), 4),
            1
        );
        assert_eq!(
            initial_selection(Some(MonitorSwitchConfig::AllLinear), 4),
            2
        );
        assert_eq!(initial_selection(Some(MonitorSwitchConfig::External), 4), 3);
        assert_eq!(initial_selection(Some(MonitorSwitchConfig::Builtin), 4), 0);
        // Without a panel the list is two long and wraps sooner.
        assert_eq!(initial_selection(None, 2), 1);
        assert_eq!(
            initial_selection(Some(MonitorSwitchConfig::AllMirror), 2),
            1
        );
        assert_eq!(
            initial_selection(Some(MonitorSwitchConfig::AllLinear), 2),
            0
        );
        assert_eq!(step(3, 4, false), 0);
        assert_eq!(step(0, 4, true), 3);
    }
}
