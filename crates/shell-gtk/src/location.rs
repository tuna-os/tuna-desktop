//! GNOME's location services (gnome-shell `status/location.js`): the
//! `org.gnome.system.location` master switch, the GeoClue2 agent the
//! shell exports, and the panel's in-use indicator.
//!
//! GeoClue asks the agent before it gives any app a fix:
//! `AuthorizeApp` answers from the permission store's `location` table
//! (unknown apps are offered to `on_prompt`; without a dialog they are
//! denied), clamped to the master switch and `max-accuracy-level`. The
//! `InUse` manager property drives the panel icon. With GeoClue absent
//! the switch still writes GSettings, and nothing is authorized.
//!
//! The agent id (`tuna-shell`) must join GeoClue's `[agent] whitelist`
//! on the image, beside `gnome-shell` (`man geoclue`); that packaging
//! change lives outside this crate.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gio::prelude::*;
use gtk::prelude::*;
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

pub const LOCATION_SCHEMA: &str = "org.gnome.system.location";
pub const ENABLED_KEY: &str = "enabled";
pub const MAX_ACCURACY_KEY: &str = "max-accuracy-level";

const GEOCLUE: &str = "org.freedesktop.GeoClue2";
const MANAGER_PATH: &str = "/org/freedesktop/GeoClue2/Manager";
const MANAGER_IFACE: &str = "org.freedesktop.GeoClue2.Manager";
const AGENT_PATH: &str = "/org/freedesktop/GeoClue2/Agent";
const AGENT_IFACE: &str = "org.freedesktop.GeoClue2.Agent";
/// This agent's id in `AddAgent` (and GeoClue's whitelist).
pub const AGENT_ID: &str = "tuna-shell";
const PROPS: &str = "org.freedesktop.DBus.Properties";

const PERM_STORE: &str = "org.freedesktop.impl.portal.PermissionStore";
const PERM_PATH: &str = "/org/freedesktop/impl/portal/PermissionStore";
const PERM_TABLE: &str = "location";
const PERM_ID: &str = "location";

const XML: &str = r#"<node>
  <interface name="org.freedesktop.GeoClue2.Agent">
    <method name="AuthorizeApp">
      <arg type="s" name="desktop_id" direction="in"/>
      <arg type="u" name="req_accuracy_level" direction="in"/>
      <arg type="b" name="authorized" direction="out"/>
      <arg type="u" name="allowed_accuracy_level" direction="out"/>
    </method>
    <property name="MaxAccuracyLevel" type="u" access="read"/>
  </interface>
</node>"#;

/// GeoClue accuracy levels (gnome-shell's `GeoclueAccuracyLevel`).
pub const ACC_NONE: u32 = 0;
pub const ACC_COUNTRY: u32 = 1;
pub const ACC_CITY: u32 = 4;
pub const ACC_NEIGHBORHOOD: u32 = 5;
pub const ACC_STREET: u32 = 6;
pub const ACC_EXACT: u32 = 8;

/// A level name to its value: the settings nick (`exact`) or the
/// store's word (`EXACT`); anything unknown is `NONE`, as GNOME reads
/// both.
pub fn accuracy_value(name: &str) -> u32 {
    match name.to_ascii_uppercase().as_str() {
        "COUNTRY" => ACC_COUNTRY,
        "CITY" => ACC_CITY,
        "NEIGHBORHOOD" => ACC_NEIGHBORHOOD,
        "STREET" => ACC_STREET,
        "EXACT" => ACC_EXACT,
        _ => ACC_NONE,
    }
}

/// A level value to the store's word; unknown values are `NONE`.
pub fn accuracy_name(value: u32) -> &'static str {
    match value {
        ACC_COUNTRY => "COUNTRY",
        ACC_CITY => "CITY",
        ACC_NEIGHBORHOOD => "NEIGHBORHOOD",
        ACC_STREET => "STREET",
        ACC_EXACT => "EXACT",
        _ => "NONE",
    }
}

/// The agent's ceiling: `NONE` while the master switch is off, else
/// the `max-accuracy-level` setting.
pub fn max_accuracy(enabled: bool, setting: &str) -> u32 {
    if enabled {
        accuracy_value(setting)
    } else {
        ACC_NONE
    }
}

/// Clamp a grant to the ceiling; `NONE` stays denied.
pub fn clamp_accuracy(level: u32, max: u32) -> u32 {
    if level == ACC_NONE {
        ACC_NONE
    } else {
        level.min(max)
    }
}

/// `AuthorizeApp`'s reply: granted exactly when some accuracy remains.
pub fn auth_reply(accuracy: u32) -> (bool, u32) {
    (accuracy != ACC_NONE, accuracy)
}

