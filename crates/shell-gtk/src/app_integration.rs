//! What gnome-shell 51's date menu shares between its World Clocks and
//! Weather sections (`js/ui/dateMenu.js` `WorldClocksSection`,
//! `js/misc/weather.js` `WeatherClient`): the app's desktop entry
//! (`Shell.AppSystem.lookup_app`), the settings the app exports on the
//! session bus (`org.gnome.Shell.ClocksIntegration`,
//! `org.gnome.Shell.WeatherIntegration`), and the GWeather locations
//! those settings serialize.
//!
//! GNOME reads the exported properties through a `Gio.DBusProxy` made
//! with `DO_NOT_AUTO_START | GET_INVALIDATED_PROPERTIES`. Here a name
//! watch and `Properties.GetAll` (never auto-starting) do the same job:
//! the properties are read when the app appears and again on every
//! `PropertiesChanged`.
//!
//! The `libgweather` crate types four calls as never returning NULL
//! although libgweather 4.4.2 documents or implements a NULL return
//! (`gweather_location_deserialize`, `gweather_location_find_nearest_city`,
//! `gweather_location_new_detached`, `gweather_info_get_symbolic_icon_name`).
//! The wrappers below call the same functions and keep the NULL.

use std::rc::Rc;

use gio::prelude::*;
use glib::translate::*;
use libgweather::ffi;

/// An installed app as `Shell.AppSystem.lookup_app` finds it.
pub struct InstalledApp {
    pub entry: tuna_shell_host::apps::AppEntry,
    /// weather.js `app_info.has_key('X-Flatpak')`.
    pub flatpak: bool,
}

/// The app `desktop_id` (`org.gnome.clocks.desktop`) names: the first
/// entry in XDG precedence order. As in GNOME's app cache a `NoDisplay`
/// entry counts and a `Hidden` one masks every lower copy.
pub fn installed_app(desktop_id: &str) -> Option<InstalledApp> {
    let path = tuna_shell_host::apps::default_app_dirs()
        .into_iter()
        .map(|dir| dir.join(desktop_id))
        .find(|path| path.is_file())?;
    let entry = tuna_shell_host::apps::handler_entry_from_file(&path)?;
    let flatpak = std::fs::read_to_string(&path).is_ok_and(|text| is_flatpak_entry(&text));
    Some(InstalledApp { entry, flatpak })
}

/// Whether a desktop file carries `X-Flatpak` in its `Desktop Entry`
/// group (Flatpak writes it into every entry it exports).
pub fn is_flatpak_entry(desktop_file: &str) -> bool {
    let keys = glib::KeyFile::new();
    keys.load_from_data(desktop_file, glib::KeyFileFlags::NONE)
        .is_ok()
        && keys.has_key("Desktop Entry", "X-Flatpak").unwrap_or(false)
}

/// GSettings for `schema` when it is installed and has every key in
/// `keys`; reading a key a schema lacks aborts the process.
pub fn settings_with_keys(schema: &str, keys: &[&str]) -> Option<gio::Settings> {
    let settings = crate::settings(schema)?;
    let definition = settings.settings_schema()?;
    keys.iter()
        .all(|key| definition.has_key(key))
        .then_some(settings)
}

/// Follow `iface`'s properties on `name` at `path` without ever
/// starting the app: `on_properties` gets the `a{sv}` of every
/// `GetAll` reply, when the name appears and after each
/// `PropertiesChanged`.
pub fn watch_exported(
    name: &'static str,
    path: &'static str,
    iface: &'static str,
    on_properties: impl Fn(&glib::Variant) + 'static,
) {
    let on_properties: Rc<dyn Fn(&glib::Variant)> = Rc::new(on_properties);
    gio::bus_get(
        gio::BusType::Session,
        None::<&gio::Cancellable>,
        move |conn| {
            let Ok(conn) = conn else { return };
            let read: Rc<dyn Fn()> = {
                let conn = conn.clone();
                Rc::new(move || {
                    let on_properties = on_properties.clone();
                    conn.call(
                        Some(name),
                        path,
                        "org.freedesktop.DBus.Properties",
                        "GetAll",
                        Some(&(iface,).to_variant()),
                        glib::VariantTy::new("(a{sv})").ok(),
                        gio::DBusCallFlags::NO_AUTO_START,
                        5000,
                        None::<&gio::Cancellable>,
                        move |reply| {
                            if let Some(properties) = reply.ok().and_then(|r| r.try_child_value(0))
                            {
                                on_properties(&properties);
                            }
                        },
                    );
                })
            };
            let changed = read.clone();
            let sub = conn.subscribe_to_signal(
                Some(name),
                Some("org.freedesktop.DBus.Properties"),
                Some("PropertiesChanged"),
                Some(path),
                Some(iface),
                gio::DBusSignalFlags::NONE,
                move |_| changed(),
            );
            // The subscription lives as long as the shell.
            std::mem::forget(sub);
            crate::services::watch_name(&conn, name, move |owned| {
                if owned {
                    read();
                }
            });
        },
    );
}

/// One property of a `GetAll` reply's `a{sv}`, when it has type `ty`.
pub fn exported_property(
    properties: &glib::Variant,
    name: &str,
    ty: &str,
) -> Option<glib::Variant> {
    if properties.type_().as_str() != "a{sv}" {
        return None;
    }
    glib::VariantDict::new(Some(properties))
        .lookup_value(name, None)
        .filter(|value| value.type_().as_str() == ty)
}

/// GJS `deepUnpack()` of an `av`: the boxed values, in order. Anything
/// that is not an `av` holds no locations.
pub fn unpack_av(value: &glib::Variant) -> Vec<glib::Variant> {
    if value.type_().as_str() != "av" {
        return Vec::new();
    }
    value
        .iter()
        .filter_map(|boxed| boxed.as_variant())
        .collect()
}

