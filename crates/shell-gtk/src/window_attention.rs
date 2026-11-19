//! GNOME 51's window attention notification (`js/ui/windowAttentionHandler.js`):
//! when a window demands attention, the shell posts "“Title” is ready"
//! under the app's name; clicking it activates the window, and it is
//! withdrawn once the window has focus or is gone. Its text follows the
//! window's title.
//!
//! The compositor decides when a window demands attention (an activation
//! with a refused or stale token, a window that mapped behind the focused
//! one, wire 0.55 `WindowDemandsAttention`); this module is the shell's
//! half. As in `_onWindowDemandsAttention`, a window that is already
//! focused gets nothing.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tuna_shell_host::notifications::Urgency;

use crate::notify::NotifyUi;

/// Demands for windows the shell has not heard of yet are kept this many.
const MAX_PENDING: usize = 32;

/// `_getTitleAndBanner`: the banner for a window titled `title`.
pub fn banner(title: &str) -> String {
    format!("“{title}” is ready")
}

/// What the shell knows of one window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowView {
    pub id: u64,
    pub title: String,
    pub app_id: Option<String>,
    pub focused: bool,
}

/// What to do to the notification list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Post the window's notification.
    Post {
        window: u64,
        app_id: Option<String>,
        body: String,
    },
    /// The window's title changed: update the notification in place.
    Retitle {
        window: u64,
        notice: u64,
        app_id: Option<String>,
        body: String,
    },
    /// The window was focused or left: remove its notification.
    Withdraw { notice: u64 },
}

#[derive(Debug)]
struct Shown {
    /// The notification, once it is posted.
    notice: Option<u64>,
    body: String,
}

/// The windows that demand attention and their notifications.
#[derive(Debug, Default)]
pub struct Tracker {
    /// Demanded before the shell knew the window.
    pending: Vec<u64>,
    shown: HashMap<u64, Shown>,
}

impl Tracker {
    /// `window-demands-attention`. A window that already has a
    /// notification keeps the one it has.
    pub fn demand(&mut self, window: u64) {
        if self.shown.contains_key(&window) || self.pending.contains(&window) {
            return;
        }
        if self.pending.len() == MAX_PENDING {
            self.pending.remove(0);
        }
        self.pending.push(window);
    }

    /// The notification the shell posted for `window`.
    pub fn posted(&mut self, window: u64, notice: u64) {
        if let Some(shown) = self.shown.get_mut(&window) {
            shown.notice = Some(notice);
        }
    }

    /// The window list changed (focus, titles, windows closing): post the
    /// notifications that are due and withdraw those no longer wanted.
    pub fn sync(&mut self, windows: &[WindowView]) -> Vec<Effect> {
        let mut effects = Vec::new();
        let find = |id: u64| windows.iter().find(|window| window.id == id);

        let pending = std::mem::take(&mut self.pending);
        for id in pending {
            match find(id) {
                // The compositor said so before the window reached the
                // shell: ask again with the next list.
                None => self.pending.push(id),
                // `window.has_focus()`: pointless to announce.
                Some(window) if window.focused => {}
                Some(window) => {
                    let body = banner(&window.title);
                    self.shown.insert(
                        id,
                        Shown {
                            notice: None,
                            body: body.clone(),
                        },
                    );
                    effects.push(Effect::Post {
                        window: id,
                        app_id: window.app_id.clone(),
                        body,
                    });
                }
            }
        }

        let mut gone = Vec::new();
        for (id, shown) in &mut self.shown {
            match find(*id) {
                Some(window) if !window.focused => {
                    let body = banner(&window.title);
                    if body != shown.body {
                        shown.body = body.clone();
                        if let Some(notice) = shown.notice {
                            effects.push(Effect::Retitle {
                                window: *id,
                                notice,
                                app_id: window.app_id.clone(),
                                body,
                            });
                        }
                    }
                }
                // `'focus'` and `'unmanaged'` destroy the source.
                _ => gone.push(*id),
            }
        }
        for id in gone {
            if let Some(Shown {
                notice: Some(notice),
                ..
            }) = self.shown.remove(&id)
            {
                effects.push(Effect::Withdraw { notice });
            }
        }
        effects
    }
}

/// The notifications on screen.
pub struct WindowAttention {
    tracker: RefCell<Tracker>,
    notify: Rc<NotifyUi>,
    /// `Main.activateWindow`.
    activate: Rc<dyn Fn(u64)>,
}

impl WindowAttention {
    pub fn new(notify: Rc<NotifyUi>, activate: Rc<dyn Fn(u64)>) -> Rc<Self> {
        Rc::new(Self {
            tracker: RefCell::default(),
            notify,
            activate,
        })
    }

    pub fn demand(&self, window: u64) {
        self.tracker.borrow_mut().demand(window);
    }

