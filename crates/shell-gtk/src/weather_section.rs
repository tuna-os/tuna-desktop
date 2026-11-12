//! GNOME 51's Weather card in the date menu (gnome-shell 51.0
//! `js/ui/dateMenu.js`, class `WeatherSection`, lines 543-740).
//!
//! The card is shown while GNOME Weather is installed. Its header reads
//! "Weather" beside the location's name, or "Select Weather Location…"
//! without a location. Under it sit up to five forecasts at least an
//! hour apart, none earlier than now, each a time (without AM/PM), a
//! symbolic icon and a rounded temperature; or one of "Loading…", "Go
//! Online for Weather Information" and "Weather Information
//! Unavailable". Mapping the card refreshes the forecast and a click
//! activates Weather and closes the menu.
//!
//! Divergences from gnome-shell: the refresh on map runs from an idle
//! callback rather than inside the map handler, and GTK mirrors the
//! grid for right-to-left locales itself, so the forecasts are not
//! reversed by hand.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;

use crate::logic::{self, ClockFormat};
use crate::weather::{Snapshot, WeatherClient};

/// `MAX_FORECASTS`.
pub const MAX_FORECASTS: usize = 5;

/// The header's title (`_sync`).
pub fn title(has_location: bool) -> &'static str {
    if has_location {
        "Weather"
    } else {
        "Select Weather Location…"
    }
}

/// What the card shows under its header (`_sync`, `_updateForecasts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    /// No location: the forecast grid is hidden.
    Hidden,
    /// One status line in place of the forecasts.
    Status(&'static str),
    Forecasts,
}

/// `_updateForecasts`' choice between the forecasts and a status line.
pub fn body(snapshot: &Snapshot) -> Body {
    if !snapshot.has_location {
        Body::Hidden
    } else if snapshot.loading {
        Body::Status("Loading…")
    } else if snapshot.valid {
        Body::Forecasts
    } else if snapshot.network_error {
        Body::Status("Go Online for Weather Information")
    } else {
        Body::Status("Weather Information Unavailable")
    }
}

/// `_getInfos`: the indices of the forecasts to show. `updates` holds
/// each entry's `get_value_update` (Unix seconds, `None` when not
/// valid) in list order and `now` is in microseconds, as GLib compares
/// them. An entry is skipped when it was never updated (0), is earlier
/// than now, or is less than an hour after the last one taken; at most
/// [`MAX_FORECASTS`] are taken.
pub fn select_forecasts(updates: &[Option<i64>], now: i64) -> Vec<usize> {
    const HOUR: i64 = 3600;
    let mut current = 0;
    let mut chosen = Vec::new();
    for (index, update) in updates.iter().enumerate() {
        let Some(timestamp) = *update else { continue };
        if timestamp == 0 || now > timestamp.saturating_mul(1_000_000) {
            continue;
        }
        if timestamp - current < HOUR {
            continue;
        }
        chosen.push(index);
        if chosen.len() == MAX_FORECASTS {
            break;
        }
        current = timestamp;
    }
    chosen
}

/// `_addForecasts`' temperature: rounded as `Math.round` does (halves
/// toward positive infinity), a degree sign after it, and a space in
/// place of the minus sign so the column stays aligned.
pub fn temperature_label(temp: f64) -> String {
    let rounded = (temp + 0.5).floor() as i64;
    let prefix = if rounded >= 0 { " " } else { "" };
    format!("{prefix}{rounded}°")
}

/// `_findBestLocationName`: the nearest city's name when the
/// location's own name contains it, else the location's name.
/// `nearest_city` is `None` for a city or a location without
/// coordinates, which keep their name.
pub fn best_location_name(name: &str, nearest_city: Option<&str>) -> String {
    match nearest_city {
        Some(city) if name.contains(city) => city.to_owned(),
        _ => name.to_owned(),
    }
}

pub struct WeatherSection {
    pub button: gtk::Button,
    title: gtk::Label,
    location: gtk::Label,
    grid: gtk::Grid,
    client: Rc<WeatherClient>,
    clock_format: Cell<ClockFormat>,
}

