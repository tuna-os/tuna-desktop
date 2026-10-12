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

use crate::services::Tile;

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

/// Decide from the store entry, as GNOME's `AppAuthorizer` does:
/// unknown apps prompt, a missing store authorizes at the requested
/// level, and an unknown desktop id is denied outright.
pub fn authorize(
    req: u32,
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
            accuracy: clamp_accuracy(req, max),
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

/// A store row (`[level, date]`) to its level; a present but garbled
/// row denies, as GNOME's `|| NONE` does.
pub fn stored_accuracy(perms: &HashMap<String, Vec<String>>, desktop_id: &str) -> Option<u32> {
    perms
        .get(desktop_id)
        .and_then(|row| row.first())
        .map(|level| accuracy_value(level))
}

/// The row `Set` writes back: the level word and the decision's date.
pub fn permission_entry(level: u32, unix_secs: u64) -> Vec<String> {
    vec![accuracy_name(level).to_owned(), unix_secs.to_string()]
}

/// One app's row for the Location settings list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPermission {
    pub desktop_id: String,
    pub accuracy: u32,
    /// When the user last decided, if the row says.
    pub seen: Option<u64>,
}

/// The whole `location` table, sorted by id.
pub fn parse_lookup(perms: &HashMap<String, Vec<String>>) -> Vec<AppPermission> {
    let mut apps: Vec<AppPermission> = perms
        .iter()
        .map(|(id, row)| AppPermission {
            desktop_id: id.clone(),
            accuracy: row.first().map(|l| accuracy_value(l)).unwrap_or(ACC_NONE),
            seen: row.get(1).and_then(|d| d.parse().ok()),
        })
        .collect();
    apps.sort_by(|a, b| a.desktop_id.cmp(&b.desktop_id));
    apps
}

/// The panel icon: shown only while location is on and in use.
pub fn indicator_icon(enabled: bool, in_use: bool) -> Option<&'static str> {
    (enabled && in_use).then_some("location-services-active-symbolic")
}

/// The Location tile's (visible, checked): hidden without its
/// GSettings schema, checked with the master switch.
pub fn tile_state(schema_present: bool, enabled: bool) -> (bool, bool) {
    (schema_present, schema_present && enabled)
}

/// The authorization dialog: its app id, requested level, and the
/// answer callback (the user's level, or `NONE`).
pub type AuthorizePrompt = dyn Fn(String, u32, Rc<dyn Fn(u32)>);

/// The GeoClue2 agent: the master switch, the `InUse` indicator and
/// the `AuthorizeApp` dialog hook.
pub struct LocationAgent {
    settings: Option<gio::Settings>,
    indicator: RefCell<Option<gtk::Image>>,
    tile: RefCell<Option<Tile>>,
    conn: RefCell<Option<gio::DBusConnection>>,
    session: RefCell<Option<gio::DBusConnection>>,
    in_use: Cell<bool>,
    prompt: RefCell<Option<Rc<AuthorizePrompt>>>,
}

impl LocationAgent {
    pub fn new() -> Rc<Self> {
        let settings = gio::SettingsSchemaSource::default()
            .and_then(|source| source.lookup(LOCATION_SCHEMA, true))
            .map(|_| gio::Settings::new(LOCATION_SCHEMA));
        let agent = Rc::new(Self {
            settings,
            indicator: RefCell::default(),
            tile: RefCell::default(),
            conn: RefCell::default(),
            session: RefCell::default(),
            in_use: Cell::new(false),
            prompt: RefCell::default(),
        });
        if let Some(settings) = agent.settings.as_ref() {
            let weak = Rc::downgrade(&agent);
            settings.connect_changed(Some(ENABLED_KEY), move |_, _| {
                if let Some(agent) = weak.upgrade() {
                    agent.sync_indicator();
                    agent.emit_max_accuracy();
                }
            });
            let weak = Rc::downgrade(&agent);
            settings.connect_changed(Some(MAX_ACCURACY_KEY), move |_, _| {
                if let Some(agent) = weak.upgrade() {
                    agent.emit_max_accuracy();
                }
            });
        }
        agent
    }

    /// The panel icon that follows `enabled && in_use`.
    pub fn set_indicator(&self, indicator: gtk::Image) {
        *self.indicator.borrow_mut() = Some(indicator);
        self.sync_indicator();
    }

