//! GNOME 51's World Clocks card in the date menu (gnome-shell 51.0
//! `js/ui/dateMenu.js`, class `WorldClocksSection`, lines 331-541).
//!
//! The card lists GNOME Clocks' world clocks: city, the time there and
//! its offset from local time, sorted by standard UTC offset. The header
//! reads "World Clocks", or "Add World Clocks…" while there are none.
//! A click activates `org.gnome.clocks.desktop` and closes the menu; the
//! card is hidden while Clocks is not installed. Times follow the panel
//! clock's tick and `clock-format`.
//!
//! Where the locations come from, in GNOME's order of preference:
//!
//! 1. `org.gnome.shell.world-clocks` `locations`, gnome-shell's own
//!    cache, when that schema is installed. As in GNOME it is rewritten
//!    from the `Locations` property Clocks exports on the session bus
//!    (`org.gnome.Shell.ClocksIntegration`) whenever Clocks runs.
//! 2. Without gnome-shell's schema (Tuna Desktop does not ship it), the
//!    exported `Locations` while Clocks runs, else Clocks' own
//!    `org.gnome.clocks` `world-clocks` when that schema is installed.
//!    This fallback is Tuna Desktop's; a Flatpak Clocks keeps its
//!    settings in its sandbox, so its clocks then show only while it
//!    runs.
//!
//! Other divergences from gnome-shell: a UTC offset that is not a whole
//! number of minutes is truncated to the minute (JavaScript would print
//! the fraction; no zone in use has one), and GTK mirrors the grid for
//! right-to-left locales itself, so the explicit column swap is not
//! reproduced.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use gtk::prelude::*;
use gtk4 as gtk;

use crate::app_integration::{self, InstalledApp};
use crate::live_apps::LiveApps;
use crate::logic::{self, ClockFormat};

pub const CLOCKS_APP_ID: &str = "org.gnome.clocks.desktop";

const SHELL_SCHEMA: &str = "org.gnome.shell.world-clocks";
const SHELL_LOCATIONS: &str = "locations";
const CLOCKS_SCHEMA: &str = "org.gnome.clocks";
const CLOCKS_WORLD_CLOCKS: &str = "world-clocks";
const CLOCKS_BUS_NAME: &str = "org.gnome.clocks";
const CLOCKS_OBJECT_PATH: &str = "/org/gnome/clocks";
const CLOCKS_INTEGRATION_IFACE: &str = "org.gnome.Shell.ClocksIntegration";

/// The header: `_clocksChanged`'s title.
pub fn title(clocks: usize) -> &'static str {
    if clocks == 0 {
        "Add World Clocks…"
    } else {
        "World Clocks"
    }
}

/// A location's offset from local time (`_getTimezoneOffsetAtLocation`),
/// both in seconds east of UTC: `+1`, `-8`, `+0`, and `+5:30` or
/// `+5:45` for a fraction of an hour (the minutes are not padded).
pub fn offset_label(location_offset: i32, local_offset: i32) -> String {
    let difference = i64::from(location_offset) - i64::from(local_offset);
    let hours = difference.abs() / 3600;
    let minutes = difference.abs() % 3600 / 60;
    let prefix = if difference >= 0 { '+' } else { '-' };
    if minutes == 0 {
        format!("{prefix}{hours}")
    } else {
        format!("{prefix}{hours}:{minutes}")
    }
}

/// `_clocksChanged`'s order: ascending standard-time UTC offset (the
/// key, in seconds); equal offsets keep their order in the settings.
pub fn sort_by_standard_offset<T>(clocks: &mut [(i32, T)]) {
    clocks.sort_by_key(|(offset, _)| *offset);
}

/// The serialized locations in Clocks' own `world-clocks` setting, an
/// `aa{sv}` whose entries hold the location under `location`. Entries
/// without one, and any other value, give nothing.
pub fn clocks_app_locations(world_clocks: &glib::Variant) -> Vec<glib::Variant> {
    if world_clocks.type_().as_str() != "aa{sv}" {
        return Vec::new();
    }
    world_clocks
        .iter()
        .filter_map(|clock| glib::VariantDict::new(Some(&clock)).lookup_value("location", None))
        .collect()
}