impl WeatherSection {
    pub fn new(client: Rc<WeatherClient>) -> Rc<Self> {
        let button = gtk::Button::new();
        button.add_css_class("weather-button");
        // _calendar.scss `.weather-box` and `.weather-header-box`:
        // $base_padding apart.
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.add_css_class("weather-box");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.add_css_class("weather-header-box");
        let title = gtk::Label::new(None);
        title.add_css_class("weather-header");
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_valign(gtk::Align::End);
        let location = gtk::Label::new(None);
        location.add_css_class("weather-header");
        location.add_css_class("location");
        location.set_halign(gtk::Align::End);
        location.set_valign(gtk::Align::End);
        location.set_ellipsize(gtk::pango::EllipsizeMode::End);
        header.append(&title);
        header.append(&location);
        // `.weather-grid`: $base_padding between rows, twice that
        // between columns, every forecast the same width.
        let grid = gtk::Grid::new();
        grid.add_css_class("weather-grid");
        grid.set_row_spacing(6);
        grid.set_column_spacing(12);
        grid.set_column_homogeneous(true);
        content.append(&header);
        content.append(&grid);
        button.set_child(Some(&content));

        let section = Rc::new(Self {
            button,
            title,
            location,
            grid,
            client,
            clock_format: Cell::new(ClockFormat::TwentyFourHour),
        });
        {
            let weak = Rc::downgrade(&section);
            section.client.connect_changed(move || {
                if let Some(section) = weak.upgrade() {
                    section.sync();
                }
            });
        }
        {
            // `vfunc_map`: refresh each time the menu shows the card.
            let client = section.client.clone();
            section.button.connect_map(move |_| {
                let client = client.clone();
                glib::idle_add_local_once(move || client.update());
            });
        }
        section.sync();
        section
    }

    /// `vfunc_clicked`: activate Weather, then close the menu.
    pub fn connect_activated(&self, close: Rc<dyn Fn()>) {
        let client = self.client.clone();
        self.button.connect_clicked(move |_| {
            client.activate_app();
            close();
        });
    }

    /// Forecast times follow GNOME's clock-format.
    pub fn set_clock_format(&self, format: ClockFormat) {
        if self.clock_format.replace(format) != format {
            self.sync();
        }
    }

    /// `_sync`.
    fn sync(&self) {
        let snapshot = self.client.snapshot();
        self.button.set_visible(snapshot.available);
        if !snapshot.available {
            return;
        }
        let title = title(snapshot.has_location);
        self.title.set_text(title);
        if snapshot.has_location {
            self.title.remove_css_class("no-location");
        } else {
            self.title.add_css_class("no-location");
        }
        // GNOME's `label_actor`: the card is named by its title.
        self.button
            .update_property(&[gtk::accessible::Property::Label(title)]);
        self.grid.set_visible(snapshot.has_location);
        self.location.set_visible(snapshot.has_location);
        self.update_forecasts(&snapshot);
    }

    /// `_updateForecasts`.
    fn update_forecasts(&self, snapshot: &Snapshot) {
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        let body = body(snapshot);
        if body == Body::Hidden {
            return;
        }
        self.location.set_text(&best_location_name(
            &snapshot.location_name,
            snapshot.nearest_city.as_deref(),
        ));
        match body {
            Body::Hidden => {}
            Body::Status(text) => {
                // `_setStatusLabel`.
                let label = gtk::Label::new(Some(text));
                label.set_xalign(0.0);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                self.grid.attach(&label, 0, 0, 1, 1);
            }
            Body::Forecasts => self.add_forecasts(snapshot),
        }
    }

