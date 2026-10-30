//! Notification policy: GNOME Settings' per-app notification
//! preferences (#348).
//!
//! GNOME 51 keeps one policy per notification source (messageTray.js
//! `NotificationPolicy`). An installed app gets an application policy
//! backed by the relocatable `org.gnome.desktop.notifications.application`
//! schema at `/org/gnome/desktop/notifications/application/<id>/`;
//! anything else (no desktop entry we can find) gets the generic policy,
//! which only follows the two global keys. The first notification from an
//! app registers it in `application-children` so GNOME Settings lists it.
//!
//! The rules are pure functions over plain snapshots ([`GlobalPrefs`],
//! [`AppPrefs`]) so they unit-test without a bus; [`GioPolicyStore`] reads
//! and writes the real keys through GSettings, so it must run on the
//! thread iterating the GLib main context, as all GSettings use must:
//! GLib's keyfile backend is one in-memory file per process, reloaded
//! there on every file change without a lock, and a read or write from
//! another thread races that reload (lost writes, a truncated file). The
//! notification daemon files `Notify` calls on that context
//! ([`crate::intake::NotificationBus::deliver_in`]).

use std::collections::HashSet;
use std::sync::Mutex;

use gio::prelude::{SettingsExt, SettingsExtManual};

use crate::notifications::Urgency;

/// The global notification schema.
pub const NOTIFICATIONS_SCHEMA: &str = "org.gnome.desktop.notifications";
/// The relocatable per-app schema.
pub const APPLICATION_SCHEMA: &str = "org.gnome.desktop.notifications.application";
/// Where the per-app schema is relocated to, before the canonical id.
pub const APPLICATION_PATH_PREFIX: &str = "/org/gnome/desktop/notifications/application/";

/// The two global keys every policy combines with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalPrefs {
    /// `show-banners` (off is Do Not Disturb).
    pub show_banners: bool,
    /// `show-in-lock-screen`.
    pub show_in_lock_screen: bool,
}

impl Default for GlobalPrefs {
    fn default() -> Self {
        Self {
            show_banners: true,
            show_in_lock_screen: true,
        }
    }
}

/// One app's keys, defaulting to the GNOME 51 schema defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppPrefs {
    /// `enable`: the app may notify at all.
    pub enable: bool,
    /// `enable-sound-alerts`.
    pub enable_sound: bool,
    /// `show-banners`.
    pub show_banners: bool,
    /// `force-expanded`: banners open expanded.
    pub force_expanded: bool,
    /// `show-in-lock-screen`.
    pub show_in_lock_screen: bool,
    /// `details-in-lock-screen`: titles and bodies on the lock screen.
    pub details_in_lock_screen: bool,
}

impl Default for AppPrefs {
    fn default() -> Self {
        Self {
            enable: true,
            enable_sound: true,
            show_banners: true,
            force_expanded: false,
            show_in_lock_screen: true,
            details_in_lock_screen: false,
        }
    }
}

/// The effective policy of one source: its app keys combined with the
/// global ones, as GNOME's policy getters combine them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePolicy {
    pub enable: bool,
    pub enable_sound: bool,
    pub show_banners: bool,
    pub force_expanded: bool,
    pub show_in_lock_screen: bool,
    pub details_in_lock_screen: bool,
}

impl SourcePolicy {
    /// A source with no installed app (`NotificationGenericPolicy`):
    /// always enabled with sound, banners and lock-screen presence from
    /// the global keys, never expanded, never detailed when locked.
    pub fn generic(global: GlobalPrefs) -> Self {
        Self {
            enable: true,
            enable_sound: true,
            show_banners: global.show_banners,
            force_expanded: false,
            show_in_lock_screen: global.show_in_lock_screen,
            details_in_lock_screen: false,
        }
    }

    /// An installed app's policy (`NotificationApplicationPolicy`): the
    /// global and per-app gates both have to allow banners and the lock
    /// screen; everything else is the app's own.
    pub fn app(global: GlobalPrefs, app: AppPrefs) -> Self {
        Self {
            enable: app.enable,
            enable_sound: app.enable_sound,
            show_banners: global.show_banners && app.show_banners,
            force_expanded: app.force_expanded,
            show_in_lock_screen: global.show_in_lock_screen && app.show_in_lock_screen,
            details_in_lock_screen: app.details_in_lock_screen,
        }
    }
}