/// One row: the zone and the labels its tick rewrites.
struct Clock {
    zone: glib::TimeZone,
    city: gtk::Label,
    time: gtk::Label,
    offset: gtk::Label,
}

pub struct WorldClocksSection {
    pub button: gtk::Button,
    grid: gtk::Grid,
    header: gtk::Label,
    clocks: RefCell<Vec<Clock>>,
    world: Option<libgweather::Location>,
    shell: Option<gio::Settings>,
    clocks_app: Option<gio::Settings>,
    /// The `Locations` Clocks last exported; kept only without
    /// gnome-shell's schema to cache it in.
    exported: RefCell<Option<Vec<glib::Variant>>>,
    app: RefCell<Option<InstalledApp>>,
    clock_format: Cell<ClockFormat>,
}

impl WorldClocksSection {
    pub fn new(apps: &Rc<LiveApps>) -> Rc<Self> {
        let button = gtk::Button::new();
        button.add_css_class("world-clocks-button");
        // _calendar.scss `.world-clocks-grid`: $base_padding between rows,
        // twice that between columns.
        let grid = gtk::Grid::new();
        grid.add_css_class("world-clocks-grid");
        grid.set_row_spacing(6);
        grid.set_column_spacing(12);
        let header = gtk::Label::new(None);
        header.add_css_class("world-clocks-header");
        header.set_halign(gtk::Align::Start);
        grid.attach(&header, 0, 0, 2, 1);
        button.set_child(Some(&grid));

        let section = Rc::new(Self {
            button,
            grid,
            header,
            clocks: RefCell::default(),
            world: libgweather::Location::world(),
            shell: app_integration::settings_with_keys(SHELL_SCHEMA, &[SHELL_LOCATIONS]),
            clocks_app: app_integration::settings_with_keys(CLOCKS_SCHEMA, &[CLOCKS_WORLD_CLOCKS]),
            exported: RefCell::default(),
            app: RefCell::default(),
            clock_format: Cell::new(ClockFormat::TwentyFourHour),
        });
        for settings in [&section.shell, &section.clocks_app].into_iter().flatten() {
            let weak = Rc::downgrade(&section);
            settings.connect_changed(None, move |_, _| {
                if let Some(section) = weak.upgrade() {
                    section.clocks_changed();
                }
            });
        }
        {
            let weak = Rc::downgrade(&section);
            app_integration::watch_exported(
                CLOCKS_BUS_NAME,
                CLOCKS_OBJECT_PATH,
                CLOCKS_INTEGRATION_IFACE,
                move |properties| {
                    if let Some(section) = weak.upgrade() {
                        section.clocks_properties_changed(properties);
                    }
                },
            );
        }
        {
            let weak = Rc::downgrade(&section);
            apps.connect_changed(move || {
                if let Some(section) = weak.upgrade() {
                    section.sync();
                }
            });
        }
        section.clocks_changed();
        section.sync();
        section
    }

    /// `vfunc_clicked`: activate Clocks, then close the menu.
    pub fn connect_activated(self: &Rc<Self>, close: Rc<dyn Fn()>) {
        let weak = Rc::downgrade(self);
        self.button.connect_clicked(move |_| {
            let Some(section) = weak.upgrade() else {
                return;
            };
            if let Some(app) = section.app.borrow().as_ref() {
                if let Err(e) = tuna_shell_host::apps::launch(&app.entry) {
                    eprintln!("tuna-shell-gtk: could not open Clocks: {e}");
                }
            }
            close();
        });
    }

    /// The wall clock ticked, its zone moved or `clock-format` changed
    /// (`notify::clock` and `notify::timezone` on GNOME's wall clock).
    pub fn tick(&self, format: ClockFormat) {
        self.clock_format.set(format);
        self.update_labels();
    }

