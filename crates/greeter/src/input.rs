//! The login screen's input settings and its input-source indicator
//! (#1164).
//!
//! GNOME's login screen runs inside Mutter, which reads the greeter
//! user's keyboard, pointer, keyboard-aid and bell settings itself; here
//! they go to the compositor as `SetInputSettings`, now and on every
//! change, read by the shell's own reader
//! (`tuna_shell_control::gsettings`). Visual Alerts and Sticky, Slow,
//! Bounce and Mouse Keys take effect through it.
//!
//! The sources: gnome-shell 51.1's `keyboard.js` takes the login screen's
//! sources from localed (`InputSourceSystemSettings`: `X11Layout`,
//! `X11Variant`, `X11Options` of `org.freedesktop.locale1`, re-read on
//! `PropertiesChanged`) and ignores the greeter user's
//! `org.gnome.desktop.input-sources`. Tuna's greeter uses the greeter
//! user's `sources` when that list is set and localed's layouts
//! otherwise (docs/greeter.md, deviations): GDM keeps the greeter user's
//! settings across boots, so a source chosen for the login screen stays.
//!
//! The panel's keyboard indicator (`InputSourceIndicator`, named
//! "Keyboard") shows the current source's short name, hides with fewer
//! than two sources, and lists every source by its xkeyboard-config
//! name; choosing one switches to it. The compositor holds the sources
//! as the layouts of one keymap and switches by relative steps
//! (`SwitchInputSource`), keeping the active layout across keymap
//! reloads while it still exists; this module mirrors that.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use tuna_osk::xkb::{unique_short_names, XkbNames};
use tuna_shell_control::InputSettings;

use crate::control::Control;
use crate::i18n::tr;

const LOCALED: &str = "org.freedesktop.locale1";
const LOCALED_PATH: &str = "/org/freedesktop/locale1";

/// localed's X11 keymap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemKeymap {
    pub layout: String,
    pub variant: String,
    pub options: String,
}

/// The XKB source ids of a layout and variant list (`InputSourceSystemSettings.inputSources`).
pub fn system_sources(keymap: &SystemKeymap) -> Vec<String> {
    let variants: Vec<&str> = keymap.variant.split(',').collect();
    keymap
        .layout
        .split(',')
        .enumerate()
        .take_while(|(_, layout)| !layout.is_empty())
        .map(
            |(i, layout)| match variants.get(i).filter(|v| !v.is_empty()) {
                Some(variant) => format!("{layout}+{variant}"),
                None => layout.to_owned(),
            },
        )
        .collect()
}

/// The greeter user's XKB sources (`sources`, IBus left out).
pub fn user_sources(list: &[(String, String)]) -> Vec<String> {
    list.iter()
        .filter(|(kind, _)| kind == "xkb")
        .map(|(_, id)| id.clone())
        .collect()
}

/// Where the login screen's sources come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    User,
    Localed,
    Default,
}

impl Origin {
    fn name(self) -> &'static str {
        match self {
            Origin::User => "user",
            Origin::Localed => "localed",
            Origin::Default => "default",
        }
    }
}

/// The greeter user's sources when set, else localed's; `settings` gets
/// the chosen keymap.
pub fn choose_sources(
    user: Vec<String>,
    system: Option<&SystemKeymap>,
    settings: &mut InputSettings,
) -> (Vec<String>, Origin) {
    if !user.is_empty() {
        return (user, Origin::User);
    }
    if let Some(system) = system {
        let sources = system_sources(system);
        if !sources.is_empty() {
            settings.xkb_layout = system.layout.clone();
            settings.xkb_variant = if system.variant.split(',').all(str::is_empty) {
                String::new()
            } else {
                system.variant.clone()
            };
            settings.xkb_options = system.options.clone();
            return (sources, Origin::Localed);
        }
    }
    (vec!["us".to_owned()], Origin::Default)
}

/// Forward steps from `from` to `to` among `n` sources.
pub fn steps(from: usize, to: usize, n: usize) -> usize {
    if n == 0 {
        0
    } else {
        (to % n + n - from % n) % n
    }
}

/// Told the current source's XKB id after every change.
type SourceListener = Box<dyn Fn(&str)>;

pub struct InputSources {
    control: Rc<Control>,
    schemas: Vec<Option<gio::Settings>>,
    system: RefCell<Option<SystemKeymap>>,
    sources: RefCell<Vec<String>>,
    current: Cell<usize>,
    names: XkbNames,
    pub button: gtk::MenuButton,
    label: gtk::Label,
    list: gtk::Box,
    changed: RefCell<Vec<SourceListener>>,
    localed_watch: RefCell<Option<gio::SignalSubscription>>,
}

