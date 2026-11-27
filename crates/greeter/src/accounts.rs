//! The login screen's user list from AccountsService, as gnome-shell 51's
//! `UserList` fills it through libaccountsservice (`loginDialog.js`
//! `addUser`): cached users that are loaded, not system accounts, not
//! locked and named.

use gio::prelude::*;

pub const ACCOUNTS_NAME: &str = "org.freedesktop.Accounts";
pub const ACCOUNTS_PATH: &str = "/org/freedesktop/Accounts";
pub const ACCOUNTS_INTERFACE: &str = "org.freedesktop.Accounts";
pub const USER_INTERFACE: &str = "org.freedesktop.Accounts.User";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct User {
    pub path: String,
    pub name: String,
    pub real_name: String,
    pub icon_file: String,
    pub system_account: bool,
    pub locked: bool,
}

impl User {
    /// `loginDialog.js` `UserList.addUser`: who the list shows.
    pub fn listed(&self) -> bool {
        !self.name.is_empty() && !self.system_account && !self.locked
    }

    /// The name the list shows (`UserWidget`): the real name, else the
    /// login name.
    pub fn display_name(&self) -> &str {
        if self.real_name.trim().is_empty() {
            &self.name
        } else {
            &self.real_name
        }
    }

    /// The avatar file, when it is a readable file.
    pub fn icon(&self) -> Option<&str> {
        (!self.icon_file.is_empty() && std::path::Path::new(&self.icon_file).is_file())
            .then_some(self.icon_file.as_str())
    }

    /// A user from `Properties.GetAll` of [`USER_INTERFACE`].
    pub fn from_properties(path: &str, props: &glib::VariantDict) -> Self {
        let string = |key: &str| {
            props
                .lookup::<String>(key)
                .ok()
                .flatten()
                .unwrap_or_default()
        };
        let boolean = |key: &str| props.lookup::<bool>(key).ok().flatten().unwrap_or(false);
        Self {
            path: path.to_owned(),
            name: string("UserName"),
            real_name: string("RealName"),
            icon_file: string("IconFile"),
            system_account: boolean("SystemAccount"),
            locked: boolean("Locked"),
        }
    }
}

/// The users the login screen lists, in AccountsService's order.
pub async fn load(system: &gio::DBusConnection) -> Result<Vec<User>, glib::Error> {
    let reply = system
        .call_future(
            Some(ACCOUNTS_NAME),
            ACCOUNTS_PATH,
            ACCOUNTS_INTERFACE,
            "ListCachedUsers",
            None,
            Some(glib::VariantTy::new("(ao)").unwrap()),
            gio::DBusCallFlags::NONE,
            -1,
        )
        .await?;
    let paths: Vec<glib::variant::ObjectPath> = reply
        .child_value(0)
        .get::<Vec<glib::variant::ObjectPath>>()
        .unwrap_or_default();
    let mut users = Vec::new();
    for path in paths {
        let props = system
            .call_future(
                Some(ACCOUNTS_NAME),
                path.as_str(),
                "org.freedesktop.DBus.Properties",
                "GetAll",
                Some(&(USER_INTERFACE,).to_variant()),
                Some(glib::VariantTy::new("(a{sv})").unwrap()),
                gio::DBusCallFlags::NONE,
                -1,
            )
            .await;
        match props {
            Ok(props) => {
                let dict = glib::VariantDict::new(Some(&props.child_value(0)));
                let user = User::from_properties(path.as_str(), &dict);
                if user.listed() {
                    users.push(user);
                }
            }
            Err(error) => eprintln!("tuna-greeter: user {}: {}", path.as_str(), error.message()),
        }
    }
    Ok(users)
}

/// The AccountsService signals that change who the list shows
/// (libaccountsservice's `user-added`/`user-removed`, which gnome-shell's
/// `UserList` follows).
pub fn changes_the_list(signal_name: &str) -> bool {
    matches!(signal_name, "UserAdded" | "UserDeleted")
}

/// Call `changed` whenever AccountsService adds or removes a user.
pub fn watch(
    system: &gio::DBusConnection,
    changed: impl Fn() + 'static,
) -> gio::SignalSubscription {
    system.subscribe_to_signal(
        Some(ACCOUNTS_NAME),
        Some(ACCOUNTS_INTERFACE),
        None,
        Some(ACCOUNTS_PATH),
        None,
        gio::DBusSignalFlags::NONE,
        move |signal| {
            if changes_the_list(signal.signal_name) {
                changed();
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(entries: &[(&str, glib::Variant)]) -> glib::VariantDict {
        let dict = glib::VariantDict::new(None);
        for (key, value) in entries {
            dict.insert_value(key, value);
        }
        dict
    }

    #[test]
    fn properties_make_a_user_and_the_list_follows_gnome() {
        let user = User::from_properties(
            "/org/freedesktop/Accounts/User1000",
            &dict(&[
                ("UserName", "alice".to_variant()),
                ("RealName", "Alice Liddell".to_variant()),
                ("IconFile", "/nonexistent/icon".to_variant()),
                ("SystemAccount", false.to_variant()),
                ("Locked", false.to_variant()),
            ]),
        );
        assert_eq!(user.name, "alice");
        assert_eq!(user.display_name(), "Alice Liddell");
        assert!(user.listed());
        assert_eq!(
            user.icon(),
            None,
            "a missing avatar file is the default icon"
        );
        let system = User {
            system_account: true,
            ..user.clone()
        };
        assert!(!system.listed());
        let locked = User {
            locked: true,
            ..user.clone()
        };
        assert!(!locked.listed());
        let unnamed = User {
            name: String::new(),
            ..user.clone()
        };
        assert!(!unnamed.listed());
        let no_real_name = User {
            real_name: " ".into(),
            ..user
        };
        assert_eq!(no_real_name.display_name(), "alice");
    }

    #[test]
    fn missing_or_mistyped_properties_default() {
        let user = User::from_properties("/u", &dict(&[("UserName", 5u32.to_variant())]));
        assert_eq!(user.name, "");
        assert!(!user.system_account);
        assert!(!user.listed());
    }

    #[test]
    fn a_readable_avatar_file_is_the_users_image() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ada.png");
        std::fs::write(&file, b"png").unwrap();
        let path = file.to_str().unwrap().to_owned();
        let user = User {
            name: "ada".into(),
            icon_file: path.clone(),
            ..User::default()
        };
        assert_eq!(user.icon(), Some(path.as_str()));
        let directory = User {
            icon_file: dir.path().to_str().unwrap().to_owned(),
            ..user.clone()
        };
        assert_eq!(directory.icon(), None, "a directory is no avatar");
        let unset = User {
            icon_file: String::new(),
            ..user
        };
        assert_eq!(unset.icon(), None, "no IconFile is the default icon");
    }

    #[test]
    fn only_added_and_deleted_users_change_the_list() {
        assert!(changes_the_list("UserAdded"));
        assert!(changes_the_list("UserDeleted"));
        for other in ["UserChanged", "Changed", "PropertiesChanged", ""] {
            assert!(!changes_the_list(other), "{other}");
        }
    }
}
