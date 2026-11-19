//! The keyring's system prompter: GNOME Shell's `components/keyring.js` on
//! gcr's `GcrSystemPrompter` (gcr/gcr-system-prompter.c). gnome-keyring and
//! other gcr clients ask `org.gnome.keyring.SystemPrompter` for a password,
//! a new password with confirmation, or a yes/no, and the shell asks the
//! user with a system-modal dialog over a dimmed screen
//! ([`crate::keyring_dialog`]).
//!
//! Protocol (`org.gnome.keyring.internal.Prompter` and `.Callback`, gcr's
//! `org.gnome.keyring.Prompter.xml`):
//!
//! 1. A client exports a callback object and calls `BeginPrompting(callback)`.
//! 2. When it is that client's turn the prompter calls the callback's
//!    `PromptReady("", {}, exchange)`, `exchange` being the start of a
//!    [`crate::keyring_exchange`] secret exchange.
//! 3. The client calls `PerformPrompt(callback, "password" | "confirm",
//!    properties, exchange)` with the properties that changed since its last
//!    prompt and its half of the exchange; the dialog shows.
//! 4. The user answers, and the prompter calls `PromptReady("yes" | "no",
//!    properties, exchange)`; a password travels in `exchange`, encrypted.
//!    The client may perform another prompt (a wrong password, a retry)
//!    until it calls `StopPrompting(callback)`, which closes the dialog and
//!    calls `PromptDone()`.
//!
//! # Trust model
//!
//! Any process of the session may be a client; that is the protocol. What
//! each client gets, and cannot get:
//!
//! * A prompt belongs to the pair (the bus's unique name of whoever called
//!   `BeginPrompting`, the callback path). The bus fills the sender in; a
//!   process cannot claim another's. `PerformPrompt` and `StopPrompting`
//!   find the prompt by that pair, so a process cannot answer, change or
//!   stop another's prompt.
//! * The answer is only ever sent to that unique name, in a secret exchange
//!   whose key was agreed with whoever answered `PerformPrompt` on it. A
//!   process that merely reads the bus sees ciphertext. A process that can
//!   also rewrite messages on the bus can substitute its own key; neither
//!   gcr nor this prompter can stop that (see [`crate::keyring_exchange`]).
//! * The password is read from the dialog only in this process, for the one
//!   prompt that is showing, and handed to nobody but its callback.
//! * What a client can make the dialog say is the client's to say, as in
//!   gnome-shell; labels are never markup. Text is capped, a client has few
//!   prompts waiting, and the session as a whole has a limit, so one cannot
//!   bury the user in dialogs or starve the shell.
//!
//! The prompter takes the bus name without queueing: if another process
//! already owns `org.gnome.keyring.SystemPrompter` (another shell, a gcr
//! prompter), this does nothing. While the session is locked, prompts are
//! cancelled at once, as GNOME's `KeyringDummyDialog` does.
//!
//! [`Core`] is the protocol with no I/O: calls in, [`Effect`]s out.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use zeroize::Zeroizing;

use crate::keyring_dialog::{Event as KeyringEvent, KeyringDialog};
use crate::keyring_exchange::Exchange;

pub const BUS_NAME: &str = "org.gnome.keyring.SystemPrompter";
pub const OBJECT_PATH: &str = "/org/gnome/keyring/Prompter";
pub const PROMPTER_IFACE: &str = "org.gnome.keyring.internal.Prompter";
pub const CALLBACK_IFACE: &str = "org.gnome.keyring.internal.Prompter.Callback";

/// Prompts one process may have waiting or showing.
const MAX_PER_SENDER: usize = 4;
/// Prompts in all, waiting or showing.
const MAX_PROMPTS: usize = 32;
/// Longest property string the dialog takes, in bytes.
const MAX_TEXT: usize = 8192;

/// A client's callback object: whose it is and where.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Callback {
    /// The client's unique bus name, filled in by the bus.
    pub name: String,
    pub path: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Password,
    Confirm,
}

/// The `GcrPrompt` properties that cross the bus are strings, booleans and
/// ints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropValue {
    Str(String),
    Bool(bool),
    Int(i32),
}

/// `GcrPrompt`'s properties as `ShellKeyringPrompt` keeps them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Props {
    pub message: String,
    pub description: String,
    pub warning: String,
    pub choice_label: String,
    pub continue_label: String,
    pub cancel_label: String,
    pub password_new: bool,
    pub choice_chosen: bool,
    pub password_strength: i32,
}

impl Default for Props {
    fn default() -> Self {
        Self {
            message: String::new(),
            description: String::new(),
            warning: String::new(),
            choice_label: String::new(),
            continue_label: "Continue".to_owned(),
            cancel_label: "Cancel".to_owned(),
            password_new: false,
            choice_chosen: false,
            password_strength: 0,
        }
    }
}

/// `remove_mnemonics` (shell-keyring-prompt.c): every `_` is dropped and
/// the character after it kept, so `__` is a literal underscore.
pub fn remove_mnemonics(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        if c == '_' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn capped(text: &str) -> String {
    if text.len() <= MAX_TEXT {
        return text.to_owned();
    }
    let mut end = MAX_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

impl Props {
    /// `g_object_set_property` on the prompt: the canonical name when the
    /// property exists with that type. `title` is a property GNOME's dialog
    /// never shows and `caller-window` it ignores; both are accepted.
    fn set(&mut self, name: &str, value: &PropValue) -> Option<&'static str> {
        use PropValue::{Bool, Int, Str};
        Some(match (name, value) {
            ("title", Str(_)) => "title",
            ("caller-window", Str(_)) => return None,
            ("message", Str(v)) => {
                self.message = capped(v);
                "message"
            }
            ("description", Str(v)) => {
                self.description = capped(v);
                "description"
            }
            ("warning", Str(v)) => {
                self.warning = capped(v);
                "warning"
            }
            ("choice-label", Str(v)) => {
                self.choice_label = remove_mnemonics(&capped(v));
                "choice-label"
            }
            ("continue-label", Str(v)) => {
                self.continue_label = remove_mnemonics(&capped(v));
                "continue-label"
            }
            ("cancel-label", Str(v)) => {
                self.cancel_label = remove_mnemonics(&capped(v));
                "cancel-label"
            }
            ("password-new", Bool(v)) => {
                self.password_new = *v;
                "password-new"
            }
            ("choice-chosen", Bool(v)) => {
                self.choice_chosen = *v;
                "choice-chosen"
            }
            ("password-strength", Int(v)) => {
                self.password_strength = *v;
                "password-strength"
            }
            _ => return None,
        })
    }

    fn get(&self, name: &str) -> Option<PropValue> {
        use PropValue::{Bool, Int, Str};
        Some(match name {
            "message" => Str(self.message.clone()),
            "description" => Str(self.description.clone()),
            "warning" => Str(self.warning.clone()),
            "choice-label" => Str(self.choice_label.clone()),
            "continue-label" => Str(self.continue_label.clone()),
            "cancel-label" => Str(self.cancel_label.clone()),
            "password-new" => Bool(self.password_new),
            "choice-chosen" => Bool(self.choice_chosen),
            "password-strength" => Int(self.password_strength),
            // `title` is write-only here: the dialog has no title to echo.
            _ => return None,
        })
    }
}

/// `calculate_password_strength` (shell-keyring-prompt.c), after Firefox's
/// master password dialog: 0 for empty, else 1 to 10.
pub fn password_strength(password: &str) -> i32 {
    let bytes = password.as_bytes();
    if bytes.is_empty() {
        return 0;
    }
    let (mut upper, mut digit, mut misc) = (0i32, 0i32, 0i32);
    for &b in bytes {
        if b.is_ascii_digit() {
            digit += 1;
        } else if b.is_ascii_uppercase() {
            upper += 1;
        } else if !b.is_ascii_lowercase() {
            misc += 1;
        }
    }
    let length = bytes.len().min(5) as f64;
    let strength = (length - 2.0)
        + f64::from(digit.min(3))
        + f64::from(misc.min(3)) * 1.5
        + f64::from(upper.min(3));
    strength.clamp(1.0, 10.0) as i32
}

/// What the dialog shows for one prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub kind: Kind,
    pub props: Props,
}