/// What happens to one arriving notification. Its sound is decided when
/// its banner shows, not here (see
/// [`NotificationCenter::banner_sound`](crate::notifications::NotificationCenter::banner_sound)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delivery {
    /// It goes up as a banner.
    pub banner: bool,
    /// The banner opens expanded.
    pub expanded: bool,
}

/// Decide one arrival. `None` drops it: a disabled app's notifications
/// are never filed, critical ones included. `dnd` is the store's Do Not
/// Disturb gate; critical notifications break through it and the
/// per-app banner switch alike, as they do in GNOME.
pub fn deliver(policy: &SourcePolicy, urgency: Urgency, dnd: bool) -> Option<Delivery> {
    if !policy.enable {
        return None;
    }
    let critical = urgency == Urgency::Critical;
    let banner = critical || (!dnd && policy.show_banners);
    Some(Delivery {
        banner,
        expanded: banner && (critical || policy.force_expanded),
    })
}

/// Whether a banner coming on screen plays its notification's sound
/// (messageTray.js `Notification.playSound`, run from
/// `_updateShowingNotification`): only when the sender named one and
/// the source's `enable-sound-alerts` allows it at that moment.
pub fn plays_sound(policy: &SourcePolicy, sound: &crate::notifications::Sound) -> bool {
    policy.enable_sound && !sound.is_empty()
}

/// GNOME's settings path id for an app id (`_canonicalizeId`):
/// lowercased, every character other than `a-z`, `0-9` and `-` turned
/// into a dash, and runs of dashes collapsed.
pub fn canonical_app_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for c in id.chars().flat_map(char::to_lowercase) {
        let c = if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
            c
        } else {
            '-'
        };
        if c == '-' && out.ends_with('-') {
            continue;
        }
        out.push(c);
    }
    out
}

/// The installed app a notification comes from, as notificationDaemon.js
/// finds it: the `desktop-entry` hint when it names an installed app,
/// else the sender's app name taken as a desktop id. `None` means the
/// generic policy. `installed` answers whether `<id>.desktop` exists.
pub fn resolve_app(
    desktop_entry: &str,
    app_name: &str,
    installed: impl Fn(&str) -> bool,
) -> Option<String> {
    [desktop_entry.trim_end_matches(".desktop"), app_name]
        .into_iter()
        .find(|id| valid_desktop_id(id) && installed(id))
        .map(str::to_owned)
}

/// Whether `id` can name a desktop file: no path separators and no
/// relative path, so a sender cannot point the lookup elsewhere.
fn valid_desktop_id(id: &str) -> bool {
    !id.is_empty() && !id.contains('/') && !id.starts_with('.') && !id.contains('\0')
}

/// The `application-children` list after registering `canonical`, or
/// `None` when it is already listed.
pub fn register_child(children: &[String], canonical: &str) -> Option<Vec<String>> {
    if canonical.is_empty() || children.iter().any(|c| c == canonical) {
        return None;
    }
    let mut next = children.to_vec();
    next.push(canonical.to_owned());
    Some(next)
}

/// One source on the lock screen (unlockDialog.js `NotificationsBox`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockSource {
    /// The source's grouping key.
    pub key: String,
    /// Its newest notification (names and icons the source).
    pub newest: u64,
    /// How many of its notifications are unseen (`unseenCount`).
    pub count: usize,
    /// Any of its notifications is critical (the box's `critical`
    /// style; seen ones count, as `source.notifications.some(...)`).
    pub critical: bool,
    /// When details show (`_shouldShowDetails`), each unseen
    /// notification's title and body, oldest first as GNOME lists them;
    /// otherwise `None` and only the source's name and count may show.
    pub details: Option<Vec<(String, String)>>,
}

