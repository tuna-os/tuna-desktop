//! GNOME 51's MPRIS client (js/ui/mpris.js): every
//! `org.mpris.MediaPlayer2.*` name on the session bus becomes a player,
//! shown once it says `CanPlay`, newest first ([`listed`]). Each player
//! is a `MediaMessage` (js/ui/messageList.js) in two places: the
//! calendar's message list (`MessageView._setupMpris`, above the
//! notifications) and the lock screen's `NotificationsBox`. Either way the
//! message follows its app's notification policy ([`media_visible`]):
//! `enable`, and on the lock screen `show-in-lock-screen` as well.
//! Clicking it in the calendar raises the player ([`raise_target`]).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use tuna_shell_host::apps::AppEntry;
use tuna_shell_host::notification_policy::{GioPolicyStore, PolicyStore, SourcePolicy};

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

/// `MprisSource.players` in the order the message lists stack them:
/// only players that can play (`CanPlay`; the others never become a
/// `MediaMessage`), newest first (`_addPlayer` inserts each at index 0).
/// `players` are `(serial, can_play, player)`, serials growing as players
/// join the bus.
pub fn listed<T>(players: impl IntoIterator<Item = (u64, bool, T)>) -> Vec<T> {
    let mut shown: Vec<(u64, T)> = players
        .into_iter()
        .filter(|(_, can_play, _)| *can_play)
        .map(|(serial, _, player)| (serial, player))
        .collect();
    shown.sort_by_key(|(serial, _)| std::cmp::Reverse(*serial));
    shown.into_iter().map(|(_, player)| player).collect()
}

/// `MediaMessage._applyPolicy`: a player's message shows while its app's
/// `enable` holds and, on the lock screen, `show-in-lock-screen` (global
/// and per app) as well. A player without an installed app has GNOME's
/// generic policy, always enabled. Do Not Disturb and lockdown play no
/// part (GNOME's media section reads neither).
pub fn media_visible(policy: &SourcePolicy, locked: bool) -> bool {
    policy.enable && (!locked || policy.show_in_lock_screen)
}

/// What clicking a player's message raises (`MprisPlayer.raise`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Raise<A> {
    /// Its app (`app.activate()`), preferred because a remote `Raise`
    /// may run into focus stealing prevention.
    App(A),
    /// The player's own `Raise()`, when it says `CanRaise`.
    Remote,
    Nothing,
}

/// `MprisPlayer.raise`: the player's app when its `DesktopEntry` names an
/// installed one, else its own `Raise()` if it `CanRaise`, else nothing.
pub fn raise_target<A>(app: Option<A>, can_raise: bool) -> Raise<A> {
    match app {
        Some(app) => Raise::App(app),
        None if can_raise => Raise::Remote,
        None => Raise::Nothing,
    }
}

/// One player that can play: what its `MediaMessage` shows and the
/// notification policy its visibility follows ([`media_visible`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPlayer {
    pub entry: MediaEntry,
    pub policy: SourcePolicy,
}

struct Player {
    /// Insertion order: newer players sit above older ones
    /// (`insert_child_at_index(message, 0)`).
    serial: u64,
    root: Option<gio::DBusProxy>,
    player: Option<gio::DBusProxy>,
}

impl Player {
    /// A non-empty string property of `org.mpris.MediaPlayer2`.
    fn root_string(&self, key: &str) -> Option<String> {
        self.root
            .as_ref()
            .and_then(|root| root.cached_property(key))
            .and_then(|v| v.get::<String>())
            .filter(|s| !s.is_empty())
    }
}

/// A boolean property as GNOME's proxies read it: absent or mistyped is
/// false.
fn flag(proxy: &gio::DBusProxy, key: &str) -> bool {
    proxy
        .cached_property(key)
        .and_then(|v| v.get::<bool>())
        .unwrap_or(false)
}