/// What one authorization round decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthDecision {
    /// Answer GeoClue now; `save` refreshes the permission store.
    Answer { accuracy: u32, save: bool },
    /// Ask the user first (the dialog's level still passes through
    /// `clamp_accuracy`, and the answer is saved).
    Prompt,
}

/// Why a permission-store lookup produced no table. Every variant
/// fails closed (denies); only a present table with no row for the
/// app prompts. Mirrors GNOME 51 `status/location.js`, which denies
/// a missing proxy and distinguishes lookup errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupError {
    /// No session bus to ask the store on.
    NoSession,
    /// The `Lookup` call timed out.
    Timeout,
    /// The `Lookup` call returned a D-Bus error.
    Dbus,
    /// The `Lookup` reply did not unpack as `a{sas}`.
    Malformed,
    /// The store answered but the `location` table is unavailable
    /// (missing table/id, unknown service).
    StoreMissing,
}

/// A permission-store lookup: either the table or why there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupOutcome {
    Available(HashMap<String, Vec<String>>),
    Unavailable(LookupError),
}

/// Classify a failed `Lookup` reply for fail-closed handling: timeouts
/// stay timeouts, store-shaped errors stay store errors, everything
/// else is a generic D-Bus error.
pub fn classify_lookup_error(message: &str) -> LookupError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("timeout") || lower.contains("timed out") {
        LookupError::Timeout
    } else if lower.contains("notfound")
        || lower.contains("not found")
        || lower.contains("unknown table")
        || lower.contains("serviceunknown")
        || lower.contains("has no owner")
        || lower.contains("no such")
    {
        LookupError::StoreMissing
    } else {
        LookupError::Dbus
    }
}

/// Decide from the store entry, as GNOME's `AppAuthorizer` does:
/// unknown apps prompt, lookup failures deny, and an unknown desktop
/// id is denied outright.
pub fn authorize(
    max: u32,
    stored: Option<u32>,
    store_missing: bool,
    app_known: bool,
) -> AuthDecision {
    if !app_known {
        return AuthDecision::Answer {
            accuracy: ACC_NONE,
            save: false,
        };
    }
    if store_missing {
        return AuthDecision::Answer {
            accuracy: ACC_NONE,
            save: false,
        };
    }
    match stored {
        Some(level) => AuthDecision::Answer {
            accuracy: clamp_accuracy(level, max),
            save: true,
        },
        None => AuthDecision::Prompt,
    }
}

/// Decide from a full lookup outcome: unavailable denies (fail
/// closed), a present table answers from its row or prompts when the
/// app has no row. No failure may grant at the requested level.
pub fn authorize_lookup(
    max: u32,
    outcome: &LookupOutcome,
    desktop_id: &str,
    app_known: bool,
) -> AuthDecision {
    if !app_known {
        return AuthDecision::Answer {
            accuracy: ACC_NONE,
            save: false,
        };
    }
    match outcome {
        LookupOutcome::Unavailable(_) => AuthDecision::Answer {
            accuracy: ACC_NONE,
            save: false,
        },
        LookupOutcome::Available(perms) => {
            let stored = stored_accuracy(perms, desktop_id);
            authorize(max, stored, false, true)
        }
    }
}

/// Recheck an in-flight grant against the live ceiling: disabling
/// Location mid-flight denies, lowering the ceiling clamps. The
/// captured ceiling is intentionally not consulted.
pub fn live_grant(proposed: u32, live_max: u32) -> u32 {
    if live_max == ACC_NONE {
        ACC_NONE
    } else {
        clamp_accuracy(proposed, live_max)
    }
}

/// A store row (`[level, date]`) to its level; a present but garbled
/// row denies, as GNOME's `|| NONE` does. A present row with no level
/// word is garbled too: only a genuinely missing row prompts.
pub fn stored_accuracy(perms: &HashMap<String, Vec<String>>, desktop_id: &str) -> Option<u32> {
    match perms.get(desktop_id) {
        None => None,
        Some(row) => Some(row.first().map(accuracy_value).unwrap_or(ACC_NONE)),
    }
}

/// The row `Set` writes back: the level word and the decision's date.
pub fn permission_entry(level: u32, unix_secs: u64) -> Vec<String> {
    vec![accuracy_name(level).to_owned(), unix_secs.to_string()]
}

/// The level a stored-grant read saves back: the stored grant itself
/// (a timestamp refresh), never a ceiling-clamped downgrade. The store
/// records the user's consent; the ceiling is separate policy, so a
/// read under a lowered ceiling must not erase the grant.
pub fn refresh_level(
    perms: Option<&HashMap<String, Vec<String>>>,
    desktop_id: &str,
    fallback: u32,
) -> u32 {
    perms
        .and_then(|p| stored_accuracy(p, desktop_id))
        .unwrap_or(fallback)
}

