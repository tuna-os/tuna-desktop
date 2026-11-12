//! GNOME's shortcuts-inhibitor consent and PermissionStore vocabulary.
//! Unknown/window-backed apps are deliberately not remembered.
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use glib::variant::ToVariant;
use gtk::prelude::*;
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

const BUS: &str = "org.freedesktop.impl.portal.PermissionStore";
const PATH: &str = "/org/freedesktop/impl/portal/PermissionStore";
const TABLE: &str = "gnome";
const ID: &str = "shortcuts-inhibitor";

type Answer = Rc<dyn Fn(u64, bool)>;

/// What the user did with the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Allow,
    Deny,
    /// GNOME binds Escape to the Deny button, so it is remembered too.
    Escape,
    /// The window went away without a button: this request only.
    Close,
}

impl Choice {
    /// Whether the request is allowed, and whether the verdict is saved.
    fn response(self) -> (bool, bool) {
        match self {
            Self::Allow => (true, true),
            Self::Deny | Self::Escape => (false, true),
            Self::Close => (false, false),
        }
    }
}

/// A PermissionStore verdict for one app (`Lookup`'s `a{sas}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stored {
    Granted,
    Denied,
    /// No usable entry, or no usable store: open the prompt.
    Ask,
}

/// `Lookup`'s reply: the table's permissions and the entry's data.
const LOOKUP_REPLY: &str = "(a{sas}v)";

/// Read `app_id`'s verdict from a `Lookup` reply (`None`: the call
/// failed). Only an exact first `GRANTED` grants; anything absent,
/// malformed or unknown asks rather than deciding. The shape is checked
/// first: glib's map conversion panics on non-dictionary containers.
fn stored_decision(reply: Option<&glib::Variant>, app_id: &str) -> Stored {
    let first = reply
        .filter(|v| v.type_().as_str() == LOOKUP_REPLY)
        .and_then(|v| v.try_child_value(0))
        .and_then(|v| v.get::<HashMap<String, Vec<String>>>())
        .and_then(|p| p.get(app_id).and_then(|v| v.first()).cloned());
    match first.as_deref() {
        Some("GRANTED") => Stored::Granted,
        Some("DENIED") => Stored::Denied,
        _ => Stored::Ask,
    }
}

/// `SetPermission(table, create, id, app, permissions)` for a verdict.
fn set_permission_args(app_id: &str, allow: bool) -> glib::Variant {
    let grant = if allow { "GRANTED" } else { "DENIED" };
    (TABLE, true, ID, app_id, vec![grant]).to_variant()
}

/// The desktop id the compositor's app id names.
fn desktop_id(app: &str) -> String {
    if app.ends_with(".desktop") {
        app.to_owned()
    } else {
        format!("{app}.desktop")
    }
}

/// The app's stable desktop id (only when a desktop file backs it, the
/// first in `dirs`' precedence, NoDisplay ones included) and its name.
/// Ids with a path separator never resolve.
fn identify(app: &str, dirs: &[PathBuf]) -> (Option<String>, Option<String>) {
    let desktop_id = desktop_id(app);
    let stem = desktop_id.trim_end_matches(".desktop");
    let desktop = (!stem.contains('/'))
        .then(|| {
            dirs.iter()
                .map(|dir| dir.join(&desktop_id))
                .find(|path| path.is_file())
        })
        .flatten();
    let name = desktop
        .as_deref()
        .and_then(tuna_shell_host::apps::handler_entry_from_file)
        .map(|entry| entry.name);
    let known = desktop.is_some();
    let name = name.or_else(|| known.then(|| desktop_id.clone()));
    (known.then_some(desktop_id), name)
}

/// The prompt's question.
fn title(name: Option<&str>) -> String {
    name.map(|n| format!("Allow {n} to inhibit shortcuts?"))
        .unwrap_or_else(|| "Allow this app to inhibit shortcuts?".into())
}

/// An answer to send, once.
#[derive(Debug, PartialEq, Eq)]
struct Answered {
    id: u64,
    allow: bool,
    /// The app id to save the verdict for.
    store: Option<String>,
}

/// The one request on screen: its id and stable app id.
#[derive(Debug, Default)]
struct Pending(Option<(u64, Option<String>)>);

impl Pending {
    fn show(&mut self, id: u64, app: Option<String>) {
        self.0 = Some((id, app));
    }

    fn clear(&mut self) {
        self.0 = None;
    }

