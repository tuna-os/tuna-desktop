//! GNOME's calendar events (calendar.js `DBusEventSource`): the session
//! bus's org.gnome.Shell.CalendarServer, activated on demand, asked for
//! the calendar grid's range and followed through its signals. The date
//! menu marks days with events and lists the selected day's.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use gio::prelude::*;

use crate::logic::{self, CalEvent};

const NAME: &str = "org.gnome.Shell.CalendarServer";
const PATH: &str = "/org/gnome/Shell/CalendarServer";
const IFACE: &str = "org.gnome.Shell.CalendarServer";

/// The calendar server's events, as GNOME's shell caches them.
#[derive(Default)]
pub struct EventSource {
    conn: RefCell<Option<gio::DBusConnection>>,
    owned: Cell<bool>,
    events: RefCell<BTreeMap<String, CalEvent>>,
    range: Cell<Option<(i64, i64)>>,
    has_calendars: Cell<bool>,
    changed: RefCell<Option<Rc<dyn Fn()>>>,
}

impl EventSource {
    /// Connect to the session bus and start following the server.
    pub fn new() -> Rc<Self> {
        let source = Rc::new(Self::default());
        let weak = Rc::downgrade(&source);
        gio::bus_get(
            gio::BusType::Session,
            None::<&gio::Cancellable>,
            move |conn| {
                let (Ok(conn), Some(source)) = (conn, weak.upgrade()) else {
                    return;
                };
                source.attach(&conn);
            },
        );
        source
    }

    /// Call `f` whenever the events or HasCalendars change.
    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        *self.changed.borrow_mut() = Some(Rc::new(f));
    }

    /// Whether the server has calendars (GNOME hides the events card
    /// without them).
    pub fn has_calendars(&self) -> bool {
        self.has_calendars.get()
    }

    /// The events overlapping `[begin, end)`, GNOME's order.
    pub fn events_between(&self, begin: i64, end: i64) -> Vec<CalEvent> {
        logic::events_between(self.events.borrow().values(), begin, end)
    }

    /// Ask for the calendar grid's range (reloading when it changes).
    pub fn request_range(&self, begin: i64, end: i64) {
        if self.range.get() == Some((begin, end)) {
            return;
        }
        self.range.set(Some((begin, end)));
        self.load(true);
    }

    fn emit(&self) {
        let f = self.changed.borrow().clone();
        if let Some(f) = f {
            f();
        }
    }

    fn attach(self: &Rc<Self>, conn: &gio::DBusConnection) {
        *self.conn.borrow_mut() = Some(conn.clone());
        let weak = Rc::downgrade(self);
        let sub = conn.subscribe_to_signal(
            Some(NAME),
            None,
            None,
            Some(PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                if let Some(source) = weak.upgrade() {
                    source.on_signal(signal.interface_name, signal.signal_name, signal.parameters);
                }
            },
        );
        // The subscription lives as long as the shell.
        std::mem::forget(sub);
        // Start the server, as GNOME's proxy activates it, and follow
        // its owner (a NameOwnerChanged watch: gio's name watcher hands
        // its callbacks a NULL connection when the bus goes away).
        conn.call(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "StartServiceByName",
            Some(&(NAME, 0u32).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
            |_| {},
        );
        let weak = Rc::downgrade(self);
        crate::services::watch_name(conn, NAME, move |owned| {
            if let Some(source) = weak.upgrade() {
                source.owner_changed(owned);
            }
        });
    }

    /// The server appeared (reload everything) or vanished (no events,
    /// no calendars): calendar.js `_onNameAppeared`/`_onNameVanished`.
    fn owner_changed(self: &Rc<Self>, owned: bool) {
        self.owned.set(owned);
        self.events.borrow_mut().clear();
        if owned {
            self.refresh_has_calendars();
            self.load(true);
        } else {
            self.has_calendars.set(false);
            self.emit();
        }
    }

    fn on_signal(self: &Rc<Self>, iface: &str, name: &str, params: &glib::Variant) {
        if iface == "org.freedesktop.DBus.Properties" {
            if name == "PropertiesChanged" {
                self.refresh_has_calendars();
            }
            return;
        }
        if iface != IFACE {
            return;
        }
        let changed = match name {
            "EventsAddedOrUpdated" => {
                let added = added_events(params);
                let any = !added.is_empty();
                logic::events_added(&mut self.events.borrow_mut(), added);
                any
            }
            "EventsRemoved" => {
                let ids = removed_ids(params);
                let mut events = self.events.borrow_mut();
                {
                    // Every id is applied: no short circuit.
                    let mut any = false;
                    for id in &ids {
                        any |= logic::events_remove_matching(&mut events, id);
                    }
                    any
                }
            }
            "ClientDisappeared" => {
                let uid = disappeared_source(params);
                logic::events_remove_matching(&mut self.events.borrow_mut(), &format!("{uid}\n"))
            }
            _ => false,
        };
        if changed {
            self.emit();
        }
    }

    /// Read HasCalendars (GNOME re-reads it on PropertiesChanged).
    fn refresh_has_calendars(self: &Rc<Self>) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let weak = Rc::downgrade(self);
        conn.call(
            Some(NAME),
            PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&(IFACE, "HasCalendars").to_variant()),
            glib::VariantTy::new("(v)").ok(),
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
            move |reply| {
                let Some(source) = weak.upgrade() else {
                    return;
                };
                let has = has_calendars_reply(reply.ok().as_ref());
                if source.has_calendars.replace(has) != has {
                    source.emit();
                }
            },
        );
    }

    /// GNOME's `_loadEvents`: SetTimeRange over the requested range.
    fn load(&self, force: bool) {
        let (Some(conn), Some((begin, end))) = (self.conn.borrow().clone(), self.range.get())
        else {
            return;
        };
        if !self.owned.get() {
            return;
        }
        if force {
            self.events.borrow_mut().clear();
            self.emit();
        }
        conn.call(
            Some(NAME),
            PATH,
            IFACE,
            "SetTimeRange",
            Some(&(begin, end, force).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
            |_| {},
        );
    }
}