/// The panel icon: shown only while location is on and in use.
pub fn indicator_icon(enabled: bool, in_use: bool) -> Option<&'static str> {
    (enabled && in_use).then_some("location-services-active-symbolic")
}

/// The authorization dialog: its app id, requested level, and the
/// answer callback (the user's level, or `NONE`).
pub type AuthorizePrompt = dyn Fn(String, u32, Rc<dyn Fn(u32)>);

/// The GeoClue2 agent: the master switch, the `InUse` indicator and
/// the `AuthorizeApp` dialog hook.
pub struct LocationAgent {
    settings: Option<gio::Settings>,
    indicator: RefCell<Option<gtk::Image>>,
    conn: RefCell<Option<gio::DBusConnection>>,
    session: RefCell<Option<gio::DBusConnection>>,
    in_use: Cell<bool>,
    prompt: RefCell<Option<Rc<AuthorizePrompt>>>,
    /// Bumped whenever the live ceiling may have moved (master switch,
    /// accuracy setting, GeoClue appearance), so in-flight lookups can
    /// tell a stale captured ceiling from the live one.
    generation: Cell<u64>,
}

impl LocationAgent {
    pub fn new() -> Rc<Self> {
        let settings = gio::SettingsSchemaSource::default()
            .and_then(|source| source.lookup(LOCATION_SCHEMA, true))
            .map(|_| gio::Settings::new(LOCATION_SCHEMA));
        let agent = Rc::new(Self {
            settings,
            indicator: RefCell::default(),
            conn: RefCell::default(),
            session: RefCell::default(),
            in_use: Cell::new(false),
            prompt: RefCell::default(),
            generation: Cell::new(0),
        });
        if let Some(settings) = agent.settings.as_ref() {
            let weak = Rc::downgrade(&agent);
            settings.connect_changed(Some(ENABLED_KEY), move |_, _| {
                if let Some(agent) = weak.upgrade() {
                    agent.bump_generation();
                    agent.sync_indicator();
                    agent.emit_max_accuracy();
                }
            });
            let weak = Rc::downgrade(&agent);
            settings.connect_changed(Some(MAX_ACCURACY_KEY), move |_, _| {
                if let Some(agent) = weak.upgrade() {
                    agent.bump_generation();
                    agent.emit_max_accuracy();
                }
            });
        }
        agent
    }

