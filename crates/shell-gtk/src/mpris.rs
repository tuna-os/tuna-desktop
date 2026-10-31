//! GNOME 51's MPRIS client (js/ui/mpris.js): every
//! `org.mpris.MediaPlayer2.*` name on the session bus becomes a player,
//! shown once it says `CanPlay`, newest first. The lock screen's
//! `NotificationsBox` lists them as `MediaMessage`s (js/ui/messageList.js)
//! when the player's notification policy allows it on the lock screen.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use tuna_shell_host::notification_policy::{GioPolicyStore, PolicyStore};

use crate::lock_notifications::{MediaAction, MediaEntry};

/// `MPRIS_PLAYER_PREFIX`.
const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// What a player's `Metadata` says about its track, validated as
/// `MprisPlayer._updateState` does: anything of the wrong type is GNOME's
/// "Unknown artist" / "Unknown title" / no cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub title: String,
    pub artists: Vec<String>,
    pub cover_url: String,
}

pub fn track(metadata: Option<&glib::Variant>) -> Track {
    let lookup = |key: &str| {
        metadata
            .filter(|m| m.type_().as_str() == "a{sv}")
            .and_then(|m| glib::VariantDict::new(Some(m)).lookup_value(key, None))
    };
    let artists = lookup("xesam:artist")
        .and_then(|v| v.get::<Vec<String>>())
        .unwrap_or_else(|| vec!["Unknown artist".to_owned()]);
    let title = lookup("xesam:title")
        .and_then(|v| v.get::<String>())
        .unwrap_or_else(|| "Unknown title".to_owned());
    let cover_url = lookup("mpris:artUrl")
        .and_then(|v| v.get::<String>())
        .unwrap_or_default();
    Track {
        title,
        artists,
        cover_url,
    }
}

struct Player {
    /// Insertion order: newer players sit above older ones
    /// (`insert_child_at_index(message, 0)`).
    serial: u64,
    root: Option<gio::DBusProxy>,
    player: Option<gio::DBusProxy>,
}

pub struct Mpris {
    players: RefCell<HashMap<String, Player>>,
    serial: Cell<u64>,
    changed: RefCell<Option<Rc<dyn Fn()>>>,
    policy: GioPolicyStore,
    /// The notification settings a shown player's policy reads, watched
    /// so a change applies at once (`notify::enable`,
    /// `notify::show-in-lock-screen`); `""` is the global schema.
    watched: RefCell<HashMap<String, gio::Settings>>,
}

impl Mpris {
    /// Follow the session bus's media players (`MprisSource`).
    pub fn start() -> Rc<Self> {
        let mpris = Rc::new(Self {
            players: RefCell::default(),
            serial: Cell::new(0),
            changed: RefCell::new(None),
            policy: GioPolicyStore::new(),
            watched: RefCell::default(),
        });
        let weak = Rc::downgrade(&mpris);
        gio::bus_get(
            gio::BusType::Session,
            None::<&gio::Cancellable>,
            move |conn| match conn {
                Ok(conn) => {
                    if let Some(mpris) = weak.upgrade() {
                        mpris.attach(&conn);
                    }
                }
                Err(error) => eprintln!("tuna-shell-gtk: MPRIS players unavailable: {error}"),
            },
        );
        mpris
    }

