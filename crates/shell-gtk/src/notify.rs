//! Notifications in the GTK shell (#56): GNOME 51 banners, the list in
//! the calendar popover, Clear, and Do Not Disturb.
//!
//! The shell owns `org.freedesktop.Notifications` through the existing
//! toolkit-free daemon (`tuna_shell_host::intake::NotificationBus`) and
//! store (`NotificationCenter`); this module only draws them. Like
//! GNOME's message tray, one banner at a time drops in top-center under
//! the top bar; the others wait in the store's queue (see
//! [`tray_step`]). A banner goes [`BANNER_TIMEOUT`] after it has dropped
//! in unless critical; the list shows history newest first, one group
//! per app (GNOME 51's NotificationMessageGroup: a collapsed stack that
//! expands on click); Do Not Disturb keeps banners away (critical ones
//! still show) while history keeps filling.
//!
//! Nothing here runs on a period: wire arrivals wake the main loop
//! through the bus's change callback, the banner's expiry is a one-shot
//! timeout re-armed after every update, and a banner waiting for an away
//! user wakes on the IdleMonitor's user-active watch.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tuna_shell_host::intake::NotificationBus;
use tuna_shell_host::notification_policy::GioPolicyStore;
use tuna_shell_host::notifications::{
    Notification, NotificationAction, NotificationCenter, Urgency,
};

/// How long a normal or low banner stays up (GNOME: about 4 s).
pub const BANNER_TIMEOUT: Duration = Duration::from_secs(4);
/// GNOME's IDLE_TIME: idle longer than this when a banner shows means
/// the user is away, and the banner waits for them.
pub const AWAY_AFTER: Duration = Duration::from_secs(1);
/// How long a banner stays once an away user comes back.
pub const BACK_TIMEOUT: Duration = Duration::from_secs(2);
/// While the notifications name is not ours, how often to try again.
const BUS_RETRY: Duration = Duration::from_secs(2);
/// Without a user-active watch, how often to look for an away user's
/// return while their banner waits.
const AWAY_RECHECK: Duration = Duration::from_millis(250);
/// Mutter's idle monitor (the compositor serves it).
const IDLE_MONITOR: &str = "org.gnome.Mutter.IdleMonitor";
const IDLE_MONITOR_CORE: &str = "/org/gnome/Mutter/IdleMonitor/Core";

/// One banner's expiry, as GNOME's message tray times it: four seconds
/// when it appears to an active user; when the user is away it stays
/// until their next input and goes two seconds after it. `idle` is the
/// IdleMonitor's idle time, `None` when there is no idle monitor (then
/// the user counts as active).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BannerTimer {
    shown: Instant,
    deadline: Option<Instant>,
}

impl BannerTimer {
    pub fn new(now: Instant, idle: Option<Duration>) -> Self {
        let active = idle.is_none_or(|i| i <= AWAY_AFTER);
        Self {
            shown: now,
            deadline: active.then_some(now + BANNER_TIMEOUT),
        }
    }

    /// Whether the banner is due to go at `now`.
    pub fn expired(&mut self, now: Instant, idle: Option<Duration>) -> bool {
        if self.deadline.is_none() {
            match idle {
                // Input since the banner showed: the user is back.
                Some(i) if i < now.saturating_duration_since(self.shown) => {
                    self.deadline = Some(now.checked_sub(i).unwrap_or(now) + BACK_TIMEOUT);
                }
                Some(_) => {}
                None => self.deadline = Some(now + BACK_TIMEOUT),
            }
        }
        self.deadline.is_some_and(|d| now >= d)
    }

    /// When the banner goes; `None` while it waits for an away user.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
}

/// What the banner timers ask for at one moment.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Due {
    /// Banners due to go now.
    pub expired: Vec<u64>,
    /// The earliest deadline still ahead.
    pub next: Option<Instant>,
    /// A banner waits for an away user to come back.
    pub away: bool,
}

/// Advance each banner's timer to `now` and collect what they ask for:
/// which banners go, when the next one is due, and whether one waits on
/// the user's return.
pub fn due<'a>(
    timers: impl IntoIterator<Item = (u64, &'a mut BannerTimer)>,
    now: Instant,
    idle: Option<Duration>,
) -> Due {
    let mut due = Due::default();
    for (id, timer) in timers {
        if timer.expired(now, idle) {
            due.expired.push(id);
            continue;
        }
        match timer.deadline() {
            Some(d) => due.next = Some(due.next.map_or(d, |n| n.min(d))),
            None => due.away = true,
        }
    }
    due
}

/// Where the banner surface is in GNOME's message tray cycle
/// (`messageTray.js` `State`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    Hidden,
    /// Dropping in.
    Showing,
    /// On screen, its timeout running.
    Shown,
    /// Sliding out.
    Hiding,
}

/// What the banner surface does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayStep {
    Stay,
    /// Drop this queued banner in; when it is the one already on screen
    /// (or sliding out), it was updated: show it again in place.
    Show(u64),
    /// The banner on screen left the queue: take it away.
    Hide,
}

/// GNOME's `MessageTray._updateState` for the banner surface, given
/// the store's banner queue as `(id, urgency, acknowledged)`: with none
/// on screen the next one drops in, critical first and otherwise oldest
/// first; the banner on screen stays until it leaves the queue (it
/// expired, was dismissed or closed); one sliding out finishes before the
/// next drops in. Showing a banner acknowledges it, so the one on screen
/// unacknowledged again was updated in place (a replacement): like
/// `_onNotificationRequestBanner` for the notification already showing,
/// it shows again (`_updateShowingNotification`), never queued behind.
pub fn tray_step(
    state: TrayState,
    showing: Option<u64>,
    queue: &[(u64, Urgency, bool)],
) -> TrayStep {
    let entry = |id: u64| queue.iter().find(|(q, _, _)| *q == id);
    match (state, showing) {
        (TrayState::Hidden, _) => queue
            .iter()
            .min_by_key(|(_, urgency, _)| std::cmp::Reverse(urgency_rank(*urgency)))
            .map_or(TrayStep::Stay, |(id, _, _)| TrayStep::Show(*id)),
        (TrayState::Showing | TrayState::Shown, Some(id)) => match entry(id) {
            None => TrayStep::Hide,
            Some((_, _, false)) => TrayStep::Show(id),
            Some(_) => TrayStep::Stay,
        },
        (TrayState::Hiding, Some(id)) if entry(id).is_some() => TrayStep::Show(id),
        _ => TrayStep::Stay,
    }
}

/// The message tray's queue order (`n2.urgency - n1.urgency`).
fn urgency_rank(urgency: Urgency) -> u8 {
    match urgency {
        Urgency::Low => 0,
        Urgency::Normal => 1,
        Urgency::High => 2,
        Urgency::Critical => 3,
    }
}

/// A one-shot timeout after `delay`, in whole milliseconds rounded up so
/// it never fires before `delay` has passed.
fn timeout_after(delay: Duration, func: impl FnOnce() + 'static) -> glib::SourceId {
    let ms = delay.as_micros().div_ceil(1000) as u64;
    glib::timeout_add_local_once(Duration::from_millis(ms), func)
}

/// The IdleMonitor's user-active watch for an away user's banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserWatch {
    Off,
    /// AddUserActiveWatch sent, no reply yet.
    Pending,
    /// Fires `WatchFired(id)` on the next input.
    Armed(u32),
    /// The idle monitor has no such watch: recheck on a short timeout.
    Unavailable,
}

/// The session's idle time from org.gnome.Mutter.IdleMonitor, or `None`
/// without one.
fn idle_time() -> Option<Duration> {
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    let reply = conn
        .call_sync(
            Some(IDLE_MONITOR),
            IDLE_MONITOR_CORE,
            IDLE_MONITOR,
            "GetIdletime",
            None,
            Some(glib::VariantTy::new("(t)").ok()?),
            gio::DBusCallFlags::NO_AUTO_START,
            200,
            gio::Cancellable::NONE,
        )
        .ok()?;
    reply.get::<(u64,)>().map(|(ms,)| Duration::from_millis(ms))
}
/// Close reason "dismissed by the user" (freedesktop spec).
const REASON_DISMISSED: u32 = 2;
/// Close reason "expired".
const REASON_EXPIRED: u32 = 1;

/// How far the second card of a collapsed group peeks below the first
/// (messageList.js HEIGHT_OFFSET_STACKED).
pub const HEIGHT_OFFSET_STACKED: f64 = 10.0;
/// Each further card peeks this many times less
/// (HEIGHT_OFFSET_REDUCTION_STACKED).
pub const HEIGHT_OFFSET_REDUCTION_STACKED: f64 = 1.4;
/// Each card down the stack is inset this much more per side
/// (WIDTH_OFFSET_STACKED).
pub const WIDTH_OFFSET_STACKED: i32 = 6;
/// Cards a collapsed group shows, the top one included
/// (MAX_VISIBLE_STACKED_MESSAGES).
pub const MAX_VISIBLE_STACKED_MESSAGES: usize = 3;

/// The message list's groups, as GNOME 51 forms them: one per source,
/// ordered by each group's newest notification, members newest first.
/// `newest_first` is the history, newest first, with each notification's
/// source key.
pub fn group_ids<K: PartialEq>(
    newest_first: impl IntoIterator<Item = (u64, K)>,
) -> Vec<(K, Vec<u64>)> {
    let mut groups: Vec<(K, Vec<u64>)> = Vec::new();
    for (id, key) in newest_first {
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, ids)) => ids.push(id),
            None => groups.push((key, vec![id])),
        }
    }
    groups
}