    /// `_sync`: shown exactly while Clocks is installed.
    fn sync(&self) {
        let app = app_integration::installed_app(CLOCKS_APP_ID);
        self.button.set_visible(app.is_some());
        *self.app.borrow_mut() = app;
    }

    /// `_onClocksPropertiesChanged`: copy the exported locations into
    /// gnome-shell's cache, or keep them here without one.
    fn clocks_properties_changed(&self, properties: &glib::Variant) {
        let Some(locations) = app_integration::exported_property(properties, "Locations", "av")
        else {
            return;
        };
        match &self.shell {
            Some(shell) => {
                if let Err(e) = shell.set_value(SHELL_LOCATIONS, &locations) {
                    eprintln!("tuna-shell-gtk: could not store world clocks: {e}");
                }
            }
            None => {
                *self.exported.borrow_mut() = Some(app_integration::unpack_av(&locations));
                self.clocks_changed();
            }
        }
    }

    /// The serialized locations, from the first source that has them.
    fn serialized_locations(&self) -> Vec<glib::Variant> {
        app_integration::effective_setting(
            self.shell
                .as_ref()
                .map(|shell| app_integration::unpack_av(&shell.value(SHELL_LOCATIONS))),
            self.exported.borrow().clone(),
            self.clocks_app
                .as_ref()
                .map(|clocks| clocks_app_locations(&clocks.value(CLOCKS_WORLD_CLOCKS))),
        )
        .unwrap_or_default()
    }

    /// `_clocksChanged`: rebuild the rows from the settings.
    fn clocks_changed(&self) {
        for clock in self.clocks.take() {
            self.grid.remove(&clock.city);
            self.grid.remove(&clock.time);
            self.grid.remove(&clock.offset);
        }
        let mut found: Vec<(i32, (String, glib::TimeZone))> = Vec::new();
        if let Some(world) = &self.world {
            let now = glib::real_time() / 1_000_000;
            for serialized in self.serialized_locations() {
                let Some(location) = app_integration::deserialize_location(world, &serialized)
                else {
                    continue;
                };
                let Some(zone) = location.timezone() else {
                    continue;
                };
                let name = location
                    .city_name()
                    .filter(|name| !name.is_empty())
                    .or_else(|| location.name())
                    .map(String::from)
                    .unwrap_or_default();
                let standard = zone.offset(zone.find_interval(glib::TimeType::Standard, now));
                found.push((standard, (name, zone)));
            }
        }
        sort_by_standard_offset(&mut found);

        let title = title(found.len());
        self.header.set_text(title);
        if found.is_empty() {
            self.header.add_css_class("no-world-clocks");
        } else {
            self.header.remove_css_class("no-world-clocks");
        }
        // GNOME's `label_actor`: the card is named by its header.
        self.button
            .update_property(&[gtk::accessible::Property::Label(title)]);

        let mut clocks = Vec::with_capacity(found.len());
        for (row, (_, (name, zone))) in found.into_iter().enumerate() {
            let row = row as i32 + 1;
            let city = gtk::Label::new(Some(&name));
            city.add_css_class("world-clocks-city");
            city.set_halign(gtk::Align::Start);
            city.set_valign(gtk::Align::Center);
            city.set_hexpand(true);
            city.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let time = gtk::Label::new(None);
            time.add_css_class("world-clocks-time");
            time.set_xalign(1.0);
            let offset = gtk::Label::new(None);
            offset.add_css_class("world-clocks-timezone");
            offset.set_halign(gtk::Align::End);
            offset.set_valign(gtk::Align::Center);
            self.grid.attach(&city, 0, row, 1, 1);
            self.grid.attach(&time, 1, row, 1, 1);
            self.grid.attach(&offset, 2, row, 1, 1);
            clocks.push(Clock {
                zone,
                city,
                time,
                offset,
            });
        }
        *self.clocks.borrow_mut() = clocks;
        self.update_labels();
    }