    fn bump_generation(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    /// The panel icon that follows `enabled && in_use`.
    pub fn set_indicator(&self, indicator: gtk::Image) {
        *self.indicator.borrow_mut() = Some(indicator);
        self.sync_indicator();
    }

    /// The authorization dialog: `respond` carries the user's level
    /// (or `NONE`). Without one every unknown app is denied.
    pub fn on_prompt(&self, f: impl Fn(String, u32, Rc<dyn Fn(u32)>) + 'static) {
        *self.prompt.borrow_mut() = Some(Rc::new(f));
    }

    pub fn enabled(&self) -> bool {
        self.settings
            .as_ref()
            .map(|s| s.boolean(ENABLED_KEY))
            .unwrap_or(false)
    }

    pub fn max_accuracy_level(&self) -> u32 {
        match self.settings.as_ref() {
            Some(settings) => max_accuracy(
                settings.boolean(ENABLED_KEY),
                &settings.string(MAX_ACCURACY_KEY),
            ),
            None => ACC_NONE,
        }
    }

    /// GeoClue on the system bus `conn`: export the agent, register it
    /// on every appearance, and follow `InUse`.
    pub fn attach(self: &Rc<Self>, conn: &gio::DBusConnection) {
        *self.conn.borrow_mut() = Some(conn.clone());
        let node = match gio::DBusNodeInfo::for_xml(XML) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("tuna-shell-gtk: location agent interface: {e}");
                return;
            }
        };
        let Some(info) = node.lookup_interface(AGENT_IFACE) else {
            return;
        };
        // The permission store lives on the session bus.
        {
            let this = Rc::downgrade(self);
            gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, move |conn| {
                if let (Ok(conn), Some(agent)) = (conn, this.upgrade()) {
                    *agent.session.borrow_mut() = Some(conn);
                }
            });
        }
        let calls = self.clone();
        let props = self.clone();
        if let Err(e) =
            conn.register_object(AGENT_PATH, &info)
                .method_call(move |_, _, _, _, method, params, invocation| match method {
                    "AuthorizeApp" => {
                        let desktop_id: String = params.child_value(0).get().unwrap_or_default();
                        let req: u32 = params.child_value(1).get().unwrap_or(ACC_NONE);
                        calls.authorize_app(&desktop_id, req, invocation);
                    }
                    _ => invocation
                        .return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
                })
                .property(move |_, _, _, _, name| {
                    if name == "MaxAccuracyLevel" {
                        props.max_accuracy_level().to_variant()
                    } else {
                        0u32.to_variant()
                    }
                })
                .build()
        {
            eprintln!("tuna-shell-gtk: location agent object: {e}");
            return;
        }
        let weak = Rc::downgrade(self);
        let conn2 = conn.clone();
        crate::services::watch_name(conn, GEOCLUE, move |owned| {
            let Some(agent) = weak.upgrade() else { return };
            agent.bump_generation();
            if owned {
                agent.register(&conn2);
                agent.refresh_in_use(&conn2);
            } else {
                agent.in_use.set(false);
                agent.sync_indicator();
            }
        });
        // PropertiesChanged on the manager: `InUse` flips the icon.
        let weak = Rc::downgrade(self);
        let sub = conn.subscribe_to_signal(
            Some(GEOCLUE),
            Some(PROPS),
            Some("PropertiesChanged"),
            Some(MANAGER_PATH),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                let iface = signal.parameters.try_child_value(0);
                if iface.as_ref().and_then(|v| v.str()) != Some(MANAGER_IFACE) {
                    return;
                }
                let changed = signal.parameters.try_child_value(1);
                let in_use = changed.as_ref().and_then(|v| {
                    glib::VariantDict::new(Some(v))
                        .lookup_value("InUse", None)
                        .and_then(|v| v.get::<bool>())
                });
                if let (Some(agent), Some(in_use)) = (weak.upgrade(), in_use) {
                    agent.in_use.set(in_use);
                    agent.sync_indicator();
                }
            },
        );
        // The subscription lives as long as the shell.
        std::mem::forget(sub);
    }

    /// Ask GeoClue for one app, answering from the store (or the
    /// dialog). The invocation stays open until the answer lands. The
    /// ceiling is rechecked live at reply time and again at save time:
    /// a captured positive never survives a mid-flight disable or a
    /// lowered ceiling.
    fn authorize_app(
        self: &Rc<Self>,
        desktop_id: &str,
        req: u32,
        invocation: gio::DBusMethodInvocation,
    ) {
        if self.max_accuracy_level() == ACC_NONE {
            invocation.return_value(Some(&auth_reply(ACC_NONE).to_variant()));
            return;
        }
        // GNOME's `lookup_app`: a request from an unknown id is denied.
        // (gio 0.22 has no `DesktopAppInfo`, so scan the installed apps;
        // authorizations are rare enough for the parse.)
        let file = format!("{desktop_id}.desktop");
        let app_known = gio::AppInfo::all()
            .iter()
            .any(|app| app.id().as_deref() == Some(file.as_str()));
        let this = self.clone();
        let start_gen = self.generation.get();
        let desktop_id = desktop_id.to_owned();
        lookup_permissions(self, move |outcome| {
            // Live policy, not the entry-time capture: disabling
            // Location or lowering the ceiling while the lookup was
            // pending denies or clamps this reply.
            let lookup_gen = this.generation.get();
            if lookup_gen != start_gen {
                eprintln!("tuna-shell-gtk: location policy moved mid-lookup, rechecked");
            }
            let live_max = this.max_accuracy_level();
            if live_max == ACC_NONE {
                invocation.return_value(Some(&auth_reply(ACC_NONE).to_variant()));
                return;
            }
            let decision = authorize_lookup(live_max, &outcome, &desktop_id, app_known);
            match decision {
                AuthDecision::Answer { accuracy, save } => {
                    let accuracy = live_grant(accuracy, this.max_accuracy_level());
                    if save {
                        let table = match &outcome {
                            LookupOutcome::Available(t) => Some(t),
                            LookupOutcome::Unavailable(_) => None,
                        };
                        // The reply is clamped; the saved row keeps the
                        // stored grant (timestamp refresh only).
                        let saved = refresh_level(table, &desktop_id, accuracy);
                        this.save_permission(table, &desktop_id, saved);
                    }
                    invocation.return_value(Some(&auth_reply(accuracy).to_variant()));
                }
                AuthDecision::Prompt => {
                    let prompt = this.prompt.borrow().clone();
                    match prompt {
                        Some(prompt) => {
                            let this = this.clone();
                            let dialog_gen = this.generation.get();
                            let table = match outcome {
                                LookupOutcome::Available(t) => Some(t),
                                LookupOutcome::Unavailable(_) => None,
                            };
                            let id = desktop_id.clone();
                            // The dialog answers later; its own reference.
                            // The hook is repeat-callable, so the first
                            // answer wins and later ones are dropped.
                            let invocation2 = RefCell::new(Some(invocation.clone()));
                            prompt(
                                desktop_id,
                                req,
                                Rc::new(move |level| {
                                    // Rechecked again at dialog-answer
                                    // time: the user may have flipped
                                    // the switch while deciding.
                                    if this.generation.get() != dialog_gen {
                                        eprintln!(
                                            "tuna-shell-gtk: location policy moved mid-prompt, rechecked"
                                        );
                                    }
                                    let live = this.max_accuracy_level();
                                    let accuracy = live_grant(level, live);
                                    if live != ACC_NONE {
                                        this.save_permission(table.as_ref(), &id, accuracy);
                                    }
                                    if let Some(inv) = invocation2.borrow_mut().take() {
                                        inv.return_value(Some(&auth_reply(accuracy).to_variant()));
                                    }
                                }),
                            );
                        }
                        None => {
                            invocation.return_value(Some(&auth_reply(ACC_NONE).to_variant()));
                        }
                    }
                }
            }
        });
    }

    /// The `location` table, distinguishing a present table from every
    /// failure (timeout, D-Bus error, malformed reply, missing table).
    fn lookup_table(session: &gio::DBusConnection, done: impl FnOnce(LookupOutcome) + 'static) {
        session.call(
            Some(PERM_STORE),
            PERM_PATH,
            PERM_STORE,
            "Lookup",
            Some(&(PERM_TABLE, PERM_ID).to_variant()),
            glib::VariantTy::new("(a{sas}v)").ok(),
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
            move |reply| match reply {
                Ok(v) => match v.child_value(0).get::<HashMap<String, Vec<String>>>() {
                    Some(table) => done(LookupOutcome::Available(table)),
                    None => done(LookupOutcome::Unavailable(LookupError::Malformed)),
                },
                Err(e) => done(LookupOutcome::Unavailable(classify_lookup_error(
                    &e.to_string(),
                ))),
            },
        );
    }

    /// One row back into the store, stamped with now. Rechecks the
    /// live switch first: nothing is saved while Location is off. The
    /// level arrives final (new decisions pre-clamped, stored grants
    /// preserved), so this never downgrades a row on read.
    fn save_permission(
        &self,
        perms: Option<&HashMap<String, Vec<String>>>,
        desktop_id: &str,
        accuracy: u32,
    ) {
        if self.max_accuracy_level() == ACC_NONE {
            return;
        }
        let Some(session) = self.session.borrow().clone() else {
            return;
        };
        let mut perms = perms.cloned().unwrap_or_default();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        perms.insert(desktop_id.to_owned(), permission_entry(accuracy, now));
        let data = glib::Variant::array_from_iter::<glib::Variant>(std::iter::empty());
        session.call(
            Some(PERM_STORE),
            PERM_PATH,
            PERM_STORE,
            "Set",
            Some(&(PERM_TABLE, true, PERM_ID, perms, data).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
            |_| {},
        );
    }

    fn register(&self, conn: &gio::DBusConnection) {
        conn.call(
            Some(GEOCLUE),
            MANAGER_PATH,
            MANAGER_IFACE,
            "AddAgent",
            Some(&(AGENT_ID,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            10_000,
            gio::Cancellable::NONE,
            |res| {
                if let Err(e) = res {
                    eprintln!("tuna-shell-gtk: location agent not registered: {e}");
                }
            },
        );
    }

    /// Re-read `InUse` (the agent may have missed the change while
    /// GeoClue was away).
    fn refresh_in_use(self: &Rc<Self>, conn: &gio::DBusConnection) {
        let weak = Rc::downgrade(self);
        conn.call(
            Some(GEOCLUE),
            MANAGER_PATH,
            PROPS,
            "Get",
            Some(&(MANAGER_IFACE, "InUse").to_variant()),
            glib::VariantTy::new("(v)").ok(),
            gio::DBusCallFlags::NONE,
            5000,
            None::<&gio::Cancellable>,
            move |reply| {
                let in_use = reply
                    .ok()
                    .and_then(|v| v.child_value(0).get::<glib::Variant>())
                    .and_then(|v| v.get::<bool>())
                    .unwrap_or(false);
                if let Some(agent) = weak.upgrade() {
                    agent.in_use.set(in_use);
                    agent.sync_indicator();
                }
            },
        );
    }

    fn sync_indicator(&self) {
        if let Some(indicator) = self.indicator.borrow().clone() {
            match indicator_icon(self.enabled(), self.in_use.get()) {
                Some(icon) => {
                    indicator.set_visible(true);
                    indicator.set_icon_name(Some(icon));
                }
                None => indicator.set_visible(false),
            }
        }
    }

    /// Tell GeoClue the ceiling moved (GNOME's `_notifyMaxAccuracyLevel`).
    fn emit_max_accuracy(&self) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let mut changed = HashMap::new();
        changed.insert(
            "MaxAccuracyLevel".to_owned(),
            self.max_accuracy_level().to_variant(),
        );
        let args = (AGENT_IFACE, changed, Vec::<String>::new()).to_variant();
        if let Err(e) = conn.emit_signal(None, AGENT_PATH, PROPS, "PropertiesChanged", Some(&args))
        {
            eprintln!("tuna-shell-gtk: location agent signal: {e}");
        }
    }
}

/// First-use location consent: the overlay dialog behind `on_prompt`.
/// Allow answers at the requested level (clamped to the live ceiling
/// by the caller); Deny, Escape and close answer `NONE`. A prompt that
/// arrives while one is pending denies the older one first (latest
/// wins, fail closed).
pub struct LocationPrompt {
    window: gtk::Window,
    title: gtk::Label,
    detail: gtk::Label,
    pending: RefCell<Option<PendingPrompt>>,
}

/// One pending consent: the app id, its requested level, and the
/// answer callback (the user's level, or `NONE`).
type PendingPrompt = (String, u32, Rc<dyn Fn(u32)>);

impl LocationPrompt {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.set_title(Some("Location access"));
        window.add_css_class("tuna-end-session");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some("tuna-location-consent"));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
        card.add_css_class("modal-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_margin_top(24);
        card.set_margin_bottom(24);
        let title = gtk::Label::new(None);
        title.add_css_class("message-dialog-title");
        title.set_wrap(true);
        title.set_max_width_chars(50);
        card.append(&title);
        let detail = gtk::Label::new(None);
        detail.add_css_class("message-dialog-description");
        detail.set_wrap(true);
        card.append(&detail);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.add_css_class("modal-dialog-button-box");
        let deny = gtk::Button::with_label("Deny");
        let allow = gtk::Button::with_label("Allow");
        for button in [&deny, &allow] {
            button.add_css_class("modal-dialog-button");
            buttons.append(button);
        }
        card.append(&buttons);
        window.set_child(Some(&card));
        let ui = Rc::new(Self {
            window,
            title,
            detail,
            pending: RefCell::new(None),
        });
        let weak = Rc::downgrade(&ui);
        deny.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.respond(false);
            }
        });
        let weak = Rc::downgrade(&ui);
        allow.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.respond(true);
            }
        });
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&ui);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(ui) = weak.upgrade() {
                    ui.respond(false);
                }
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        ui.window.add_controller(keys);
        let weak = Rc::downgrade(&ui);
        ui.window.connect_close_request(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.respond(false);
            }
            glib::Propagation::Stop
        });
        ui
    }

    /// Show first-use consent for `desktop_id` at `req`. Denies any
    /// older pending prompt first.
    pub fn show(&self, desktop_id: String, req: u32, respond: Rc<dyn Fn(u32)>) {
        if let Some((_, _, old)) = self.pending.borrow_mut().take() {
            old(ACC_NONE);
        }
        self.title.set_text(&format!(
            "Allow {} to access your location?",
            app_display_name(&desktop_id)
        ));
        let level = accuracy_name(req).to_ascii_lowercase();
        self.detail.set_text(&format!(
            "This app is asking for {level} location. You can change this later in Settings."
        ));
        *self.pending.borrow_mut() = Some((desktop_id, req, respond));
        self.window.present();
    }

    fn respond(&self, allow: bool) {
        if let Some((_, req, respond)) = self.pending.borrow_mut().take() {
            self.window.set_visible(false);
            respond(if allow { req } else { ACC_NONE });
        } else {
            self.window.set_visible(false);
        }
    }
}