/// The cards peeking under the top one of a collapsed group of `count`:
/// for each, its inset per side and how far its bottom edge reaches
/// below the top card's (10 px, then 10/1.4 px more; at most
/// [`MAX_VISIBLE_STACKED_MESSAGES`] cards in all).
pub fn stack_layout(count: usize) -> Vec<(i32, i32)> {
    let mut reach = 0.0_f64;
    let mut step = HEIGHT_OFFSET_STACKED;
    (1..count.min(MAX_VISIBLE_STACKED_MESSAGES))
        .map(|k| {
            reach += step;
            step /= HEIGHT_OFFSET_REDUCTION_STACKED;
            (WIDTH_OFFSET_STACKED * k as i32, reach.round() as i32)
        })
        .collect()
}

/// The source a notification groups under: its desktop entry when it
/// names one, else the sender's app name (GNOME's per-app sources).
fn group_key(n: &Notification) -> String {
    if n.desktop_entry().is_empty() {
        n.app().to_owned()
    } else {
        n.desktop_entry().to_owned()
    }
}

/// Snapshot of what the list shows, to rebuild only on change.
#[derive(Default, PartialEq)]
struct Shown {
    history: Vec<u64>,
    /// The store's in-place replacements: an id kept, its text changed.
    replacements: u64,
    expanded: Vec<String>,
    dnd: bool,
}

/// Local dispatch for shell-own notice actions (wellbeing Take/Delay/Skip).
/// External notices route through the bus to their sender; shell-own
/// notices have no sender, so the shell registers a callback per
/// (notice id, action key) instead. The banner path always runs the
/// bus invoke first (per-key replay guard + banner dismiss + signal)
/// and only dispatches locally when that succeeds.
///
/// Admission is shell-owned: `live` holds the resident generation that
/// may still fire. Center ids are monotonic and never reused, so
/// dropping an id from `live` invalidates exactly that resident
/// generation. The bus does NOT guard residency: `dismiss` removes only
/// the banner while history keeps every unconsumed key invokable, so a
/// second old key would still invoke after a first action or a banner
/// close unless the shell retires the resident. Every terminal event
/// therefore retires: a successful dispatch retires all sibling keys
/// at once (first press wins), and banner close, body click, timeout
/// expiry, list removal, and programmatic dismiss retire without
/// firing. Unregistered ids behave exactly as before. Pure and
/// headless-testable; the UI owns one.
#[derive(Default)]
struct ActionDispatch {
    handlers: HashMap<(u64, String), Rc<dyn Fn()>>,
    live: HashSet<u64>,
}

impl ActionDispatch {
    /// Register (or replace) one action callback, admitting the resident.
    fn register(&mut self, id: u64, action: &str, handler: Rc<dyn Fn()>) {
        self.handlers.insert((id, action.to_owned()), handler);
        self.live.insert(id);
    }

    /// Retire one resident generation entirely (action/dismiss/replace):
    /// its keys must never fire again, even through a previously
    /// captured path. History entries are untouched.
    fn drop_notice(&mut self, id: u64) {
        self.handlers.retain(|(known, _), _| *known != id);
        self.live.remove(&id);
    }

    /// Release callbacks whose center entries were removed or history-pruned.
    /// Banner absence is not removal: dismissed history still belongs to the center.
    fn retain_history(&mut self, center: &NotificationCenter) {
        let present: HashSet<u64> = center.history().iter().map(|n| n.id).collect();
        self.handlers.retain(|(id, _), _| present.contains(id));
        self.live.retain(|id| present.contains(id));
    }

    /// Run one registered callback, then retire the whole resident so no
    /// sibling key can fire afterwards. False when unadmitted: unknown
    /// ids, retired residents, and unregistered keys never fire.
    fn dispatch(&mut self, id: u64, action: &str) -> bool {
        if !self.live.contains(&id) {
            return false;
        }
        match self.handlers.get(&(id, action.to_owned())) {
            Some(handler) => {
                handler();
                self.drop_notice(id);
                true
            }
            None => false,
        }
    }
}

/// Notification surfaces and their store.
pub struct NotifyUi {
    center: Arc<Mutex<NotificationCenter>>,
    bus: RefCell<NotificationBus>,
    banner_window: gtk::ApplicationWindow,
    banner_box: gtk::Box,
    /// Drops the banner in and slides it out (`messageTray.js`).
    banner_bin: crate::transient::MotionBin,
    banner_motion: crate::transient::Player,
    /// The tray's cycle, and the banner it holds.
    tray: Cell<TrayState>,
    showing: Cell<Option<u64>>,
    /// The banner this shell just expired: it slides out, where one
    /// dismissed or closed goes at once (`_notificationRemoved`).
    expired: Cell<Option<u64>>,
    /// The calendar popover's left pane.
    pane: gtk::Box,
    list: gtk::Box,
    empty: gtk::Box,
    clear: gtk::Button,
    /// GNOME binds Clear's visibility to the placeholder without
    /// SYNC_CREATE, so Clear shows (insensitive) until the list first
    /// changes; after that it hides whenever the list is empty.
    clear_bound: std::cell::Cell<bool>,
    /// The banner on screen's expiry, from when it has dropped in.
    timer: RefCell<Option<BannerTimer>>,
    /// GNOME's `_userActiveWhileNotificationShown`: the user was active
    /// when the banner on screen dropped in, or has been since. It holds
    /// across an in-place update, which then times out like any banner
    /// shown to an active user.
    user_active: Cell<bool>,
    /// The groups the user expanded, by source key.
    expanded: RefCell<HashSet<String>>,
    group_widgets: RefCell<HashMap<String, Rc<crate::group_animation::Group>>>,
    /// The history list's widgets lag the store: they are built only while
    /// the list is on screen, and when it next maps.
    history_stale: Cell<bool>,
    /// What the list last showed; `None` until the first refresh.
    shown: RefCell<Option<Shown>>,
    local_actions: RefCell<ActionDispatch>,
    /// The next banner expiry (a one-shot timeout).
    deadline: RefCell<Option<glib::SourceId>>,
    /// The next try at the bus while it is not ours.
    bus_retry: RefCell<Option<glib::SourceId>>,
    user_watch: Cell<UserWatch>,
    user_watch_signal: RefCell<Option<gio::SignalSubscription>>,
    /// Told when Do Not Disturb turns on or off.
    dnd_changed: RefCell<Vec<DndListener>>,
    /// The date menu is open: banners hide (`MessageTray.bannerBlocked`).
    banner_blocked: Cell<bool>,
}

/// Told when Do Not Disturb turns on or off.
type DndListener = Box<dyn Fn(bool)>;

/// One notification as GNOME 51 draws it (messageList.js
/// NotificationMessage): a header with the source icon, app name, time
/// and close button, then the title and body. Returns the card and its
/// close button. Banner and list cards alike carry GNOME's message role
/// (`Atk.Role.NOTIFICATION`, GTK's alert).
fn card(n: &Notification) -> (gtk::Box, gtk::Button) {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .accessible_role(gtk::AccessibleRole::Alert)
        .build();
    card.add_css_class("message");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header.add_css_class("message-header");
    let (title, gicon) = source_of(n);
    let icon = gtk::Image::new();
    icon.add_css_class("message-source-icon");
    icon.set_pixel_size(16);
    match gicon {
        Some(gicon) => icon.set_from_gicon(&gicon),
        None => icon.set_visible(false),
    }
    header.append(&icon);
    let source = gtk::Label::new(Some(&title));
    source.add_css_class("message-source-title");
    header.append(&source);
    let age = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
        .saturating_sub(n.received());
    let time = gtk::Label::new(Some(&crate::logic::time_span(age)));
    time.add_css_class("event-time");
    time.set_hexpand(true);
    time.set_xalign(0.0);
    header.append(&time);
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.add_css_class("message-close-button");
    close.set_valign(gtk::Align::Center);
    close.update_property(&[gtk::accessible::Property::Label("Close")]);
    header.append(&close);
    card.append(&header);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.add_css_class("message-content");
    let summary = gtk::Label::new(Some(n.summary()));
    summary.add_css_class("message-title");
    summary.set_xalign(0.0);
    summary.set_wrap(true);
    content.append(&summary);
    if !n.body().is_empty() {
        let body = gtk::Label::new(Some(n.body()));
        body.add_css_class("message-body");
        body.set_xalign(0.0);
        body.set_wrap(true);
        body.set_max_width_chars(48);
        content.append(&body);
    }
    card.append(&content);
    card.update_property(&[gtk::accessible::Property::Label(n.summary())]);
    if !n.body().is_empty() {
        card.update_property(&[gtk::accessible::Property::Description(n.body())]);
    }
    (card, close)
}

/// What a screen reader says when a banner arrives. GNOME's banner is an
/// ATK `NOTIFICATION` (messageList.js), which Orca presents on showing as
/// its role name, then the message's labels: source, title and body.
pub fn announcement(source: &str, summary: &str, body: &str) -> String {
    let mut text = String::from("Notification");
    for part in [source, summary, body] {
        if !part.is_empty() {
            text.push_str(", ");
            text.push_str(part);
        }
    }
    text
}

/// A notification's source name and icon: the app's own when its
/// desktop entry is known, else the sender's name and app_icon
/// (notificationDaemon.js).
fn source_of(n: &Notification) -> (String, Option<gio::Icon>) {
    let app = (!n.desktop_entry().is_empty())
        .then(|| desktop_app(n.desktop_entry()))
        .flatten();
    match app {
        Some(app) => (app.name.clone(), app.icon.as_deref().and_then(source_icon)),
        None => (n.app().to_owned(), source_icon(n.icon())),
    }
}

