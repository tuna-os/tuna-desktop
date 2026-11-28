//! Smartcard tokens as gnome-shell 51's `js/gdm/smartcardManager.js`
//! sees them: gnome-settings-daemon's smartcard plugin on the session bus
//! (`org.gnome.SettingsDaemon.Smartcard`, an ObjectManager at
//! `/org/gnome/SettingsDaemon/Smartcard` whose
//! `org.gnome.SettingsDaemon.Smartcard.Token` objects carry
//! `IsInserted`). Each insertion or removal is reported with whether any
//! token is still inserted (`hasInsertedTokens`); the `login_token`
//! alias is skipped, as GNOME skips it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

pub const NAME: &str = "org.gnome.SettingsDaemon.Smartcard";
pub const PATH: &str = "/org/gnome/SettingsDaemon/Smartcard";
pub const TOKEN: &str = "org.gnome.SettingsDaemon.Smartcard.Token";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// The tokens by object path, inserted or not.
#[derive(Default)]
struct Tokens(BTreeMap<String, bool>);

impl Tokens {
    fn any_inserted(&self) -> bool {
        self.0.values().any(|inserted| *inserted)
    }

    /// Record a token's state; `Some(inserted)` when it changed (a new
    /// token that is inserted counts: GNOME emits `smartcard-inserted`
    /// for a token already in at startup).
    fn set(&mut self, path: &str, inserted: bool) -> Option<bool> {
        if path.ends_with("/login_token") {
            return None;
        }
        let before = self.0.insert(path.to_owned(), inserted);
        (before != Some(inserted) && (before.is_some() || inserted)).then_some(inserted)
    }

    fn remove(&mut self, path: &str) -> Option<bool> {
        self.0
            .remove(path)
            .filter(|inserted| *inserted)
            .map(|_| false)
    }
}

/// `IsInserted` of the Token interface in an `a{sa{sv}}` of interfaces;
/// `None` when the object is not a token.
fn token_inserted(interfaces: &glib::Variant) -> Option<bool> {
    if interfaces.type_().as_str() != "a{sa{sv}}" {
        return None;
    }
    let props = interfaces
        .iter()
        .find(|entry| entry.child_value(0).str() == Some(TOKEN))?
        .child_value(1);
    let props = glib::VariantDict::new(Some(&props));
    Some(
        props
            .lookup::<bool>("IsInserted")
            .ok()
            .flatten()
            .unwrap_or(false),
    )
}

type Changed = Rc<dyn Fn(bool, bool)>;

/// Watches the tokens while alive.
pub struct Watch {
    unwatch: Option<Box<dyn FnOnce()>>,
    _signals: Rc<RefCell<Option<gio::SignalSubscription>>>,
}