    /// The Location toggle: checked with the master switch, hidden
    /// without its schema. A click flips the switch; writes from
    /// elsewhere (Settings, the proof) update the tile the same way.
    pub fn set_tile(self: &Rc<Self>, tile: Tile) {
        let weak = Rc::downgrade(self);
        tile.on_user_toggle(move |on| {
            let Some(agent) = weak.upgrade() else { return };
            agent.set_enabled(on);
            // The tile shows GSettings' state, not the click.
            agent.sync_tile();
        });
        *self.tile.borrow_mut() = Some(tile);
        if let Some(settings) = self.settings.as_ref() {
            let weak = Rc::downgrade(self);
            settings.connect_changed(Some(ENABLED_KEY), move |_, _| {
                if let Some(agent) = weak.upgrade() {
                    agent.sync_tile();
                }
            });
        }
        self.sync_tile();
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

    /// Flip the master switch (the Location settings toggle).
    pub fn set_enabled(&self, on: bool) {
        if let Some(settings) = self.settings.as_ref() {
            let _ = settings.set_boolean(ENABLED_KEY, on);
        }
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
    /// dialog). The invocation stays open until the answer lands.
    fn authorize_app(
        self: &Rc<Self>,
        desktop_id: &str,
        req: u32,
        invocation: gio::DBusMethodInvocation,
    ) {
        let max = self.max_accuracy_level();
        if max == ACC_NONE {
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
        let desktop_id = desktop_id.to_owned();
        lookup_permissions(self, move |perms| {
            // `None` is a missing store: authorize at the request. A
            // present store with no row for the app prompts instead.
            let stored = perms.as_ref().and_then(|p| stored_accuracy(p, &desktop_id));
            let decision = authorize(req, max, stored, perms.is_none(), app_known);
            match decision {
                AuthDecision::Answer { accuracy, save } => {
                    if save {
                        this.save_permission(perms.as_ref(), &desktop_id, accuracy);
                    }
                    invocation.return_value(Some(&auth_reply(accuracy).to_variant()));
                }
                AuthDecision::Prompt => {
                    let prompt = this.prompt.borrow().clone();
                    match prompt {
                        Some(prompt) => {
                            let this = this.clone();
                            let perms = perms.clone();
                            let id = desktop_id.clone();
                            // The dialog answers later; its own reference.
                            // The hook is repeat-callable, so the first
                            // answer wins and later ones are dropped.
                            let invocation2 = RefCell::new(Some(invocation.clone()));
                            prompt(
                                desktop_id,
                                req,
                                Rc::new(move |level| {
                                    let accuracy = clamp_accuracy(level, max);
                                    this.save_permission(perms.as_ref(), &id, accuracy);
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

    /// The `location` table, or `None` when the store is missing.
    fn lookup_table(
        session: &gio::DBusConnection,
        done: impl FnOnce(Option<HashMap<String, Vec<String>>>) + 'static,
    ) {
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
            move |reply| {
                done(
                    reply
                        .ok()
                        .and_then(|v| v.child_value(0).get::<HashMap<String, Vec<String>>>()),
                );
            },
        );
    }

    /// One row back into the store, stamped with now.
    fn save_permission(
        &self,
        perms: Option<&HashMap<String, Vec<String>>>,
        desktop_id: &str,
        accuracy: u32,
    ) {
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
            let show = self.enabled() && self.in_use.get();
            indicator.set_visible(show);
            if show {
                indicator.set_icon_name(Some("location-services-active-symbolic"));
            }
        }
    }

    fn sync_tile(&self) {
        let Some(tile) = self.tile.borrow().clone() else {
            return;
        };
        let (visible, checked) = tile_state(self.settings.is_some(), self.enabled());
        tile.present(visible);
        tile.show_state(checked, None);
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

/// The `location` table on the caller's session bus, or `None` when
/// the store (or the bus) is missing.
fn lookup_permissions(
    agent: &Rc<LocationAgent>,
    done: impl FnOnce(Option<HashMap<String, Vec<String>>>) + 'static,
) {
    if let Some(session) = agent.session.borrow().clone() {
        LocationAgent::lookup_table(&session, done);
    } else {
        done(None);
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
            authorize(ACC_EXACT, ACC_CITY, Some(ACC_EXACT), false, true),
            AuthDecision::Answer {
                accuracy: ACC_CITY,
                save: true
            }
        );
        // Unknown apps prompt.
        assert_eq!(
            authorize(ACC_EXACT, ACC_EXACT, None, false, true),
            AuthDecision::Prompt
        );
        // A missing store authorizes at the request, unsaved.
        assert_eq!(
            authorize(ACC_CITY, ACC_EXACT, None, true, true),
            AuthDecision::Answer {
                accuracy: ACC_CITY,
                save: false
            }
        );
        // An unknown desktop id is denied outright.
        assert_eq!(
            authorize(ACC_EXACT, ACC_EXACT, Some(ACC_EXACT), false, false),
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
        let apps = parse_lookup(&perms);
        assert_eq!(apps.len(), 3);
        assert!(apps.windows(2).all(|w| w[0].desktop_id <= w[1].desktop_id));
        // Sorted: bad.app, org.gnome.Maps, org.gnome.Weather.
        assert_eq!(apps[0].accuracy, ACC_NONE);
        assert_eq!(apps[1].seen, Some(1_700_000_000));
        assert_eq!(apps[2].seen, None);
    }

    #[test]
    fn tile_hides_without_schema_and_mirrors_switch() {
        assert_eq!(tile_state(false, false), (false, false));
        assert_eq!(tile_state(false, true), (false, false));
        assert_eq!(tile_state(true, false), (true, false));
        assert_eq!(tile_state(true, true), (true, true));
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
}