/// The installed app a `desktop-entry` hint names, if any.
fn desktop_app(id: &str) -> Option<tuna_shell_host::apps::AppEntry> {
    tuna_shell_host::apps::default_app_dirs()
        .into_iter()
        .map(|dir| dir.join(format!("{id}.desktop")))
        .find(|path| path.is_file())
        .and_then(|path| tuna_shell_host::apps::entry_from_file(&path))
}

/// A `Notify` app_icon as GNOME reads it: a file URI, an absolute path,
/// or an icon name.
fn source_icon(icon: &str) -> Option<gio::Icon> {
    if icon.is_empty() {
        None
    } else if icon.starts_with("file://") {
        Some(gio::FileIcon::new(&gio::File::for_uri(icon)).upcast())
    } else if icon.starts_with('/') {
        Some(gio::FileIcon::new(&gio::File::for_path(icon)).upcast())
    } else {
        Some(gio::ThemedIcon::new(icon).upcast())
    }
}

impl NotifyUi {
    /// Build the surfaces and claim the bus.
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let center = Arc::new(Mutex::new(NotificationCenter::load_system()));
        // GNOME Settings' notification policy, read live on every
        // arrival (#348); a restart drops whatever an app disabled
        // since it was filed.
        if let Ok(mut filed) = center.lock() {
            filed.set_policy(Arc::new(GioPolicyStore::new()));
            filed.enforce_policy();
        }
        Self::build(app, center, true)
    }

    // Tests use an in-memory center and ordinary GTK window. Production
    // always retains the layer-shell surface, persisted notification store
    // and GNOME's notification policy.
    fn build(
        app: &gtk::Application,
        center: Arc<Mutex<NotificationCenter>>,
        layered: bool,
    ) -> Rc<Self> {
        let mut bus = NotificationBus::new(center.clone());
        // The policy store reads and registers through GSettings: file
        // wire arrivals on the GTK main loop, never a zbus thread.
        bus.deliver_in(glib::MainContext::default());
        // Wire arrivals (on zbus threads) wake the main loop; a wake
        // already queued covers any that follow it.
        let (wake, woken) = async_channel::bounded::<()>(1);
        bus.on_change(move || {
            let _ = wake.try_send(());
        });

        let banner_window = gtk::ApplicationWindow::new(app);
        banner_window.add_css_class("tuna-banners");
        if layered {
            banner_window.init_layer_shell();
            banner_window.set_layer(Layer::Overlay);
            banner_window.set_namespace(Some(tuna_shell_control::GTK_BANNERS_NAMESPACE));
            banner_window.set_anchor(Edge::Top, true);
            // The card's own 4px margin puts it 4px under the bar (GNOME).
            banner_window.set_margin(Edge::Top, 0);
            banner_window.set_keyboard_mode(KeyboardMode::None);
        }
        let keys = gtk::EventControllerKey::new();
        let banner_weak = banner_window.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(window) = banner_weak.upgrade() {
                    window.set_keyboard_mode(KeyboardMode::None);
                }
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        banner_window.add_controller(keys);
        banner_window.set_title(Some("Notifications"));
        let banner_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let banner_bin = crate::transient::MotionBin::new(&banner_box);
        banner_window.set_child(Some(&banner_bin));

        // Calendar pane, GNOME 51's CalendarMessageList: the placeholder
        // overlaid on the list, and a controls row holding only Clear (Do
        // Not Disturb lives in quick settings).
        let pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pane.add_css_class("message-list");
        let overlay = gtk::Overlay::new();
        overlay.set_vexpand(true);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("message-view");
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        column.append(&scroller);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        controls.add_css_class("message-list-controls");
        let clear = gtk::Button::with_label("Clear");
        clear.add_css_class("message-list-clear-button");
        clear.set_halign(gtk::Align::Start);
        clear.update_property(&[gtk::accessible::Property::Description(
            "Clear all notifications",
        )]);
        controls.append(&clear);
        column.append(&controls);
        overlay.set_child(Some(&column));
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 0);
        empty.add_css_class("message-list-placeholder");
        empty.set_halign(gtk::Align::Center);
        empty.set_valign(gtk::Align::Center);
        empty.set_can_target(false);
        let bell = gtk::Image::from_icon_name("no-notifications-symbolic");
        bell.set_pixel_size(96);
        let none = gtk::Label::new(Some("No Notifications"));
        empty.append(&bell);
        empty.append(&none);
        overlay.add_overlay(&empty);
        pane.append(&overlay);

        let ui = Rc::new(Self {
            center,
            bus: RefCell::new(bus),
            banner_window,
            banner_box,
            banner_bin,
            banner_motion: crate::transient::Player::new("banner"),
            tray: Cell::new(TrayState::Hidden),
            showing: Cell::new(None),
            expired: Cell::new(None),
            pane,
            list,
            empty,
            clear,
            clear_bound: std::cell::Cell::new(false),
            timer: RefCell::new(None),
            user_active: Cell::new(false),
            expanded: RefCell::new(HashSet::new()),
            group_widgets: RefCell::new(HashMap::new()),
            history_stale: Cell::new(false),
            shown: RefCell::new(None),
            local_actions: RefCell::new(ActionDispatch::default()),
            deadline: RefCell::new(None),
            bus_retry: RefCell::new(None),
            user_watch: Cell::new(UserWatch::Off),
            user_watch_signal: RefCell::new(None),
            dnd_changed: RefCell::new(Vec::new()),
            banner_blocked: Cell::new(false),
        });
        {
            let weak = Rc::downgrade(&ui);
            glib::spawn_future_local(async move {
                while woken.recv().await.is_ok() {
                    let Some(ui) = weak.upgrade() else {
                        break;
                    };
                    ui.update();
                }
            });
        }
        {
            let me = ui.clone();
            ui.clear.connect_clicked(move |_| {
                if let Ok(mut center) = me.center.lock() {
                    center.clear_history();
                }
                me.refresh();
            });
        }
        {
            // Opening the calendar maps the list: build what changed while
            // it was hidden before its first frame.
            let weak = Rc::downgrade(&ui);
            ui.list.connect_map(move |_| {
                if let Some(me) = weak.upgrade() {
                    me.sync_history();
                }
            });
        }
        ui.update();
        ui
    }

    /// Post the shell's own notification under a GNOME source name and
    /// icon (screenshots: "Screenshot", `screenshot-recorded-symbolic`).
    pub fn post(self: &Rc<Self>, app: &str, icon: &str, summary: &str, body: &str) {
        self.post_replacing(app, icon, summary, body, Urgency::Normal, None);
    }

    /// Post, or with `replaces` naming a live notification update it in
    /// place, as a shell source updates its resident notification
    /// (`notification.set()` then `source.addNotification()`): the list
    /// entry takes the new text, and its banner shows again, in place
    /// when it is the one on screen. Returns the notification's id
    /// (`None` when the store is unreachable).
    pub fn post_replacing(
        self: &Rc<Self>,
        app: &str,
        icon: &str,
        summary: &str,
        body: &str,
        urgency: Urgency,
        replaces: Option<u64>,
    ) -> Option<u64> {
        self.post_with_actions(app, icon, summary, body, &[], urgency, replaces)
    }

    /// Post the shell's own notification WITH invokable actions, returning
    /// the center id (None when the store is unreachable). `replaces`
    /// updates a live one in place as [`Self::post_replacing`] does; its
    /// actions are replaced too. The banner renders one button per
    /// action; a press runs the bus invoke first (per-key replay guard +
    /// banner dismiss + signal) and then the local handler, retiring all
    /// sibling keys on success.
    #[allow(clippy::too_many_arguments)]
    pub fn post_with_actions(
        self: &Rc<Self>,
        app: &str,
        icon: &str,
        summary: &str,
        body: &str,
        actions: &[(String, String)],
        urgency: Urgency,
        replaces: Option<u64>,
    ) -> Option<u64> {
        let id = {
            let mut center = self.center.lock().ok()?;
            let listed = actions
                .iter()
                .map(|(id, label)| NotificationAction {
                    id: id.clone(),
                    label: label.clone(),
                })
                .collect();
            let id = center.notify(app, summary, body, listed, urgency, replaces);
            center.set_source(id, icon, "");
            id
        };
        // Arms the new banner's expiry, as for any other arrival.
        self.update();
        Some(id)
    }

    /// Remove a shell-own notification from the list and any banner it
    /// holds (GNOME's `notification.destroy()`).
    pub fn remove_notification(self: &Rc<Self>, id: u64) {
        self.remove(&[id]);
    }

    /// Register a local callback for one shell-own (notice, action key).
    pub fn on_notification_action(self: &Rc<Self>, id: u64, action: &str, handler: Rc<dyn Fn()>) {
        self.local_actions
            .borrow_mut()
            .register(id, action, handler);
    }

    /// Forget one notice's local callbacks (replacement/removal).
    pub fn drop_notification_actions(self: &Rc<Self>, id: u64) {
        self.local_actions.borrow_mut().drop_notice(id);
    }

    /// Run one registered local callback; false when unadmitted. Only
    /// called after a successful bus invoke (per-key replay guard). A
    /// successful dispatch retires the whole resident, so sibling keys
    /// captured before the press can never fire afterwards.
    fn dispatch_local_action(&self, id: u64, action: &str) -> bool {
        self.local_actions.borrow_mut().dispatch(id, action)
    }

    /// Retire one resident's local callbacks without firing (banner
    /// close, body click, timeout expiry, list removal, programmatic
    /// dismiss). History entries are untouched; unregistered ids are a
    /// no-op, so external notices behave exactly as before.
    fn retire_local_actions(&self, id: u64) {
        self.local_actions.borrow_mut().drop_notice(id);
    }

    /// The calendar popover's notification pane.
    pub fn pane(&self) -> &gtk::Box {
        &self.pane
    }

    /// GNOME 51 panel.js: the menu in the banners' box (the date menu)
    /// blocks banners while open; they return, if still live, on close.
    pub fn set_banner_blocked(self: &Rc<Self>, on: bool) {
        if self.banner_blocked.replace(on) != on {
            self.refresh();
        }
    }

    /// Turn Do Not Disturb on or off (the quick-settings toggle and the
    /// show-banners key).
    pub fn set_dnd(self: &Rc<Self>, on: bool) {
        let changed = match self.center.lock() {
            Ok(mut center) if center.dnd() != on => {
                center.set_dnd(on);
                true
            }
            _ => false,
        };
        if !changed {
            return;
        }
        self.update();
        for f in self.dnd_changed.borrow().iter() {
            f(on);
        }
    }

    /// Whether Do Not Disturb is on.
    pub fn dnd(&self) -> bool {
        self.center.lock().map(|c| c.dnd()).unwrap_or(false)
    }

    /// Call `f` with the new state whenever Do Not Disturb changes.
    pub fn connect_dnd(&self, f: impl Fn(bool) + 'static) {
        self.dnd_changed.borrow_mut().push(Box::new(f));
    }

    /// GNOME's Super+N gives the current banner keyboard focus: the
    /// banner itself (messageList.js's focusable message), not its first
    /// button.
    pub fn focus_active(&self) {
        if !self.banner_window.is_visible() {
            return;
        }
        self.banner_window
            .set_keyboard_mode(KeyboardMode::Exclusive);
        self.banner_window.present();
        self.banner_window.set_focus_visible(true);
        if let Some(banner) = self.banner_box.first_child() {
            banner.grab_focus();
        }
    }

    /// A locked session must not keep the banner's exclusive keyboard grab.
    pub fn release_focus(&self) {
        self.banner_window.set_keyboard_mode(KeyboardMode::None);
        self.banner_window.set_focus_visible(false);
    }

    /// Claim the bus (retrying every [`BUS_RETRY`] while it is not
    /// ours), expire the banner on screen, redraw on change, and arm the
    /// next wake: its deadline, or an away user's return. Runs on every
    /// change to the notifications, never on a period.
    fn update(self: &Rc<Self>) {
        self.ensure_bus();
        for id in self.banner_due().expired {
            self.expired.set(Some(id));
            let _ = self.bus.borrow().dismiss_banner(id as u32, REASON_EXPIRED);
            self.retire_local_actions(id);
        }
        self.refresh();
        // After the refresh, which can bring the next banner on.
        let due = self.banner_due();
        let mut next = due.next;
        if due.away {
            if self.user_watch.get() == UserWatch::Unavailable {
                let recheck = Instant::now() + AWAY_RECHECK;
                next = Some(next.map_or(recheck, |n| n.min(recheck)));
            } else {
                self.watch_user_active();
            }
        }
        self.arm_deadline(next);
    }

    /// Advance the banner on screen's timer to now. GNOME starts its
    /// four seconds once the banner has dropped in, and never times a
    /// critical one.
    fn banner_due(&self) -> Due {
        let timed = match (self.tray.get(), self.showing.get()) {
            (TrayState::Shown, Some(id)) => self
                .center
                .lock()
                .is_ok_and(|center| {
                    center
                        .banners()
                        .iter()
                        .any(|n| n.id == id && n.urgency != Urgency::Critical)
                })
                .then_some(id),
            _ => None,
        };
        let mut timer = self.timer.borrow_mut();
        let Some(id) = timed else {
            *timer = None;
            return Due::default();
        };
        // Ask for the idle time only while it decides: when the banner
        // has just dropped in to a user not yet seen active, or while it
        // waits for an away user.
        let idle =
            if !self.user_active.get() && timer.as_ref().is_none_or(|t| t.deadline().is_none()) {
                idle_time()
            } else {
                None
            };
        let now = Instant::now();
        // A user already seen active (an updated banner shown again) has
        // no idle time asked: the active user's four seconds.
        let timer = timer.get_or_insert_with(|| BannerTimer::new(now, idle));
        let due = due([(id, &mut *timer)], now, idle);
        if timer.deadline().is_some() {
            self.user_active.set(true);
        }
        due
    }

    /// Connect the bus unless it is ours or a retry is already pending;
    /// on failure, try again after [`BUS_RETRY`].
    fn ensure_bus(self: &Rc<Self>) {
        if self.bus.borrow().connected() || self.bus_retry.borrow().is_some() {
            return;
        }
        if self.bus.borrow_mut().ensure() {
            return;
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(BUS_RETRY, move || {
            if let Some(me) = weak.upgrade() {
                me.bus_retry.take();
                me.update();
            }
        });
        *self.bus_retry.borrow_mut() = Some(id);
    }

    /// Wake at `at` (replacing any earlier wake), or not at all.
    fn arm_deadline(self: &Rc<Self>, at: Option<Instant>) {
        if let Some(id) = self.deadline.take() {
            id.remove();
        }
        let Some(at) = at else {
            return;
        };
        let weak = Rc::downgrade(self);
        let id = timeout_after(at.saturating_duration_since(Instant::now()), move || {
            if let Some(me) = weak.upgrade() {
                me.deadline.take();
                me.update();
            }
        });
        *self.deadline.borrow_mut() = Some(id);
    }

    /// Ask the idle monitor to say when the away user next does
    /// something (GNOME's message tray waits on the same watch); its
    /// `WatchFired` runs [`NotifyUi::update`], which starts the banner's
    /// two seconds.
    fn watch_user_active(self: &Rc<Self>) {
        if self.user_watch.get() != UserWatch::Off {
            return;
        }
        let Ok(conn) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
            self.user_watch.set(UserWatch::Unavailable);
            return;
        };
        if self.user_watch_signal.borrow().is_none() {
            let weak = Rc::downgrade(self);
            let subscription = conn.subscribe_to_signal(
                Some(IDLE_MONITOR),
                Some(IDLE_MONITOR),
                Some("WatchFired"),
                Some(IDLE_MONITOR_CORE),
                None,
                gio::DBusSignalFlags::NONE,
                move |signal| {
                    let Some(me) = weak.upgrade() else {
                        return;
                    };
                    let Some((id,)) = signal.parameters.get::<(u32,)>() else {
                        return;
                    };
                    if me.user_watch.get() == UserWatch::Armed(id) {
                        me.user_watch.set(UserWatch::Off);
                        me.update();
                    }
                },
            );
            *self.user_watch_signal.borrow_mut() = Some(subscription);
        }
        self.user_watch.set(UserWatch::Pending);
        let weak = Rc::downgrade(self);
        conn.call(
            Some(IDLE_MONITOR),
            IDLE_MONITOR_CORE,
            IDLE_MONITOR,
            "AddUserActiveWatch",
            None,
            glib::VariantTy::new("(u)").ok(),
            gio::DBusCallFlags::NO_AUTO_START,
            1000,
            gio::Cancellable::NONE,
            move |reply| {
                let Some(me) = weak.upgrade() else {
                    return;
                };
                match reply.ok().and_then(|r| r.get::<(u32,)>()) {
                    Some((id,)) => me.user_watch.set(UserWatch::Armed(id)),
                    None => me.user_watch.set(UserWatch::Unavailable),
                }
                // Input between reading the idle time and arming the
                // watch would not fire it: look again now.
                me.update();
            },
        );
    }

    fn refresh(self: &Rc<Self>) {
        self.refresh_list();
        self.sync_banner();
    }

    /// Redraw the calendar's list and its placeholder on change.
    fn refresh_list(self: &Rc<Self>) {
        let Ok(center) = self.center.lock() else {
            return;
        };
        // Reconcile even when the visible view is unchanged. The center can
        // remove/prune entries independently of this UI; stale widgets may
        // still exist, but their local callbacks must no longer own state.
        self.local_actions.borrow_mut().retain_history(&center);
        let groups = group_ids(center.history().iter().rev().map(|n| (n.id, group_key(n))));
        // A group that falls back to one card (or goes) forgets it was
        // expanded.
        self.expanded
            .borrow_mut()
            .retain(|k| groups.iter().any(|(g, ids)| g == k && ids.len() > 1));
        let mut expanded: Vec<String> = self.expanded.borrow().iter().cloned().collect();
        expanded.sort();
        let now = Shown {
            history: center.history().iter().rev().map(|n| n.id).collect(),
            replacements: center.replacements(),
            expanded,
            dnd: center.dnd(),
        };
        if self.shown.borrow().as_ref() == Some(&now) {
            return;
        }
        let before = self.shown.take().unwrap_or_default();
        // History list. Like GNOME's message list it is only built for the
        // open calendar: rebuilding every card of every group on each
        // arrival and expiry while the list was off screen was most of the
        // shell's notification CPU (#782). It catches up when it maps.
        if self.list.is_mapped() {
            self.build_history(&center, &groups, &before.history, &now.expanded);
        } else {
            self.history_stale.set(true);
        }
        let any = !now.history.is_empty();
        self.list.set_visible(any);
        if self.empty.is_visible() == any {
            // The placeholder changes: from here on Clear follows it.
            self.clear_bound.set(true);
        }
        self.empty.set_visible(!any);
        self.clear.set_visible(!self.clear_bound.get() || any);
        self.clear.set_sensitive(any);
        drop(center);
        *self.shown.borrow_mut() = Some(now);
        // Showing the list above maps it while the store was held.
        if self.list.is_mapped() {
            self.sync_history();
        }
    }

    /// Move the banner surface along GNOME's message tray cycle (see
    /// [`tray_step`]): one banner on screen at a time, the next one
    /// dropping in once the last has gone, the one on screen showing
    /// again when it was updated.
    fn sync_banner(self: &Rc<Self>) {
        if self.banner_blocked.get() {
            // `bannerBlocked`: the open date menu hides the tray at once;
            // the banner keeps its place until the menu closes.
            self.banner_window.set_visible(false);
            return;
        }
        let (step, card) = {
            let Ok(mut center) = self.center.lock() else {
                return;
            };
            let queue: Vec<(u64, Urgency, bool)> = center
                .banners()
                .iter()
                .map(|n| (n.id, n.urgency, n.acknowledged()))
                .collect();
            let step = tray_step(self.tray.get(), self.showing.get(), &queue);
            let card = match step {
                TrayStep::Show(id) => center.banners().into_iter().find(|n| n.id == id).map(|n| {
                    let spoken = announcement(&source_of(n).0, n.summary(), n.body());
                    (self.banner_card(n), spoken)
                }),
                _ => None,
            };
            if let TrayStep::Show(id) = step {
                // `_updateShowingNotification`: what shows is seen.
                center.acknowledge(&[id]);
            }
            (step, card)
        };
        match (step, card) {
            (TrayStep::Show(id), Some((card, spoken))) => {
                while let Some(child) = self.banner_box.first_child() {
                    self.banner_box.remove(&child);
                }
                self.banner_box.append(&card);
                let again = self.showing.replace(Some(id)) == Some(id);
                if !again {
                    self.user_active.set(false);
                }
                *self.timer.borrow_mut() = None;
                self.enter_banner(again);
                if !again {
                    crate::accessibility::announce(&self.banner_window, &spoken);
                }
            }
            (TrayStep::Hide, _) => {
                // GNOME slides out only an expired banner; a dismissed
                // or closed one goes at once.
                let expired = self.expired.take() == self.showing.get();
                self.leave_banner(expired && self.banner_window.is_visible());
            }
            _ => {
                // The menu that blocked the banner closed: it shows again.
                let on = matches!(self.tray.get(), TrayState::Showing | TrayState::Shown);
                if on && !self.banner_window.is_visible() {
                    self.banner_window.set_visible(true);
                }
            }
        }
    }

    /// One banner card, its buttons wired to the bus.
    fn banner_card(self: &Rc<Self>, n: &Notification) -> gtk::Box {
        let (c, close) = card(n);
        c.add_css_class("banner");
        let has_default = n.actions().iter().any(|a| a.id == "default");
        if n.actions().iter().any(|a| a.id != "default") {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            for action in n.actions().iter().filter(|a| a.id != "default") {
                let button = gtk::Button::with_label(&action.label);
                let (me, id, key) = (self.clone(), n.id, action.id.clone());
                button.connect_clicked(move |_| {
                    // Bus first: its per-key replay guard rejects
                    // unknown/replayed keys. Local dispatch then fires
                    // only for admitted shell-own (id, key) pairs and
                    // retires the whole resident on success, so no
                    // sibling key captured before the press can fire
                    // afterwards. Unregistered notices behave exactly
                    // as before.
                    if me.bus.borrow().invoke_action(id as u32, &key).is_ok() {
                        me.dispatch_local_action(id, &key);
                    }
                    me.refresh();
                });
                row.append(&button);
            }
            c.append(&row);
        }
        {
            let (me, id) = (self.clone(), n.id);
            close.connect_clicked(move |_| {
                let _ = me.bus.borrow().dismiss_banner(id as u32, REASON_DISMISSED);
                me.retire_local_actions(id);
                me.refresh();
            });
        }
        // GNOME's banner is a focusable button (messageList.js): the
        // focus-active-notification key lands on it, and Enter or Space
        // does what a click does.
        c.set_focusable(true);
        let activate: Rc<dyn Fn()> = {
            let (me, id) = (self.clone(), n.id);
            Rc::new(move || {
                let bus = me.bus.borrow();
                let _ = if has_default {
                    bus.invoke_action(id as u32, "default")
                } else {
                    bus.dismiss_banner(id as u32, REASON_DISMISSED)
                };
                drop(bus);
                me.retire_local_actions(id);
                me.refresh();
            })
        };
        let click = gtk::GestureClick::new();
        {
            let activate = activate.clone();
            click.connect_released(move |_, _, _, _| activate());
        }
        c.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |keys, key, _, _| {
            use gtk::gdk::Key;
            // Only the card's own focus: its buttons handle their keys.
            let on_card = keys.widget().is_some_and(|card| card.has_focus());
            match key {
                Key::Return | Key::KP_Enter | Key::space if on_card => {
                    activate();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        c.add_controller(keys);
        c
    }

    /// Build the history list deferred while it was hidden. Its cards
    /// arrived off screen, so none zooms in.
    fn sync_history(self: &Rc<Self>) {
        if !self.history_stale.get() {
            return;
        }
        // Held by a refresh that is mapping the list: it syncs after.
        let Ok(center) = self.center.try_lock() else {
            return;
        };
        let groups = group_ids(center.history().iter().rev().map(|n| (n.id, group_key(n))));
        let (listed, expanded) = {
            let shown = self.shown.borrow();
            let shown = shown.as_ref();
            (
                shown.map(|s| s.history.clone()).unwrap_or_default(),
                shown.map(|s| s.expanded.clone()).unwrap_or_default(),
            )
        };
        self.build_history(&center, &groups, &listed, &expanded);
    }

    /// Replace the history list's cards and groups. `listed` are the ids
    /// already on the list: only the others zoom in.
    fn build_history(
        self: &Rc<Self>,
        center: &NotificationCenter,
        groups: &[(String, Vec<u64>)],
        listed: &[u64],
        expanded: &[String],
    ) {
        self.history_stale.set(false);
        self.group_widgets.borrow_mut().clear();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let by_id: HashMap<u64, &Notification> =
            center.history().iter().map(|n| (n.id, n)).collect();
        for (key, ids) in groups {
            let members: Vec<&Notification> =
                ids.iter().filter_map(|id| by_id.get(id).copied()).collect();
            match members.as_slice() {
                [] => {}
                // A group of one is just its card.
                [n] => {
                    let (c, close) = card(n);
                    let item = crate::transient::MotionBin::new(&c);
                    let (me, id, weak) = (self.clone(), n.id, item.downgrade());
                    close.connect_clicked(move |_| {
                        // GNOME zooms the message out, then removes it.
                        let Some(item) = weak.upgrade() else {
                            return;
                        };
                        // Like add_item, animate only a list on screen: an
                        // unmapped item gets no frames, so its zoom would never
                        // finish and the removal (with the notice's callbacks)
                        // would never run.
                        if !item.is_mapped() {
                            me.remove(&[id]);
                            return;
                        }
                        item.set_can_target(false);
                        let me = me.clone();
                        item.animate(
                            &crate::transient::Player::new("message"),
                            crate::transient::MESSAGE_MS,
                            |_, t| crate::transient::message(t, false),
                            move || me.remove(&[id]),
                        );
                    });
                    self.add_item(&item, !listed.contains(&n.id));
                }
                _ => {
                    let group = crate::group_animation::Group::new(
                        &self.collapsed_group(key, &members),
                        &self.expanded_group(key, &members),
                        expanded.contains(key),
                    );
                    let item = crate::transient::MotionBin::new(&group.widget);
                    // A group is new when none of its cards were listed.
                    self.add_item(&item, members.iter().all(|n| !listed.contains(&n.id)));
                    self.group_widgets.borrow_mut().insert(key.clone(), group);
                }
            }
        }
    }

    /// Append a list item, zooming it in when it is new and the list is
    /// on screen (`_addMessage`).
    fn add_item(&self, item: &crate::transient::MotionBin, new: bool) {
        self.list.append(item);
        if new && self.list.is_mapped() {
            item.animate(
                &crate::transient::Player::new("message"),
                crate::transient::MESSAGE_MS,
                |_, t| crate::transient::message(t, true),
                || {},
            );
        }
    }

    /// Drop the banner in: fade, slide down and grow from 90%. `again`
    /// shows the banner already on screen (or sliding out) again after
    /// an update, from where it stands. Its four seconds start once it
    /// has (`_showNotificationCompleted`).
    fn enter_banner(self: &Rc<Self>, again: bool) {
        self.tray.set(TrayState::Showing);
        self.banner_bin.set_can_target(true);
        self.banner_window.set_visible(true);
        let from = again.then(|| self.banner_bin.frame());
        let weak = Rc::downgrade(self);
        self.banner_bin.animate(
            &self.banner_motion,
            crate::transient::BANNER_MS,
            move |bin, t| match from {
                Some(from) => crate::transient::banner_reshow(from, t),
                None => crate::transient::banner_enter(t, f64::from(bin.height())),
            },
            move || {
                if let Some(me) = weak.upgrade() {
                    me.tray.set(TrayState::Shown);
                    me.update();
                }
            },
        );
    }

    /// Take the banner away, sliding it back up when `slide`, then let
    /// the next queued one drop in (`_hideNotification`).
    fn leave_banner(self: &Rc<Self>, slide: bool) {
        self.tray.set(TrayState::Hiding);
        self.banner_bin.set_can_target(false);
        if self.banner_window.is_layer_window() {
            self.banner_window.set_keyboard_mode(KeyboardMode::None);
        }
        let weak = Rc::downgrade(self);
        let done = move || {
            let Some(me) = weak.upgrade() else {
                return;
            };
            me.banner_window.set_visible(false);
            while let Some(child) = me.banner_box.first_child() {
                me.banner_box.remove(&child);
            }
            me.banner_bin.set_frame(crate::transient::Frame::REST);
            me.banner_bin.set_can_target(true);
            me.showing.set(None);
            me.tray.set(TrayState::Hidden);
            me.update();
        };
        if slide {
            self.banner_bin.animate(
                &self.banner_motion,
                crate::transient::BANNER_MS,
                |bin, t| crate::transient::banner_leave(t, f64::from(bin.height())),
                done,
            );
        } else {
            self.banner_motion.stop();
            done();
        }
    }

    /// Close notifications from the list (and any banner they hold).
    fn remove(self: &Rc<Self>, ids: &[u64]) {
        for &id in ids {
            let _ = self
                .bus
                .borrow()
                .dismiss_banner(id as u32, REASON_DISMISSED);
            self.retire_local_actions(id);
            if let Ok(mut center) = self.center.lock() {
                center.remove(id);
            }
        }
        self.refresh();
    }

    fn set_expanded(self: &Rc<Self>, key: &str, on: bool) {
        if on {
            self.expanded.borrow_mut().insert(key.to_owned());
        } else {
            self.expanded.borrow_mut().remove(key);
        }
        let group = self.group_widgets.borrow().get(key).cloned();
        if let Some(group) = group {
            group.set_expanded(on);
            // Keep the next refresh from rebuilding the group mid-animation.
            let mut expanded: Vec<_> = self.expanded.borrow().iter().cloned().collect();
            expanded.sort();
            if let Some(shown) = self.shown.borrow_mut().as_mut() {
                shown.expanded = expanded;
            }
        } else {
            self.refresh();
        }
    }

    /// A collapsed group (2+ notifications), as GNOME 51 stacks it: the
    /// newest card in full, the next ones peeking out below it, each
    /// inset more and darker. Clicking expands it; closing the top card
    /// closes the whole group.
    fn collapsed_group(self: &Rc<Self>, key: &str, members: &[&Notification]) -> gtk::Button {
        let (top, close) = card(members[0]);
        let layout = stack_layout(members.len());
        let reveal = layout.last().map_or(0, |&(_, reach)| reach);
        top.set_margin_bottom(reveal);
        // The top card is the overlay's main child, so it sizes the group
        // and gets its height for the list's width (a measured overlay is
        // allocated at least its height for its minimum width, which a
        // wrapping card overshoots). The peeking cards fill the overlay
        // above the card's margin, lowest first.
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&top));
        for (k, &(inset, reach)) in layout.iter().enumerate().rev() {
            let stub = gtk::Box::new(gtk::Orientation::Vertical, 0);
            stub.add_css_class("message-stack-card");
            stub.add_css_class(if k == 0 {
                "second-in-stack"
            } else {
                "lower-in-stack"
            });
            stub.set_margin_start(inset);
            stub.set_margin_end(inset);
            stub.set_margin_bottom(reveal - reach);
            stub.set_can_target(false);
            overlay.add_overlay(&stub);
        }
        // Overlays draw over the main child; moving the card to the end
        // of the overlay's children draws it over the stack instead
        // (layout still treats it as the main child).
        top.insert_before(&overlay, gtk::Widget::NONE);

        let group = gtk::Button::new();
        group.set_child(Some(&overlay));
        group.add_css_class("message-group");
        group.add_css_class("collapsed");
        let (title, _) = source_of(members[0]);
        group.update_property(&[
            gtk::accessible::Property::Label(&title),
            gtk::accessible::Property::Description(&format!(
                "{} notifications, collapsed",
                members.len()
            )),
        ]);
        {
            let (me, key) = (self.clone(), key.to_owned());
            group.connect_clicked(move |_| me.set_expanded(&key, true));
        }
        let (me, ids) = (
            self.clone(),
            members.iter().map(|n| n.id).collect::<Vec<_>>(),
        );
        close.connect_clicked(move |_| me.remove(&ids));
        group
    }

    /// An expanded group: a header with the app name and a round
    /// collapse button, then every card.
    fn expanded_group(self: &Rc<Self>, key: &str, members: &[&Notification]) -> gtk::Box {
        let (title, _) = source_of(members[0]);
        let group = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        group.add_css_class("message-group");
        group.add_css_class("expanded");
        group.update_property(&[gtk::accessible::Property::Label(&title)]);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.add_css_class("message-group-header");
        let label = gtk::Label::new(Some(&title));
        label.add_css_class("message-group-title");
        label.set_hexpand(true);
        label.set_xalign(0.0);
        header.append(&label);
        let collapse = gtk::Button::from_icon_name("group-collapse-symbolic");
        collapse.add_css_class("message-collapse-button");
        collapse.set_valign(gtk::Align::Center);
        collapse.update_property(&[gtk::accessible::Property::Label("Collapse")]);
        {
            let (me, key) = (self.clone(), key.to_owned());
            collapse.connect_clicked(move |_| me.set_expanded(&key, false));
        }
        header.append(&collapse);
        group.append(&header);
        for n in members {
            let (c, close) = card(n);
            let (me, id) = (self.clone(), n.id);
            close.connect_clicked(move |_| me.remove(&[id]));
            group.append(&c);
        }
        group
    }
}