/// EventsAddedOrUpdated's `a(ssxxa{sv})`: (id, summary, start, end,
/// extras). Anything else, including a non-array from a misbehaving
/// server, adds nothing.
fn added_events(params: &glib::Variant) -> Vec<CalEvent> {
    let Some(list) = params.try_child_value(0).filter(|v| v.is_container()) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|item| {
            let (id, summary, start, end, _extras) =
                item.get::<(String, String, i64, i64, glib::VariantDict)>()?;
            Some(CalEvent {
                id,
                summary,
                start,
                end,
            })
        })
        .collect()
}

/// EventsRemoved's `as` of id prefixes.
fn removed_ids(params: &glib::Variant) -> Vec<String> {
    params
        .try_child_value(0)
        .and_then(|v| v.get::<Vec<String>>())
        .unwrap_or_default()
}

/// ClientDisappeared's source uid (calendar.js defaults it to "").
fn disappeared_source(params: &glib::Variant) -> String {
    params
        .try_child_value(0)
        .and_then(|v| v.get::<String>())
        .unwrap_or_default()
}

/// `Properties.Get(HasCalendars)`'s `(v)`; false unless a boolean true.
fn has_calendars_reply(reply: Option<&glib::Variant>) -> bool {
    reply
        .and_then(|v| v.try_child_value(0))
        .filter(|v| v.type_() == glib::VariantTy::VARIANT)
        .and_then(|v| v.as_variant())
        .and_then(|v| v.get::<bool>())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    /// A source with no bus: signals are fed in directly, the way the
    /// GDBus subscription would deliver them.
    fn source() -> (Rc<EventSource>, Rc<Cell<u32>>) {
        let source = Rc::new(EventSource::default());
        let changes = Rc::new(Cell::new(0));
        let counter = changes.clone();
        source.connect_changed(move || counter.set(counter.get() + 1));
        (source, changes)
    }

    fn added(events: &[(&str, &str, i64, i64)]) -> glib::Variant {
        let items: Vec<glib::Variant> = events
            .iter()
            .map(|(id, summary, start, end)| {
                (*id, *summary, *start, *end, glib::VariantDict::new(None)).to_variant()
            })
            .collect();
        glib::Variant::tuple_from_iter([glib::Variant::array_from_iter_with_type(
            glib::VariantTy::new("(ssxxa{sv})").unwrap(),
            items,
        )])
    }

    fn signal(source: &Rc<EventSource>, name: &str, params: &glib::Variant) {
        source.on_signal(IFACE, name, params);
    }

    fn summaries(source: &EventSource, begin: i64, end: i64) -> Vec<String> {
        source
            .events_between(begin, end)
            .into_iter()
            .map(|e| e.summary)
            .collect()
    }

    fn day(source: &EventSource, d: jiff::civil::Date, tz: &jiff::tz::TimeZone) -> Vec<String> {
        let (begin, end) = logic::day_bounds(d, tz);
        summaries(source, begin, end)
    }

    #[test]
    fn added_updated_and_removed_payloads_follow_the_calendar_server() {
        let (source, changes) = source();
        signal(
            &source,
            "EventsAddedOrUpdated",
            &added(&[
                ("work\nstandup\n", "Standup", 1_000, 2_000),
                ("work\nreview\n", "Review", 3_000, 4_000),
                ("home\ndentist\n", "Dentist", 1_500, 1_800),
            ]),
        );
        assert_eq!(changes.get(), 1);
        assert_eq!(
            summaries(&source, 0, 10_000),
            ["Standup", "Dentist", "Review"]
        );
        // An update replaces the event with the same id.
        signal(
            &source,
            "EventsAddedOrUpdated",
            &added(&[("work\nstandup\n", "Standup (moved)", 5_000, 6_000)]),
        );
        assert_eq!(
            summaries(&source, 0, 10_000),
            ["Dentist", "Review", "Standup (moved)"]
        );
        // Removal is by id prefix, every id applied.
        signal(
            &source,
            "EventsRemoved",
            &(vec!["work\nreview\n", "home\n"],).to_variant(),
        );
        assert_eq!(summaries(&source, 0, 10_000), ["Standup (moved)"]);
        assert_eq!(changes.get(), 3);
        // Removing what is not there changes nothing.
        signal(&source, "EventsRemoved", &(vec!["home\n"],).to_variant());
        assert_eq!(changes.get(), 3);
        // A source going away takes only its own events.
        signal(
            &source,
            "EventsAddedOrUpdated",
            &added(&[("workshop\nx\n", "Workshop", 1, 2)]),
        );
        signal(&source, "ClientDisappeared", &("work",).to_variant());
        assert_eq!(summaries(&source, 0, 10_000), ["Workshop"]);
        assert_eq!(changes.get(), 5);
    }

    #[test]
    fn malformed_payloads_change_nothing_and_never_panic() {
        let (source, changes) = source();
        signal(
            &source,
            "EventsAddedOrUpdated",
            &added(&[("a\nkeep\n", "Keep", 10, 20)]),
        );
        assert_eq!(changes.get(), 1);
        let malformed = [
            // A string where the array belongs: this used to abort the
            // shell (glib's Variant::iter asserts a container).
            ("EventsAddedOrUpdated", ("nope",).to_variant()),
            ("EventsAddedOrUpdated", (7i64,).to_variant()),
            (
                "EventsAddedOrUpdated",
                glib::Variant::tuple_from_iter(std::iter::empty::<glib::Variant>()),
            ),
            ("EventsAddedOrUpdated", "bare".to_variant()),
            // Items without extras, or with 32-bit times, are not events.
            (
                "EventsAddedOrUpdated",
                (vec![("a\nkeep\n", "Gone", 1i64, 2i64)],).to_variant(),
            ),
            (
                "EventsAddedOrUpdated",
                (vec![(
                    "a\nkeep\n",
                    "Gone",
                    1i32,
                    2i32,
                    glib::VariantDict::new(None),
                )],)
                    .to_variant(),
            ),
            // A tuple where the array belongs: its children are not events.
            (
                "EventsAddedOrUpdated",
                ((
                    "a\nkeep\n",
                    "Gone",
                    1i64,
                    2i64,
                    glib::VariantDict::new(None),
                ),)
                    .to_variant(),
            ),
            ("EventsRemoved", ("a\n",).to_variant()),
            ("EventsRemoved", (vec![1u32],).to_variant()),
            ("ClientDisappeared", (vec!["a"],).to_variant()),
            (
                "ClientDisappeared",
                glib::Variant::tuple_from_iter(std::iter::empty::<glib::Variant>()),
            ),
            ("SomethingElse", (vec!["a\n"],).to_variant()),
        ];
        for (name, params) in &malformed {
            signal(&source, name, params);
            assert_eq!(summaries(&source, 0, 100), ["Keep"], "{name} {params}");
        }
        assert_eq!(changes.get(), 1, "no spurious change notifications");
        // Another interface's signals are not the calendar server's.
        source.on_signal(
            "org.example.Other",
            "EventsRemoved",
            &(vec!["a\n"],).to_variant(),
        );
        assert_eq!(summaries(&source, 0, 100), ["Keep"]);
    }

    #[test]
    fn has_calendars_reads_only_a_boolean_true() {
        let boxed =
            |v: glib::Variant| glib::Variant::tuple_from_iter([glib::Variant::from_variant(&v)]);
        assert!(has_calendars_reply(Some(&boxed(true.to_variant()))));
        assert!(!has_calendars_reply(Some(&boxed(false.to_variant()))));
        assert!(!has_calendars_reply(Some(&boxed("true".to_variant()))));
        assert!(!has_calendars_reply(Some(&(true,).to_variant())));
        assert!(!has_calendars_reply(Some(&true.to_variant())));
        assert!(!has_calendars_reply(None), "a failed Get is no calendars");
    }

    #[test]
    fn server_vanishing_clears_events_and_calendars() {
        let (source, changes) = source();
        source.has_calendars.set(true);
        signal(
            &source,
            "EventsAddedOrUpdated",
            &added(&[("a\nx\n", "X", 10, 20)]),
        );
        source.owner_changed(false);
        assert!(!source.has_calendars());
        assert!(source.events_between(i64::MIN, i64::MAX).is_empty());
        assert_eq!(changes.get(), 2, "the date menu hears about it");
        // Coming back starts from an empty cache too.
        signal(
            &source,
            "EventsAddedOrUpdated",
            &added(&[("a\nx\n", "X", 10, 20)]),
        );
        source.owner_changed(true);
        assert!(source.events_between(i64::MIN, i64::MAX).is_empty());
    }

    #[test]
    fn all_day_and_multi_day_events_land_on_their_days_across_dst() {
        // US Eastern: 2026-03-08 is 23 hours long, 2026-11-01 is 25.
        let tz = jiff::tz::TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        let bounds = |d| logic::day_bounds(d, &tz);
        let (spring, spring_end) = bounds(date(2026, 3, 8));
        assert_eq!(spring_end - spring, 23 * 3600);
        let (fall, fall_end) = bounds(date(2026, 11, 1));
        assert_eq!(fall_end - fall, 25 * 3600);
        let (trip, _) = bounds(date(2026, 10, 30));
        let (_, trip_end) = bounds(date(2026, 11, 2));

        let (source, _) = source();
        signal(
            &source,
            "EventsAddedOrUpdated",
            &added(&[
                // All-day events run local midnight to local midnight.
                ("c\nspring\n", "Spring forward", spring, spring_end),
                ("c\nfall\n", "Fall back", fall, fall_end),
                // Four whole days, through the 25-hour one.
                ("c\ntrip\n", "Trip", trip, trip_end),
                // Ends exactly at midnight: not on the next day.
                ("c\nlate\n", "Late", spring_end - 3600, spring_end),
                // Zero length at midnight: on the day it starts.
                ("c\nmark\n", "Mark", fall, fall),
            ]),
        );
        assert_eq!(day(&source, date(2026, 3, 7), &tz), Vec::<String>::new());
        assert_eq!(
            day(&source, date(2026, 3, 8), &tz),
            ["Spring forward", "Late"]
        );
        assert_eq!(day(&source, date(2026, 3, 9), &tz), Vec::<String>::new());
        assert_eq!(day(&source, date(2026, 10, 29), &tz), Vec::<String>::new());
        for d in [date(2026, 10, 30), date(2026, 10, 31), date(2026, 11, 2)] {
            assert_eq!(day(&source, d, &tz), ["Trip"], "{d}");
        }
        // GNOME's order: running in from before the day sorts by start.
        assert_eq!(
            day(&source, date(2026, 11, 1), &tz),
            ["Trip", "Fall back", "Mark"]
        );
        assert_eq!(day(&source, date(2026, 11, 3), &tz), Vec::<String>::new());
        // The 23- and 25-hour all-day events still read "All Day".
        for (id, d) in [
            ("c\nspring\n", date(2026, 3, 8)),
            ("c\nfall\n", date(2026, 11, 1)),
        ] {
            let event = source.events.borrow()[id].clone();
            assert_eq!(
                logic::event_time_text(&event, d, &tz, logic::ClockFormat::TwentyFourHour, 2026),
                "All Day"
            );
        }
        // The grid's range edges: [begin, end) keeps a boundary event
        // out of the range it only touches.
        assert_eq!(summaries(&source, spring_end, trip), Vec::<String>::new());
        assert_eq!(
            summaries(&source, spring_end - 1, spring_end),
            ["Late", "Spring forward"]
        );
    }

    #[test]
    fn requesting_the_same_range_twice_reloads_once() {
        let (source, changes) = source();
        // No server yet: the range is kept for when it appears, and the
        // cache is not cleared under the date menu.
        source.request_range(0, 100);
        source.request_range(0, 100);
        assert_eq!(source.range.get(), Some((0, 100)));
        assert_eq!(changes.get(), 0);
        source.request_range(100, 200);
        assert_eq!(source.range.get(), Some((100, 200)));
    }
}