impl InputSources {
    pub fn new(control: Rc<Control>) -> Rc<Self> {
        let label = gtk::Label::new(None);
        label.add_css_class("input-source-label");
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("input-source-list");
        let popover = gtk::Popover::new();
        popover.set_child(Some(&list));
        let button = gtk::MenuButton::new();
        button.set_child(Some(&label));
        button.add_css_class("flat");
        button.add_css_class("panel-button");
        button.update_property(&[gtk::accessible::Property::Label(&tr("Keyboard"))]);
        button.set_popover(Some(&popover));
        button.set_visible(false);
        let sources = Rc::new(Self {
            control,
            schemas: tuna_shell_control::gsettings::input_schemas(),
            system: RefCell::default(),
            sources: RefCell::default(),
            current: Cell::new(0),
            names: XkbNames::system(),
            button,
            label,
            list,
            changed: RefCell::default(),
            localed_watch: RefCell::default(),
        });
        for settings in sources.schemas.iter().flatten() {
            let weak = Rc::downgrade(&sources);
            settings.connect_changed(None, move |_, _| {
                if let Some(sources) = weak.upgrade() {
                    sources.sync();
                }
            });
        }
        sources.sync();
        sources.watch_localed();
        sources
    }

    /// Call `f` with the current source's XKB id after every change.
    pub fn connect_changed(&self, f: impl Fn(&str) + 'static) {
        self.changed.borrow_mut().push(Box::new(f));
    }

    /// The current source's XKB id (the on-screen keyboard's group).
    pub fn current_id(&self) -> String {
        self.sources
            .borrow()
            .get(self.current.get())
            .cloned()
            .unwrap_or_else(|| "us".to_owned())
    }

    /// Send the input settings and follow the sources they carry.
    fn sync(self: &Rc<Self>) {
        let mut settings = tuna_shell_control::gsettings::input_settings(
            &self.schemas,
            gtk::Widget::default_direction() == gtk::TextDirection::Rtl,
        );
        let user = self
            .schemas
            .first()
            .and_then(Option::as_ref)
            .map(|s| {
                let list: Vec<(String, String)> = s.value("sources").get().unwrap_or_default();
                user_sources(&list)
            })
            .unwrap_or_default();
        let (sources, origin) = choose_sources(user, self.system.borrow().as_ref(), &mut settings);
        crate::trace(&format!(
            "greeter input-settings layout={} variant={} visual-bell={} sticky={} slow={} bounce={} mouse={}",
            settings.xkb_layout,
            settings.xkb_variant,
            settings.bell.visual,
            settings.keyboard_aids.sticky,
            settings.keyboard_aids.slow,
            settings.keyboard_aids.bounce,
            settings.keyboard_aids.mouse,
        ));
        self.control.set_input_settings(settings);
        if *self.sources.borrow() != sources {
            // The compositor keeps the active layout while it exists.
            if self.current.get() >= sources.len() {
                self.current.set(0);
            }
            crate::trace(&format!(
                "greeter input-sources from={} ids={}",
                origin.name(),
                sources.join(",")
            ));
            *self.sources.borrow_mut() = sources;
            self.rebuild();
            self.notify();
        }
    }

    fn rebuild(self: &Rc<Self>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let sources = self.sources.borrow().clone();
        let mut group: Option<gtk::CheckButton> = None;
        for (index, id) in sources.iter().enumerate() {
            let (name, _) = self.names.lookup(id);
            let item = gtk::CheckButton::with_label(&name);
            if let Some(first) = &group {
                item.set_group(Some(first));
            } else {
                group = Some(item.clone());
            }
            item.set_active(index == self.current.get());
            let weak = Rc::downgrade(self);
            item.connect_toggled(move |item| {
                if let Some(sources) = weak.upgrade() {
                    if item.is_active() && sources.current.get() != index {
                        sources.activate(index);
                    }
                }
            });
            self.list.append(&item);
        }
        self.button.set_visible(sources.len() > 1);
    }

    /// `activateInputSource`: switch the compositor to source `index`.
    fn activate(self: &Rc<Self>, index: usize) {
        let n = self.sources.borrow().len();
        for _ in 0..steps(self.current.get(), index, n) {
            self.control.switch_input_source(false);
        }
        self.current.set(index);
        crate::trace(&format!(
            "greeter input-source current={} id={}",
            index,
            self.current_id()
        ));
        self.button.popdown();
        self.notify();
    }