#[cfg(test)]
mod group_tests {
    use super::*;

    #[test]
    fn banner_announcement_is_role_then_source_title_and_body() {
        assert_eq!(
            announcement("Tuna Proof", "Build finished", "All tests passed"),
            "Notification, Tuna Proof, Build finished, All tests passed"
        );
        assert_eq!(
            announcement("Tuna Proof", "Build finished", ""),
            "Notification, Tuna Proof, Build finished"
        );
    }

    #[test]
    fn notifications_group_per_app_newest_first() {
        // History newest first: b2, a2, a1, c, b1.
        let history = [(5, "b"), (4, "a"), (3, "a"), (2, "c"), (1, "b")];
        assert_eq!(
            group_ids(history),
            vec![("b", vec![5, 1]), ("a", vec![4, 3]), ("c", vec![2])]
        );
        assert!(group_ids(Vec::<(u64, &str)>::new()).is_empty());
    }

    #[test]
    fn collapsed_stack_peeks_like_gnome() {
        // One card: nothing peeks.
        assert!(stack_layout(1).is_empty());
        // The second card 10 px below, 6 px in per side.
        assert_eq!(stack_layout(2), vec![(6, 10)]);
        // The third 10/1.4 px further, 12 px in; never more than three.
        assert_eq!(stack_layout(3), vec![(6, 10), (12, 17)]);
        assert_eq!(stack_layout(7), stack_layout(3));
    }
}