/// Which copy of an app's setting the shell uses: gnome-shell's cache
/// when its schema is installed (even when that holds nothing yet),
/// else what the app exports while it runs, else the app's own schema.
pub fn effective_setting<T>(shell: Option<T>, exported: Option<T>, app: Option<T>) -> Option<T> {
    shell.or(exported).or(app)
}

/// `GWeather.Location.deserialize`: `None` for anything libgweather
/// does not recognize (it answers NULL for another variant type, an
/// unknown format number or an unknown station code).
pub fn deserialize_location(
    world: &libgweather::Location,
    serialized: &glib::Variant,
) -> Option<libgweather::Location> {
    // SAFETY: both pointers are live for the call; the result is a new
    // reference or NULL.
    unsafe {
        from_glib_full(ffi::gweather_location_deserialize(
            world.to_glib_none().0,
            serialized.to_glib_none().0,
        ))
    }
}

/// `GWeather.Location.find_nearest_city` (degrees), `None` when the
/// database holds no city.
pub fn nearest_city(
    world: &libgweather::Location,
    lat: f64,
    lon: f64,
) -> Option<libgweather::Location> {
    // SAFETY: `world` is live for the call; the result is a new
    // reference or NULL.
    unsafe {
        from_glib_full(ffi::gweather_location_find_nearest_city(
            world.to_glib_none().0,
            lat,
            lon,
        ))
    }
}

/// `GWeather.Location.new_detached('', null, lat, lon)` (degrees): the
/// empty name lets GWeather name the place after its nearest city.
/// `None` without the locations database.
pub fn detached_location(lat: f64, lon: f64) -> Option<libgweather::Location> {
    // SAFETY: the name is a NUL-terminated string for the call and a
    // NULL station code is allowed; the result is a new reference or
    // NULL.
    unsafe {
        from_glib_full(ffi::gweather_location_new_detached(
            "".to_glib_none().0,
            std::ptr::null(),
            lat,
            lon,
        ))
    }
}

/// `GWeather.Info.get_symbolic_icon_name`, `None` for a sky value the
/// library does not recognize.
pub fn symbolic_icon_name(info: &libgweather::Info) -> Option<String> {
    // SAFETY: `info` is live for the call; the result is a static string
    // or NULL, copied here.
    let name: Option<glib::GString> = unsafe {
        from_glib_none(ffi::gweather_info_get_symbolic_icon_name(
            info.to_glib_none().0,
        ))
    };
    name.map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatpak_entries_are_told_by_their_key() {
        assert!(is_flatpak_entry(
            "[Desktop Entry]\nName=Weather\nExec=flatpak run org.gnome.Weather\nX-Flatpak=org.gnome.Weather\n"
        ));
        assert!(!is_flatpak_entry(
            "[Desktop Entry]\nName=Weather\nExec=gnome-weather\n"
        ));
        // The key only counts in the entry's own group.
        assert!(!is_flatpak_entry(
            "[Desktop Entry]\nName=Weather\nExec=gnome-weather\n\n[Desktop Action x]\nX-Flatpak=1\n"
        ));
        assert!(!is_flatpak_entry("not a key file"));
    }

    fn properties(text: &str) -> glib::Variant {
        glib::Variant::parse(glib::VariantTy::new("a{sv}").ok(), text).unwrap()
    }

    #[test]
    fn exported_properties_are_read_by_name_and_type() {
        let props = properties(
            "{'AutomaticLocation': <true>, 'Locations': <[<(uint32 2, <('Lisbon', 'LPPT', true, [(0.67, -0.15)], [(0.67, -0.15)])>)>]>}",
        );
        assert_eq!(
            exported_property(&props, "AutomaticLocation", "b").and_then(|v| v.get::<bool>()),
            Some(true)
        );
        let locations = exported_property(&props, "Locations", "av").unwrap();
        let unpacked = unpack_av(&locations);
        assert_eq!(unpacked.len(), 1);
        assert_eq!(unpacked[0].type_().as_str(), "(uv)");
        // A property of another type is not taken for the one asked for,
        // and a reply that is no dictionary holds nothing.
        assert!(exported_property(&props, "Locations", "as").is_none());
        assert!(exported_property(&props, "Missing", "av").is_none());
        assert!(exported_property(&"Locations".to_variant(), "Locations", "av").is_none());
    }

    #[test]
    fn only_an_av_unpacks_to_locations() {
        let empty = glib::Variant::parse(glib::VariantTy::new("av").ok(), "[]").unwrap();
        assert!(unpack_av(&empty).is_empty());
        let names = glib::Variant::parse(glib::VariantTy::new("as").ok(), "['London']").unwrap();
        assert!(unpack_av(&names).is_empty());
        assert!(unpack_av(&true.to_variant()).is_empty());
    }

    #[test]
    fn shell_cache_wins_then_the_running_app_then_its_own_settings() {
        // gnome-shell's schema is authoritative even when it holds
        // nothing: the app's other copies must not resurface.
        assert_eq!(
            effective_setting(Some(Vec::<&str>::new()), Some(vec!["a"]), Some(vec!["b"])),
            Some(Vec::new())
        );
        assert_eq!(
            effective_setting(Some(false), Some(true), Some(true)),
            Some(false)
        );
        assert_eq!(
            effective_setting(None, Some(vec!["a"]), Some(vec!["b"])),
            Some(vec!["a"])
        );
        assert_eq!(effective_setting(None, None, Some(true)), Some(true));
        assert_eq!(effective_setting::<bool>(None, None, None), None);
    }
}