    /// Follow the window list. `source` gives the app's name and icon for
    /// an app id (the notification's title and picture, `app.get_name()`
    /// and `app.get_icon()`).
    pub fn sync(
        self: &Rc<Self>,
        windows: &[WindowView],
        source: &dyn Fn(Option<&str>) -> (String, String),
    ) {
        let effects = self.tracker.borrow_mut().sync(windows);
        for effect in effects {
            match effect {
                Effect::Post {
                    window,
                    app_id,
                    body,
                } => {
                    let (name, icon) = source(app_id.as_deref());
                    if let Some(notice) = self.post(window, &name, &icon, &body, None) {
                        self.tracker.borrow_mut().posted(window, notice);
                    }
                }
                Effect::Retitle {
                    window,
                    notice,
                    app_id,
                    body,
                } => {
                    let (name, icon) = source(app_id.as_deref());
                    self.notify.drop_notification_actions(notice);
                    if let Some(notice) = self.post(window, &name, &icon, &body, Some(notice)) {
                        self.tracker.borrow_mut().posted(window, notice);
                    }
                }
                Effect::Withdraw { notice } => {
                    self.notify.drop_notification_actions(notice);
                    self.notify.remove_notification(notice);
                }
            }
        }
    }

    /// One notification whose click (`'activated'`) raises the window.
    fn post(
        self: &Rc<Self>,
        window: u64,
        name: &str,
        icon: &str,
        body: &str,
        replaces: Option<u64>,
    ) -> Option<u64> {
        let actions = [("default".to_owned(), String::new())];
        let notice = self.notify.post_with_actions(
            name,
            icon,
            name,
            body,
            &actions,
            Urgency::Normal,
            replaces,
        )?;
        let weak = Rc::downgrade(self);
        // Runs inside the notification store's dispatch, so it only asks
        // for the window (`source.open()`): the notification goes when
        // the window has focus.
        self.notify.on_notification_action(
            notice,
            "default",
            Rc::new(move || {
                if let Some(me) = weak.upgrade() {
                    (me.activate)(window);
                }
            }),
        );
        Some(notice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(id: u64, title: &str, focused: bool) -> WindowView {
        WindowView {
            id,
            title: title.to_owned(),
            app_id: Some("org.gnome.TextEditor".to_owned()),
            focused,
        }
    }

    fn post(window: u64, body: &str) -> Effect {
        Effect::Post {
            window,
            app_id: Some("org.gnome.TextEditor".to_owned()),
            body: body.to_owned(),
        }
    }

    #[test]
    fn the_banner_quotes_the_title() {
        assert_eq!(banner("notes.txt"), "“notes.txt” is ready");
    }

    #[test]
    fn a_demanding_window_gets_one_notification_until_it_has_focus() {
        let mut tracker = Tracker::default();
        let windows = [view(1, "front", true), view(2, "notes", false)];
        tracker.demand(2);
        assert_eq!(tracker.sync(&windows), [post(2, "“notes” is ready")]);
        tracker.posted(2, 40);
        // Asking again, or another sync, posts nothing more.
        tracker.demand(2);
        assert_eq!(tracker.sync(&windows), []);
        // The notification goes once the window is focused.
        let focused = [view(1, "front", false), view(2, "notes", true)];
        assert_eq!(tracker.sync(&focused), [Effect::Withdraw { notice: 40 }]);
        assert_eq!(tracker.sync(&focused), []);
        // A later demand is new.
        tracker.demand(2);
        assert_eq!(tracker.sync(&focused), [], "focused: pointless");
        tracker.demand(1);
        assert_eq!(tracker.sync(&focused), [post(1, "“front” is ready")]);
    }

    #[test]
    fn a_focused_window_is_not_announced() {
        let mut tracker = Tracker::default();
        tracker.demand(1);
        assert_eq!(tracker.sync(&[view(1, "front", true)]), []);
    }

    #[test]
    fn a_window_the_shell_learns_of_later_is_announced_then() {
        let mut tracker = Tracker::default();
        tracker.demand(5);
        assert_eq!(tracker.sync(&[view(1, "front", true)]), []);
        assert_eq!(
            tracker.sync(&[view(1, "front", true), view(5, "late", false)]),
            [post(5, "“late” is ready")]
        );
    }

    #[test]
    fn the_notification_follows_the_title_and_goes_with_the_window() {
        let mut tracker = Tracker::default();
        tracker.demand(2);
        assert_eq!(
            tracker.sync(&[view(1, "front", true), view(2, "Untitled", false)]),
            [post(2, "“Untitled” is ready")]
        );
        tracker.posted(2, 7);
        assert_eq!(
            tracker.sync(&[view(1, "front", true), view(2, "report.odt", false)]),
            [Effect::Retitle {
                window: 2,
                notice: 7,
                app_id: Some("org.gnome.TextEditor".to_owned()),
                body: "“report.odt” is ready".to_owned(),
            }]
        );
        // `unmanaged`: the window closed.
        assert_eq!(
            tracker.sync(&[view(1, "front", true)]),
            [Effect::Withdraw { notice: 7 }]
        );
    }

    #[test]
    fn demands_for_unknown_windows_are_bounded() {
        let mut tracker = Tracker::default();
        for id in 0..100 {
            tracker.demand(id);
        }
        let known: Vec<WindowView> = (0..100).map(|id| view(id, "w", false)).collect();
        assert_eq!(tracker.sync(&known).len(), MAX_PENDING);
    }
}