/// The lock screen's notification sources, as unlockDialog.js 51's
/// `NotificationsBox` shows them. `oldest_first` is the history. A
/// source shows while it has unseen notifications and its policy keeps
/// it on the lock screen (`enable`, then `show-in-lock-screen`, global
/// and per app); critical ones get no exception. Details show with the
/// app's `details-in-lock-screen`, or when every notification of the
/// source is system-scoped (`narrowestPrivacyScope`); only then are
/// titles and bodies copied, so nothing else can draw them. Sources come
/// newest first by when they started (each new source box goes on top).
pub fn lock_screen<'a>(
    oldest_first: impl IntoIterator<Item = &'a crate::notifications::Notification>,
    mut policy_for: impl FnMut(&crate::notifications::Notification) -> SourcePolicy,
) -> Vec<LockSource> {
    struct Source<'a> {
        policy: SourcePolicy,
        members: Vec<&'a crate::notifications::Notification>,
    }
    let mut sources: Vec<Source<'a>> = Vec::new();
    for n in oldest_first {
        let key = n.source_key();
        match sources
            .iter_mut()
            .find(|s| s.members[0].source_key() == key)
        {
            Some(source) => source.members.push(n),
            None => sources.push(Source {
                policy: policy_for(n),
                members: vec![n],
            }),
        }
    }
    sources
        .into_iter()
        .rev()
        .filter(|s| s.policy.enable && s.policy.show_in_lock_screen)
        .filter_map(|s| {
            let unseen: Vec<_> = s.members.iter().filter(|n| !n.acknowledged()).collect();
            if unseen.is_empty() {
                return None;
            }
            let detailed =
                s.policy.details_in_lock_screen || s.members.iter().all(|n| n.system_scope());
            Some(LockSource {
                key: s.members[0].source_key().to_owned(),
                newest: s.members.last().map_or(0, |n| n.id),
                count: unseen.len(),
                critical: s.members.iter().any(|n| n.urgency == Urgency::Critical),
                details: detailed.then(|| {
                    unseen
                        .iter()
                        .map(|n| (n.summary().to_owned(), n.body().to_owned()))
                        .collect()
                }),
            })
        })
        .collect()
}

/// Where policy comes from. The center asks on every arrival, on
/// whichever thread files it.
pub trait PolicyStore: Send + Sync {
    /// The installed app a notification belongs to (see [`resolve_app`]).
    fn app_for(&self, desktop_entry: &str, app_name: &str) -> Option<String>;
    /// The global keys.
    fn global(&self) -> GlobalPrefs;
    /// One app's keys, by desktop id (without `.desktop`).
    fn app(&self, app_id: &str) -> AppPrefs;
    /// Record that `app_id` notified, so GNOME Settings lists it.
    fn register(&self, app_id: &str);

    /// The effective policy for a source; `None` is the generic one.
    fn policy(&self, app_id: Option<&str>) -> SourcePolicy {
        let global = self.global();
        match app_id {
            Some(id) => SourcePolicy::app(global, self.app(id)),
            None => SourcePolicy::generic(global),
        }
    }
}

/// The real store: GNOME's keys through GSettings, apps through the XDG
/// application dirs. Without the schemas everything reads as defaults.
/// Call it only on the thread iterating the GLib main context (see the
/// module docs).
#[derive(Debug, Default)]
pub struct GioPolicyStore {
    /// Apps registered this session (each is written once).
    registered: Mutex<HashSet<String>>,
}

impl GioPolicyStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The global notification settings, or `None` without the schema.
    pub fn global_settings() -> Option<gio::Settings> {
        let source = gio::SettingsSchemaSource::default()?;
        source.lookup(NOTIFICATIONS_SCHEMA, true)?;
        Some(gio::Settings::new(NOTIFICATIONS_SCHEMA))
    }

    /// The per-app settings for `app_id`, or `None` without the schema.
    pub fn app_settings(app_id: &str) -> Option<gio::Settings> {
        let source = gio::SettingsSchemaSource::default()?;
        source.lookup(APPLICATION_SCHEMA, true)?;
        let path = format!("{APPLICATION_PATH_PREFIX}{}/", canonical_app_id(app_id));
        Some(gio::Settings::with_path(APPLICATION_SCHEMA, &path))
    }
}

impl PolicyStore for GioPolicyStore {
    fn app_for(&self, desktop_entry: &str, app_name: &str) -> Option<String> {
        resolve_app(desktop_entry, app_name, crate::apps::desktop_file_exists)
    }

    fn global(&self) -> GlobalPrefs {
        let Some(settings) = Self::global_settings() else {
            return GlobalPrefs::default();
        };
        GlobalPrefs {
            show_banners: settings.boolean("show-banners"),
            show_in_lock_screen: settings.boolean("show-in-lock-screen"),
        }
    }

