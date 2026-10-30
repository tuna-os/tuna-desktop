//! IBus panel properties. Composition/candidate code remains in ibus_panel.
use gtk::prelude::*;
use gtk::{gio, glib};
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::rc::Rc;
use std::time::{Duration, Instant};
use tuna_shell_host::ibus_properties::{self as wire, Kind, Properties, Property, Token};
use unicode_segmentation::UnicodeSegmentation;

const PANEL: &str = "org.freedesktop.IBus.Panel";
const PANEL_PATH: &str = "/org/freedesktop/IBus/Panel";
const IBUS: &str = "org.freedesktop.IBus";
const IBUS_PATH: &str = "/org/freedesktop/IBus";
const DIRECT_SENDER: &str = "org.freedesktop.DBus";

/// Held expected executable, matched to SO_PEERCRED on the actual bus socket.
/// PATH is user environment: this is provenance, not a same-user sandbox.
struct Peer {
    expected: File,
    pid: i32,
    start: u64,
}
fn process_start(pid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}
fn same_file(a: &File, b: &File) -> bool {
    let (Ok(a), Ok(b)) = (a.metadata(), b.metadata()) else {
        return false;
    };
    a.dev() == b.dev() && a.ino() == b.ino()
}
fn expected_daemon() -> Option<File> {
    // GLib resolves the first executable on PATH with its normal permission
    // checks. Resolve symlinks, then hold the exact file; no basename fallback.
    File::open(std::fs::canonicalize(glib::find_program_in_path("ibus-daemon")?).ok()?).ok()
}
impl Peer {
    fn bind(conn: &gio::DBusConnection) -> Option<Self> {
        let socket = conn
            .stream()
            .downcast::<gio::SocketConnection>()
            .ok()?
            .socket();
        let credentials = socket.credentials().ok()?;
        if credentials.unix_user().ok()? != unsafe { libc::geteuid() } {
            return None;
        }
        let pid = credentials.unix_pid().ok()?;
        if pid <= 0 {
            return None;
        }
        let expected = expected_daemon()?;
        let start = process_start(pid)?;
        let peer = Self {
            expected,
            pid,
            start,
        };
        peer.live().then_some(peer)
    }
    fn live(&self) -> bool {
        process_start(self.pid) == Some(self.start)
            && File::open(format!("/proc/{}/exe", self.pid))
                .is_ok_and(|actual| same_file(&self.expected, &actual))
    }
}

#[derive(Clone)]
struct Refresh {
    connection: u64,
    owner_epoch: u64,
    id: u64,
    owner: String,
    deadline: Instant,
}
impl Refresh {
    fn timeout_ms(&self) -> Option<i32> {
        let left = self.deadline.checked_duration_since(Instant::now())?;
        (left.as_millis() > 0).then_some(left.as_millis().min(2000) as i32)
    }
}

/// Shared async-reply admission: stale callbacks cannot touch a newer refresh;
/// an expired current callback MUST run failure cleanup instead of staying busy.
fn admit_refresh_reply(current: bool, refresh: &Refresh, fail: impl FnOnce()) -> bool {
    if !current {
        return false;
    }
    if refresh.timeout_ms().is_none() {
        fail();
        return false;
    }
    true
}