    fn is(&self, id: u64) -> bool {
        self.0.as_ref().is_some_and(|(pending, _)| *pending == id)
    }

    /// Answer the request at most once; only known apps are saved.
    fn answer(&mut self, allow: bool, remember: bool) -> Option<Answered> {
        let (id, app) = self.0.take()?;
        Some(Answered {
            id,
            allow,
            store: app.filter(|_| remember),
        })
    }
}

pub struct Consent {
    window: gtk::Window,
    title: gtk::Label,
    pending: RefCell<Pending>,
    answer: Answer,
}

impl Consent {
    pub fn new(app: &gtk::Application, answer: Answer) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.set_title(Some("Allow inhibiting shortcuts"));
        window.add_css_class("tuna-end-session");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::SHORTCUT_CONSENT_NAMESPACE));
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
        let hint = gtk::Label::new(Some("You can restore shortcuts by pressing Super+Escape"));
        hint.add_css_class("message-dialog-description");
        hint.set_wrap(true);
        card.append(&hint);
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
            pending: RefCell::default(),
            answer,
        });
        let weak = Rc::downgrade(&ui);
        deny.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.choose(Choice::Deny);
            }
        });
        let weak = Rc::downgrade(&ui);
        allow.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.choose(Choice::Allow);
            }
        });
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&ui);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(ui) = weak.upgrade() {
                    ui.choose(Choice::Escape);
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
                ui.choose(Choice::Close);
            }
            glib::Propagation::Stop
        });
        ui
    }

    pub fn dismiss(&self) {
        self.pending.borrow_mut().clear();
        self.window.set_visible(false);
    }

    pub fn sync(self: &Rc<Self>, request: Option<(u64, String)>) {
        self.dismiss();
        let Some((id, app)) = request else {
            return;
        };
        // Consent also identifies NoDisplay apps, which catalog discovery
        // excludes. Resolve the first desktop file in XDG precedence order.
        let (stable, name) = identify(&app, &tuna_shell_host::apps::default_app_dirs());
        self.title.set_text(&title(name.as_deref()));
        self.pending.borrow_mut().show(id, stable.clone());
        let Some(app_id) = stable else {
            self.window.present();
            return;
        };
        // Both bus acquisition and calls are asynchronous and bounded;
        // missing/broken PermissionStore opens the prompt, never grants.
        let weak = Rc::downgrade(self);
        gio::bus_get(
            gio::BusType::Session,
            gio::Cancellable::NONE,
            move |result| {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                if !ui.is_pending(id) {
                    return;
                }
                let Ok(conn) = result else {
                    ui.window.present();
                    return;
                };
                let weak = Rc::downgrade(&ui);
                conn.call(
                    Some(BUS),
                    PATH,
                    BUS,
                    "Lookup",
                    Some(&(TABLE, ID).to_variant()),
                    glib::VariantTy::new(LOOKUP_REPLY).ok(),
                    gio::DBusCallFlags::NONE,
                    2000,
                    gio::Cancellable::NONE,
                    move |result| {
                        let Some(ui) = weak.upgrade() else {
                            return;
                        };
                        if !ui.is_pending(id) {
                            return;
                        }
                        match stored_decision(result.ok().as_ref(), &app_id) {
                            Stored::Granted => ui.respond(true, false),
                            Stored::Denied => ui.respond(false, false),
                            Stored::Ask => ui.window.present(),
                        }
                    },
                );
            },
        );
    }

    fn is_pending(&self, id: u64) -> bool {
        self.pending.borrow().is(id)
    }

    fn choose(&self, choice: Choice) {
        let (allow, remember) = choice.response();
        self.respond(allow, remember);
    }

    fn respond(&self, allow: bool, remember: bool) {
        let Some(Answered { id, allow, store }) = self.pending.borrow_mut().answer(allow, remember)
        else {
            return;
        };
        (self.answer)(id, allow);
        self.window.set_visible(false);
        if let Some(app) = store {
            gio::bus_get(
                gio::BusType::Session,
                gio::Cancellable::NONE,
                move |result| {
                    let Ok(conn) = result else {
                        return;
                    };
                    conn.call(
                        Some(BUS),
                        PATH,
                        BUS,
                        "SetPermission",
                        Some(&set_permission_args(&app, allow)),
                        None,
                        gio::DBusCallFlags::NONE,
                        2000,
                        gio::Cancellable::NONE,
                        |_| {},
                    );
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup_reply(entries: &[(&str, &[&str])]) -> glib::Variant {
        let table: HashMap<String, Vec<String>> = entries
            .iter()
            .map(|(app, perms)| {
                (
                    (*app).to_owned(),
                    perms.iter().map(|p| (*p).to_owned()).collect(),
                )
            })
            .collect();
        // Lookup returns (a{sas}v): permissions and the entry's data.
        glib::Variant::tuple_from_iter([
            table.to_variant(),
            glib::Variant::from_variant(&0u8.to_variant()),
        ])
    }

    #[test]
    fn buttons_escape_and_close_map_to_gnome_responses() {
        // inhibitShortcutsDialog.js: Allow saves GRANTED, Deny (and its
        // Escape key) saves DENIED; a dialog closed without a button
        // denies just this request.
        assert_eq!(Choice::Allow.response(), (true, true));
        assert_eq!(Choice::Deny.response(), (false, true));
        assert_eq!(Choice::Escape.response(), Choice::Deny.response());
        assert_eq!(Choice::Close.response(), (false, false));
    }

    #[test]
    fn verdicts_are_written_for_the_app_to_gnomes_table() {
        let allow = set_permission_args("org.example.Vm.desktop", true);
        assert_eq!(allow.type_().as_str(), "(sbssas)");
        let (table, create, id, app, perms) = allow
            .get::<(String, bool, String, String, Vec<String>)>()
            .unwrap();
        assert_eq!(
            (table.as_str(), create, id.as_str(), app.as_str()),
            (
                "gnome",
                true,
                "shortcuts-inhibitor",
                "org.example.Vm.desktop"
            )
        );
        assert_eq!(perms, ["GRANTED"]);
        let deny = set_permission_args("org.example.Vm.desktop", false);
        let (.., perms) = deny
            .get::<(String, bool, String, String, Vec<String>)>()
            .unwrap();
        assert_eq!(perms, ["DENIED"]);
        // A written verdict reads back as the same decision.
        assert_eq!(
            stored_decision(
                Some(&lookup_reply(&[("a.desktop", &["GRANTED"])])),
                "a.desktop"
            ),
            Stored::Granted
        );
    }

    #[test]
    fn only_an_exact_stored_grant_skips_the_prompt() {
        let reply = lookup_reply(&[
            ("granted.desktop", &["GRANTED"]),
            ("denied.desktop", &["DENIED", "GRANTED"]),
            ("empty.desktop", &[]),
            ("lower.desktop", &["granted"]),
            ("odd.desktop", &["yes"]),
        ]);
        assert_eq!(
            stored_decision(Some(&reply), "granted.desktop"),
            Stored::Granted
        );
        // The first entry decides.
        assert_eq!(
            stored_decision(Some(&reply), "denied.desktop"),
            Stored::Denied
        );
        for app in [
            "empty.desktop",
            "lower.desktop",
            "odd.desktop",
            "absent.desktop",
        ] {
            assert_eq!(stored_decision(Some(&reply), app), Stored::Ask, "{app}");
        }
        // Another app's grant is not this app's.
        assert_eq!(stored_decision(Some(&reply), "granted"), Stored::Ask);
    }

    #[test]
    fn missing_or_malformed_store_data_asks_and_never_grants() {
        // The call failed (no PermissionStore, timeout, no table yet).
        assert_eq!(stored_decision(None, "a.desktop"), Stored::Ask);
        // Wrong shapes from a broken or hostile store.
        let malformed = [
            ("GRANTED",).to_variant(),
            glib::Variant::tuple_from_iter(std::iter::empty::<glib::Variant>()),
            // Containers glib's map conversion would index blindly.
            (vec!["GRANTED"],).to_variant(),
            (vec![("a.desktop", "GRANTED")],).to_variant(),
            (HashMap::from([(
                "a.desktop".to_owned(),
                vec!["GRANTED".to_owned()],
            )]),)
                .to_variant(),
            (HashMap::from([(
                "a.desktop".to_owned(),
                "GRANTED".to_owned(),
            )]),)
                .to_variant(),
            "GRANTED".to_variant(),
        ];
        for reply in &malformed {
            assert_eq!(
                stored_decision(Some(reply), "a.desktop"),
                Stored::Ask,
                "{reply}"
            );
        }
    }

    #[test]
    fn each_request_is_answered_once_and_superseded_ones_never() {
        let mut pending = Pending::default();
        assert_eq!(pending.answer(true, true), None, "nothing to answer");
        pending.show(1, Some("one.desktop".into()));
        // The compositor moved on to request 2 before 1 was answered:
        // a late PermissionStore verdict for 1 must not answer 2.
        pending.show(2, Some("two.desktop".into()));
        assert!(!pending.is(1));
        assert!(pending.is(2));
        assert_eq!(
            pending.answer(true, true),
            Some(Answered {
                id: 2,
                allow: true,
                store: Some("two.desktop".into()),
            })
        );
        // A second click, Escape or close after the answer is a no-op.
        assert!(!pending.is(2));
        assert_eq!(pending.answer(false, true), None);

        // Requests answered in the order they are shown.
        let mut answered = Vec::new();
        for id in 3..6 {
            pending.show(id, None);
            answered.extend(pending.answer(id % 2 == 0, true).map(|a| a.id));
        }
        assert_eq!(answered, [3, 4, 5]);

        // Dismissal (lock) drops the request without answering it.
        pending.show(6, Some("six.desktop".into()));
        pending.clear();
        assert_eq!(pending.answer(true, true), None);
    }

    #[test]
    fn only_remembered_answers_for_known_apps_are_saved() {
        let mut pending = Pending::default();
        pending.show(1, Some("app.desktop".into()));
        let (allow, remember) = Choice::Close.response();
        assert_eq!(pending.answer(allow, remember).unwrap().store, None);
        // A stored verdict replays without rewriting the store.
        pending.show(2, Some("app.desktop".into()));
        assert_eq!(pending.answer(true, false).unwrap().store, None);
        // Window-backed / unknown apps are never remembered.
        pending.show(3, None);
        let (allow, remember) = Choice::Deny.response();
        let answered = pending.answer(allow, remember).unwrap();
        assert!(!answered.allow);
        assert_eq!(answered.store, None);
        pending.show(4, Some("app.desktop".into()));
        let (allow, remember) = Choice::Escape.response();
        assert_eq!(
            pending.answer(allow, remember),
            Some(Answered {
                id: 4,
                allow: false,
                store: Some("app.desktop".into()),
            })
        );
    }

    #[test]
    fn apps_are_identified_by_their_first_desktop_file_only() {
        let user = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        let entry = |dir: &std::path::Path, file: &str, name: &str, extra: &str| {
            std::fs::write(
                dir.join(file),
                format!("[Desktop Entry]\nType=Application\nName={name}\nExec=true\n{extra}"),
            )
            .unwrap();
        };
        entry(system.path(), "org.example.Vm.desktop", "System VM", "");
        entry(user.path(), "org.example.Vm.desktop", "User VM", "");
        entry(
            system.path(),
            "org.example.Hidden.desktop",
            "Hidden Tool",
            "NoDisplay=true\n",
        );
        let dirs = [user.path().to_path_buf(), system.path().to_path_buf()];

        // XDG precedence: the user's file wins; ".desktop" is optional.
        let (id, name) = identify("org.example.Vm", &dirs);
        assert_eq!(id.as_deref(), Some("org.example.Vm.desktop"));
        assert_eq!(name.as_deref(), Some("User VM"));
        assert_eq!(identify("org.example.Vm.desktop", &dirs).0, id);
        assert_eq!(
            title(name.as_deref()),
            "Allow User VM to inhibit shortcuts?"
        );
        // NoDisplay apps are still apps.
        assert_eq!(
            identify("org.example.Hidden", &dirs).0.as_deref(),
            Some("org.example.Hidden.desktop")
        );
        // Unknown, empty and path-like ids stay anonymous (never saved).
        std::fs::create_dir(system.path().join("sub")).unwrap();
        entry(system.path(), "sub/Evil.desktop", "Evil", "");
        for app in [
            "org.example.Missing",
            "",
            "sub/Evil",
            "../applications/org.example.Vm",
        ] {
            assert_eq!(identify(app, &dirs), (None, None), "{app:?}");
        }
        assert_eq!(title(None), "Allow this app to inhibit shortcuts?");
        // An unparseable desktop file still identifies the app by id.
        std::fs::write(
            system.path().join("org.example.Broken.desktop"),
            b"\xff\xfe",
        )
        .unwrap();
        assert_eq!(
            identify("org.example.Broken", &dirs),
            (
                Some("org.example.Broken.desktop".into()),
                Some("org.example.Broken.desktop".into())
            )
        );
    }
}