#[cfg(test)]
mod action_dispatch_tests {
    use super::*;
    use std::cell::Cell;

    fn counting(flag: &Rc<Cell<bool>>) -> Rc<dyn Fn()> {
        let flag = flag.clone();
        Rc::new(move || flag.set(true))
    }

    #[test]
    fn removed_history_releases_callbacks_without_invocation() {
        let mut center = NotificationCenter::new();
        let old = center.notify("old", "old", "", vec![], Urgency::Normal, None);
        let other = center.notify("other", "other", "", vec![], Urgency::Normal, None);
        let fired = Rc::new(Cell::new(false));
        let weak = Rc::downgrade(&fired);
        let mut dispatch = ActionDispatch::default();
        dispatch.register(old, "take", counting(&fired));
        dispatch.register(other, "take", Rc::new(|| {}));
        drop(fired);
        center.dismiss(old).unwrap();
        dispatch.retain_history(&center);
        assert!(
            weak.upgrade().is_some(),
            "banner absence must not mean history removal"
        );
        assert!(dispatch.live.contains(&old));
        assert!(
            !weak.upgrade().unwrap().get(),
            "reconciliation must not invoke"
        );
        center.remove(old);
        dispatch.retain_history(&center);
        assert!(
            weak.upgrade().is_none(),
            "removed callback must release captured state"
        );
        assert!(!dispatch.live.contains(&old));
        assert_eq!(dispatch.handlers.len(), 1);
        assert!(!dispatch.dispatch(old, "take"));
        center.clear_history();
        dispatch.retain_history(&center);
        assert!(dispatch.handlers.is_empty() && dispatch.live.is_empty());
    }