pub struct PropertyMenu {
    pub button: gtk::MenuButton,
    popover: gtk::Popover,
    content: gtk::Box,
    model: RefCell<Properties>,
    conn: RefCell<Option<gio::DBusConnection>>,
    peer: RefCell<Option<Peer>>,
    subscriptions: RefCell<Vec<gio::SignalSubscription>>,
    owner: RefCell<Option<String>>,
    owner_epoch: Cell<u64>,
    engine_watch: RefCell<Option<gio::SignalSubscription>>,
    focus_epoch: Cell<u64>,
    refresh_id: Cell<u64>,
    refreshing: Cell<bool>,
    release_offered: Cell<bool>,
    had_panel: Cell<bool>,
    owned: Cell<bool>,
    generation: Cell<u64>,
    engine_query: Cell<u64>,
    locked: Rc<dyn Fn() -> bool>,
    was_locked: Cell<bool>,
    unavailable: RefCell<Option<Rc<dyn Fn()>>>,
}
impl PropertyMenu {
    pub fn new(locked: Rc<dyn Fn() -> bool>) -> Rc<Self> {
        let button = gtk::MenuButton::new();
        // keyboard.js's InputSourceIndicator is a PanelMenu.Button.
        crate::panel_menu_role(&button);
        button.set_label("Input Method");
        button.set_tooltip_text(Some("Input method properties"));
        button.set_visible(false);
        let popover = crate::transient::MenuPopover::new_popover();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        popover.set_child(Some(&content));
        button.set_popover(Some(&popover));
        Rc::new(Self {
            button,
            popover,
            content,
            model: RefCell::default(),
            conn: RefCell::default(),
            peer: RefCell::default(),
            subscriptions: RefCell::default(),
            owner: RefCell::default(),
            owner_epoch: Cell::new(0),
            engine_watch: RefCell::default(),
            focus_epoch: Cell::new(0),
            refresh_id: Cell::new(0),
            refreshing: Cell::new(false),
            release_offered: Cell::new(false),
            had_panel: Cell::new(false),
            owned: Cell::new(false),
            generation: Cell::new(0),
            engine_query: Cell::new(0),
            locked,
            was_locked: Cell::new(false),
            unavailable: RefCell::default(),
        })
    }
    pub fn set_unavailable(&self, callback: Rc<dyn Fn()>) {
        self.unavailable.replace(Some(callback));
    }
    fn hide_candidates(&self) {
        if let Some(callback) = self.unavailable.borrow().as_ref() {
            callback();
        }
    }
    fn current(&self, conn: &gio::DBusConnection, generation: u64) -> bool {
        self.generation.get() == generation
            && !conn.is_closed()
            && self.conn.borrow().as_ref() == Some(conn)
            && self.peer.borrow().as_ref().is_some_and(Peer::live)
    }
    pub fn reset(self: &Rc<Self>) {
        self.hide_candidates();
        self.owned.set(false);
        self.model.borrow_mut().reset();
        self.owner.replace(None);
        self.subscriptions.borrow_mut().clear();
        self.engine_watch.replace(None);
        self.refreshing.set(false);
        self.release_offered.set(false);
        self.had_panel.set(false);
        self.peer.replace(None);
        self.conn.replace(None);
        self.popover.popdown();
        self.render();
    }
    pub fn connect(self: &Rc<Self>, conn: &gio::DBusConnection) {
        self.reset();
        let Some(generation) = self.generation.get().checked_add(1) else {
            return;
        };
        self.generation.set(generation);
        let Some(peer) = Peer::bind(conn) else {
            eprintln!("tuna-shell-gtk: IBus properties unavailable: bus peer/executable identity unsupported");
            return;
        };
        self.conn.replace(Some(conn.clone()));
        self.peer.replace(Some(peer));
        let weak = Rc::downgrade(self);
        let sub = conn.subscribe_to_signal(
            Some(DIRECT_SENDER),
            Some(DIRECT_SENDER),
            Some("NameOwnerChanged"),
            Some("/org/freedesktop/DBus"),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let Some(menu) = weak.upgrade() else { return };
                if !menu.current(signal.connection, generation)
                    || signal.sender_name != DIRECT_SENDER
                {
                    return;
                }
                let Some((name, _, owner)) = signal.parameters.get::<(String, String, String)>()
                else {
                    return;
                };
                if name == IBUS {
                    if !menu.retire_owner() {
                        return;
                    }
                    if !owner.is_empty() {
                        menu.resolve_owner(signal.connection, generation, Some(owner));
                    }
                } else if name == PANEL {
                    let ours = Some(owner.as_str()) == signal.connection.unique_name().as_deref();
                    menu.owned.set(ours);
                    panel_owner_changed(&mut menu.model.borrow_mut(), ours);
                    menu.retire_query();
                    if ours {
                        menu.had_panel.set(true);
                        menu.release_offered.set(false);
                        menu.query_engine(signal.connection, generation);
                    } else {
                        menu.hide_candidates();
                        menu.popover.popdown();
                        // A previous owned panel can reclaim a freshly empty
                        // name. Another owner is always left alone.
                        if owner.is_empty()
                            && menu.had_panel.get()
                            && !menu.refreshing.get()
                            && !(menu.locked)()
                        {
                            menu.refresh(signal.connection);
                        }
                    }
                    menu.render();
                }
            },
        );
        self.subscriptions.borrow_mut().push(sub);
        self.resolve_owner(conn, generation, None);
    }
    fn retire_query(self: &Rc<Self>) -> bool {
        if let Some(next) = self.engine_query.get().checked_add(1) {
            self.engine_query.set(next);
            true
        } else {
            self.reset();
            false
        }
    }
    fn retire_owner(self: &Rc<Self>) -> bool {
        self.hide_candidates();
        self.owner.replace(None);
        self.engine_watch.replace(None);
        self.model.borrow_mut().reset();
        self.popover.popdown();
        self.refreshing.set(false);
        let Some(next) = self.owner_epoch.get().checked_add(1) else {
            self.reset();
            return false;
        };
        self.owner_epoch.set(next);
        self.retire_query();
        self.render();
        true
    }
    fn resolve_owner(
        self: &Rc<Self>,
        conn: &gio::DBusConnection,
        generation: u64,
        expected: Option<String>,
    ) {
        let epoch = self.owner_epoch.get();
        let weak = Rc::downgrade(self);
        let c = conn.clone();
        conn.call(
            Some(DIRECT_SENDER),
            "/org/freedesktop/DBus",
            DIRECT_SENDER,
            "GetNameOwner",
            Some(&(IBUS,).to_variant()),
            glib::VariantTy::new("(s)").ok(),
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
            move |reply| {
                let Some(menu) = weak.upgrade() else { return };
                if !menu.current(&c, generation) || menu.owner_epoch.get() != epoch {
                    return;
                }
                let Some((owner,)) = reply.ok().and_then(|v| v.get::<(String,)>()) else {
                    return;
                };
                if owner.is_empty()
                    || owner.len() > wire::MAX_TEXT
                    || expected.as_ref().is_some_and(|e| e != &owner)
                {
                    return;
                }
                menu.owner.replace(Some(owner.clone()));
                let weak = Rc::downgrade(&menu);
                let sub = c.subscribe_to_signal(
                    Some(&owner),
                    Some(IBUS),
                    Some("GlobalEngineChanged"),
                    Some(IBUS_PATH),
                    None,
                    gio::DBusSignalFlags::NONE,
                    move |signal| {
                        let Some(menu) = weak.upgrade() else { return };
                        if !menu.current(signal.connection, generation)
                            || menu.owner_epoch.get() != epoch
                            || menu.owner.borrow().as_deref() != Some(signal.sender_name)
                        {
                            return;
                        }
                        let Some((name,)) = signal.parameters.get::<(String,)>() else {
                            return;
                        };
                        if !menu.retire_query() {
                            return;
                        }
                        menu.model.borrow_mut().engine(
                            (!name.is_empty() && name.len() <= wire::MAX_TEXT).then_some(name),
                        );
                        menu.render();
                    },
                );
                menu.engine_watch.replace(Some(sub));
                if expected.is_some() && !(menu.locked)() {
                    menu.refresh(&c);
                } else {
                    menu.query_engine(&c, generation);
                }
            },
        );
    }
    fn query_engine(self: &Rc<Self>, conn: &gio::DBusConnection, generation: u64) {
        if (self.locked)() {
            return;
        }
        let Some(owner) = self.owner.borrow().clone() else {
            return;
        };
        if !self.retire_query() {
            return;
        }
        let query = self.engine_query.get();
        let epoch = self.owner_epoch.get();
        let weak = Rc::downgrade(self);
        let c = conn.clone();
        // The resolved owner is the destination, never the well-known name.
        let destination = owner.clone();
        conn.call(
            Some(&destination),
            IBUS_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&(IBUS, "GlobalEngine").to_variant()),
            glib::VariantTy::new("(v)").ok(),
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
            move |reply| {
                let Some(menu) = weak.upgrade() else { return };
                if !menu.owner_current(&c, generation, epoch, &owner)
                    || menu.engine_query.get() != query
                    || (menu.locked)()
                {
                    return;
                }
                // Properties.Get adds its own v around the property's v.
                let Some(name) = reply
                    .ok()
                    .and_then(|v| v.child_value(0).as_variant())
                    .and_then(|v| engine_name(&v))
                else {
                    return;
                };
                menu.model.borrow_mut().initial_engine(name);
                menu.query_context(&c, generation, epoch, query, owner);
                menu.render();
            },
        );
    }
    fn owner_current(
        &self,
        conn: &gio::DBusConnection,
        generation: u64,
        epoch: u64,
        owner: &str,
    ) -> bool {
        self.current(conn, generation)
            && same_owner(
                self.owner_epoch.get(),
                epoch,
                self.owner.borrow().as_deref(),
                owner,
            )
    }
    fn query_context(
        self: &Rc<Self>,
        conn: &gio::DBusConnection,
        generation: u64,
        epoch: u64,
        query: u64,
        owner: String,
    ) {
        let focus = self.focus_epoch.get();
        let weak = Rc::downgrade(self);
        let c = conn.clone();
        let destination = owner.clone();
        conn.call(
            Some(&destination),
            IBUS_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&(IBUS, "CurrentInputContext").to_variant()),
            glib::VariantTy::new("(v)").ok(),
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
            move |reply| {
                let Some(menu) = weak.upgrade() else { return };
                if !menu.owner_current(&c, generation, epoch, &owner)
                    || menu.engine_query.get() != query
                    || menu.focus_epoch.get() != focus
                    || (menu.locked)()
                {
                    return;
                }
                let Some(value) = reply.ok().and_then(|v| v.child_value(0).as_variant()) else {
                    return;
                };
                if !value.is_type(glib::VariantTy::OBJECT_PATH) {
                    return;
                }
                let Some(path) = value.str().filter(|s| s.len() <= wire::MAX_TEXT) else {
                    return;
                };
                // Only fill a missing context. An incoming FocusIn is newer
                // authority and must not lose its just-registered property list.
                let mut model = menu.model.borrow_mut();
                if model.context.is_none() {
                    model.focus_in(path.into());
                }
                drop(model);
                menu.render();
            },
        );
    }
    pub fn name_acquired(self: &Rc<Self>, conn: &gio::DBusConnection, acquired: bool) {
        if !self.current(conn, self.generation.get()) {
            return;
        }
        self.owned.set(acquired);
        if acquired {
            self.had_panel.set(true);
        }
        if !acquired {
            self.model.borrow_mut().reset();
            self.retire_query();
        } else {
            self.query_engine(conn, self.generation.get());
        }
        self.render();
    }
    /// Only new properties/focus handling uses this strict gate. Existing
    /// preedit/candidate behavior is not broadened or refactored here.
    pub fn method(
        self: &Rc<Self>,
        conn: &gio::DBusConnection,
        sender: Option<&str>,
        method: &str,
        params: &glib::Variant,
    ) -> Result<(), &'static str> {
        if !self.current(conn, self.generation.get())
            || !trusted_sender(self.peer.borrow().as_ref(), sender)
            || self.owner.borrow().is_none()
        {
            return Err("org.freedesktop.DBus.Error.AccessDenied");
        }
        let mut model = self.model.borrow_mut();
        match method {
            "RegisterProperties" => {
                let items = match wire::parse_list(&params.child_value(0)) {
                    Ok(items) => items,
                    Err(_) => {
                        model.register(vec![]);
                        drop(model);
                        self.render();
                        return Err("org.freedesktop.DBus.Error.InvalidArgs");
                    }
                };
                if !(self.locked)() {
                    model.register(items);
                }
            }
            "UpdateProperty" => {
                let item = match wire::parse_property(&params.child_value(0)) {
                    Ok(item) => item,
                    Err(_) => {
                        model.register(vec![]);
                        drop(model);
                        self.render();
                        return Err("org.freedesktop.DBus.Error.InvalidArgs");
                    }
                };
                if !(self.locked)() {
                    model.update(item);
                }
            }
            "FocusIn" => {
                let value = params.child_value(0);
                let path = value
                    .str()
                    .filter(|s| s.len() <= wire::MAX_TEXT)
                    .ok_or("org.freedesktop.DBus.Error.InvalidArgs")?;
                let Some(next) = self.focus_epoch.get().checked_add(1) else {
                    drop(model);
                    self.reset();
                    return Err("org.freedesktop.DBus.Error.Failed");
                };
                self.focus_epoch.set(next);
                model.focus_in(path.into());
            }
            "FocusOut" | "DestroyContext" => {
                let value = params.child_value(0);
                let path = value
                    .str()
                    .ok_or("org.freedesktop.DBus.Error.InvalidArgs")?;
                if model.context.as_deref() == Some(path) {
                    let Some(next) = self.focus_epoch.get().checked_add(1) else {
                        drop(model);
                        self.reset();
                        return Err("org.freedesktop.DBus.Error.Failed");
                    };
                    self.focus_epoch.set(next);
                    model.focus_out(path);
                }
            }
            _ => return Ok(()),
        }
        let diagnostic = (tuna_shell_host::ibus::debug() && !(self.locked)()).then(|| {
            (
                model.epoch(),
                model
                    .context
                    .as_deref()
                    .and_then(|path| {
                        glib::compute_checksum_for_data(glib::ChecksumType::Sha256, path.as_bytes())
                    })
                    .unwrap_or_else(|| "none".into()),
            )
        });
        drop(model);
        if let Some((epoch, context_sha)) = diagnostic {
            // Fixed methods/counters and a hash of the daemon object path:
            // no raw context paths, engine keys, labels, text or input values.
            eprintln!("tuna-shell-gtk: ibus properties applied {method} epoch={epoch} focus={} context_sha={context_sha}", self.focus_epoch.get());
        }
        self.render();
        Ok(())
    }
    pub fn sync_lock(self: &Rc<Self>) {
        let locked = (self.locked)();
        if locked != self.was_locked.replace(locked) {
            let Some(id) = self.refresh_id.get().checked_add(1) else {
                self.reset();
                return;
            };
            self.refresh_id.set(id);
            self.refreshing.set(false);
            self.retire_query();
            self.model.borrow_mut().reset();
            if locked {
                self.hide_candidates();
                self.popover.popdown();
            } else if let Some(conn) = self.conn.borrow().clone() {
                self.refresh(&conn);
            }
            self.render();
        }
    }
    fn refresh_current(&self, conn: &gio::DBusConnection, refresh: &Refresh) -> bool {
        self.owner_current(
            conn,
            refresh.connection,
            refresh.owner_epoch,
            &refresh.owner,
        ) && self.refresh_id.get() == refresh.id
            && self.refreshing.get()
            && !(self.locked)()
    }
    fn refresh_ready(self: &Rc<Self>, conn: &gio::DBusConnection, refresh: &Refresh) -> bool {
        admit_refresh_reply(self.refresh_current(conn, refresh), refresh, || {
            self.refresh_failed("deadline expired before current reply")
        })
    }
    fn refresh_failed(self: &Rc<Self>, phase: &str) {
        self.hide_candidates();
        self.refreshing.set(false);
        self.owned.set(false);
        self.model.borrow_mut().reset();
        self.popover.popdown();
        self.render();
        eprintln!("tuna-shell-gtk: IBus properties refresh unavailable: {phase}");
    }
    /// The actual daemon has no property-list getter. Reacquiring our panel
    /// name makes ibusimpl send fresh FocusIn + cached engine RegisterProperties.
    /// No context or engine is changed, and a competing panel is never replaced.
    fn refresh(self: &Rc<Self>, conn: &gio::DBusConnection) {
        if (self.locked)() || !self.current(conn, self.generation.get()) {
            return;
        }
        let Some(owner) = self.owner.borrow().clone() else {
            return;
        };
        let Some(id) = self.refresh_id.get().checked_add(1) else {
            self.reset();
            return;
        };
        self.refresh_id.set(id);
        self.refreshing.set(true);
        self.owned.set(false);
        self.retire_query();
        self.hide_candidates();
        self.model.borrow_mut().reset();
        self.popover.popdown();
        self.render();
        let refresh = Refresh {
            connection: self.generation.get(),
            owner_epoch: self.owner_epoch.get(),
            id,
            owner,
            deadline: Instant::now() + Duration::from_secs(5),
        };
        let weak = Rc::downgrade(self);
        let c = conn.clone();
        conn.call(
            Some(DIRECT_SENDER),
            "/org/freedesktop/DBus",
            DIRECT_SENDER,
            "GetNameOwner",
            Some(&(IBUS,).to_variant()),
            glib::VariantTy::new("(s)").ok(),
            gio::DBusCallFlags::NONE,
            refresh.timeout_ms().unwrap_or(1),
            gio::Cancellable::NONE,
            move |reply| {
                let Some(menu) = weak.upgrade() else { return };
                if !menu.refresh_ready(&c, &refresh) {
                    return;
                }
                if reply
                    .ok()
                    .and_then(|v| v.get::<(String,)>())
                    .is_none_or(|(owner,)| owner != refresh.owner)
                {
                    menu.refresh_failed("daemon owner changed before panel release");
                    return;
                }
                menu.check_panel_for_refresh(&c, refresh);
            },
        );
    }
    fn check_panel_for_refresh(self: &Rc<Self>, conn: &gio::DBusConnection, refresh: Refresh) {
        let weak = Rc::downgrade(self);
        let c = conn.clone();
        let Some(timeout) = refresh.timeout_ms() else {
            self.refresh_failed("deadline");
            return;
        };
        conn.call(
            Some(DIRECT_SENDER),
            "/org/freedesktop/DBus",
            DIRECT_SENDER,
            "GetNameOwner",
            Some(&(PANEL,).to_variant()),
            glib::VariantTy::new("(s)").ok(),
            gio::DBusCallFlags::NONE,
            timeout,
            gio::Cancellable::NONE,
            move |reply| {
                let Some(menu) = weak.upgrade() else { return };
                if !menu.refresh_ready(&c, &refresh) {
                    return;
                }
                match reply {
                    Ok(value)
                        if value.get::<(String,)>().is_some_and(|(owner,)| {
                            Some(owner.as_str()) == c.unique_name().as_deref()
                        }) =>
                    {
                        menu.release_for_refresh(&c, refresh)
                    }
                    Err(error)
                        if (menu.release_offered.get() || menu.had_panel.get())
                            && gio::DBusError::remote_error(&error).as_deref()
                                == Some("org.freedesktop.DBus.Error.NameHasNoOwner") =>
                    {
                        menu.acquire_for_refresh(&c, refresh)
                    }
                    _ => menu.refresh_failed("another panel owns the name or owner is unavailable"),
                }
            },
        );
    }
    fn release_for_refresh(self: &Rc<Self>, conn: &gio::DBusConnection, refresh: Refresh) {
        if !self.refresh_ready(conn, &refresh) {
            return;
        }
        // This records an offered release, not a successful one. If interrupted
        // by lock, a later fresh GetNameOwner must observe no owner before reclaim.
        self.release_offered.set(true);
        let weak = Rc::downgrade(self);
        let c = conn.clone();
        let Some(timeout) = refresh.timeout_ms() else {
            self.refresh_failed("deadline");
            return;
        };
        conn.call(
            Some(DIRECT_SENDER),
            "/org/freedesktop/DBus",
            DIRECT_SENDER,
            "ReleaseName",
            Some(&(PANEL,).to_variant()),
            glib::VariantTy::new("(u)").ok(),
            gio::DBusCallFlags::NONE,
            timeout,
            gio::Cancellable::NONE,
            move |reply| {
                let Some(menu) = weak.upgrade() else { return };
                if !menu.refresh_ready(&c, &refresh) {
                    return;
                }
                if reply.ok().and_then(|v| v.get::<(u32,)>()) != Some((1,)) {
                    menu.refresh_failed("panel release not confirmed");
                    return;
                }
                menu.acquire_for_refresh(&c, refresh);
            },
        );
    }
    fn acquire_for_refresh(self: &Rc<Self>, conn: &gio::DBusConnection, refresh: Refresh) {
        if !self.refresh_ready(conn, &refresh) {
            return;
        }
        let weak = Rc::downgrade(self);
        let c = conn.clone();
        let Some(timeout) = refresh.timeout_ms() else {
            self.refresh_failed("deadline");
            return;
        };
        // ALLOW_REPLACEMENT | DO_NOT_QUEUE. Never REPLACE_EXISTING.
        conn.call(
            Some(DIRECT_SENDER),
            "/org/freedesktop/DBus",
            DIRECT_SENDER,
            "RequestName",
            Some(&(PANEL, 1u32 | 4u32).to_variant()),
            glib::VariantTy::new("(u)").ok(),
            gio::DBusCallFlags::NONE,
            timeout,
            gio::Cancellable::NONE,
            move |reply| {
                let Some(menu) = weak.upgrade() else { return };
                if !menu.refresh_ready(&c, &refresh) {
                    return;
                }
                if !matches!(reply.ok().and_then(|v| v.get::<(u32,)>()), Some((1 | 4,))) {
                    menu.refresh_failed("panel name taken during refresh");
                    return;
                }
                menu.refreshing.set(false);
                menu.release_offered.set(false);
                // Ownership watcher already reset before fresh FocusIn/cache.
                // Do not reset this just-registered graph here.
                menu.name_acquired(&c, true);
            },
        );
    }
    fn activate(self: &Rc<Self>, token: &Token) {
        let conn = self.conn.borrow().clone();
        let Some(conn) = conn else { return };
        if self.refreshing.get() {
            return;
        }
        if !self.owned.get() || (self.locked)() || !self.current(&conn, self.generation.get()) {
            self.model.borrow_mut().reset();
            self.render();
            return;
        }
        let signals = self.model.borrow_mut().activate(token);
        for (key, state) in signals {
            if conn
                .emit_signal(
                    None,
                    PANEL_PATH,
                    PANEL,
                    "PropertyActivate",
                    Some(&(key, state).to_variant()),
                )
                .is_err()
            {
                self.model.borrow_mut().reset();
                break;
            }
        }
        self.render();
    }
    fn rows(self: &Rc<Self>, parent: &gtk::Box, items: &[Property]) {
        let mut group: Option<gtk::CheckButton> = None;
        for p in items.iter().filter(|p| p.visible) {
            let token = self.model.borrow().token(&p.key);
            let weak = Rc::downgrade(self);
            match p.kind {
                Kind::Separator => {
                    parent.append(&gtk::Separator::new(gtk::Orientation::Horizontal))
                }
                Kind::Menu => {
                    let expander = gtk::Expander::new(Some(&p.label));
                    expander.set_sensitive(p.sensitive);
                    expander.set_tooltip_text(Some(&p.tooltip));
                    let child = gtk::Box::new(gtk::Orientation::Vertical, 4);
                    self.rows(&child, &p.children);
                    expander.set_child(Some(&child));
                    parent.append(&expander);
                }
                Kind::Toggle | Kind::Radio => {
                    let button = gtk::CheckButton::with_label(&p.label);
                    if p.kind == Kind::Radio {
                        if let Some(first) = &group {
                            button.set_group(Some(first));
                        } else {
                            group = Some(button.clone());
                        }
                    }
                    button.set_active(p.state == 1);
                    button.set_inconsistent(p.state == 2);
                    button.set_sensitive(p.sensitive);
                    button.set_tooltip_text(Some(&p.tooltip));
                    expose_toggle_action(&button);
                    let radio = p.kind == Kind::Radio;
                    button.connect_toggled(move |button| {
                        if radio && !button.is_active() {
                            return;
                        }
                        if let Some(menu) = weak.upgrade() {
                            menu.activate(&token);
                        }
                    });
                    parent.append(&button);
                }
                Kind::Action => {
                    let button = gtk::Button::with_label(&p.label);
                    button.set_sensitive(p.sensitive);
                    button.set_tooltip_text(Some(&p.tooltip));
                    button.connect_clicked(move |_| {
                        if let Some(menu) = weak.upgrade() {
                            menu.activate(&token);
                        }
                    });
                    parent.append(&button);
                }
            }
        }
    }
    fn render(self: &Rc<Self>) {
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        let model = self.model.borrow();
        let show = self.owned.get()
            && !self.refreshing.get()
            && !(self.locked)()
            && model.engine.is_some();
        self.button.set_visible(show);
        let Some(engine) = model.engine.clone() else {
            return;
        };
        let items = model.items.clone();
        drop(model);
        let indicator = items
            .iter()
            .find(|p| p.key == "InputMode" && p.visible)
            .and_then(|p| p.symbol.as_deref().or(Some(&p.label)))
            .filter(|s| (1..=2).contains(&s.graphemes(true).count()));
        self.button.set_label(indicator.unwrap_or("Input Method"));
        self.button
            .set_tooltip_text(Some(&format!("Input method: {engine}")));
        let header = gtk::Label::new(Some(&engine));
        header.set_xalign(0.0);
        self.content.append(&header);
        self.rows(&self.content, &items);
    }
}
/// IBus answers RequestName, then sends FocusIn + RegisterProperties straight
/// from its owner-changed handler, and only broadcasts NameOwnerChanged from an
/// idle queue. So the graph present when our own acquisition is signalled is
/// the fresh one; only a loss discards it (a lost panel is sent nothing, so the
/// graph is already empty by the time the name comes back).
fn panel_owner_changed(model: &mut Properties, ours: bool) {
    if !ours {
        model.reset();
    }
}
fn engine_name(value: &glib::Variant) -> Option<String> {
    if value.size() > wire::MAX_BYTES {
        return None;
    }
    if !value.is_type(glib::VariantTy::VARIANT) {
        return None;
    }
    let value = value.as_variant()?;
    // ibusenginedesc.c: base fields through setup, then explicit versioned
    // additions (layout variant/options together, version, domain, icon key).
    let count = value.n_children();
    if !matches!(count, 14 | 16..=19)
        || !value.type_().as_str().starts_with("(sa{sv}")
        || value.child_value(0).str() != Some("IBusEngineDesc")
        || value.child_value(1).type_().as_str() != "a{sv}"
        || value.child_value(10).get::<u32>().is_none()
        || (2..count)
            .filter(|i| *i != 10)
            .any(|i| value.child_value(i).str().is_none())
    {
        return None;
    }
    let name = value.child_value(2).str()?.to_owned();
    (!name.is_empty() && name.len() <= wire::MAX_TEXT).then_some(name)
}

