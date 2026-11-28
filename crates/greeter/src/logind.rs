//! What the login dialog reads from logind (`js/misc/loginManager.js`):
//! the sessions (who is logged in, for the user list's ring; the
//! conflicting session of a login, `loginDialog.js`
//! `_findConflictingSession`), the login screen's own session (whether it
//! is remote; it becoming active again resets the prompt,
//! `_loginScreenSessionActivated`) and sessions going away.

use gio::prelude::*;

pub const LOGIND: &str = "org.freedesktop.login1";
pub const LOGIND_PATH: &str = "/org/freedesktop/login1";
pub const MANAGER: &str = "org.freedesktop.login1.Manager";
pub const SESSION: &str = "org.freedesktop.login1.Session";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// One entry of `Manager.ListSessions` (`a(susso)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub id: String,
    pub uid: u32,
    pub user: String,
    pub seat: String,
    pub path: String,
}

/// The session properties the login dialog reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    /// The user's login name (`Name`).
    pub name: String,
    /// `Type`: `wayland`, `x11`, `tty`, …
    pub kind: String,
    /// `Class`: `user`, `greeter`, …
    pub class: String,
    /// `State`: `online`, `active`, `closing`.
    pub state: String,
    pub remote: bool,
    pub active: bool,
}

impl Session {
    fn from_properties(props: &glib::VariantDict) -> Self {
        let text = |key: &str| {
            props
                .lookup::<String>(key)
                .ok()
                .flatten()
                .unwrap_or_default()
        };
        let flag = |key: &str| props.lookup::<bool>(key).ok().flatten().unwrap_or(false);
        Self {
            id: text("Id"),
            name: text("Name"),
            kind: text("Type"),
            class: text("Class"),
            state: text("State"),
            remote: flag("Remote"),
            active: flag("Active"),
        }
    }

    /// `_findConflictingSession`: a graphical session that is active or
    /// online.
    pub fn graphical_and_running(&self) -> bool {
        matches!(self.kind.as_str(), "wayland" | "x11")
            && matches!(self.state.as_str(), "active" | "online")
    }

    /// libaccountsservice's logged-in count: the user's sessions, not the
    /// login screen's and not one closing.
    pub fn logged_in(&self) -> bool {
        self.class == "user" && self.state != "closing"
    }
}

async fn call(
    system: &gio::DBusConnection,
    path: &str,
    interface: &str,
    method: &str,
    args: Option<glib::Variant>,
    reply: &str,
) -> Result<glib::Variant, glib::Error> {
    system
        .call_future(
            Some(LOGIND),
            path,
            interface,
            method,
            args.as_ref(),
            glib::VariantTy::new(reply).ok(),
            gio::DBusCallFlags::NONE,
            5000,
        )
        .await
}

pub async fn list(system: &gio::DBusConnection) -> Result<Vec<Listed>, glib::Error> {
    let reply = call(
        system,
        LOGIND_PATH,
        MANAGER,
        "ListSessions",
        None,
        "(a(susso))",
    )
    .await?;
    Ok(reply
        .child_value(0)
        .iter()
        .filter_map(|entry| {
            let (id, uid, user, seat, path) =
                entry.get::<(String, u32, String, String, glib::variant::ObjectPath)>()?;
            Some(Listed {
                id,
                uid,
                user,
                seat,
                path: path.as_str().to_owned(),
            })
        })
        .collect())
}

pub async fn session(system: &gio::DBusConnection, path: &str) -> Result<Session, glib::Error> {
    let reply = call(
        system,
        path,
        PROPERTIES,
        "GetAll",
        Some((SESSION,).to_variant()),
        "(a{sv})",
    )
    .await?;
    Ok(Session::from_properties(&glib::VariantDict::new(Some(
        &reply.child_value(0),
    ))))
}

