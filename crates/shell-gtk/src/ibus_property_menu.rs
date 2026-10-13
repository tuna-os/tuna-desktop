//! IBus panel properties. Composition/candidate code remains in ibus_panel.
use gtk::prelude::*;
use gtk::{gio, glib};
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::rc::Rc;
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
    let paths = std::env::var_os("PATH")?;
    // First executable, exactly as PATH resolution. Symlinks work; scripts do
    // not silently substitute basename/comm trust for the actual running ELF.
    for dir in std::env::split_paths(&paths) {
        let path = dir.join("ibus-daemon");
        let Ok(metadata) = path.metadata() else {
            continue;
        };
        if metadata.is_file() && metadata.mode() & 0o111 != 0 {
            return File::open(std::fs::canonicalize(path).ok()?).ok();
        }
    }
    None
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

pub struct PropertyMenu {
    pub button: gtk::MenuButton,
    popover: gtk::Popover,
    content: gtk::Box,
    model: RefCell<Properties>,
    conn: RefCell<Option<gio::DBusConnection>>,
    peer: RefCell<Option<Peer>>,
    subscriptions: RefCell<Vec<gio::SignalSubscription>>,
    owner: RefCell<Option<String>>,
    owned: Cell<bool>,
    generation: Cell<u64>,
    engine_query: Cell<u64>,
    locked: Rc<dyn Fn() -> bool>,
    was_locked: Cell<bool>,
}
impl PropertyMenu {
    pub fn new(locked: Rc<dyn Fn() -> bool>) -> Rc<Self> {
        let button = gtk::MenuButton::new();
        button.set_label("Input Method");
        button.set_tooltip_text(Some("Input method properties"));
        button.set_visible(false);
        let popover = gtk::Popover::new();
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
            owned: Cell::new(false),
            generation: Cell::new(0),
            engine_query: Cell::new(0),
            locked,
            was_locked: Cell::new(false),
        })
    }
    fn current(&self, conn: &gio::DBusConnection, generation: u64) -> bool {
        self.generation.get() == generation
            && !conn.is_closed()
            && self.conn.borrow().as_ref() == Some(conn)
            && self.peer.borrow().as_ref().is_some_and(Peer::live)
    }
    pub fn reset(self: &Rc<Self>) {
        self.owned.set(false);
        self.model.borrow_mut().reset();
        self.owner.replace(None);
        self.subscriptions.borrow_mut().clear();
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
            Some(PANEL),
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let Some(menu) = weak.upgrade() else { return };
                if !menu.current(signal.connection, generation) {
                    return;
                }
                if let Some((_, _, owner)) = signal.parameters.get::<(String, String, String)>() {
                    if Some(owner.as_str()) != signal.connection.unique_name().as_deref() {
                        menu.owned.set(false);
                        menu.model.borrow_mut().reset();
                        menu.popover.popdown();
                        menu.render();
                    }
                }
            },
        );
        self.subscriptions.borrow_mut().push(sub);
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
                if !menu.current(&c, generation) {
                    return;
                }
                let Some((owner,)) = reply.ok().and_then(|v| v.get::<(String,)>()) else {
                    return;
                };
                if owner.is_empty() || owner.len() > wire::MAX_TEXT {
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
                            || menu.owner.borrow().as_deref() != Some(signal.sender_name)
                        {
                            return;
                        }
                        let Some((name,)) = signal.parameters.get::<(String,)>() else {
                            return;
                        };
                        if name.len() > wire::MAX_TEXT {
                            menu.model.borrow_mut().reset();
                            menu.render();
                            return;
                        }
                        // Invalidate before any async query can finish. The
                        // daemon registers the new engine's cached properties.
                        let Some(next) = menu.engine_query.get().checked_add(1) else {
                            menu.reset();
                            return;
                        };
                        menu.engine_query.set(next);
                        menu.model
                            .borrow_mut()
                            .engine((!name.is_empty()).then_some(name));
                        menu.render();
                    },
                );
                menu.subscriptions.borrow_mut().push(sub);
                menu.query_engine(&c, generation);
            },
        );
    }
    fn query_engine(self: &Rc<Self>, conn: &gio::DBusConnection, generation: u64) {
        let Some(query) = self.engine_query.get().checked_add(1) else {
            self.reset();
            return;
        };
        self.engine_query.set(query);
        let weak = Rc::downgrade(self);
        let c = conn.clone();
        conn.call(
            Some(IBUS),
            IBUS_PATH,
            IBUS,
            "GetGlobalEngine",
            None,
            glib::VariantTy::new("(v)").ok(),
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
            move |reply| {
                let Some(menu) = weak.upgrade() else { return };
                if !menu.current(&c, generation) || menu.engine_query.get() != query {
                    return;
                }
                let Some(name) = reply.ok().and_then(|v| engine_name(&v.child_value(0))) else {
                    return;
                };
                // Properties may precede this initial async reply. They were
                // not actionable without an engine; bind them to this reply.
                menu.model.borrow_mut().initial_engine(name);
                menu.render();
            },
        );
    }
    pub fn name_acquired(self: &Rc<Self>, conn: &gio::DBusConnection, acquired: bool) {
        if !self.current(conn, self.generation.get()) {
            return;
        }
        self.owned.set(acquired);
        if !acquired {
            self.model.borrow_mut().reset();
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
                model.focus_in(path.into());
            }
            "FocusOut" | "DestroyContext" => {
                let value = params.child_value(0);
                let path = value
                    .str()
                    .ok_or("org.freedesktop.DBus.Error.InvalidArgs")?;
                model.focus_out(path);
            }
            _ => return Ok(()),
        }
        drop(model);
        self.render();
        Ok(())
    }
    pub fn poll(self: &Rc<Self>) {
        let conn = self.conn.borrow().clone();
        if conn
            .as_ref()
            .is_some_and(|c| !self.current(c, self.generation.get()))
        {
            self.reset();
        }
    }
    pub fn sync_lock(self: &Rc<Self>) {
        let locked = (self.locked)();
        if locked != self.was_locked.replace(locked) {
            if locked {
                self.model.borrow_mut().reset();
                self.popover.popdown();
            }
            self.render();
        }
    }
    fn activate(self: &Rc<Self>, token: &Token) {
        let conn = self.conn.borrow().clone();
        let Some(conn) = conn else { return };
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
        let show = self.owned.get() && !(self.locked)() && model.engine.is_some();
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

fn trusted_sender(peer: Option<&Peer>, sender: Option<&str>) -> bool {
    sender == Some(DIRECT_SENDER) && peer.is_some_and(Peer::live)
}

#[cfg(test)]
mod tests {
    use super::*;
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