/// An app's display name for the consent dialog: its desktop entry's
/// name when installed, else the id stem.
fn app_display_name(desktop_id: &str) -> String {
    let file = format!("{desktop_id}.desktop");
    let desktop = (!desktop_id.contains('/'))
        .then(|| {
            tuna_shell_host::apps::default_app_dirs()
                .into_iter()
                .map(|dir| dir.join(&file))
                .find(|path| path.is_file())
        })
        .flatten();
    desktop
        .as_deref()
        .and_then(tuna_shell_host::apps::handler_entry_from_file)
        .map(|entry| entry.name)
        .unwrap_or_else(|| desktop_id.to_owned())
}

/// The `location` table on the caller's session bus, or the lookup
/// failure (a missing session bus denies, it never grants).
fn lookup_permissions(agent: &Rc<LocationAgent>, done: impl FnOnce(LookupOutcome) + 'static) {
    if let Some(session) = agent.session.borrow().clone() {
        LocationAgent::lookup_table(&session, done);
    } else {
        done(LookupOutcome::Unavailable(LookupError::NoSession));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accuracy_round_trips_like_gnome() {
        assert_eq!(accuracy_value("exact"), ACC_EXACT);
        assert_eq!(accuracy_value("EXACT"), ACC_EXACT);
        assert_eq!(accuracy_value("City"), ACC_CITY);
        assert_eq!(accuracy_value("bogus"), ACC_NONE);
        assert_eq!(accuracy_name(ACC_EXACT), "EXACT");
        assert_eq!(accuracy_name(ACC_CITY), "CITY");
        assert_eq!(accuracy_name(7), "NONE");
        // The numeric levels are GNOME's, not 0..=5.
        assert_eq!(
            (
                ACC_NONE,
                ACC_COUNTRY,
                ACC_CITY,
                ACC_NEIGHBORHOOD,
                ACC_STREET,
                ACC_EXACT
            ),
            (0, 1, 4, 5, 6, 8)
        );
    }

    #[test]
    fn ceiling_is_none_while_disabled() {
        assert_eq!(max_accuracy(false, "exact"), ACC_NONE);
        assert_eq!(max_accuracy(true, "exact"), ACC_EXACT);
        assert_eq!(max_accuracy(true, "city"), ACC_CITY);
        assert_eq!(max_accuracy(true, "bogus"), ACC_NONE);
        assert_eq!(clamp_accuracy(ACC_EXACT, ACC_CITY), ACC_CITY);
        assert_eq!(clamp_accuracy(ACC_COUNTRY, ACC_CITY), ACC_COUNTRY);
        assert_eq!(clamp_accuracy(ACC_NONE, ACC_EXACT), ACC_NONE);
        assert_eq!(auth_reply(ACC_CITY), (true, ACC_CITY));
        assert_eq!(auth_reply(ACC_NONE), (false, ACC_NONE));
    }

    #[test]
    fn authorization_matches_the_authorizer() {
        // A stored grant answers, clamped to the ceiling.
        assert_eq!(
            authorize(ACC_CITY, Some(ACC_EXACT), false, true),
            AuthDecision::Answer {
                accuracy: ACC_CITY,
                save: true
            }
        );
        // Unknown apps prompt.
        assert_eq!(
            authorize(ACC_EXACT, None, false, true),
            AuthDecision::Prompt
        );
        // A missing store denies, unsaved (fail closed, like GNOME's
        // missing proxy): no failure may grant at the request.
        assert_eq!(
            authorize(ACC_EXACT, None, true, true),
            AuthDecision::Answer {
                accuracy: ACC_NONE,
                save: false
            }
        );
        // An unknown desktop id is denied outright.
        assert_eq!(
            authorize(ACC_EXACT, Some(ACC_EXACT), false, false),
            AuthDecision::Answer {
                accuracy: ACC_NONE,
                save: false
            }
        );
    }

    #[test]
    fn store_rows_parse_and_write_back() {
        let perms = HashMap::from([
            (
                "org.gnome.Maps".to_owned(),
                vec!["EXACT".to_owned(), "1700000000".to_owned()],
            ),
            ("org.gnome.Weather".to_owned(), vec!["CITY".to_owned()]),
            ("bad.app".to_owned(), vec!["BOGUS".to_owned()]),
        ]);
        assert_eq!(stored_accuracy(&perms, "org.gnome.Maps"), Some(ACC_EXACT));
        assert_eq!(stored_accuracy(&perms, "missing.app"), None);
        // A garbled row denies rather than prompting again.
        assert_eq!(stored_accuracy(&perms, "bad.app"), Some(ACC_NONE));
        assert_eq!(
            permission_entry(ACC_CITY, 42),
            vec!["CITY".to_owned(), "42".to_owned()]
        );
    }

    #[test]
    fn present_empty_row_denies_without_prompt() {
        let perms = HashMap::from([("hollow.app".to_owned(), Vec::new())]);
        // Present-but-empty is garbled: deny, never prompt.
        assert_eq!(stored_accuracy(&perms, "hollow.app"), Some(ACC_NONE));
        assert_eq!(
            authorize_lookup(
                ACC_EXACT,
                &LookupOutcome::Available(perms.clone()),
                "hollow.app",
                true
            ),
            AuthDecision::Answer {
                accuracy: ACC_NONE,
                save: true
            }
        );
        // A genuinely missing row still prompts: the distinct contrast.
        assert_eq!(stored_accuracy(&perms, "missing.app"), None);
        assert_eq!(
            authorize_lookup(
                ACC_EXACT,
                &LookupOutcome::Available(perms),
                "missing.app",
                true
            ),
            AuthDecision::Prompt
        );
    }

    #[test]
    fn indicator_shows_only_while_in_use() {
        assert_eq!(
            indicator_icon(true, true),
            Some("location-services-active-symbolic")
        );
        assert_eq!(indicator_icon(true, false), None);
        assert_eq!(indicator_icon(false, true), None);
    }

    #[test]
    fn lookup_timeout_denies_known_apps() {
        let denied = AuthDecision::Answer {
            accuracy: ACC_NONE,
            save: false,
        };
        assert_eq!(
            authorize_lookup(
                ACC_EXACT,
                &LookupOutcome::Unavailable(LookupError::Timeout),
                "proof-location-app",
                true
            ),
            denied
        );
        assert_eq!(
            classify_lookup_error("GDBus.Error:org.freedesktop.DBus.Error.Timeout: timed out"),
            LookupError::Timeout
        );
    }

    #[test]
    fn lookup_dbus_error_denies_known_apps() {
        assert_eq!(
            authorize_lookup(
                ACC_EXACT,
                &LookupOutcome::Unavailable(LookupError::Dbus),
                "proof-location-app",
                true
            ),
            AuthDecision::Answer {
                accuracy: ACC_NONE,
                save: false
            }
        );
        assert_eq!(
            classify_lookup_error("GDBus.Error:org.freedesktop.DBus.Error.Failed: boom"),
            LookupError::Dbus
        );
    }

    #[test]
    fn lookup_malformed_denies_known_apps() {
        assert_eq!(
            authorize_lookup(
                ACC_EXACT,
                &LookupOutcome::Unavailable(LookupError::Malformed),
                "proof-location-app",
                true
            ),
            AuthDecision::Answer {
                accuracy: ACC_NONE,
                save: false
            }
        );
    }

    #[test]
    fn lookup_missing_session_denies_known_apps() {
        assert_eq!(
            authorize_lookup(
                ACC_EXACT,
                &LookupOutcome::Unavailable(LookupError::NoSession),
                "proof-location-app",
                true
            ),
            AuthDecision::Answer {
                accuracy: ACC_NONE,
                save: false
            }
        );
    }

    #[test]
    fn lookup_store_missing_denies_known_apps() {
        assert_eq!(
            authorize_lookup(
                ACC_EXACT,
                &LookupOutcome::Unavailable(LookupError::StoreMissing),
                "proof-location-app",
                true
            ),
            AuthDecision::Answer {
                accuracy: ACC_NONE,
                save: false
            }
        );
        assert_eq!(
            classify_lookup_error(
                "GDBus.Error:org.freedesktop.portal.Error.NotFound: Unknown table/id"
            ),
            LookupError::StoreMissing
        );
        assert_eq!(
            classify_lookup_error(
                "GDBus.Error:org.freedesktop.DBus.Error.ServiceUnknown: has no owner"
            ),
            LookupError::StoreMissing
        );
        // Only a present table with no row prompts; every failure denies.
        let empty = LookupOutcome::Available(HashMap::new());
        assert_eq!(
            authorize_lookup(ACC_EXACT, &empty, "proof-location-app", true),
            AuthDecision::Prompt
        );
    }

    #[test]
    fn pending_toggle_denies_the_in_flight_grant() {
        // Captured EXACT, but Location was disabled mid-flight.
        assert_eq!(live_grant(ACC_EXACT, ACC_NONE), ACC_NONE);
        assert_eq!(live_grant(ACC_CITY, ACC_NONE), ACC_NONE);
        assert_eq!(
            auth_reply(live_grant(ACC_EXACT, ACC_NONE)),
            (false, ACC_NONE)
        );
    }

    #[test]
    fn pending_replacement_clamps_the_in_flight_grant() {
        // Captured EXACT, but the ceiling dropped to city mid-flight.
        assert_eq!(live_grant(ACC_EXACT, ACC_CITY), ACC_CITY);
        assert_eq!(live_grant(ACC_COUNTRY, ACC_CITY), ACC_COUNTRY);
        assert_eq!(live_grant(ACC_NONE, ACC_CITY), ACC_NONE);
        assert_eq!(
            auth_reply(live_grant(ACC_EXACT, ACC_CITY)),
            (true, ACC_CITY)
        );
    }

    #[test]
    fn prompt_answer_flows_through_the_live_ceiling() {
        // Allow at the request, clamped to the live ceiling; deny is NONE.
        assert_eq!(live_grant(ACC_EXACT, ACC_EXACT), ACC_EXACT);
        assert_eq!(live_grant(ACC_EXACT, ACC_CITY), ACC_CITY);
        assert_eq!(live_grant(ACC_NONE, ACC_EXACT), ACC_NONE);
    }

    #[test]
    fn clamped_read_refreshes_but_never_downgrades_the_grant() {
        let perms = HashMap::from([(
            "proof-location-app".to_owned(),
            vec!["EXACT".to_owned(), "1700000000".to_owned()],
        )]);
        // A read under a city ceiling replies CITY yet saves EXACT back.
        assert_eq!(
            authorize(ACC_CITY, Some(ACC_EXACT), false, true),
            AuthDecision::Answer {
                accuracy: ACC_CITY,
                save: true
            }
        );
        assert_eq!(
            refresh_level(Some(&perms), "proof-location-app", ACC_CITY),
            ACC_EXACT
        );
        assert_eq!(
            refresh_level(None, "proof-location-app", ACC_CITY),
            ACC_CITY
        );
        assert_eq!(
            refresh_level(Some(&perms), "missing.app", ACC_CITY),
            ACC_CITY
        );
    }
}