    /// Call `f` whenever what the players show may have changed.
    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        *self.changed.borrow_mut() = Some(Rc::new(f));
    }

    fn emit(&self) {
        let f = self.changed.borrow().clone();
        if let Some(f) = f {
            f();
        }
    }

    fn attach(self: &Rc<Self>, conn: &gio::DBusConnection) {
        let weak = Rc::downgrade(self);
        let sub = conn.subscribe_to_signal(
            Some("org.freedesktop.DBus"),
            Some("org.freedesktop.DBus"),
            Some("NameOwnerChanged"),
            Some("/org/freedesktop/DBus"),
            Some(ROOT),
            gio::DBusSignalFlags::MATCH_ARG0_NAMESPACE,
            move |signal| {
                let Some(mpris) = weak.upgrade() else {
                    return;
                };
                let Some((name, old, new)) = signal.parameters.get::<(String, String, String)>()
                else {
                    return;
                };
                mpris.owner_changed(signal.connection, &name, !old.is_empty(), !new.is_empty());
            },
        );
        // The subscription lives as long as the shell.
        std::mem::forget(sub);
        let weak = Rc::downgrade(self);
        let list_conn = conn.clone();
        conn.call(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "ListNames",
            None,
            Some(glib::VariantTy::new("(as)").expect("valid type")),
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
            move |reply| {
                let (Some(mpris), Ok(reply)) = (weak.upgrade(), reply) else {
                    return;
                };
                let Some((names,)) = reply.get::<(Vec<String>,)>() else {
                    return;
                };
                for name in names.iter().filter(|n| n.starts_with(PREFIX)) {
                    mpris.add(&list_conn, name);
                }
            },
        );
    }

    /// `_onNameOwnerChanged`.
    fn owner_changed(
        self: &Rc<Self>,
        conn: &gio::DBusConnection,
        name: &str,
        old: bool,
        new: bool,
    ) {
        if !name.starts_with(PREFIX) {
            return;
        }
        if old && self.players.borrow_mut().remove(name).is_some() {
            self.emit();
        }
        if new {
            self.add(conn, name);
        }
    }

    /// `_addPlayer`: both proxies load asynchronously, as GNOME's do.
    fn add(self: &Rc<Self>, conn: &gio::DBusConnection, name: &str) {
        if self.players.borrow().contains_key(name) {
            return;
        }
        let serial = self.serial.get() + 1;
        self.serial.set(serial);
        self.players.borrow_mut().insert(
            name.to_owned(),
            Player {
                serial,
                root: None,
                player: None,
            },
        );
        for interface in [ROOT, PLAYER] {
            let weak = Rc::downgrade(self);
            let name_owned = name.to_owned();
            gio::DBusProxy::new(
                conn,
                gio::DBusProxyFlags::DO_NOT_AUTO_START,
                None,
                Some(name),
                PATH,
                interface,
                None::<&gio::Cancellable>,
                move |proxy| {
                    let (Some(mpris), Ok(proxy)) = (weak.upgrade(), proxy) else {
                        return;
                    };
                    mpris.proxy_ready(&name_owned, serial, interface, proxy);
                },
            );
        }
    }

    fn proxy_ready(
        self: &Rc<Self>,
        name: &str,
        serial: u64,
        interface: &str,
        proxy: gio::DBusProxy,
    ) {
        {
            let mut players = self.players.borrow_mut();
            let Some(player) = players.get_mut(name).filter(|p| p.serial == serial) else {
                return;
            };
            let weak: Weak<Self> = Rc::downgrade(self);
            proxy.connect_local("g-properties-changed", false, move |_| {
                if let Some(mpris) = weak.upgrade() {
                    mpris.emit();
                }
                None
            });
            if interface == ROOT {
                player.root = Some(proxy);
            } else {
                player.player = Some(proxy);
            }
        }
        self.emit();
    }

    /// The players the lock screen shows, newest first: those that can
    /// play, whose policy allows them there (`MediaMessage._applyPolicy`:
    /// `enable` and the combined `show-in-lock-screen`). Each player's app
    /// is registered with GNOME Settings (`policy.store()`).
    pub fn lock_entries(self: &Rc<Self>) -> Vec<MediaEntry> {
        let players = self.players.borrow();
        let mut shown: Vec<(u64, MediaEntry)> = Vec::new();
        let mut apps_seen: Vec<String> = Vec::new();
        for (name, player) in players.iter() {
            let Some(proxy) = &player.player else {
                continue;
            };
            let flag = |key: &str| {
                proxy
                    .cached_property(key)
                    .and_then(|v| v.get::<bool>())
                    .unwrap_or(false)
            };
            if !flag("CanPlay") {
                continue;
            }
            let root_string = |key: &str| {
                player
                    .root
                    .as_ref()
                    .and_then(|root| root.cached_property(key))
                    .and_then(|v| v.get::<String>())
                    .filter(|s| !s.is_empty())
            };
            // `Shell.AppSystem.lookup_app(`${DesktopEntry}.desktop`)`, with
            // the id checked as a notification's `desktop-entry` is.
            let app = root_string("DesktopEntry")
                .and_then(|entry| self.policy.app_for(&entry, ""))
                .and_then(|entry| crate::notify::desktop_app(&entry).map(|app| (entry, app)));
            let app_id: Option<String> = app.as_ref().map(|(entry, _)| entry.clone());
            if let Some(id) = &app_id {
                self.policy.register(id);
                apps_seen.push(id.clone());
            }
            let policy = self.policy.policy(app_id.as_deref());
            if !(policy.enable && policy.show_in_lock_screen) {
                continue;
            }
            let track = track(proxy.cached_property("Metadata").as_ref());
            let source = app
                .as_ref()
                .map(|(_, app)| app.name.clone())
                .or_else(|| root_string("Identity"))
                .unwrap_or_default();
            let source_icon = app
                .as_ref()
                .and_then(|(_, app)| app.icon.as_deref())
                .and_then(crate::notify::source_icon)
                .and_then(|icon| IconExt::to_string(&icon).map(Into::into));
            let status = proxy
                .cached_property("PlaybackStatus")
                .and_then(|v| v.get::<String>())
                .unwrap_or_default();
            shown.push((
                player.serial,
                MediaEntry {
                    bus_name: name.clone(),
                    source,
                    source_icon,
                    title: track.title,
                    body: track.artists.join(", "),
                    cover_url: Some(track.cover_url).filter(|u| !u.is_empty()),
                    playing: status == "Playing",
                    can_go_previous: flag("CanGoPrevious"),
                    can_go_next: flag("CanGoNext"),
                },
            ));
        }
        drop(players);
        self.watch_policy(&apps_seen);
        shown.sort_by_key(|(serial, _)| std::cmp::Reverse(*serial));
        shown.into_iter().map(|(_, entry)| entry).collect()
    }

    /// Watch the global keys and each shown player's app keys.
    fn watch_policy(self: &Rc<Self>, apps: &[String]) {
        let mut watched = self.watched.borrow_mut();
        for id in std::iter::once("").chain(apps.iter().map(String::as_str)) {
            if watched.contains_key(id) {
                continue;
            }
            let settings = if id.is_empty() {
                GioPolicyStore::global_settings()
            } else {
                GioPolicyStore::app_settings(id)
            };
            let Some(settings) = settings else {
                continue;
            };
            let weak = Rc::downgrade(self);
            settings.connect_changed(None, move |_, _| {
                if let Some(mpris) = weak.upgrade() {
                    mpris.emit();
                }
            });
            watched.insert(id.to_owned(), settings);
        }
    }

    /// `previous()`, `playPause()`, `next()` on the named player.
    pub fn act(&self, bus_name: &str, action: MediaAction) {
        let players = self.players.borrow();
        let Some(proxy) = players.get(bus_name).and_then(|p| p.player.as_ref()) else {
            return;
        };
        let method = match action {
            MediaAction::Previous => "Previous",
            MediaAction::PlayPause => "PlayPause",
            MediaAction::Next => "Next",
        };
        proxy.call(
            method,
            None,
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
            move |reply| {
                if let Err(error) = reply {
                    eprintln!("tuna-shell-gtk: MPRIS {method}: {error}");
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(entries: &[(&str, glib::Variant)]) -> glib::Variant {
        let dict = glib::VariantDict::new(None);
        for (key, value) in entries {
            dict.insert_value(key, value);
        }
        dict.end()
    }

    #[test]
    fn metadata_is_validated_like_mpris_js() {
        let good = metadata(&[
            ("xesam:title", "Song".to_variant()),
            (
                "xesam:artist",
                vec!["A".to_owned(), "B".to_owned()].to_variant(),
            ),
            ("mpris:artUrl", "file:///tmp/cover.png".to_variant()),
        ]);
        assert_eq!(
            track(Some(&good)),
            Track {
                title: "Song".into(),
                artists: vec!["A".into(), "B".into()],
                cover_url: "file:///tmp/cover.png".into(),
            }
        );
        // Faulty types fall back as GNOME's do: an artist string is not
        // an array, a numeric title is not a string.
        let faulty = metadata(&[
            ("xesam:title", 7_i32.to_variant()),
            ("xesam:artist", "Solo".to_variant()),
            ("mpris:artUrl", true.to_variant()),
        ]);
        let unknown = Track {
            title: "Unknown title".into(),
            artists: vec!["Unknown artist".into()],
            cover_url: String::new(),
        };
        assert_eq!(track(Some(&faulty)), unknown);
        assert_eq!(track(None), unknown);
    }
}