    /// `_updateTimeLabels` and `_updateTimezoneLabels`.
    fn update_labels(&self) {
        let format = self.clock_format.get();
        let local_offset = crate::wall_clock::now().offset().seconds();
        for clock in self.clocks.borrow().iter() {
            let Ok(now) = glib::DateTime::now(&clock.zone) else {
                continue;
            };
            clock.time.set_text(&logic::time_only(
                now.hour() as i8,
                now.minute() as i8,
                format,
                true,
            ));
            let offset = now.utc_offset().as_seconds() as i32;
            clock.offset.set_text(&offset_label(offset, local_offset));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_asks_for_clocks_until_there_are_some() {
        assert_eq!(title(0), "Add World Clocks…");
        assert_eq!(title(1), "World Clocks");
        assert_eq!(title(7), "World Clocks");
    }

    #[test]
    fn offsets_read_as_gnome_writes_them() {
        const H: i32 = 3600;
        // London seen from Berlin, and the reverse.
        assert_eq!(offset_label(0, H), "-1");
        assert_eq!(offset_label(H, 0), "+1");
        // The local zone itself is "+0".
        assert_eq!(offset_label(2 * H, 2 * H), "+0");
        assert_eq!(offset_label(-8 * H, H), "-9");
        assert_eq!(offset_label(14 * H, -11 * H), "+25");
        // Fractions of an hour: Kolkata and Kathmandu from UTC, St. John's
        // from UTC, and Kolkata from Kathmandu.
        assert_eq!(offset_label(5 * H + 1800, 0), "+5:30");
        assert_eq!(offset_label(5 * H + 2700, 0), "+5:45");
        assert_eq!(offset_label(-(3 * H + 1800), 0), "-3:30");
        assert_eq!(offset_label(5 * H + 1800, 5 * H + 2700), "-0:15");
        // Minutes are not zero-padded, exactly as the template string.
        assert_eq!(offset_label(300, 0), "+0:5");
    }

    #[test]
    fn clocks_sort_by_standard_offset_and_keep_ties_in_settings_order() {
        let mut clocks = vec![
            (9 * 3600, "Tokyo"),
            (0, "London"),
            (-5 * 3600, "New York"),
            (0, "Lisbon"),
            (5 * 3600 + 1800, "Kolkata"),
            (0, "Reykjavik"),
        ];
        sort_by_standard_offset(&mut clocks);
        let names: Vec<&str> = clocks.iter().map(|(_, name)| *name).collect();
        assert_eq!(
            names,
            [
                "New York",
                "London",
                "Lisbon",
                "Reykjavik",
                "Kolkata",
                "Tokyo"
            ]
        );
    }

    fn variant(ty: &str, text: &str) -> glib::Variant {
        glib::Variant::parse(glib::VariantTy::new(ty).ok(), text).unwrap()
    }

    const LONDON: &str =
        "(uint32 2, <('London', 'EGWU', true, [(0.8997, -0.0073)], [(0.8988, -0.0020)])>)";
    const TOKYO: &str =
        "(uint32 2, <('Tokyo', 'RJTI', true, [(0.6219, 2.4408)], [(0.6228, 2.4391)])>)";

    #[test]
    fn clocks_own_setting_yields_each_location() {
        let setting = variant(
            "aa{sv}",
            &format!("[{{'location': <{LONDON}>}}, {{'location': <{TOKYO}>}}]"),
        );
        let locations = clocks_app_locations(&setting);
        assert_eq!(locations, [variant("(uv)", LONDON), variant("(uv)", TOKYO)]);
    }

    #[test]
    fn clocks_own_setting_skips_what_is_not_a_location_entry() {
        // An entry without `location` is skipped; the rest still count.
        let setting = variant(
            "aa{sv}",
            &format!("[{{'name': <'stray'>}}, {{'location': <{TOKYO}>}}]"),
        );
        assert_eq!(clocks_app_locations(&setting), [variant("(uv)", TOKYO)]);
        assert!(clocks_app_locations(&variant("aa{sv}", "[]")).is_empty());
        // gnome-shell's `av` is not Clocks' format.
        assert!(clocks_app_locations(&variant("av", &format!("[<{LONDON}>]"))).is_empty());
    }
}