    #[test]
    fn bounded_history_pruning_retires_only_missing_ids() {
        let mut center = NotificationCenter::new();
        let old = center.notify("old", "old", "", vec![], Urgency::Normal, None);
        let fired = Rc::new(Cell::new(false));
        let mut dispatch = ActionDispatch::default();
        dispatch.register(old, "take", counting(&fired));
        let mut last = old;
        for _ in 0..tuna_shell_host::notifications::MAX_HISTORY {
            last = center.notify("new", "new", "", vec![], Urgency::Normal, None);
        }
        dispatch.register(last, "take", counting(&fired));
        dispatch.retain_history(&center);
        assert!(!dispatch.live.contains(&old));
        assert_eq!(dispatch.handlers.len(), 1);
        assert!(!dispatch.dispatch(old, "take"));
        assert!(!fired.get());
        assert!(dispatch.live.contains(&last));
    }

    #[test]
    fn registered_keys_fire_and_unknown_keys_do_not() {
        let mut dispatch = ActionDispatch::default();
        let fired = Rc::new(Cell::new(false));
        dispatch.register(7, "delay", counting(&fired));
        // Denied: unknown key on a known notice never fires.
        assert!(!dispatch.dispatch(7, "snooze"));
        assert!(!fired.get());
        // Denied: unknown notice never fires.
        assert!(!dispatch.dispatch(8, "delay"));
        assert!(!fired.get());
        // Registered pair fires exactly.
        assert!(dispatch.dispatch(7, "delay"));
        assert!(fired.get());
    }