/// Call `changed(inserted, any_inserted)` for every token inserted or
/// removed.
pub fn watch(session: &gio::DBusConnection, changed: impl Fn(bool, bool) + 'static) -> Watch {
    let tokens: Rc<RefCell<Tokens>> = Rc::default();
    let changed: Changed = Rc::new(changed);
    let signals: Rc<RefCell<Option<gio::SignalSubscription>>> = Rc::default();
    let report = {
        let (tokens, changed) = (tokens.clone(), changed.clone());
        move |path: &str, inserted: Option<bool>| {
            let event = {
                let mut tokens = tokens.borrow_mut();
                match inserted {
                    Some(inserted) => tokens.set(path, inserted),
                    None => tokens.remove(path),
                }
                .map(|inserted| (inserted, tokens.any_inserted()))
            };
            if let Some((inserted, any)) = event {
                changed(inserted, any);
            }
        }
    };
    let report = Rc::new(report);
    let subscription = {
        let report = report.clone();
        session.subscribe_to_signal(
            Some(NAME),
            None,
            None,
            None,
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| match (signal.interface_name, signal.signal_name) {
                (OBJECT_MANAGER, "InterfacesAdded") => {
                    let params = &signal.parameters;
                    if params.type_().as_str() == "(oa{sa{sv}})" {
                        let path = params.child_value(0);
                        if let (Some(path), Some(inserted)) =
                            (path.str(), token_inserted(&params.child_value(1)))
                        {
                            report(path, Some(inserted));
                        }
                    }
                }
                (OBJECT_MANAGER, "InterfacesRemoved") => {
                    if let Some((path, interfaces)) = signal
                        .parameters
                        .get::<(glib::variant::ObjectPath, Vec<String>)>()
                    {
                        if interfaces.iter().any(|i| i == TOKEN) {
                            report(path.as_str(), None);
                        }
                    }
                }
                (PROPERTIES, "PropertiesChanged") => {
                    if let Some((interface, props, _)) =
                        signal
                            .parameters
                            .get::<(String, glib::VariantDict, Vec<String>)>()
                    {
                        if interface == TOKEN {
                            if let Ok(Some(inserted)) = props.lookup::<bool>("IsInserted") {
                                report(signal.object_path, Some(inserted));
                            }
                        }
                    }
                }
                _ => {}
            },
        )
    };
    *signals.borrow_mut() = Some(subscription);
    let name = {
        let report = report.clone();
        gio::bus_watch_name_on_connection(
            session,
            NAME,
            gio::BusNameWatcherFlags::NONE,
            move |connection, _, _| {
                let report = report.clone();
                glib::spawn_future_local(async move {
                    let Ok(reply) = connection
                        .call_future(
                            Some(NAME),
                            PATH,
                            OBJECT_MANAGER,
                            "GetManagedObjects",
                            None,
                            glib::VariantTy::new("(a{oa{sa{sv}}})").ok(),
                            gio::DBusCallFlags::NONE,
                            5000,
                        )
                        .await
                    else {
                        return;
                    };
                    for object in reply.child_value(0).iter() {
                        let path = object.child_value(0);
                        let Some(inserted) = token_inserted(&object.child_value(1)) else {
                            continue;
                        };
                        if let Some(path) = path.str() {
                            report(path, Some(inserted));
                        }
                    }
                });
            },
            {
                let tokens = tokens.clone();
                let changed = changed.clone();
                move |_, _| {
                    // The daemon went away: its tokens with it.
                    let had = std::mem::take(&mut *tokens.borrow_mut()).any_inserted();
                    if had {
                        changed(false, false);
                    }
                }
            },
        )
    };
    Watch {
        unwatch: Some(Box::new(move || gio::bus_unwatch_name(name))),
        _signals: signals,
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(unwatch) = self.unwatch.take() {
            unwatch();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glib::prelude::*;

    #[test]
    fn managed_objects_name_their_token_state() {
        let props = glib::VariantDict::new(None);
        props.insert("IsInserted", true);
        let token = glib::Variant::from_dict_entry(&TOKEN.to_variant(), &props.end());
        let other = glib::Variant::from_dict_entry(
            &"org.example.Other".to_variant(),
            &glib::VariantDict::new(None).end(),
        );
        let ty = glib::VariantTy::new("{sa{sv}}").unwrap();
        let interfaces = glib::Variant::array_from_iter_with_type(ty, [other.clone(), token]);
        assert_eq!(token_inserted(&interfaces), Some(true));
        let not_token = glib::Variant::array_from_iter_with_type(ty, [other]);
        assert_eq!(token_inserted(&not_token), None);
        assert_eq!(token_inserted(&"x".to_variant()), None);
    }

    #[test]
    fn tokens_report_insertions_like_smartcard_manager() {
        let mut tokens = Tokens::default();
        assert_eq!(
            tokens.set("/t/a", false),
            None,
            "a token not inserted is quiet"
        );
        assert_eq!(tokens.set("/t/a", true), Some(true));
        assert_eq!(tokens.set("/t/a", true), None);
        assert_eq!(
            tokens.set("/t/login_token", true),
            None,
            "the alias is skipped"
        );
        assert!(tokens.any_inserted());
        assert_eq!(
            tokens.set("/t/b", true),
            Some(true),
            "already in at startup"
        );
        assert_eq!(tokens.set("/t/a", false), Some(false));
        assert!(tokens.any_inserted());
        assert_eq!(tokens.remove("/t/b"), Some(false));
        assert!(!tokens.any_inserted());
        assert_eq!(
            tokens.remove("/t/a"),
            None,
            "removing an out token is quiet"
        );
    }
}