    /// `_addForecasts`: a column per forecast, time over icon over
    /// temperature.
    fn add_forecasts(&self, snapshot: &Snapshot) {
        let updates: Vec<Option<i64>> = snapshot.forecasts.iter().map(|f| f.update).collect();
        let zone = crate::wall_clock::local_zone();
        let format = self.clock_format.get();
        let chosen = select_forecasts(&updates, glib::real_time());
        for (column, index) in chosen.into_iter().enumerate() {
            let column = column as i32;
            let forecast = &snapshot.forecasts[index];
            let at = jiff::Timestamp::from_second(forecast.update.unwrap_or(0))
                .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
                .to_zoned(zone.clone());
            let time = gtk::Label::new(Some(&logic::time_only(
                at.hour(),
                at.minute(),
                format,
                false,
            )));
            time.add_css_class("weather-forecast-time");
            time.set_halign(gtk::Align::Center);
            let icon = gtk::Image::new();
            icon.set_icon_name(forecast.icon.as_deref());
            icon.add_css_class("weather-forecast-icon");
            icon.set_halign(gtk::Align::Center);
            icon.set_hexpand(true);
            // An unset temperature reads 0, as the out parameter does
            // in gnome-shell.
            let temp = gtk::Label::new(Some(&temperature_label(forecast.temp.unwrap_or(0.0))));
            temp.add_css_class("weather-forecast-temp");
            temp.set_halign(gtk::Align::Center);
            self.grid.attach(&time, column, 0, 1, 1);
            self.grid.attach(&icon, column, 1, 1, 1);
            self.grid.attach(&temp, column, 2, 1, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(has_location: bool, loading: bool, valid: bool, network_error: bool) -> Snapshot {
        Snapshot {
            available: true,
            has_location,
            loading,
            valid,
            network_error,
            ..Snapshot::default()
        }
    }

    #[test]
    fn title_asks_for_a_location_until_there_is_one() {
        assert_eq!(title(false), "Select Weather Location…");
        assert_eq!(title(true), "Weather");
    }

    #[test]
    fn states_map_to_gnome_51_labels() {
        // No location hides the grid whatever else is set.
        assert_eq!(body(&state(false, true, true, true)), Body::Hidden);
        // Loading wins over a stale valid forecast and over an error.
        assert_eq!(
            body(&state(true, true, true, false)),
            Body::Status("Loading…")
        );
        assert_eq!(
            body(&state(true, true, false, true)),
            Body::Status("Loading…")
        );
        assert_eq!(body(&state(true, false, true, false)), Body::Forecasts);
        // A valid forecast shows even when the last fetch failed.
        assert_eq!(body(&state(true, false, true, true)), Body::Forecasts);
        assert_eq!(
            body(&state(true, false, false, true)),
            Body::Status("Go Online for Weather Information")
        );
        assert_eq!(
            body(&state(true, false, false, false)),
            Body::Status("Weather Information Unavailable")
        );
    }

    const NOW: i64 = 1_800_000_000;
    const USEC: i64 = 1_000_000;
    const HOUR: i64 = 3600;

    #[test]
    fn forecasts_are_hourly_from_now_and_capped_at_five() {
        // Hourly entries from two hours ago to nine hours ahead.
        let updates: Vec<Option<i64>> = (-2..10).map(|h| Some(NOW + h * HOUR)).collect();
        // Index 2 is exactly now: not earlier, so it is the first shown.
        assert_eq!(select_forecasts(&updates, NOW * USEC), [2, 3, 4, 5, 6]);
        // A microsecond later that entry is in the past.
        assert_eq!(select_forecasts(&updates, NOW * USEC + 1), [3, 4, 5, 6, 7]);
    }

    #[test]
    fn forecasts_closer_than_an_hour_are_thinned() {
        // Every twenty minutes: only each third entry is an hour on.
        let updates: Vec<Option<i64>> = (1..=12).map(|i| Some(NOW + i * 1200)).collect();
        assert_eq!(select_forecasts(&updates, NOW * USEC), [0, 3, 6, 9]);
        // 59 minutes after the last taken entry is still too close; the
        // full hour is not.
        let updates = [
            Some(NOW + 600),
            Some(NOW + 600 + 59 * 60),
            Some(NOW + 600 + HOUR),
        ];
        assert_eq!(select_forecasts(&updates, NOW * USEC), [0, 2]);
    }

    #[test]
    fn invalid_and_never_updated_forecasts_are_skipped() {
        let updates = [None, Some(0), Some(NOW + HOUR), None, Some(NOW + 2 * HOUR)];
        assert_eq!(select_forecasts(&updates, NOW * USEC), [2, 4]);
        assert!(select_forecasts(&[], NOW * USEC).is_empty());
        assert!(select_forecasts(&[Some(NOW - 1)], NOW * USEC).is_empty());
    }

    #[test]
    fn the_fifth_forecast_ends_the_list_even_when_later_ones_qualify() {
        let updates: Vec<Option<i64>> = (1..=8).map(|h| Some(NOW + h * 2 * HOUR)).collect();
        assert_eq!(select_forecasts(&updates, NOW * USEC), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn temperatures_round_like_math_round_and_pad_non_negatives() {
        assert_eq!(temperature_label(21.4), " 21°");
        assert_eq!(temperature_label(21.5), " 22°");
        assert_eq!(temperature_label(0.0), " 0°");
        // Math.round(-0.4) is -0, which is not below zero and prints "0".
        assert_eq!(temperature_label(-0.4), " 0°");
        // Halves round toward positive infinity.
        assert_eq!(temperature_label(-0.5), " 0°");
        assert_eq!(temperature_label(-1.5), "-1°");
        assert_eq!(temperature_label(-2.6), "-3°");
        assert_eq!(temperature_label(99.5), " 100°");
    }

    #[test]
    fn location_names_prefer_the_nearest_city_they_contain() {
        // A weather station named after its city reads as the city.
        assert_eq!(
            best_location_name("Lisbon Portela Airport", Some("Lisbon")),
            "Lisbon"
        );
        // A name that does not contain the nearest city stays.
        assert_eq!(
            best_location_name("Sintra Air Base", Some("Lisbon")),
            "Sintra Air Base"
        );
        // Cities and locations without coordinates keep their name.
        assert_eq!(best_location_name("Lisbon", None), "Lisbon");
    }
}