/// How the user answered the dialog.
pub enum Answer {
    /// Continue on a password prompt.
    Password {
        secret: Zeroizing<String>,
        choice: bool,
    },
    /// Continue on a confirm prompt.
    Confirm { choice: bool },
    /// Cancel, Escape, or the prompt could not be shown.
    Cancel,
}

/// What the I/O layer does next.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Call the callback's `PromptReady`.
    Ready {
        to: Callback,
        reply: &'static str,
        changed: Vec<(&'static str, PropValue)>,
        exchange: String,
    },
    /// Call the callback's `PromptDone`.
    Done(Callback),
    /// Show the prompt in the dialog.
    Show(Callback, View),
    /// Close the dialog.
    Close(Callback),
    /// Start and stop watching the callback's owner for leaving the bus.
    Watch(Callback),
    Unwatch(Callback),
}

/// Why a call was refused, as the D-Bus error it becomes.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// `org.freedesktop.DBus.Error.Failed`.
    Failed(&'static str),
    /// `org.freedesktop.DBus.Error.InvalidArgs`.
    InvalidArgs(&'static str),
}

struct Entry {
    /// Made when the prompt's turn comes.
    exchange: Option<Exchange>,
    /// It is this prompt's turn (gcr's `ActivePrompt`).
    active: bool,
    /// The client has sent its half of the exchange.
    received: bool,
    /// A prompt is showing or being cancelled, and no `PromptReady` has
    /// answered it yet.
    prompting: Option<Kind>,
    /// The dialog is up for this prompt (until it is stopped or closed).
    shown: bool,
    props: Props,
    /// Properties changed since the last `PromptReady`.
    changed: BTreeSet<&'static str>,
}

/// The prompter's protocol state (`GcrSystemPrompter` in single mode: one
/// prompt's turn at a time, the rest waiting in order).
#[derive(Default)]
pub struct Core {
    enabled: bool,
    callbacks: HashMap<Callback, Entry>,
    waiting: VecDeque<Callback>,
    active: Option<Callback>,
}

impl Core {
    #[cfg(test)]
    pub fn is_prompting(&self) -> bool {
        !self.callbacks.is_empty()
    }

    /// `BeginPrompting`.
    pub fn begin(&mut self, callback: Callback) -> Result<Vec<Effect>, Refusal> {
        if self.callbacks.contains_key(&callback) {
            return Err(Refusal::Failed(
                "Already begun prompting for this prompt callback",
            ));
        }
        if self.callbacks.len() >= MAX_PROMPTS
            || self
                .callbacks
                .keys()
                .filter(|c| c.name == callback.name)
                .count()
                >= MAX_PER_SENDER
        {
            return Err(Refusal::Failed("Too many prompts"));
        }
        self.callbacks.insert(
            callback.clone(),
            Entry {
                exchange: None,
                active: false,
                received: false,
                prompting: None,
                shown: false,
                props: Props::default(),
                changed: BTreeSet::new(),
            },
        );
        self.waiting.push_back(callback.clone());
        let mut effects = vec![Effect::Watch(callback)];
        self.next_ready(&mut effects);
        Ok(effects)
    }

    /// `prompt_next_ready`: when no prompt has its turn, give the next
    /// waiting one its turn and tell it to go ahead.
    fn next_ready(&mut self, effects: &mut Vec<Effect>) {
        while self.active.is_none() {
            let Some(callback) = self.waiting.pop_front() else {
                return;
            };
            let Some(entry) = self.callbacks.get_mut(&callback) else {
                continue;
            };
            let Ok(exchange) = Exchange::new() else {
                // No randomness: nothing can be sent safely.
                effects.extend(self.stop(&callback, true));
                continue;
            };
            let first = exchange.begin();
            entry.exchange = Some(exchange);
            entry.active = true;
            self.active = Some(callback.clone());
            effects.push(Effect::Ready {
                to: callback,
                reply: "",
                changed: Vec::new(),
                exchange: first,
            });
        }
    }

    /// `PerformPrompt`.
    pub fn perform(
        &mut self,
        callback: &Callback,
        kind: &str,
        properties: &[(String, PropValue)],
        exchange: &str,
    ) -> Result<Vec<Effect>, Refusal> {
        let entry = match self.callbacks.get_mut(callback) {
            Some(entry) if entry.active => entry,
            _ => {
                return Err(Refusal::Failed(
                    "Not begun prompting for this prompt callback",
                ))
            }
        };
        if entry.prompting.is_some() {
            return Err(Refusal::Failed(
                "Already performing a prompt for this prompt callback",
            ));
        }
        for (name, value) in properties {
            if let Some(name) = entry.props.set(name, value) {
                entry.changed.insert(name);
            }
        }
        let Some(secrets) = entry.exchange.as_mut() else {
            return Err(Refusal::Failed(
                "Not begun prompting for this prompt callback",
            ));
        };
        if secrets.receive(exchange).is_err() {
            return Err(Refusal::InvalidArgs("Invalid secret exchange received"));
        }
        entry.received = true;
        let kind = match kind {
            "confirm" => Kind::Confirm,
            "password" => Kind::Password,
            _ => return Err(Refusal::InvalidArgs("Invalid type argument")),
        };
        entry.prompting = Some(kind);
        if !self.enabled {
            // GNOME's KeyringDummyDialog: every prompt is cancelled.
            return Ok(self.answer(callback, Answer::Cancel));
        }
        entry.shown = true;
        Ok(vec![Effect::Show(
            callback.clone(),
            View {
                kind,
                props: entry.props.clone(),
            },
        )])
    }

    /// The dialog's own warning ("Passwords do not match"), which goes
    /// back to the client with the next answer.
    pub fn warn(&mut self, callback: &Callback, warning: &str) {
        if let Some(entry) = self.callbacks.get_mut(callback) {
            entry.props.warning = warning.to_owned();
            entry.changed.insert("warning");
        }
    }

    /// The user's answer to the prompt that is showing.
    pub fn answer(&mut self, callback: &Callback, answer: Answer) -> Vec<Effect> {
        let Some(entry) = self.callbacks.get_mut(callback) else {
            return Vec::new();
        };
        let Some(kind) = entry.prompting.take() else {
            return Vec::new();
        };
        let choose = |entry: &mut Entry, choice: bool| {
            if entry.props.choice_chosen != choice {
                entry.props.choice_chosen = choice;
                entry.changed.insert("choice-chosen");
            }
        };
        let (reply, secret) = match (kind, answer) {
            (Kind::Password, Answer::Password { secret, choice }) => {
                choose(entry, choice);
                let strength = password_strength(&secret);
                if entry.props.password_strength != strength {
                    entry.props.password_strength = strength;
                    entry.changed.insert("password-strength");
                }
                ("yes", Some(secret))
            }
            (Kind::Confirm, Answer::Confirm { choice }) => {
                choose(entry, choice);
                ("yes", None)
            }
            // A cancel, or an answer of the wrong kind, is a no.
            _ => ("no", None),
        };
        let Some(exchange) = entry.exchange.as_ref() else {
            return Vec::new();
        };
        let Ok(sent) = exchange.send(secret.as_ref().map(|s| s.as_bytes())) else {
            return Vec::new();
        };
        let changed = std::mem::take(&mut entry.changed)
            .into_iter()
            .filter_map(|name| entry.props.get(name).map(|v| (name, v)))
            .collect();
        vec![Effect::Ready {
            to: callback.clone(),
            reply,
            changed,
            exchange: sent,
        }]
    }

    /// `prompt_stop_prompting`: forget the callback, close its dialog, and
    /// tell the client when `send_done`.
    fn stop(&mut self, callback: &Callback, send_done: bool) -> Vec<Effect> {
        let Some(entry) = self.callbacks.remove(callback) else {
            return Vec::new();
        };
        self.waiting.retain(|c| c != callback);
        let mut effects = Vec::new();
        if self.active.as_ref() == Some(callback) {
            self.active = None;
            if entry.shown {
                effects.push(Effect::Close(callback.clone()));
            }
        }
        if send_done {
            effects.push(Effect::Done(callback.clone()));
        }
        effects.push(Effect::Unwatch(callback.clone()));
        effects
    }

    /// `StopPrompting`.
    pub fn stop_prompting(&mut self, callback: &Callback) -> Vec<Effect> {
        let mut effects = self.stop(callback, true);
        self.next_ready(&mut effects);
        effects
    }

    /// A client's `PromptReady` failed: it is gone or broken. Nothing is
    /// sent to it.
    pub fn ready_failed(&mut self, callback: &Callback) -> Vec<Effect> {
        let mut effects = self.stop(callback, false);
        self.next_ready(&mut effects);
        effects
    }

    /// A bus name left the bus: its prompts end. (gcr leaves the queue
    /// stalled until the next call; the next prompt goes ahead here.)
    pub fn vanished(&mut self, name: &str) -> Vec<Effect> {
        let gone: Vec<Callback> = self
            .callbacks
            .keys()
            .filter(|c| c.name == name)
            .cloned()
            .collect();
        let mut effects = Vec::new();
        for callback in gone {
            effects.extend(self.stop(&callback, false));
        }
        self.next_ready(&mut effects);
        effects
    }

    /// The session lock mode: the dialog works only unlocked. Locking
    /// cancels a prompt that is showing and closes the dialog.
    pub fn set_enabled(&mut self, enabled: bool) -> Vec<Effect> {
        if self.enabled == enabled {
            return Vec::new();
        }
        self.enabled = enabled;
        if enabled {
            return Vec::new();
        }
        let Some(active) = self.active.clone() else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        let Some(entry) = self.callbacks.get_mut(&active) else {
            return effects;
        };
        if std::mem::take(&mut entry.shown) {
            effects.push(Effect::Close(active.clone()));
        }
        if entry.prompting.is_some() {
            effects.extend(self.answer(&active, Answer::Cancel));
        }
        effects
    }

    /// The prompter is going away: every client is told it is done.
    pub fn stop_all(&mut self) -> Vec<Effect> {
        let all: Vec<Callback> = self.callbacks.keys().cloned().collect();
        let mut effects = Vec::new();
        for callback in all {
            effects.extend(self.stop(&callback, true));
        }
        effects
    }
}

// ---- D-Bus and the dialog ---------------------------------------------------

const XML: &str = r#"<node>
  <interface name="org.gnome.keyring.internal.Prompter">
    <method name="BeginPrompting">
      <arg name="callback" type="o" direction="in"/>
    </method>
    <method name="PerformPrompt">
      <arg name="callback" type="o" direction="in"/>
      <arg name="type" type="s" direction="in"/>
      <arg name="properties" type="a{sv}" direction="in"/>
      <arg name="exchange" type="s" direction="in"/>
    </method>
    <method name="StopPrompting">
      <arg name="callback" type="o" direction="in"/>
    </method>
  </interface>
</node>"#;

/// The property values of a `PerformPrompt`'s `a{sv}` that a prompt can
/// hold; the rest are dropped.
fn parse_properties(dict: &glib::Variant) -> Vec<(String, PropValue)> {
    (0..dict.n_children())
        .filter_map(|i| {
            let entry = dict.child_value(i);
            let name = entry.child_value(0).str()?.to_owned();
            let value = entry.child_value(1).as_variant()?;
            let value = if value.type_() == glib::VariantTy::STRING {
                PropValue::Str(value.str()?.to_owned())
            } else if value.type_() == glib::VariantTy::BOOLEAN {
                PropValue::Bool(value.get::<bool>()?)
            } else if value.type_() == glib::VariantTy::INT32 {
                PropValue::Int(value.get::<i32>()?)
            } else {
                return None;
            };
            Some((name, value))
        })
        .collect()
}

/// The `a{sv}` of `PromptReady`.
fn properties_variant(changed: &[(&'static str, PropValue)]) -> glib::Variant {
    let dict = glib::VariantDict::new(None);
    for (name, value) in changed {
        match value {
            PropValue::Str(v) => dict.insert_value(name, &v.to_variant()),
            PropValue::Bool(v) => dict.insert_value(name, &v.to_variant()),
            PropValue::Int(v) => dict.insert_value(name, &v.to_variant()),
        }
    }
    dict.end()
}

/// Where prompts are shown: the dialog in the shell, a recording stand-in
/// in tests.
pub trait Surface {
    fn show(&self, view: &View);
    fn close(&self);
}

impl Surface for KeyringDialog {
    fn show(&self, view: &View) {
        KeyringDialog::show(self, view);
    }

    fn close(&self) {
        KeyringDialog::close(self);
    }
}

/// Makes the surface when the first prompt needs it, given where to send
/// what the user does.
type MakeSurface = Box<dyn Fn(Rc<dyn Fn(KeyringEvent)>) -> Rc<dyn Surface>>;

/// The shell's keyring prompter.
pub struct Prompter {
    core: RefCell<Core>,
    make_surface: MakeSurface,
    surface: OnceCell<Rc<dyn Surface>>,
    conn: RefCell<Option<gio::DBusConnection>>,
    registration: RefCell<Option<gio::RegistrationId>>,
    /// A subscription to each callback owner's `NameOwnerChanged`.
    watches: RefCell<HashMap<Callback, gio::SignalSubscription>>,
    /// The prompt the surface is showing.
    current: RefCell<Option<Callback>>,
}

/// Own `org.gnome.keyring.SystemPrompter` on the session bus and serve it
/// with the shell's dialog. Does nothing when the name already has an
/// owner.
pub fn start(app: &gtk::Application) -> Rc<Prompter> {
    let app = app.clone();
    let me = Prompter::new(Box::new(move |on_event| {
        KeyringDialog::new(&app, on_event) as Rc<dyn Surface>
    }));
    let owner = me.clone();
    gio::bus_get(
        gio::BusType::Session,
        gio::Cancellable::NONE,
        move |conn| match conn {
            Ok(conn) => owner.own(&conn),
            Err(e) => eprintln!("tuna-shell-gtk: keyring prompter: no session bus: {e}"),
        },
    );
    me
}

impl Prompter {
    pub fn new(make_surface: MakeSurface) -> Rc<Self> {
        Rc::new(Self {
            core: RefCell::new(Core::default()),
            make_surface,
            surface: OnceCell::new(),
            conn: RefCell::new(None),
            registration: RefCell::new(None),
            watches: RefCell::new(HashMap::new()),
            current: RefCell::new(None),
        })
    }

    /// Ask for the bus name and serve once it is ours. The request does not
    /// queue: a name that is taken stays with its owner and nothing is
    /// served. It allows replacement, as gnome-shell does, so a later
    /// prompter can take over; the clients are then told they are done.
    pub fn own(self: &Rc<Self>, conn: &gio::DBusConnection) {
        let (acquired, lost) = (self.clone(), self.clone());
        let _owner = gio::bus_own_name_on_connection(
            conn,
            BUS_NAME,
            gio::BusNameOwnerFlags::ALLOW_REPLACEMENT | gio::BusNameOwnerFlags::DO_NOT_QUEUE,
            move |conn, _| acquired.serve(&conn),
            move |_, _| {
                if lost.registration.borrow().is_some() {
                    eprintln!("tuna-shell-gtk: keyring prompter replaced");
                } else {
                    eprintln!("tuna-shell-gtk: keyring prompter name owned elsewhere");
                }
                lost.stop();
            },
        );
    }

    fn serve(self: &Rc<Self>, conn: &gio::DBusConnection) {
        let Some(info) = gio::DBusNodeInfo::for_xml(XML)
            .ok()
            .and_then(|node| node.lookup_interface(PROMPTER_IFACE))
        else {
            eprintln!("tuna-shell-gtk: keyring prompter interface");
            return;
        };
        let calls = self.clone();
        let registered = conn
            .register_object(OBJECT_PATH, &info)
            .method_call(move |_, _, _, _, method, params, invocation| {
                calls.method_call(method, &params, invocation)
            })
            .build();
        match registered {
            Ok(id) => {
                *self.registration.borrow_mut() = Some(id);
                *self.conn.borrow_mut() = Some(conn.clone());
            }
            Err(e) => eprintln!("tuna-shell-gtk: keyring prompter object: {e}"),
        }
    }

    /// The session lock mode: prompts are cancelled unless unlocked.
    /// `None` (no authoritative lock state yet) changes nothing.
    pub fn sync_lock(self: &Rc<Self>, locked: Option<bool>) {
        let effects = match locked {
            Some(locked) => self.core.borrow_mut().set_enabled(!locked),
            None => return,
        };
        self.apply(effects);
    }

    /// The name was lost or the shell is leaving: every client is done.
    fn stop(self: &Rc<Self>) {
        let effects = self.core.borrow_mut().stop_all();
        self.apply(effects);
        let conn = self.conn.borrow_mut().take();
        if let (Some(conn), Some(id)) = (conn, self.registration.borrow_mut().take()) {
            let _ = conn.unregister_object(id);
        }
    }

    fn surface(self: &Rc<Self>) -> &Rc<dyn Surface> {
        self.surface.get_or_init(|| {
            let weak = Rc::downgrade(self);
            (self.make_surface)(Rc::new(move |event| {
                if let Some(me) = weak.upgrade() {
                    me.on_event(event);
                }
            }))
        })
    }

    fn method_call(
        self: &Rc<Self>,
        method: &str,
        params: &glib::Variant,
        invocation: gio::DBusMethodInvocation,
    ) {
        // The bus fills the sender in; it is the half of a prompt's identity
        // a client cannot choose.
        let (Some(name), Some(path)) = (
            invocation.sender().map(|s| s.to_string()),
            params.child_value(0).str().map(str::to_owned),
        ) else {
            invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", "no callback");
            return;
        };
        let callback = Callback { name, path };
        let outcome = match method {
            "BeginPrompting" => self.core.borrow_mut().begin(callback),
            "PerformPrompt" => {
                let kind = params.child_value(1).str().unwrap_or_default().to_owned();
                let properties = parse_properties(&params.child_value(2));
                let exchange = params.child_value(3).str().unwrap_or_default().to_owned();
                self.core
                    .borrow_mut()
                    .perform(&callback, &kind, &properties, &exchange)
            }
            "StopPrompting" => {
                // gcr tells the client it is done before it answers the call.
                let effects = self.core.borrow_mut().stop_prompting(&callback);
                self.apply(effects);
                invocation.return_value(None);
                return;
            }
            _ => {
                invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method);
                return;
            }
        };
        match outcome {
            Ok(effects) => {
                // The reply first: a cancelled prompt's PromptReady follows it.
                invocation.return_value(None);
                self.apply(effects);
            }
            Err(Refusal::Failed(why)) => {
                invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", why)
            }
            Err(Refusal::InvalidArgs(why)) => {
                invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", why)
            }
        }
    }

    fn on_event(self: &Rc<Self>, event: KeyringEvent) {
        let Some(callback) = self.current.borrow().clone() else {
            return;
        };
        let effects = {
            let mut core = self.core.borrow_mut();
            match event {
                KeyringEvent::Warning(text) => {
                    core.warn(&callback, &text);
                    return;
                }
                KeyringEvent::Password { secret, choice } => {
                    core.answer(&callback, Answer::Password { secret, choice })
                }
                KeyringEvent::Confirm { choice } => {
                    core.answer(&callback, Answer::Confirm { choice })
                }
                KeyringEvent::Cancel => core.answer(&callback, Answer::Cancel),
            }
        };
        self.apply(effects);
    }

    fn apply(self: &Rc<Self>, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Show(callback, view) => {
                    *self.current.borrow_mut() = Some(callback);
                    self.surface().show(&view);
                }
                Effect::Close(callback) => {
                    if self.current.borrow().as_ref() == Some(&callback) {
                        *self.current.borrow_mut() = None;
                        if let Some(surface) = self.surface.get() {
                            surface.close();
                        }
                    }
                }
                Effect::Ready {
                    to,
                    reply,
                    changed,
                    exchange,
                } => {
                    let params = glib::Variant::tuple_from_iter([
                        reply.to_variant(),
                        properties_variant(&changed),
                        exchange.to_variant(),
                    ]);
                    let me = self.clone();
                    let target = to.clone();
                    self.call(&to, "PromptReady", &params, move |result| {
                        if let Err(e) = result {
                            eprintln!("tuna-shell-gtk: keyring prompt callback: {e}");
                            let effects = me.core.borrow_mut().ready_failed(&target);
                            me.apply(effects);
                        }
                    });
                }
                Effect::Done(to) => {
                    let none = glib::Variant::tuple_from_iter(std::iter::empty::<glib::Variant>());
                    self.call(&to, "PromptDone", &none, |_| {});
                }
                Effect::Watch(callback) => self.watch(callback),
                Effect::Unwatch(callback) => {
                    self.watches.borrow_mut().remove(&callback);
                }
            }
        }
    }

    fn call(
        &self,
        to: &Callback,
        method: &str,
        params: &glib::Variant,
        done: impl FnOnce(Result<glib::Variant, glib::Error>) + 'static,
    ) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        conn.call(
            Some(&to.name),
            &to.path,
            CALLBACK_IFACE,
            method,
            Some(params),
            Some(glib::VariantTy::new("()").expect("valid type")),
            gio::DBusCallFlags::NO_AUTO_START,
            -1,
            gio::Cancellable::NONE,
            done,
        );
    }

    /// End the callback's prompts when its owner leaves the bus.
    fn watch(self: &Rc<Self>, callback: Callback) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let name = callback.name.clone();
        let me = Rc::downgrade(self);
        let gone = {
            let name = name.clone();
            move || {
                if let Some(me) = me.upgrade() {
                    let effects = me.core.borrow_mut().vanished(&name);
                    me.apply(effects);
                }
            }
        };
        let on_signal = gone.clone();
        let subscription = conn.subscribe_to_signal(
            Some("org.freedesktop.DBus"),
            Some("org.freedesktop.DBus"),
            Some("NameOwnerChanged"),
            Some("/org/freedesktop/DBus"),
            Some(&name),
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let owner = signal.parameters.try_child_value(2);
                if owner
                    .as_ref()
                    .and_then(|v| v.str())
                    .is_some_and(str::is_empty)
                {
                    on_signal();
                }
            },
        );
        self.watches.borrow_mut().insert(callback, subscription);
        // A name that left before the subscription was made is gone too.
        conn.call(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(name,).to_variant()),
            Some(glib::VariantTy::new("(b)").expect("valid type")),
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |reply| {
                if reply
                    .ok()
                    .and_then(|v| v.child_value(0).get::<bool>())
                    .is_some_and(|owned| !owned)
                {
                    gone();
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PropValue::{Bool, Str};

    fn cb(name: &str, path: &str) -> Callback {
        Callback {
            name: name.to_owned(),
            path: path.to_owned(),
        }
    }

    fn enabled() -> Core {
        let mut core = Core::default();
        core.set_enabled(true);
        core
    }

    /// A client's end: what `GcrSystemPrompt` does with the prompter's
    /// exchange strings.
    struct Client {
        callback: Callback,
        exchange: Exchange,
    }

    impl Client {
        fn new(name: &str) -> Self {
            Self {
                callback: cb(name, "/org/gnome/keyring/Prompt/p1"),
                exchange: Exchange::new().unwrap(),
            }
        }

        /// Begin and take the first PromptReady.
        fn begin(&mut self, core: &mut Core) -> Vec<Effect> {
            let effects = core.begin(self.callback.clone()).unwrap();
            self.take_first(&effects);
            effects
        }

        fn take_first(&mut self, effects: &[Effect]) {
            let exchange = effects
                .iter()
                .find_map(|e| match e {
                    Effect::Ready { to, exchange, .. } if *to == self.callback => Some(exchange),
                    _ => None,
                })
                .expect("PromptReady");
            assert!(self.exchange.receive(exchange).unwrap().is_none());
        }

        fn perform(
            &mut self,
            core: &mut Core,
            kind: &str,
            props: &[(&str, PropValue)],
        ) -> Result<Vec<Effect>, Refusal> {
            let props: Vec<_> = props
                .iter()
                .map(|(n, v)| ((*n).to_owned(), v.clone()))
                .collect();
            core.perform(
                &self.callback,
                kind,
                &props,
                &self
                    .exchange
                    .send(None)
                    .unwrap_or_else(|_| self.exchange.begin()),
            )
        }

        /// The password in an answer, or None for a no.
        fn open(&mut self, effects: &[Effect]) -> (&'static str, Option<String>) {
            let Some(Effect::Ready {
                reply, exchange, ..
            }) = effects.iter().find(|e| matches!(e, Effect::Ready { .. }))
            else {
                panic!("no PromptReady in {effects:?}")
            };
            let secret = self
                .exchange
                .receive(exchange)
                .unwrap()
                .map(|s| String::from_utf8(s.to_vec()).unwrap());
            (reply, secret)
        }
    }

    fn password(secret: &str, choice: bool) -> Answer {
        Answer::Password {
            secret: Zeroizing::new(secret.to_owned()),
            choice,
        }
    }

    #[test]
    fn a_password_prompt_end_to_end() {
        let mut core = enabled();
        let mut client = Client::new(":1.10");
        let began = client.begin(&mut core);
        assert!(matches!(began[0], Effect::Watch(_)));
        let Effect::Ready { reply, changed, .. } = &began[1] else {
            panic!("{began:?}")
        };
        assert_eq!((*reply, changed.len()), ("", 0));

        let shown = client
            .perform(
                &mut core,
                "password",
                &[
                    ("message", Str("Unlock Login keyring".into())),
                    ("description", Str("An application wants access".into())),
                    ("choice-label", Str("_Automatically unlock".into())),
                    ("cancel-label", Str("_Cancel".into())),
                ],
            )
            .unwrap();
        let [Effect::Show(id, view)] = &shown[..] else {
            panic!("{shown:?}")
        };
        assert_eq!(*id, client.callback);
        assert_eq!(view.kind, Kind::Password);
        assert_eq!(view.props.message, "Unlock Login keyring");
        assert_eq!(view.props.choice_label, "Automatically unlock");
        assert_eq!(view.props.cancel_label, "Cancel");
        assert_eq!(view.props.continue_label, "Continue");

        let answered = core.answer(&client.callback, password("hunter2 \u{e9}", true));
        let (reply, secret) = client.open(&answered);
        assert_eq!((reply, secret.as_deref()), ("yes", Some("hunter2 \u{e9}")));
        let Effect::Ready { changed, .. } = &answered[0] else {
            panic!()
        };
        // What the dialog changed goes back: the strength and the choice.
        assert!(changed.contains(&("choice-chosen", Bool(true))));
        assert!(changed.iter().any(|(n, _)| *n == "password-strength"));
        assert!(changed.contains(&("message", Str("Unlock Login keyring".into()))));

        // A retry on the same prompt, then the client stops.
        let again = client
            .perform(&mut core, "password", &[("warning", Str("Wrong".into()))])
            .unwrap();
        assert!(matches!(again[0], Effect::Show(..)));
        let cancelled = core.answer(&client.callback, Answer::Cancel);
        assert_eq!(client.open(&cancelled), ("no", None));
        let stopped = core.stop_prompting(&client.callback);
        assert_eq!(
            stopped,
            vec![
                Effect::Close(client.callback.clone()),
                Effect::Done(client.callback.clone()),
                Effect::Unwatch(client.callback.clone()),
            ]
        );
        assert!(!core.is_prompting());
    }

    #[test]
    fn a_confirm_prompt_answers_without_a_secret() {
        let mut core = enabled();
        let mut client = Client::new(":1.10");
        client.begin(&mut core);
        let shown = client.perform(&mut core, "confirm", &[]).unwrap();
        assert!(matches!(&shown[0], Effect::Show(_, v) if v.kind == Kind::Confirm));
        let yes = core.answer(&client.callback, Answer::Confirm { choice: false });
        assert_eq!(client.open(&yes), ("yes", None));
        // The wrong kind of answer is a no and never carries a secret.
        client.perform(&mut core, "confirm", &[]).unwrap();
        let no = core.answer(&client.callback, password("leak", false));
        assert_eq!(client.open(&no), ("no", None));
    }

    #[test]
    fn an_answer_goes_only_to_the_callback_that_asked() {
        let mut core = enabled();
        let mut a = Client::new(":1.10");
        let mut b = Client::new(":1.11");
        a.begin(&mut core);
        // b waits its turn: no PromptReady for b yet.
        let waiting = core.begin(b.callback.clone()).unwrap();
        assert_eq!(waiting, vec![Effect::Watch(b.callback.clone())]);
        a.perform(&mut core, "password", &[]).unwrap();
        // b cannot perform, answer or stop a's prompt: the sender is part of
        // the key.
        let impostor = Callback {
            name: ":1.11".to_owned(),
            path: a.callback.path.clone(),
        };
        assert_eq!(
            core.perform(&impostor, "password", &[], "[sx-aes-1]\npublic=AQ==\n"),
            Err(Refusal::Failed(
                "Not begun prompting for this prompt callback"
            ))
        );
        assert!(core.answer(&impostor, password("x", false)).is_empty());
        // b's own callback at the same path is b's, not a's: b is still
        // waiting, so it has no turn to perform on.
        assert_eq!(
            b.perform(&mut core, "password", &[]),
            Err(Refusal::Failed(
                "Not begun prompting for this prompt callback"
            ))
        );
        let answered = core.answer(&a.callback, password("a-secret", false));
        for effect in &answered {
            let Effect::Ready { to, exchange, .. } = effect else {
                continue;
            };
            assert_eq!(*to, a.callback);
            assert!(!exchange.contains("a-secret"));
        }
        assert_eq!(a.open(&answered).1.as_deref(), Some("a-secret"));
        // Stopping a hands the turn to b, with b's own exchange.
        let next = core.stop_prompting(&a.callback);
        let Some(Effect::Ready { to, exchange, .. }) =
            next.iter().find(|e| matches!(e, Effect::Ready { .. }))
        else {
            panic!("{next:?}")
        };
        assert_eq!(*to, b.callback);
        b.take_first(&next);
        // a's key does not open b's exchanges.
        let mut stranger = Client::new(":1.12");
        assert!(stranger.exchange.receive(exchange).is_ok());
        b.perform(&mut core, "password", &[]).unwrap();
        let b_answer = core.answer(&b.callback, password("b-secret", false));
        assert_eq!(b.open(&b_answer).1.as_deref(), Some("b-secret"));
        let Effect::Ready { exchange, .. } = &b_answer[0] else {
            panic!()
        };
        let eavesdropped = a.exchange.receive(exchange);
        assert!(
            !matches!(&eavesdropped, Ok(Some(s)) if s.as_slice() == b"b-secret"),
            "a read b's password"
        );
    }

    #[test]
    fn performing_needs_a_turn_and_one_prompt_at_a_time() {
        let mut core = enabled();
        let mut client = Client::new(":1.10");
        let silent = Callback {
            name: ":1.99".to_owned(),
            path: "/x".to_owned(),
        };
        assert!(core.perform(&silent, "password", &[], "").is_err());
        client.begin(&mut core);
        assert_eq!(
            core.begin(client.callback.clone()),
            Err(Refusal::Failed(
                "Already begun prompting for this prompt callback"
            ))
        );
        client.perform(&mut core, "password", &[]).unwrap();
        assert_eq!(
            client.perform(&mut core, "password", &[]),
            Err(Refusal::Failed(
                "Already performing a prompt for this prompt callback"
            ))
        );
    }

    #[test]
    fn bad_exchanges_and_types_are_invalid_arguments() {
        let mut core = enabled();
        let mut client = Client::new(":1.10");
        client.begin(&mut core);
        assert_eq!(
            core.perform(&client.callback, "password", &[], "nonsense"),
            Err(Refusal::InvalidArgs("Invalid secret exchange received"))
        );
        assert_eq!(
            client.perform(&mut core, "pin", &[]),
            Err(Refusal::InvalidArgs("Invalid type argument"))
        );
        // Neither started a prompt.
        assert!(client.perform(&mut core, "password", &[]).is_ok());
    }

    #[test]
    fn property_values_of_the_wrong_type_or_name_are_ignored() {
        let mut core = enabled();
        let mut client = Client::new(":1.10");
        client.begin(&mut core);
        let shown = client
            .perform(
                &mut core,
                "password",
                &[
                    ("message", Bool(true)),
                    ("no-such-property", Str("x".into())),
                    ("password-new", Bool(true)),
                    ("caller-window", Str("x11:1".into())),
                    ("title", Str("unused".into())),
                ],
            )
            .unwrap();
        let [Effect::Show(_, view)] = &shown[..] else {
            panic!()
        };
        assert_eq!(view.props.message, "");
        assert!(view.props.password_new);
    }

    #[test]
    fn long_text_is_cut_at_a_character_boundary() {
        let long = "\u{e9}".repeat(MAX_TEXT);
        let mut core = enabled();
        let mut client = Client::new(":1.10");
        client.begin(&mut core);
        let shown = client
            .perform(&mut core, "confirm", &[("message", Str(long))])
            .unwrap();
        let [Effect::Show(_, view)] = &shown[..] else {
            panic!()
        };
        assert_eq!(view.props.message.len(), MAX_TEXT);
    }

    #[test]
    fn a_client_has_few_prompts_and_the_session_has_a_limit() {
        let mut core = enabled();
        for i in 0..MAX_PER_SENDER {
            core.begin(cb(":1.10", &format!("/p{i}"))).unwrap();
        }
        assert_eq!(
            core.begin(cb(":1.10", "/more")),
            Err(Refusal::Failed("Too many prompts"))
        );
        let mut n = 0;
        while core.begin(cb(&format!(":2.{n}"), "/p")).is_ok() {
            n += 1;
        }
        assert_eq!(core.callbacks.len(), MAX_PROMPTS);
    }

    #[test]
    fn a_client_that_leaves_the_bus_ends_its_prompts_and_the_next_goes_ahead() {
        let mut core = enabled();
        let mut a = Client::new(":1.10");
        let mut b = Client::new(":1.11");
        a.begin(&mut core);
        core.begin(b.callback.clone()).unwrap();
        a.perform(&mut core, "password", &[]).unwrap();
        let effects = core.vanished(":1.10");
        assert!(effects.contains(&Effect::Close(a.callback.clone())));
        assert!(!effects.contains(&Effect::Done(a.callback.clone())));
        assert!(effects.contains(&Effect::Unwatch(a.callback.clone())));
        b.take_first(&effects);
        assert!(core.vanished(":1.10").is_empty());
    }

    #[test]
    fn a_failed_prompt_ready_drops_the_client_without_telling_it() {
        let mut core = enabled();
        let mut a = Client::new(":1.10");
        let mut b = Client::new(":1.11");
        a.begin(&mut core);
        core.begin(b.callback.clone()).unwrap();
        let effects = core.ready_failed(&a.callback);
        assert!(!effects.contains(&Effect::Done(a.callback.clone())));
        b.take_first(&effects);
    }

    #[test]
    fn a_locked_session_cancels_every_prompt() {
        let mut core = Core::default();
        let mut client = Client::new(":1.10");
        client.begin(&mut core);
        let effects = client
            .perform(&mut core, "password", &[("password-new", Bool(true))])
            .unwrap();
        assert!(!effects.iter().any(|e| matches!(e, Effect::Show(..))));
        assert_eq!(client.open(&effects), ("no", None));
        // Unlocked, it shows; locking while it shows cancels and closes.
        core.set_enabled(true);
        let shown = client.perform(&mut core, "password", &[]).unwrap();
        assert!(matches!(shown[0], Effect::Show(..)));
        let locked = core.set_enabled(false);
        assert_eq!(locked[0], Effect::Close(client.callback.clone()));
        assert_eq!(client.open(&locked), ("no", None));
    }

    #[test]
    fn stopping_everything_tells_every_client() {
        let mut core = enabled();
        let mut a = Client::new(":1.10");
        let b = Client::new(":1.11");
        a.begin(&mut core);
        core.begin(b.callback.clone()).unwrap();
        let effects = core.stop_all();
        assert!(effects.contains(&Effect::Done(a.callback.clone())));
        assert!(effects.contains(&Effect::Done(b.callback.clone())));
        assert!(!core.is_prompting());
    }

    #[test]
    fn the_dialog_warning_goes_back_with_the_next_answer() {
        let mut core = enabled();
        let mut client = Client::new(":1.10");
        client.begin(&mut core);
        client.perform(&mut core, "password", &[]).unwrap();
        core.warn(&client.callback, "Passwords do not match");
        let answered = core.answer(&client.callback, password("x", false));
        let Effect::Ready { changed, .. } = &answered[0] else {
            panic!()
        };
        assert!(changed.contains(&("warning", Str("Passwords do not match".into()))));
    }

    #[test]
    fn mnemonics_are_dropped_like_shell_keyring_prompt() {
        assert_eq!(remove_mnemonics("_Unlock"), "Unlock");
        assert_eq!(remove_mnemonics("a__b"), "a_b");
        assert_eq!(remove_mnemonics("trailing_"), "trailing");
        assert_eq!(remove_mnemonics(""), "");
    }

    #[test]
    fn password_strength_follows_shell_keyring_prompt() {
        // (length - 2) + digits + 1.5 * other + uppercase, each count capped
        // (length at 5, the others at 3), clamped to 1..=10.
        assert_eq!(password_strength(""), 0);
        assert_eq!(password_strength("a"), 1);
        assert_eq!(password_strength("abcde"), 3);
        assert_eq!(password_strength("abcdefgh"), 3);
        assert_eq!(password_strength("Abcde1"), 5);
        assert_eq!(password_strength("Abc1!"), 6);
        assert_eq!(password_strength("ABC123!!!zz"), 10);
    }
}

/// The prompter on a private bus, with clients that speak gcr's protocol
/// (`GcrSystemPrompt`) and a stand-in for the dialog.
#[cfg(test)]
mod bus_tests {
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    struct TestBus {
        child: Child,
        address: String,
    }

    impl TestBus {
        fn spawn() -> Self {
            let mut child = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .expect("a private test bus (dbus-daemon) starts");
            let mut address = String::new();
            BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut address)
                .unwrap();
            Self {
                child,
                address: address.trim().to_owned(),
            }
        }

        fn connect(&self) -> gio::DBusConnection {
            gio::DBusConnection::for_address_sync(
                &self.address,
                gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                    | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                None,
                gio::Cancellable::NONE,
            )
            .expect("connects to the test bus")
        }
    }

    impl Drop for TestBus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Run the main context until `done`, as the bus's traffic needs.
    fn pump_until(what: &str, done: impl Fn() -> bool) {
        let ctx = glib::MainContext::ref_thread_default();
        let end = Instant::now() + Duration::from_secs(15);
        while !done() {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            if !ctx.iteration(false) {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    fn pump_for(ms: u64) {
        let ctx = glib::MainContext::ref_thread_default();
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            if !ctx.iteration(false) {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    fn call(
        conn: &gio::DBusConnection,
        dest: &str,
        path: &str,
        iface: &str,
        method: &str,
        params: Option<glib::Variant>,
    ) -> Result<glib::Variant, glib::Error> {
        let reply = Rc::new(RefCell::new(None));
        let slot = reply.clone();
        conn.call(
            Some(dest),
            path,
            iface,
            method,
            params.as_ref(),
            None,
            gio::DBusCallFlags::NONE,
            10_000,
            gio::Cancellable::NONE,
            move |result| *slot.borrow_mut() = Some(result),
        );
        pump_until(method, || reply.borrow().is_some());
        reply.take().unwrap()
    }

    fn object(path: &str) -> glib::variant::ObjectPath {
        glib::variant::ObjectPath::try_from(path.to_owned()).unwrap()
    }

    type Handler = Rc<dyn Fn(KeyringEvent)>;

    /// What the stand-in dialog saw.
    #[derive(Default)]
    struct Seen {
        shown: RefCell<Vec<View>>,
        closed: Cell<u32>,
        on_event: RefCell<Option<Handler>>,
    }

    struct FakeSurface(Rc<Seen>);

    impl Surface for FakeSurface {
        fn show(&self, view: &View) {
            self.0.shown.borrow_mut().push(view.clone());
        }

        fn close(&self) {
            self.0.closed.set(self.0.closed.get() + 1);
        }
    }

    impl Seen {
        fn user(&self, event: KeyringEvent) {
            let handler = self.on_event.borrow().clone().expect("a prompt was shown");
            handler(event);
        }
    }

    fn prompter(bus: &TestBus, seen: &Rc<Seen>) -> (Rc<Prompter>, gio::DBusConnection) {
        let server = bus.connect();
        let seen = seen.clone();
        let prompter = Prompter::new(Box::new(move |on_event| {
            *seen.on_event.borrow_mut() = Some(on_event);
            Rc::new(FakeSurface(seen.clone())) as Rc<dyn Surface>
        }));
        prompter.sync_lock(Some(false));
        prompter.own(&server);
        let served = prompter.clone();
        pump_until("the prompter to own its name", move || {
            served.registration.borrow().is_some()
        });
        (prompter, server)
    }

    type Ready = (String, glib::Variant, String);

    /// `GcrSystemPrompt`'s end of the protocol.
    struct Client {
        conn: gio::DBusConnection,
        path: String,
        exchange: RefCell<Exchange>,
        ready: Rc<RefCell<VecDeque<Ready>>>,
        done: Rc<Cell<bool>>,
        _registration: gio::RegistrationId,
    }

    const CALLBACK_XML: &str = r#"<node>
      <interface name="org.gnome.keyring.internal.Prompter.Callback">
        <method name="PromptReady">
          <arg name="reply" type="s" direction="in"/>
          <arg name="properties" type="a{sv}" direction="in"/>
          <arg name="exchange" type="s" direction="in"/>
        </method>
        <method name="PromptDone"/>
      </interface>
    </node>"#;

    impl Client {
        fn new(bus: &TestBus, path: &str) -> Self {
            let conn = bus.connect();
            let ready = Rc::new(RefCell::new(VecDeque::new()));
            let done = Rc::new(Cell::new(false));
            let info = gio::DBusNodeInfo::for_xml(CALLBACK_XML)
                .unwrap()
                .lookup_interface(CALLBACK_IFACE)
                .unwrap();
            let (queue, finished) = (ready.clone(), done.clone());
            let registration = conn
                .register_object(path, &info)
                .method_call(move |_, _, _, _, method, params, invocation| {
                    match method {
                        "PromptReady" => queue.borrow_mut().push_back((
                            params.child_value(0).str().unwrap().to_owned(),
                            params.child_value(1),
                            params.child_value(2).str().unwrap().to_owned(),
                        )),
                        "PromptDone" => finished.set(true),
                        _ => {}
                    }
                    invocation.return_value(None);
                })
                .build()
                .unwrap();
            Self {
                conn,
                path: path.to_owned(),
                exchange: RefCell::new(Exchange::new().unwrap()),
                ready,
                done,
                _registration: registration,
            }
        }

        fn prompter(
            &self,
            method: &str,
            params: glib::Variant,
        ) -> Result<glib::Variant, glib::Error> {
            call(
                &self.conn,
                BUS_NAME,
                OBJECT_PATH,
                PROMPTER_IFACE,
                method,
                Some(params),
            )
        }

        fn begin(&self) -> Result<(), glib::Error> {
            self.prompter("BeginPrompting", (object(&self.path),).to_variant())
                .map(|_| ())
        }

        /// The next PromptReady: the reply, the changed properties and the
        /// secret in its exchange, if there is one.
        fn next_ready(&self) -> (String, glib::VariantDict, Option<String>) {
            let ready = self.ready.clone();
            pump_until("PromptReady", move || !ready.borrow().is_empty());
            let (reply, props, exchange) = self.ready.borrow_mut().pop_front().unwrap();
            let secret = self
                .exchange
                .borrow_mut()
                .receive(&exchange)
                .expect("the prompter's exchange is valid")
                .map(|s| String::from_utf8(s.to_vec()).unwrap());
            (reply, glib::VariantDict::new(Some(&props)), secret)
        }

        fn perform(&self, kind: &str, props: &[(&str, glib::Variant)]) -> Result<(), glib::Error> {
            let dict = glib::VariantDict::new(None);
            for (name, value) in props {
                dict.insert_value(name, value);
            }
            // A client that never got its turn has no key yet: its exchange
            // is just its public key.
            let exchange = {
                let own = self.exchange.borrow();
                own.send(None).unwrap_or_else(|_| own.begin())
            };
            self.prompter(
                "PerformPrompt",
                glib::Variant::tuple_from_iter([
                    object(&self.path).to_variant(),
                    kind.to_variant(),
                    dict.end(),
                    exchange.to_variant(),
                ]),
            )
            .map(|_| ())
        }

        fn stop(&self) {
            self.prompter("StopPrompting", (object(&self.path),).to_variant())
                .expect("StopPrompting");
        }
    }

    fn in_context(test: impl FnOnce()) {
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(test).expect("owns its context");
    }

    fn password(secret: &str) -> KeyringEvent {
        KeyringEvent::Password {
            secret: Zeroizing::new(secret.to_owned()),
            choice: false,
        }
    }

    #[test]
    fn a_password_prompt_over_the_bus() {
        in_context(|| {
            let bus = TestBus::spawn();
            let seen = Rc::new(Seen::default());
            let (_prompter, _server) = prompter(&bus, &seen);
            let client = Client::new(&bus, "/org/gnome/keyring/Prompt/p1");
            client.begin().unwrap();
            let (reply, props, secret) = client.next_ready();
            assert_eq!(
                (reply.as_str(), props.end().n_children(), secret),
                ("", 0, None)
            );

            client
                .perform(
                    "password",
                    &[
                        ("message", "Unlock Login keyring".to_variant()),
                        ("choice-label", "_Automatically unlock".to_variant()),
                        ("password-new", true.to_variant()),
                    ],
                )
                .unwrap();
            pump_until("the dialog", || !seen.shown.borrow().is_empty());
            let shown = seen.shown.borrow()[0].clone();
            assert_eq!(shown.kind, Kind::Password);
            assert_eq!(shown.props.message, "Unlock Login keyring");
            assert_eq!(shown.props.choice_label, "Automatically unlock");
            assert!(shown.props.password_new);

            seen.user(KeyringEvent::Password {
                secret: Zeroizing::new("correct horse \u{e9}".to_owned()),
                choice: true,
            });
            let (reply, props, secret) = client.next_ready();
            assert_eq!(reply, "yes");
            assert_eq!(secret.as_deref(), Some("correct horse \u{e9}"));
            assert_eq!(props.lookup::<bool>("choice-chosen").unwrap(), Some(true));
            assert!(props.lookup::<i32>("password-strength").unwrap().is_some());

            // The client asks again, the user cancels, the client stops.
            client.perform("password", &[]).unwrap();
            pump_until("the retry", || seen.shown.borrow().len() == 2);
            seen.user(KeyringEvent::Cancel);
            let (reply, _, secret) = client.next_ready();
            assert_eq!((reply.as_str(), secret), ("no", None));
            client.stop();
            pump_until("PromptDone", || client.done.get());
            assert_eq!(seen.closed.get(), 1);
        });
    }

    #[test]
    fn a_confirm_prompt_over_the_bus() {
        in_context(|| {
            let bus = TestBus::spawn();
            let seen = Rc::new(Seen::default());
            let (_prompter, _server) = prompter(&bus, &seen);
            let client = Client::new(&bus, "/org/gnome/keyring/Prompt/p1");
            client.begin().unwrap();
            client.next_ready();
            client
                .perform("confirm", &[("continue-label", "_Delete".to_variant())])
                .unwrap();
            pump_until("the dialog", || !seen.shown.borrow().is_empty());
            assert_eq!(seen.shown.borrow()[0].kind, Kind::Confirm);
            assert_eq!(seen.shown.borrow()[0].props.continue_label, "Delete");
            seen.user(KeyringEvent::Confirm { choice: false });
            let (reply, _, secret) = client.next_ready();
            assert_eq!((reply.as_str(), secret), ("yes", None));
        });
    }

    #[test]
    fn one_client_cannot_touch_another_clients_prompt() {
        in_context(|| {
            let bus = TestBus::spawn();
            let seen = Rc::new(Seen::default());
            let (_prompter, _server) = prompter(&bus, &seen);
            let a = Client::new(&bus, "/org/gnome/keyring/Prompt/p1");
            a.begin().unwrap();
            a.next_ready();
            a.perform("password", &[("message", "A's prompt".to_variant())])
                .unwrap();
            pump_until("the dialog", || !seen.shown.borrow().is_empty());

            // b names a's callback path: the bus says who b is, so it is
            // another callback, one that has not begun.
            let b = Client::new(&bus, "/org/gnome/keyring/Prompt/p1");
            let err = b.perform("password", &[]).unwrap_err();
            assert!(err.message().contains("Not begun prompting"), "{err}");
            // Stopping it is a no-op for a: its prompt still answers.
            b.stop();
            assert_eq!(seen.closed.get(), 0);
            // b's own begin waits for a's turn to end.
            b.begin().unwrap();
            pump_for(150);
            assert!(b.ready.borrow().is_empty(), "b was given a's turn");

            seen.user(password("a-only-secret"));
            let (reply, _, secret) = a.next_ready();
            assert_eq!(
                (reply.as_str(), secret.as_deref()),
                ("yes", Some("a-only-secret"))
            );
            pump_for(100);
            assert!(b.ready.borrow().is_empty());

            // a is done; b gets its turn, with an exchange of its own.
            let a_first = a.exchange.borrow().begin();
            a.stop();
            let (reply, _, secret) = b.next_ready();
            assert_eq!((reply.as_str(), secret), ("", None));
            assert_ne!(
                b.exchange.borrow().begin(),
                a_first,
                "b's exchange is its own key pair"
            );
        });
    }

    #[test]
    fn a_client_that_leaves_the_bus_ends_its_prompt() {
        in_context(|| {
            let bus = TestBus::spawn();
            let seen = Rc::new(Seen::default());
            let (_prompter, _server) = prompter(&bus, &seen);
            let a = Client::new(&bus, "/org/gnome/keyring/Prompt/p1");
            let b = Client::new(&bus, "/org/gnome/keyring/Prompt/p2");
            a.begin().unwrap();
            a.next_ready();
            b.begin().unwrap();
            a.perform("password", &[]).unwrap();
            pump_until("the dialog", || !seen.shown.borrow().is_empty());
            a.conn.close_sync(gio::Cancellable::NONE).unwrap();
            pump_until("the dialog to close", || seen.closed.get() == 1);
            let (reply, _, _) = b.next_ready();
            assert_eq!(reply, "");
        });
    }

    #[test]
    fn a_locked_session_cancels_at_once() {
        in_context(|| {
            let bus = TestBus::spawn();
            let seen = Rc::new(Seen::default());
            let (prompter, _server) = prompter(&bus, &seen);
            prompter.sync_lock(Some(true));
            let client = Client::new(&bus, "/org/gnome/keyring/Prompt/p1");
            client.begin().unwrap();
            client.next_ready();
            client.perform("password", &[]).unwrap();
            let (reply, _, secret) = client.next_ready();
            assert_eq!((reply.as_str(), secret), ("no", None));
            assert!(seen.shown.borrow().is_empty());
        });
    }

    #[test]
    fn a_name_that_is_taken_is_left_alone() {
        in_context(|| {
            let bus = TestBus::spawn();
            // Another prompter (or anything) owns the name first.
            let squatter = bus.connect();
            let held = Rc::new(Cell::new(false));
            let flag = held.clone();
            let _owner = gio::bus_own_name_on_connection(
                &squatter,
                BUS_NAME,
                gio::BusNameOwnerFlags::NONE,
                move |_, _| flag.set(true),
                |_, _| {},
            );
            pump_until("the squatter to own the name", || held.get());

            let seen = Rc::new(Seen::default());
            let server = bus.connect();
            let made = seen.clone();
            let prompter = Prompter::new(Box::new(move |_| {
                Rc::new(FakeSurface(made.clone())) as Rc<dyn Surface>
            }));
            prompter.own(&server);
            pump_for(300);
            assert!(prompter.registration.borrow().is_none());
            assert!(prompter.conn.borrow().is_none());

            // The name still answers as the squatter, and the prompter's
            // own connection serves nothing at the prompter's path.
            let client = Client::new(&bus, "/org/gnome/keyring/Prompt/p1");
            let owner = call(
                &client.conn,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "GetNameOwner",
                Some((BUS_NAME,).to_variant()),
            )
            .unwrap();
            assert_eq!(
                owner.child_value(0).str(),
                squatter.unique_name().as_deref()
            );
            let direct = call(
                &client.conn,
                server.unique_name().unwrap().as_str(),
                OBJECT_PATH,
                PROMPTER_IFACE,
                "BeginPrompting",
                Some((object("/x"),).to_variant()),
            );
            assert!(
                direct.is_err(),
                "the prompter answered without owning the name"
            );
            assert!(!prompter.core.borrow().is_prompting());
        });
    }

    #[test]
    fn a_replacing_prompter_ends_the_clients_prompts() {
        in_context(|| {
            let bus = TestBus::spawn();
            let seen = Rc::new(Seen::default());
            let (prompter, _server) = prompter(&bus, &seen);
            let client = Client::new(&bus, "/org/gnome/keyring/Prompt/p1");
            client.begin().unwrap();
            client.next_ready();
            let replacer = bus.connect();
            let taken = Rc::new(Cell::new(false));
            let flag = taken.clone();
            let _owner = gio::bus_own_name_on_connection(
                &replacer,
                BUS_NAME,
                gio::BusNameOwnerFlags::REPLACE,
                move |_, _| flag.set(true),
                |_, _| {},
            );
            pump_until("the replacement", || taken.get());
            pump_until("PromptDone", || client.done.get());
            assert!(prompter.registration.borrow().is_none());
        });
    }
}