    #[test]
    fn dropped_notices_stop_firing_like_closed_ones() {
        let mut dispatch = ActionDispatch::default();
        let fired = Rc::new(Cell::new(false));
        dispatch.register(7, "take", counting(&fired));
        dispatch.drop_notice(7);
        assert!(!dispatch.dispatch(7, "take"));
        assert!(!fired.get());
    }

    #[test]
    fn replacement_forgets_the_old_resident_only() {
        let mut dispatch = ActionDispatch::default();
        let old = Rc::new(Cell::new(false));
        let new = Rc::new(Cell::new(false));
        dispatch.register(7, "skip", counting(&old));
        dispatch.register(9, "skip", counting(&new));
        dispatch.drop_notice(7);
        assert!(!dispatch.dispatch(7, "skip"));
        assert!(dispatch.dispatch(9, "skip"));
        assert!(!old.get());
        assert!(new.get());
    }

    #[test]
    fn reregistering_replaces_the_handler() {
        let mut dispatch = ActionDispatch::default();
        let first = Rc::new(Cell::new(false));
        let second = Rc::new(Cell::new(false));
        dispatch.register(7, "delay", counting(&first));
        dispatch.register(7, "delay", counting(&second));
        assert!(dispatch.dispatch(7, "delay"));
        assert!(!first.get());
        assert!(second.get());
    }

    #[test]
    fn first_action_retires_captured_sibling_keys() {
        let mut dispatch = ActionDispatch::default();
        let take = Rc::new(Cell::new(false));
        let delay = Rc::new(Cell::new(false));
        dispatch.register(7, "take", counting(&take));
        dispatch.register(7, "delay", counting(&delay));
        // First press wins...
        assert!(dispatch.dispatch(7, "take"));
        assert!(take.get());
        // ...then the sibling key captured before the press is dead, as
        // is a replay of the fired key: the resident is retired.
        assert!(!dispatch.dispatch(7, "delay"));
        assert!(!dispatch.dispatch(7, "take"));
        assert!(!delay.get());
    }

    #[test]
    fn dismiss_retires_all_keys_but_keeps_other_residents() {
        let mut dispatch = ActionDispatch::default();
        let take = Rc::new(Cell::new(false));
        let other = Rc::new(Cell::new(false));
        dispatch.register(7, "take", counting(&take));
        dispatch.register(7, "skip", counting(&take));
        dispatch.register(9, "take", counting(&other));
        // Same call the banner-close, body-click, expiry, removal, and
        // programmatic-dismiss paths make: retire without firing.
        dispatch.drop_notice(7);
        assert!(!dispatch.dispatch(7, "take"));
        assert!(!dispatch.dispatch(7, "skip"));
        assert!(!take.get());
        // A different live resident is unaffected.
        assert!(dispatch.dispatch(9, "take"));
        assert!(other.get());
    }

    #[test]
    fn retired_resident_never_readmits_without_register() {
        let mut dispatch = ActionDispatch::default();
        let fired = Rc::new(Cell::new(false));
        dispatch.register(7, "take", counting(&fired));
        assert!(dispatch.dispatch(7, "take"));
        // Retired by the dispatch above; no re-fire without a fresh post.
        assert!(!dispatch.dispatch(7, "take"));
        // A fresh post for the same id re-admits exactly its new keys.
        dispatch.register(7, "take", counting(&fired));
        fired.set(false);
        assert!(dispatch.dispatch(7, "take"));
        assert!(fired.get());
    }
}

#[cfg(test)]
mod timer_tests {
    use super::*;

    #[test]
    fn banners_time_out_like_gnome() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        // Shown to an active user: four seconds.
        let mut t = BannerTimer::new(t0, Some(Duration::from_millis(200)));
        assert!(!t.expired(t0 + Duration::from_millis(3900), Some(s(3))));
        assert!(t.expired(t0 + s(4), Some(s(4))));
        // Shown to an away user: stays while they stay away...
        let mut t = BannerTimer::new(t0, Some(s(30)));
        assert!(!t.expired(t0 + s(60), Some(s(90))));
        // ...and goes two seconds after they come back.
        assert!(!t.expired(t0 + s(61), Some(Duration::from_millis(500))));
        assert!(t.expired(t0 + Duration::from_millis(62_500), Some(s(2))));
        // No idle monitor: the user counts as active.
        let mut t = BannerTimer::new(t0, None);
        assert!(t.expired(t0 + s(4), None));
    }

    #[test]
    fn due_collects_expiries_the_next_deadline_and_away_banners() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let active = Some(Duration::from_millis(200));
        let mut early = BannerTimer::new(t0, active);
        let mut late = BannerTimer::new(t0 + s(3), active);
        // Nothing due yet: wake when the first banner goes.
        assert_eq!(
            due([(1, &mut early), (2, &mut late)], t0 + s(1), active),
            Due {
                expired: Vec::new(),
                next: Some(t0 + s(4)),
                away: false,
            }
        );
        // The first goes; wake for the second.
        assert_eq!(
            due([(1, &mut early), (2, &mut late)], t0 + s(4), Some(s(4))),
            Due {
                expired: vec![1],
                next: Some(t0 + s(7)),
                away: false,
            }
        );
        // A banner shown to an away user has no deadline until they
        // come back, then two seconds from their input.
        let mut waiting = BannerTimer::new(t0, Some(s(30)));
        let away = due([(3, &mut waiting)], t0 + s(5), Some(s(35)));
        assert_eq!(away.next, None);
        assert!(away.away && away.expired.is_empty());
        let back = due(
            [(3, &mut waiting)],
            t0 + s(6),
            Some(Duration::from_millis(500)),
        );
        assert_eq!(back.next, Some(t0 + Duration::from_millis(7_500)));
        assert!(!back.away);
        assert_eq!(due(std::iter::empty(), t0, None), Due::default());
    }

    #[test]
    fn one_banner_at_a_time_like_the_message_tray() {
        use TrayState::*;
        let (n, c) = (Urgency::Normal, Urgency::Critical);
        // (id, urgency, acknowledged): a banner is acknowledged once it
        // has shown.
        // Nothing queued: nothing shows.
        assert_eq!(tray_step(Hidden, None, &[]), TrayStep::Stay);
        // The oldest queued banner drops in, never two at once...
        assert_eq!(
            tray_step(Hidden, None, &[(1, n, false), (2, n, false)]),
            TrayStep::Show(1)
        );
        // ...and a critical one goes first.
        assert_eq!(
            tray_step(Hidden, None, &[(1, n, false), (2, c, false), (3, c, false)]),
            TrayStep::Show(2)
        );
        // GNOME's HIGH ranks between normal and critical.
        let h = Urgency::High;
        assert_eq!(
            tray_step(Hidden, None, &[(1, n, false), (2, h, false)]),
            TrayStep::Show(2)
        );
        assert_eq!(
            tray_step(Hidden, None, &[(1, h, false), (2, c, false)]),
            TrayStep::Show(2)
        );
        // Newer arrivals wait while one is dropping in or on screen,
        // critical or not: the banner on screen is never replaced.
        for state in [Showing, Shown] {
            assert_eq!(
                tray_step(
                    state,
                    Some(1),
                    &[(1, n, true), (2, c, false), (3, n, false)]
                ),
                TrayStep::Stay
            );
            // It goes once it leaves the queue (expired, dismissed, closed).
            assert_eq!(tray_step(state, Some(1), &[(2, n, false)]), TrayStep::Hide);
        }
        // The next waits for the last to slide out, unless the one
        // sliding out was queued again.
        assert_eq!(tray_step(Hiding, Some(1), &[(2, n, false)]), TrayStep::Stay);
        assert_eq!(
            tray_step(Hiding, Some(1), &[(2, n, false), (1, n, false)]),
            TrayStep::Show(1)
        );
    }

    #[test]
    fn an_updated_banner_on_screen_shows_again_in_place() {
        use TrayState::*;
        let n = Urgency::Normal;
        // The banner on screen was replaced (unacknowledged again): it
        // shows again at once, ahead of the queue, as GNOME's
        // `_updateShowingNotification` does for the notification already
        // up (a screen-time warning turning into "Limit Reached").
        for state in [Showing, Shown] {
            assert_eq!(
                tray_step(state, Some(1), &[(2, n, false), (1, n, false)]),
                TrayStep::Show(1)
            );
        }
        // Once shown again it is seen, and stays.
        assert_eq!(
            tray_step(Shown, Some(1), &[(2, n, false), (1, n, true)]),
            TrayStep::Stay
        );
    }
}

#[cfg(test)]
mod gtk_action_lifecycle_tests {
    use super::*;
    use std::cell::Cell;