pub struct Mpris {
    players: RefCell<HashMap<String, Player>>,
    serial: Cell<u64>,
    /// Told when what the players show may have changed: the calendar's
    /// message list and the lock screen.
    changed: RefCell<Vec<Rc<dyn Fn()>>>,
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
            changed: RefCell::default(),
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
        self.changed.borrow_mut().push(Rc::new(f));
    }

    fn emit(&self) {
        let listeners = self.changed.borrow().clone();
        for f in listeners {
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

    /// The installed app a player's `DesktopEntry` names
    /// (`Shell.AppSystem.lookup_app(`${DesktopEntry}.desktop`)`), with the
    /// id checked as a notification's `desktop-entry` is.
    fn app(&self, player: &Player) -> Option<(String, AppEntry)> {
        player
            .root_string("DesktopEntry")
            .and_then(|entry| self.policy.app_for(&entry, ""))
            .and_then(|id| crate::notify::desktop_app(&id).map(|app| (id, app)))
    }

    /// Every player that can play, newest first ([`listed`]), with what
    /// its `MediaMessage` shows and its policy. Each player's app is
    /// registered with GNOME Settings (`policy.store()`) and its keys are
    /// watched, so a change applies at once.
    pub fn players(self: &Rc<Self>) -> Vec<MediaPlayer> {
        let players = self.players.borrow();
        let can_play = listed(players.iter().filter_map(|(name, player)| {
            let proxy = player.player.as_ref()?;
            Some((player.serial, flag(proxy, "CanPlay"), (name, player, proxy)))
        }));
        let mut shown = Vec::with_capacity(can_play.len());
        let mut apps_seen: Vec<String> = Vec::new();
        for (name, player, proxy) in can_play {
            let app = self.app(player);
            let app_id: Option<String> = app.as_ref().map(|(id, _)| id.clone());
            if let Some(id) = &app_id {
                self.policy.register(id);
                apps_seen.push(id.clone());
            }
            let track = track(proxy.cached_property("Metadata").as_ref());
            let source = app
                .as_ref()
                .map(|(_, app)| app.name.clone())
                .or_else(|| player.root_string("Identity"))
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
            shown.push(MediaPlayer {
                entry: MediaEntry {
                    bus_name: name.clone(),
                    source,
                    source_icon,
                    title: track.title,
                    body: track.artists.join(", "),
                    cover_url: Some(track.cover_url).filter(|u| !u.is_empty()),
                    playing: status == "Playing",
                    can_go_previous: flag(proxy, "CanGoPrevious"),
                    can_go_next: flag(proxy, "CanGoNext"),
                },
                policy: self.policy.policy(app_id.as_deref()),
            });
        }
        drop(players);
        self.watch_policy(&apps_seen);
        shown
    }

    /// The players the lock screen shows, newest first: those whose policy
    /// allows them there ([`media_visible`] while locked).
    pub fn lock_entries(self: &Rc<Self>) -> Vec<MediaEntry> {
        self.players()
            .into_iter()
            .filter(|player| media_visible(&player.policy, true))
            .map(|player| player.entry)
            .collect()
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

    /// `raise()` on the named player ([`raise_target`]); `activate` is
    /// `app.activate()`, which raises a running app's window or launches
    /// it.
    pub fn raise(&self, bus_name: &str, activate: &dyn Fn(&AppEntry)) {
        // Decide under the borrow, act after it: activating an app reaches
        // the compositor, and either may come back here.
        let (target, root) = {
            let players = self.players.borrow();
            let Some(player) = players.get(bus_name) else {
                return;
            };
            let can_raise = player
                .root
                .as_ref()
                .is_some_and(|root| flag(root, "CanRaise"));
            (
                raise_target(self.app(player).map(|(_, app)| app), can_raise),
                player.root.clone(),
            )
        };
        match (target, root) {
            (Raise::App(app), _) => activate(&app),
            (Raise::Remote, Some(root)) => root.call(
                "Raise",
                None,
                gio::DBusCallFlags::NONE,
                -1,
                None::<&gio::Cancellable>,
                |reply| {
                    if let Err(error) = reply {
                        eprintln!("tuna-shell-gtk: MPRIS Raise: {error}");
                    }
                },
            ),
            (Raise::Remote | Raise::Nothing, _) => {}
        }
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

    #[test]
    fn media_section_hides_players_that_cannot_play() {
        // Serials grow as players join the bus; `CanPlay` false never
        // becomes a message, and the newest player sits on top.
        let players = [
            (1, true, "first"),
            (2, false, "silent"),
            (3, true, "third"),
            (4, false, "loading"),
        ];
        assert_eq!(listed(players), vec!["third", "first"]);
        assert!(listed([(1, false, "silent")]).is_empty());
        assert!(listed(Vec::<(u64, bool, &str)>::new()).is_empty());
    }

    #[test]
    fn media_policy_follows_enable_and_lock_screen_like_gnome() {
        use tuna_shell_host::notification_policy::{AppPrefs, GlobalPrefs};
        let global = GlobalPrefs::default();
        let app = |prefs: AppPrefs| SourcePolicy::app(global, prefs);
        let on = app(AppPrefs::default());
        assert!(media_visible(&on, false) && media_visible(&on, true));
        // A disabled app's player shows nowhere.
        let off = app(AppPrefs {
            enable: false,
            ..AppPrefs::default()
        });
        assert!(!media_visible(&off, false) && !media_visible(&off, true));
        // show-in-lock-screen only matters while locked: the calendar
        // keeps the player.
        let unlocked_only = app(AppPrefs {
            show_in_lock_screen: false,
            ..AppPrefs::default()
        });
        assert!(media_visible(&unlocked_only, false));
        assert!(!media_visible(&unlocked_only, true));
        let global_off = SourcePolicy::app(
            GlobalPrefs {
                show_in_lock_screen: false,
                ..global
            },
            AppPrefs::default(),
        );
        assert!(media_visible(&global_off, false) && !media_visible(&global_off, true));
        // A player without an installed app: the generic policy, always
        // enabled; the banners switch (Do Not Disturb) plays no part.
        let generic = SourcePolicy::generic(GlobalPrefs {
            show_banners: false,
            ..global
        });
        assert!(media_visible(&generic, false) && media_visible(&generic, true));
    }

    #[test]
    fn raising_prefers_the_app_then_the_players_own_raise() {
        assert_eq!(raise_target(Some("app"), true), Raise::App("app"));
        assert_eq!(raise_target(Some("app"), false), Raise::App("app"));
        assert_eq!(raise_target(None::<&str>, true), Raise::Remote);
        assert_eq!(raise_target(None::<&str>, false), Raise::Nothing);
    }
}