    fn app(&self, app_id: &str) -> AppPrefs {
        let Some(settings) = Self::app_settings(app_id) else {
            return AppPrefs::default();
        };
        AppPrefs {
            enable: settings.boolean("enable"),
            enable_sound: settings.boolean("enable-sound-alerts"),
            show_banners: settings.boolean("show-banners"),
            force_expanded: settings.boolean("force-expanded"),
            show_in_lock_screen: settings.boolean("show-in-lock-screen"),
            details_in_lock_screen: settings.boolean("details-in-lock-screen"),
        }
    }

    fn register(&self, app_id: &str) {
        if let Ok(mut seen) = self.registered.lock() {
            if !seen.insert(app_id.to_owned()) {
                return;
            }
        }
        let (Some(global), Some(app)) = (Self::global_settings(), Self::app_settings(app_id))
        else {
            return;
        };
        let desktop_id = format!("{app_id}.desktop");
        if app.string("application-id") != desktop_id {
            let _ = app.set_string("application-id", &desktop_id);
        }
        let children: Vec<String> = global
            .strv("application-children")
            .iter()
            .map(|s| s.to_string())
            .collect();
        if let Some(next) = register_child(&children, &canonical_app_id(app_id)) {
            let _ = global.set_strv("application-children", next);
        }
        // Land the write before the sender hears back.
        gio::Settings::sync();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids_canonicalize_like_gnome() {
        assert_eq!(canonical_app_id("org.gnome.Nautilus"), "org-gnome-nautilus");
        assert_eq!(canonical_app_id("firefox"), "firefox");
        assert_eq!(canonical_app_id("My__App..Two"), "my-app-two");
        assert_eq!(canonical_app_id("a-.-b"), "a-b");
        assert_eq!(canonical_app_id("Ünï"), "-n-");
    }

    #[test]
    fn apps_resolve_from_the_hint_then_the_name() {
        let installed = |id: &str| matches!(id, "org.example.Mail" | "chat");
        assert_eq!(
            resolve_app("org.example.Mail.desktop", "Mail", installed).as_deref(),
            Some("org.example.Mail")
        );
        assert_eq!(resolve_app("", "chat", installed).as_deref(), Some("chat"));
        assert_eq!(
            resolve_app("not.installed", "chat", installed).as_deref(),
            Some("chat")
        );
        // Nothing installed by either name: the generic policy.
        assert_eq!(resolve_app("", "Random Sender", installed), None);
        // A path never resolves, even when something answers for it.
        assert_eq!(resolve_app("../chat", "", |_| true), None);
        assert_eq!(resolve_app("", ".hidden", |_| true), None);
    }

    #[test]
    fn app_policy_combines_global_and_app_gates() {
        let global = GlobalPrefs::default();
        let app = AppPrefs::default();
        let p = SourcePolicy::app(global, app);
        assert!(p.enable && p.enable_sound && p.show_banners && p.show_in_lock_screen);
        assert!(!p.force_expanded && !p.details_in_lock_screen);
        // Either gate closes banners and the lock screen.
        let off = GlobalPrefs {
            show_banners: false,
            show_in_lock_screen: false,
        };
        let p = SourcePolicy::app(off, app);
        assert!(!p.show_banners && !p.show_in_lock_screen);
        let quiet = AppPrefs {
            show_banners: false,
            show_in_lock_screen: false,
            ..app
        };
        let p = SourcePolicy::app(global, quiet);
        assert!(!p.show_banners && !p.show_in_lock_screen);
        // Details are the app's alone.
        let detailed = AppPrefs {
            details_in_lock_screen: true,
            ..app
        };
        assert!(SourcePolicy::app(off, detailed).details_in_lock_screen);
    }

    #[test]
    fn generic_policy_follows_only_the_global_keys() {
        let p = SourcePolicy::generic(GlobalPrefs::default());
        assert!(p.enable && p.enable_sound && p.show_banners && p.show_in_lock_screen);
        assert!(!p.details_in_lock_screen && !p.force_expanded);
        let p = SourcePolicy::generic(GlobalPrefs {
            show_banners: false,
            show_in_lock_screen: false,
        });
        assert!(p.enable && !p.show_banners && !p.show_in_lock_screen);
    }

    #[test]
    fn disabled_apps_drop_even_critical_notifications() {
        let p = SourcePolicy::app(
            GlobalPrefs::default(),
            AppPrefs {
                enable: false,
                ..AppPrefs::default()
            },
        );
        for urgency in [Urgency::Low, Urgency::Normal, Urgency::Critical] {
            assert_eq!(deliver(&p, urgency, false), None);
            assert_eq!(deliver(&p, urgency, true), None);
        }
    }

    #[test]
    fn banners_follow_the_app_switch_and_dnd_but_critical_breaks_through() {
        let on = SourcePolicy::app(GlobalPrefs::default(), AppPrefs::default());
        let normal = deliver(&on, Urgency::Normal, false).unwrap();
        assert!(normal.banner && !normal.expanded);
        // Do Not Disturb holds the banner.
        let dnd = deliver(&on, Urgency::Normal, true).unwrap();
        assert!(!dnd.banner);
        // The app's own banner switch does the same.
        let quiet = SourcePolicy::app(
            GlobalPrefs::default(),
            AppPrefs {
                show_banners: false,
                ..AppPrefs::default()
            },
        );
        assert!(!deliver(&quiet, Urgency::Normal, false).unwrap().banner);
        // Critical shows through both, expanded.
        for dnd in [false, true] {
            let crit = deliver(&quiet, Urgency::Critical, dnd).unwrap();
            assert!(crit.banner && crit.expanded);
        }
    }

    #[test]
    fn sound_and_expansion_are_per_app() {
        let sound = crate::notifications::Sound {
            name: "message-new-instant".into(),
            file: String::new(),
        };
        let on = SourcePolicy::app(GlobalPrefs::default(), AppPrefs::default());
        assert!(plays_sound(&on, &sound));
        // Nothing named: nothing plays, whatever the policy.
        assert!(!plays_sound(&on, &crate::notifications::Sound::default()));
        let p = SourcePolicy::app(
            GlobalPrefs::default(),
            AppPrefs {
                enable_sound: false,
                force_expanded: true,
                ..AppPrefs::default()
            },
        );
        let d = deliver(&p, Urgency::Normal, false).unwrap();
        assert!(d.banner && d.expanded);
        // enable-sound-alerts off silences critical banners too.
        assert!(!plays_sound(&p, &sound));
        // The generic policy always allows sound (NotificationPolicy).
        assert!(plays_sound(
            &SourcePolicy::generic(GlobalPrefs {
                show_banners: false,
                show_in_lock_screen: false,
            }),
            &sound
        ));
    }

    fn lock_fixture() -> crate::notifications::NotificationCenter {
        let mut c = crate::notifications::NotificationCenter::new();
        for (app, title, body, urgency) in [
            ("mail", "Old mail", "seen already", Urgency::Normal),
            ("mail", "Invoice", "Amount due: 12", Urgency::Normal),
            ("chat", "Alice", "Secret plans", Urgency::Critical),
            ("mail", "Lunch?", "At noon", Urgency::Normal),
            ("chat", "Bob", "Hi", Urgency::Normal),
        ] {
            c.notify(app, title, body, vec![], urgency, None);
        }
        c.acknowledge(&[1]);
        c
    }

    fn lock_policy(details_for: &'static str) -> impl FnMut(&Notification) -> SourcePolicy {
        move |n| {
            SourcePolicy::app(
                GlobalPrefs::default(),
                AppPrefs {
                    details_in_lock_screen: n.app() == details_for,
                    ..AppPrefs::default()
                },
            )
        }
    }

    use crate::notifications::Notification;

    #[test]
    fn lock_screen_counts_unseen_notifications_per_source_without_details() {
        let c = lock_fixture();
        let view = lock_screen(c.history().iter(), lock_policy(""));
        assert_eq!(
            view.iter()
                .map(|s| (s.key.as_str(), s.newest, s.count, s.critical))
                .collect::<Vec<_>>(),
            vec![("chat", 5, 2, true), ("mail", 4, 2, false)]
        );
        // Redacted: no title or body leaves the store.
        assert!(view.iter().all(|s| s.details.is_none()));
    }

    #[test]
    fn lock_screen_details_are_per_app() {
        let c = lock_fixture();
        let view = lock_screen(c.history().iter(), lock_policy("mail"));
        let mail = view.iter().find(|s| s.key == "mail").unwrap();
        // In arrival order, as unlockDialog.js walks source.notifications.
        assert_eq!(
            mail.details.as_deref(),
            Some(
                &[
                    ("Invoice".to_owned(), "Amount due: 12".to_owned()),
                    ("Lunch?".to_owned(), "At noon".to_owned()),
                ][..]
            )
        );
        // The acknowledged one never shows; chat stays redacted.
        let chat = view.iter().find(|s| s.key == "chat").unwrap();
        assert!(chat.details.is_none());
    }

    #[test]
    fn lock_screen_hides_sources_the_policy_keeps_off_it() {
        let c = lock_fixture();
        let hide_chat = |n: &Notification| {
            SourcePolicy::app(
                GlobalPrefs::default(),
                AppPrefs {
                    show_in_lock_screen: n.app() != "chat",
                    ..AppPrefs::default()
                },
            )
        };
        let view = lock_screen(c.history().iter(), hide_chat);
        assert_eq!(
            view.iter().map(|s| s.key.as_str()).collect::<Vec<_>>(),
            ["mail"]
        );
        // The global switch hides every source, generic ones included.
        let off = GlobalPrefs {
            show_banners: true,
            show_in_lock_screen: false,
        };
        assert!(lock_screen(c.history().iter(), |_| SourcePolicy::generic(off)).is_empty());
        // Everything seen: nothing to show.
        let mut c = c;
        c.acknowledge(&[1, 2, 3, 4, 5]);
        assert!(lock_screen(c.history().iter(), lock_policy("mail")).is_empty());
    }

    #[test]
    fn critical_notifications_get_no_lock_screen_exception() {
        let c = lock_fixture();
        // chat holds the critical one: off the lock screen it stays off,
        // and a disabled app never shows there.
        let policy = |n: &Notification| {
            SourcePolicy::app(
                GlobalPrefs::default(),
                AppPrefs {
                    show_in_lock_screen: n.app() != "chat",
                    enable: n.app() != "mail",
                    ..AppPrefs::default()
                },
            )
        };
        assert!(lock_screen(c.history().iter(), policy).is_empty());
        // A source keeps its critical style once that one is seen.
        let mut c = c;
        c.acknowledge(&[3]);
        let view = lock_screen(c.history().iter(), lock_policy(""));
        let chat = view.iter().find(|s| s.key == "chat").unwrap();
        assert_eq!((chat.count, chat.critical), (1, true));
    }

    #[test]
    fn lock_sources_stack_newest_started_first() {
        let mut c = crate::notifications::NotificationCenter::new();
        for app in ["mail", "chat", "mail"] {
            c.notify(app, "t", "b", vec![], Urgency::Normal, None);
        }
        // mail has the newest notification but started first: each new
        // source box goes on top (`insert_child_at_index`).
        let view = lock_screen(c.history().iter(), lock_policy(""));
        assert_eq!(
            view.iter()
                .map(|s| (s.key.as_str(), s.newest))
                .collect::<Vec<_>>(),
            [("chat", 2), ("mail", 3)]
        );
    }

    #[test]
    fn system_scoped_sources_show_details_without_the_app_key() {
        use crate::notifications::{Incoming, NotificationCenter};
        let incoming = |app: &str, title: &str, system: bool| Incoming {
            app: app.into(),
            title: title.into(),
            body: format!("{title} body"),
            system_scope: system,
            ..Incoming::default()
        };
        let mut c = NotificationCenter::new();
        c.deliver(incoming("power", "Battery low", true));
        c.deliver(incoming("mixed", "System", true));
        c.deliver(incoming("mixed", "User", false));
        let view = lock_screen(c.history().iter(), lock_policy(""));
        let power = view.iter().find(|s| s.key == "power").unwrap();
        assert_eq!(
            power.details.as_deref(),
            Some(&[("Battery low".to_owned(), "Battery low body".to_owned())][..])
        );
        // One user-scoped notification makes the whole source private.
        let mixed = view.iter().find(|s| s.key == "mixed").unwrap();
        assert_eq!((mixed.count, mixed.details.as_ref()), (2, None));
    }

    #[test]
    fn registration_appends_each_app_once() {
        let none: Vec<String> = Vec::new();
        assert_eq!(
            register_child(&none, "org-example-mail"),
            Some(vec!["org-example-mail".to_owned()])
        );
        let listed = vec!["a".to_owned(), "org-example-mail".to_owned()];
        assert_eq!(register_child(&listed, "org-example-mail"), None);
        assert_eq!(
            register_child(&listed, "b"),
            Some(vec!["a".into(), "org-example-mail".into(), "b".into()])
        );
        assert_eq!(register_child(&listed, ""), None);
    }
}