    fn button(root: &impl IsA<gtk::Widget>, wanted: &str) -> gtk::Button {
        fn collect(widget: &gtk::Widget, wanted: &str, found: &mut Vec<gtk::Button>) {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if button.label().as_deref() == Some(wanted)
                    || (wanted == "close" && button.has_css_class("message-close-button"))
                {
                    found.push(button.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                collect(&widget, wanted, found);
                child = widget.next_sibling();
            }
        }
        let mut found = Vec::new();
        collect(root.as_ref(), wanted, &mut found);
        assert_eq!(found.len(), 1, "one captured {wanted} button required");
        found.pop().unwrap()
    }

    /// Every label's text under `root`, in widget order.
    fn labels(root: &impl IsA<gtk::Widget>) -> Vec<String> {
        fn collect(widget: &gtk::Widget, found: &mut Vec<String>) {
            if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                found.push(label.text().to_string());
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                collect(&widget, found);
                child = widget.next_sibling();
            }
        }
        let mut found = Vec::new();
        collect(root.as_ref(), &mut found);
        found
    }

    /// Let the banner finish dropping in or sliding out, as a running
    /// session would: one banner at a time, so the next waits for it.
    fn settle(ui: &Rc<NotifyUi>) {
        let end = Instant::now() + Duration::from_secs(5);
        while matches!(ui.tray.get(), TrayState::Showing | TrayState::Hiding) {
            assert!(Instant::now() < end, "banner never settled");
            glib::MainContext::default().iteration(true);
        }
    }

    fn post(ui: &Rc<NotifyUi>) -> (u64, gtk::Button, gtk::Button, Rc<Cell<u32>>) {
        settle(ui);
        let id = ui
            .post_with_actions(
                "Owned callback fixture",
                "",
                "Break",
                "",
                &[
                    ("take".into(), "Take".into()),
                    ("delay".into(), "Delay".into()),
                ],
                Urgency::Normal,
                None,
            )
            .unwrap();
        settle(ui);
        assert_eq!(ui.showing.get(), Some(id), "the new banner is on screen");
        let fired = Rc::new(Cell::new(0));
        for key in ["take", "delay"] {
            let count = fired.clone();
            ui.on_notification_action(id, key, Rc::new(move || count.set(count.get() + 1)));
        }
        (
            id,
            button(&ui.banner_box, "Take"),
            button(&ui.banner_box, "Delay"),
            fired,
        )
    }

    #[test]
    #[ignore = "requires an owned Xvfb/private D-Bus session and explicit admission"]
    fn actual_gtk_captured_actions_obey_resident_lifecycle() {
        assert_eq!(std::env::var("TUNA_REQUIRE_NOTIFY_UI").as_deref(), Ok("1"));
        assert!(std::env::var("DBUS_SESSION_BUS_ADDRESS").is_ok());
        assert!(std::env::var_os("WAYLAND_DISPLAY").is_none());
        assert!(std::env::var_os("WAYLAND_SOCKET").is_none());
        gtk::init().expect("real GTK display required");
        let app = gtk::Application::builder()
            .application_id("org.tuna.NotifyCallbackProof")
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        let center = Arc::new(Mutex::new(NotificationCenter::new()));
        let ui = NotifyUi::build(&app, center.clone(), false);
        // Real clicked callbacks, retained even after their widgets leave the UI.
        let (id, take, delay, fired) = post(&ui);
        take.emit_clicked();
        take.emit_clicked();
        delay.emit_clicked();
        assert_eq!(
            fired.get(),
            1,
            "Take wins exactly once, stale sibling stays inert"
        );
        assert_eq!(center.lock().unwrap().history().len(), 1);
        ui.clear.emit_clicked();
        assert!(center.lock().unwrap().history().is_empty());
        assert!(ui.bus.borrow().invoke_action(id as u32, "delay").is_err());

        for terminal in [
            "user-close",
            "remove",
            "replacement",
            "timeout",
            "history-close",
            "center-remove",
            "clear",
        ] {
            let (id, take, delay, fired) = post(&ui);
            ui.refresh(); // Unchanged visible state must preserve admitted actions.
            assert_eq!(ui.local_actions.borrow().handlers.len(), 2);
            assert!(ui.local_actions.borrow().live.contains(&id));
            match terminal {
                "user-close" => button(&ui.banner_box, "close").emit_clicked(),
                "remove" => ui.remove_notification(id),
                "replacement" => {
                    // Production wellbeing updates its resident in place
                    // through these public APIs: old callbacks retire.
                    ui.drop_notification_actions(id);
                    let same = ui.post_with_actions(
                        "Owned callback fixture",
                        "",
                        "Break 2",
                        "",
                        &[],
                        Urgency::High,
                        Some(id),
                    );
                    assert_eq!(same, Some(id));
                }
                "timeout" => {
                    *ui.timer.borrow_mut() = Some(BannerTimer {
                        shown: Instant::now() - Duration::from_secs(10),
                        deadline: Some(Instant::now() - Duration::from_secs(1)),
                    });
                    ui.update(); // Real expiry and retirement path, no sleep/retry.
                    assert!(!center.lock().unwrap().banners().iter().any(|n| n.id == id));
                }
                "history-close" => {
                    // This window never maps the list: build it as opening
                    // the calendar would.
                    ui.sync_history();
                    button(&ui.list, "close").emit_clicked()
                }
                "center-remove" => {
                    center.lock().unwrap().remove(id);
                    ui.refresh();
                }
                "clear" => ui.clear.emit_clicked(),
                _ => unreachable!(),
            }
            assert!(
                ui.local_actions.borrow().handlers.is_empty(),
                "unused callbacks after {terminal}"
            );
            assert!(
                ui.local_actions.borrow().live.is_empty(),
                "unused admission after {terminal}"
            );
            assert_eq!(
                Rc::strong_count(&fired),
                1,
                "callback capture not released after {terminal}"
            );
            take.emit_clicked();
            delay.emit_clicked();
            assert_eq!(
                fired.get(),
                0,
                "captured old callbacks fired after {terminal}"
            );
            if matches!(
                terminal,
                "clear" | "history-close" | "center-remove" | "remove"
            ) {
                assert!(ui.bus.borrow().invoke_action(id as u32, "take").is_err());
            } else {
                assert!(center.lock().unwrap().history().iter().any(|n| n.id == id));
            }
            ui.clear.emit_clicked();
            let (_, new_take, _, new_fired) = post(&ui);
            new_take.emit_clicked();
            assert_eq!(
                new_fired.get(),
                1,
                "fresh resident remains actionable after {terminal}"
            );
            ui.clear.emit_clicked();
        }
        // External notices have no local handler. Dismiss keeps each history
        // action invokable once, including a captured button after dismissal.
        settle(&ui);
        let id = ui
            .post_with_actions(
                "External fixture",
                "",
                "External",
                "",
                &[
                    ("take".into(), "Take".into()),
                    ("delay".into(), "Delay".into()),
                ],
                Urgency::Normal,
                None,
            )
            .unwrap();
        let take = button(&ui.banner_box, "Take");
        let delay = button(&ui.banner_box, "Delay");
        button(&ui.banner_box, "close").emit_clicked();
        take.emit_clicked();
        delay.emit_clicked();
        assert!(ui.bus.borrow().invoke_action(id as u32, "take").is_err());
        assert!(ui.bus.borrow().invoke_action(id as u32, "delay").is_err());
        assert_eq!(center.lock().unwrap().history().len(), 1);
        ui.clear.emit_clicked();
        // A resident notice updated in place (GNOME's screen-time warning
        // turning into "Limit Reached") shows again on the banner already
        // up with its new text, never queued behind a later arrival.
        settle(&ui);
        let warning = ui
            .post_replacing(
                "Wellbeing",
                "",
                "Screen Time Limit in 10 Minutes",
                "",
                Urgency::High,
                None,
            )
            .unwrap();
        settle(&ui);
        assert_eq!(ui.showing.get(), Some(warning));
        let later = ui
            .post_replacing("Later", "", "Later", "", Urgency::Normal, None)
            .unwrap();
        settle(&ui);
        assert_eq!(ui.showing.get(), Some(warning), "the later one waits");
        // The list as the open calendar would draw it, before the update.
        ui.sync_history();
        assert!(labels(&ui.list).iter().any(|l| l.contains("10 Minutes")));
        let reached = ui.post_replacing(
            "Wellbeing",
            "",
            "Screen Time Limit Reached",
            "",
            Urgency::High,
            Some(warning),
        );
        assert_eq!(reached, Some(warning), "updated in place");
        settle(&ui);
        assert_eq!(ui.showing.get(), Some(warning), "shown again, not queued");
        let text = labels(&ui.banner_box);
        assert!(
            text.iter().any(|l| l == "Screen Time Limit Reached"),
            "{text:?}"
        );
        assert!(!text.iter().any(|l| l.contains("10 Minutes")), "{text:?}");
        // The list entry takes the new text too (same id, new content).
        ui.sync_history();
        let listed = labels(&ui.list);
        assert!(
            listed.iter().any(|l| l == "Screen Time Limit Reached"),
            "{listed:?}"
        );
        assert!(
            !listed.iter().any(|l| l.contains("10 Minutes")),
            "{listed:?}"
        );
        {
            let center = center.lock().unwrap();
            assert_eq!(center.history().len(), 2);
            assert!(center
                .history()
                .iter()
                .any(|n| n.id == warning && n.acknowledged()));
            assert!(center.banners().iter().any(|n| n.id == later));
        }
        ui.clear.emit_clicked();
        ui.banner_window.close();
        println!("G-NOTIFY-LOCAL-LIFECYCLE pass actual GTK clicked handlers and in-memory NotificationCenter; Xvfb source scope only");
    }
}