    fn notify(&self) {
        let sources = self.sources.borrow().clone();
        let shorts: Vec<String> = sources.iter().map(|id| self.names.lookup(id).1).collect();
        let shorts = unique_short_names(&shorts);
        if let Some(short) = shorts.get(self.current.get()) {
            self.label.set_label(short);
        }
        let id = self.current_id();
        for f in self.changed.borrow().iter() {
            f(&id);
        }
    }

    /// localed's keymap, now and on every `PropertiesChanged`.
    fn watch_localed(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(system) = gio::bus_get_future(gio::BusType::System).await else {
                return;
            };
            let Some(sources) = weak.upgrade() else {
                return;
            };
            let weak = Rc::downgrade(&sources);
            let watch = system.subscribe_to_signal(
                Some(LOCALED),
                Some("org.freedesktop.DBus.Properties"),
                Some("PropertiesChanged"),
                Some(LOCALED_PATH),
                None,
                gio::DBusSignalFlags::NONE,
                move |_| {
                    if let Some(sources) = weak.upgrade() {
                        sources.load_localed();
                    }
                },
            );
            *sources.localed_watch.borrow_mut() = Some(watch);
            sources.load_localed();
        });
    }

    fn load_localed(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(system) = gio::bus_get_future(gio::BusType::System).await else {
                return;
            };
            let reply = system
                .call_future(
                    Some(LOCALED),
                    LOCALED_PATH,
                    "org.freedesktop.DBus.Properties",
                    "GetAll",
                    Some(&(LOCALED,).to_variant()),
                    Some(glib::VariantTy::new("(a{sv})").expect("valid type")),
                    gio::DBusCallFlags::NONE,
                    -1,
                )
                .await;
            let keymap = match reply {
                Ok(reply) => {
                    let props = glib::VariantDict::new(Some(&reply.child_value(0)));
                    let get = |key: &str| {
                        props
                            .lookup_value(key, Some(glib::VariantTy::STRING))
                            .and_then(|v| v.get::<String>())
                            .unwrap_or_default()
                    };
                    Some(SystemKeymap {
                        layout: get("X11Layout"),
                        variant: get("X11Variant"),
                        options: get("X11Options"),
                    })
                }
                Err(error) => {
                    eprintln!("tuna-greeter: localed: {}", error.message());
                    None
                }
            };
            let Some(sources) = weak.upgrade() else {
                return;
            };
            if *sources.system.borrow() != keymap {
                *sources.system.borrow_mut() = keymap;
                sources.sync();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keymap(layout: &str, variant: &str) -> SystemKeymap {
        SystemKeymap {
            layout: layout.into(),
            variant: variant.into(),
            options: "grp:alt_shift_toggle".into(),
        }
    }

    #[test]
    fn localed_layouts_are_sources_as_keyboard_js_reads_them() {
        assert_eq!(
            system_sources(&keymap("us,de", ",nodeadkeys")),
            ["us", "de+nodeadkeys"]
        );
        assert_eq!(system_sources(&keymap("fr", "")), ["fr"]);
        assert!(system_sources(&keymap("", "")).is_empty());
    }

    #[test]
    fn the_greeter_users_sources_win_over_localed() {
        let mut settings = InputSettings::default();
        let system = keymap("us,de", ",nodeadkeys");
        let (sources, origin) = choose_sources(vec!["fr".into()], Some(&system), &mut settings);
        assert_eq!((sources, origin), (vec!["fr".to_owned()], Origin::User));

        let mut settings = InputSettings::default();
        let (sources, origin) = choose_sources(Vec::new(), Some(&system), &mut settings);
        assert_eq!(origin, Origin::Localed);
        assert_eq!(sources, ["us", "de+nodeadkeys"]);
        assert_eq!(settings.xkb_layout, "us,de");
        assert_eq!(settings.xkb_variant, ",nodeadkeys");
        assert_eq!(settings.xkb_options, "grp:alt_shift_toggle");

        let mut settings = InputSettings::default();
        let (sources, origin) = choose_sources(Vec::new(), None, &mut settings);
        assert_eq!((sources, origin), (vec!["us".to_owned()], Origin::Default));
    }

    #[test]
    fn ibus_sources_are_left_out() {
        let list = vec![
            ("xkb".to_owned(), "us".to_owned()),
            ("ibus".to_owned(), "anthy".to_owned()),
        ];
        assert_eq!(user_sources(&list), ["us"]);
    }

    #[test]
    fn switching_steps_forward_and_wraps() {
        assert_eq!(steps(0, 1, 2), 1);
        assert_eq!(steps(1, 0, 2), 1);
        assert_eq!(steps(2, 1, 3), 2);
        assert_eq!(steps(0, 0, 3), 0);
        assert_eq!(steps(0, 1, 0), 0);
    }
}