/// The login names with a session (libaccountsservice
/// `act_user_is_logged_in`).
pub async fn logged_in_users(system: &gio::DBusConnection) -> Vec<String> {
    let Ok(listed) = list(system).await else {
        return Vec::new();
    };
    let mut users = Vec::new();
    for entry in listed {
        if users.contains(&entry.user) {
            continue;
        }
        if session(system, &entry.path)
            .await
            .is_ok_and(|s| s.logged_in())
        {
            users.push(entry.user);
        }
    }
    users
}

/// `loginManager.getCurrentSessionProxy`: `XDG_SESSION_ID`, else the
/// session of this process (as libgdm finds the greeter's).
pub async fn own_session_path(system: &gio::DBusConnection) -> Option<String> {
    let reply = match std::env::var("XDG_SESSION_ID")
        .ok()
        .filter(|id| !id.is_empty())
    {
        Some(id) => {
            call(
                system,
                LOGIND_PATH,
                MANAGER,
                "GetSession",
                Some((id,).to_variant()),
                "(o)",
            )
            .await
        }
        None => {
            call(
                system,
                LOGIND_PATH,
                MANAGER,
                "GetSessionByPID",
                Some((std::process::id(),).to_variant()),
                "(o)",
            )
            .await
        }
    };
    reply
        .ok()
        .and_then(|r| r.child_value(0).str().map(str::to_owned))
}

/// loginDialog.js `_findConflictingSession`: another graphical, running
/// session of the user whose session `starting` just opened.
pub async fn conflicting_session(
    system: &gio::DBusConnection,
    starting: &str,
) -> Result<Option<Session>, glib::Error> {
    let listed = list(system).await?;
    let Some(owner) = listed
        .iter()
        .find(|s| s.id == starting)
        .map(|s| s.user.clone())
    else {
        return Err(glib::Error::new(
            gio::IOErrorEnum::NotFound,
            &format!("no session {starting}"),
        ));
    };
    for entry in listed
        .iter()
        .filter(|s| s.id != starting && s.user == owner)
    {
        let session = session(system, &entry.path).await?;
        if session.graphical_and_running() {
            return Ok(Some(session));
        }
    }
    Ok(None)
}

/// logind's news the login dialog follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum News {
    SessionNew,
    SessionRemoved(String),
    /// A session object's properties changed (its path).
    Changed(String),
}

/// Follow `SessionNew`, `SessionRemoved` and session property changes.
pub fn watch(
    system: &gio::DBusConnection,
    news: impl Fn(News) + 'static,
) -> gio::SignalSubscription {
    system.subscribe_to_signal(
        Some(LOGIND),
        None,
        None,
        None,
        None,
        gio::DBusSignalFlags::NONE,
        move |signal| {
            let event = match (signal.interface_name, signal.signal_name) {
                (MANAGER, "SessionNew") => Some(News::SessionNew),
                (MANAGER, "SessionRemoved") => signal
                    .parameters
                    .get::<(String, glib::variant::ObjectPath)>()
                    .map(|(id, _)| News::SessionRemoved(id)),
                (PROPERTIES, "PropertiesChanged") => {
                    Some(News::Changed(signal.object_path.to_owned()))
                }
                _ => None,
            };
            if let Some(event) = event {
                news(event);
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_follow_gnome_rules() {
        let dict = glib::VariantDict::new(None);
        dict.insert("Id", "5");
        dict.insert("Name", "ada");
        dict.insert("Type", "wayland");
        dict.insert("Class", "user");
        dict.insert("State", "online");
        dict.insert("Remote", true);
        let s = Session::from_properties(&dict);
        assert!(s.graphical_and_running() && s.logged_in() && s.remote);
        let tty = Session {
            kind: "tty".into(),
            ..s.clone()
        };
        assert!(!tty.graphical_and_running());
        let greeter = Session {
            class: "greeter".into(),
            ..s.clone()
        };
        assert!(!greeter.logged_in());
        let closing = Session {
            state: "closing".into(),
            ..s
        };
        assert!(!closing.logged_in() && !closing.graphical_and_running());
    }
}