fn same_owner(
    current_epoch: u64,
    captured_epoch: u64,
    current: Option<&str>,
    expected: &str,
) -> bool {
    current_epoch == captured_epoch && current == Some(expected)
}

fn trusted_sender(peer: Option<&Peer>, sender: Option<&str>) -> bool {
    sender == Some(DIRECT_SENDER) && peer.is_some_and(Peer::live)
}

/// GTK's AT-SPI bridge gives GtkCheckButton no Action (only GtkButton,
/// GtkSwitch, GtkExpander and entries get one), so assistive technology could
/// read these toggles but not operate them. A widget-local action group is
/// what GTK exports for any other widget; its one action clicks the button,
/// and GTK withholds it while the button is insensitive.
fn expose_toggle_action(button: &gtk::CheckButton) {
    let toggle = gio::SimpleAction::new("toggle", None);
    let weak = button.downgrade();
    toggle.connect_activate(move |_, _| {
        if let Some(button) = weak.upgrade() {
            button.activate();
        }
    });
    let actions = gio::SimpleActionGroup::new();
    actions.add_action(&toggle);
    button.insert_action_group("property", Some(&actions));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn own_panel_signal_after_ibus_resent_graph_keeps_it_and_loss_clears_it() {
        let mut model = Properties::default();
        // Reacquire order on the IBus bus: reply, FocusIn, RegisterProperties,
        // then the queued NameOwnerChanged naming us.
        model.initial_engine("pinyin".into());
        model.focus_in("/org/freedesktop/IBus/InputContext_7".into());
        let mode = Property {
            key: "InputMode".into(),
            kind: Kind::Toggle,
            label: "中".into(),
            symbol: Some("中".into()),
            tooltip: String::new(),
            sensitive: true,
            visible: true,
            state: 0,
            children: vec![],
        };
        model.register(vec![mode.clone()]);
        panel_owner_changed(&mut model, true);
        assert_eq!(model.engine.as_deref(), Some("pinyin"));
        assert!(model.context.is_some());
        assert_eq!(model.items, vec![mode]);
        panel_owner_changed(&mut model, false);
        assert!(model.engine.is_none() && model.context.is_none() && model.items.is_empty());
    }
    #[test]
    fn current_expired_reply_cleans_up_but_stale_reply_cannot_touch_new_refresh() {
        let mut refresh = Refresh {
            connection: 1,
            owner_epoch: 1,
            id: 1,
            owner: ":1.999999".into(),
            deadline: Instant::now() - Duration::from_millis(1),
        };
        let busy = Cell::new(true);
        let failed = Cell::new(0);
        assert!(!admit_refresh_reply(true, &refresh, || {
            busy.set(false);
            failed.set(failed.get() + 1);
        }));
        assert!(!busy.get());
        assert_eq!(failed.get(), 1);
        // The same expired ticket, after a newer operation took ownership:
        // neither its denial nor its successful late reply can clear that owner.
        busy.set(true);
        assert!(!admit_refresh_reply(false, &refresh, || {
            busy.set(false);
            failed.set(failed.get() + 1);
        }));
        assert!(busy.get());
        assert_eq!(failed.get(), 1);
        refresh.deadline = Instant::now() + Duration::from_secs(5);
        assert!(admit_refresh_reply(true, &refresh, || panic!(
            "fresh reply"
        )));
    }
    #[test]
    fn old_owner_replies_and_absent_or_replaced_owner_are_refused() {
        assert!(same_owner(2, 2, Some(":1.1"), ":1.1"));
        assert!(!same_owner(3, 2, Some(":1.1"), ":1.1"));
        assert!(!same_owner(2, 2, Some(":1.2"), ":1.1"));
        assert!(!same_owner(2, 2, None, ":1.1"));
    }
    #[test]
    fn refresh_deadline_is_absolute_and_bounds_each_phase() {
        let mut refresh = Refresh {
            connection: 1,
            owner_epoch: 1,
            id: 1,
            owner: ":1.999999".into(),
            deadline: Instant::now() + Duration::from_secs(5),
        };
        assert!(refresh.timeout_ms().is_some_and(|ms| ms > 0 && ms <= 2000));
        refresh.deadline = Instant::now() - Duration::from_millis(1);
        assert_eq!(refresh.timeout_ms(), None);
    }
    #[test]
    fn held_executable_and_process_start_are_required_not_basename() {
        let pid = std::process::id() as i32;
        let expected = File::open(format!("/proc/{pid}/exe")).unwrap();
        let mut peer = Peer {
            expected,
            pid,
            start: process_start(pid).unwrap(),
        };
        assert!(peer.live());
        assert!(trusted_sender(Some(&peer), Some(DIRECT_SENDER)));
        assert!(!trusted_sender(Some(&peer), Some(":1.99")));
        assert!(!trusted_sender(Some(&peer), Some(IBUS)));
        assert!(!trusted_sender(Some(&peer), None));
        assert!(!trusted_sender(None, Some(DIRECT_SENDER)));
        peer.start += 1;
        assert!(!peer.live());
        peer.start -= 1;
        peer.expected = tempfile::tempfile().unwrap();
        assert!(!peer.live());
        assert!(!trusted_sender(Some(&peer), Some(DIRECT_SENDER)));
    }
    #[test]
    fn dead_peer_and_missing_process_never_admit_callbacks() {
        let expected = File::open("/proc/self/exe").unwrap();
        let peer = Peer {
            expected,
            pid: -1,
            start: 1,
        };
        assert!(!peer.live());
        assert!(!trusted_sender(Some(&peer), Some(DIRECT_SENDER)));
    }
    #[test]
    fn engine_description_requires_tagged_typed_base_and_bound_name() {
        let value = |name: &str| {
            glib::Variant::from_variant(&glib::Variant::tuple_from_iter([
                "IBusEngineDesc".to_variant(),
                glib::VariantDict::new(None).end(),
                name.to_variant(),
                "long name".to_variant(),
                "desc".to_variant(),
                "zh".to_variant(),
                "MIT".to_variant(),
                "author".to_variant(),
                "".to_variant(),
                "us".to_variant(),
                0u32.to_variant(),
                "".to_variant(),
                "".to_variant(),
                "".to_variant(),
            ]))
        };
        assert_eq!(
            engine_name(&value("tuna-test")).as_deref(),
            Some("tuna-test")
        );
        assert!(engine_name(&value("")).is_none());
        assert!(engine_name(&value(&"x".repeat(wire::MAX_TEXT + 1))).is_none());
        assert!(engine_name(&tuna_shell_host::ibus::text_variant("bad")).is_none());
    }
}
