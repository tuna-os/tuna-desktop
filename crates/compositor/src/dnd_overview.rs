//! A client's drag-and-drop through the overview (#1189), as GNOME does it.
//!
//! While a client drag runs, the pointer belongs to the drag, so no shell
//! surface sees hover and the overview's own controls never answer. GNOME
//! reads the drag's position itself:
//!
//! - the Activities button opens the overview once the drag has rested on
//!   it for [`DND_ACTIVITIES_TIMEOUT`] (`panel.js`, `handleDragOver`); the
//!   hot corner opens it at once (`layout.js`, `HotCorner.handleDragOver`,
//!   answered by [`crate::windows::TriggerState::set_client_drag`]);
//! - a window preview held under the drag for [`DND_WINDOW_SWITCH_TIMEOUT`]
//!   activates that window and closes the overview, so the drop lands on it
//!   (`overview.js`, `_onDragMotion`); a workspace thumbnail under it
//!   activates its workspace at once (`workspaceThumbnail.js`,
//!   `handleDragOverInternal`);
//! - when the drag ends, by drop or by cancel, an overview still showing
//!   closes onto the workspace the drag started on (`overview.js`,
//!   `_onDragEnd`).
//!
//! The shell's own drags (the dash and the app grid, started by the
//! supervised shell inside the overview) are the shell's: they never
//! reach this module (see `State::client_dnd_overview_active`).
//!
//! This module only decides; the runtime applies the [`DndAction`]s with
//! the same calls a click uses.

use std::time::{Duration, Instant};

use smithay::utils::{Logical, Point};

use crate::overview::OverviewLayout;

/// `overview.js` `DND_WINDOW_SWITCH_TIMEOUT`: how long a drag rests on a
/// window preview before that window is activated.
pub const DND_WINDOW_SWITCH_TIMEOUT: Duration = Duration::from_millis(750);

/// `panel.js` `BUTTON_DND_ACTIVATION_TIMEOUT`: how long a drag rests on the
/// Activities button before the overview opens. Every motion over the
/// button restarts it.
pub const DND_ACTIVITIES_TIMEOUT: Duration = Duration::from_millis(250);

/// What a client drag is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Hover {
    /// The Activities control of the primary panel.
    pub activities: bool,
    /// The workspace whose thumbnail it is over.
    pub thumbnail: Option<u32>,
    /// The window whose preview it is over: on the picker's card, on a
    /// neighboring card, or a thumbnail's miniature.
    pub window: Option<u64>,
}

impl Hover {
    /// What `layout` has at `pos`; `activities` as the panel says.
    pub fn in_overview(
        layout: &OverviewLayout,
        pos: Point<f64, Logical>,
        activities: bool,
    ) -> Self {
        Self {
            activities,
            thumbnail: layout
                .thumbnails
                .iter()
                .find(|t| t.rect.to_f64().contains(pos))
                .map(|t| t.workspace),
            window: layout
                .previews
                .iter()
                .rev()
                .find(|p| p.rect.to_f64().contains(pos))
                .map(|p| p.id),
        }
    }
}

/// What the runtime does about a drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DndAction {
    /// Open the overview.
    OpenOverview,
    /// Activate this workspace (the overview stays).
    SwitchWorkspace(u32),
    /// Activate this window, on its workspace, and close the overview.
    ActivateWindow(u64),
    /// Close the overview.
    CloseOverview,
}

/// One drag in flight.
#[derive(Debug)]
struct Session {
    /// The workspace the drag began on (`_lastActiveWorkspaceIndex`).
    start: u32,
    /// The thumbnail the last motion was over.
    thumbnail: Option<u32>,
    /// The window preview the drag rests on, and when it activates.
    window: Option<(u64, Instant)>,
    /// When the Activities button opens the overview.
    activities: Option<Instant>,
}

/// GNOME's XDND handling of the overview, one drag at a time.
#[derive(Debug, Default)]
pub struct DndOverview {
    session: Option<Session>,
}

impl DndOverview {
    /// Whether a drag is being followed.
    pub fn active(&self) -> bool {
        self.session.is_some()
    }

    /// Follow whether a drag is running (`active`), as of `workspace`, the
    /// active workspace, with the overview `overview_open`. Call before
    /// anything else a tick or an input does to the drag: a drag is
    /// noticed on the first call that sees it, and its workspace is the
    /// one that call sees. When it ends, an overview still showing closes
    /// onto the workspace the drag began on.
    pub fn sync(&mut self, active: bool, workspace: u32, overview_open: bool) -> Vec<DndAction> {
        match (active, self.session.take()) {
            (true, None) => {
                self.session = Some(Session {
                    start: workspace,
                    thumbnail: None,
                    window: None,
                    activities: None,
                });
                Vec::new()
            }
            (true, session) => {
                self.session = session;
                Vec::new()
            }
            (false, Some(session)) if overview_open => {
                let mut actions = Vec::new();
                if workspace != session.start {
                    actions.push(DndAction::SwitchWorkspace(session.start));
                }
                actions.push(DndAction::CloseOverview);
                actions
            }
            (false, _) => Vec::new(),
        }
    }

    /// The drag moved to `hover`, with `workspace` active. A thumbnail
    /// entered activates its workspace at once; resting on a window
    /// preview or the Activities button arms their timers, which a move
    /// onto another window or off the button disarms.
    pub fn motion(&mut self, hover: Hover, workspace: u32, now: Instant) -> Vec<DndAction> {
        let Some(session) = self.session.as_mut() else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        // Every motion over the button restarts its wait.
        session.activities = hover.activities.then_some(now + DND_ACTIVITIES_TIMEOUT);
        // The same window keeps the timer it has; another starts afresh.
        session.window = match (hover.window, session.window) {
            (Some(id), Some((armed, due))) if id == armed => Some((id, due)),
            (Some(id), _) => Some((id, now + DND_WINDOW_SWITCH_TIMEOUT)),
            (None, _) => None,
        };
        if hover.thumbnail != session.thumbnail {
            session.thumbnail = hover.thumbnail;
            if let Some(target) = hover.thumbnail.filter(|target| *target != workspace) {
                actions.push(DndAction::SwitchWorkspace(target));
            }
        }
        actions
    }

    /// Run the timers due at `now`. `hover` is what the drag is over now
    /// (the scene may have moved under a still pointer); a timer fires
    /// only onto what it was armed for.
    pub fn poll(&mut self, hover: Hover, overview_open: bool, now: Instant) -> Vec<DndAction> {
        let Some(session) = self.session.as_mut() else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        if session.activities.is_some_and(|due| now >= due) {
            session.activities = None;
            if hover.activities && !overview_open {
                actions.push(DndAction::OpenOverview);
            }
        }
        if let Some((id, _)) = session.window.filter(|(_, due)| now >= *due) {
            session.window = None;
            if overview_open && hover.window == Some(id) {
                actions.push(DndAction::ActivateWindow(id));
            }
        }
        actions
    }

    /// When the next timer is due.
    pub fn deadline(&self) -> Option<Instant> {
        let session = self.session.as_ref()?;
        [session.activities, session.window.map(|(_, due)| due)]
            .into_iter()
            .flatten()
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn over(window: u64) -> Hover {
        Hover {
            window: Some(window),
            ..Hover::default()
        }
    }

    const BUTTON: Hover = Hover {
        activities: true,
        thumbnail: None,
        window: None,
    };

    #[test]
    fn nothing_happens_without_a_drag() {
        let mut dnd = DndOverview::default();
        let now = Instant::now();
        assert!(dnd.motion(BUTTON, 0, now).is_empty());
        assert!(dnd
            .poll(BUTTON, false, now + Duration::from_secs(9))
            .is_empty());
        assert!(dnd.sync(false, 0, true).is_empty());
        assert_eq!(dnd.deadline(), None);
    }

    #[test]
    fn the_button_opens_the_overview_once_the_drag_rests_250_ms() {
        let mut dnd = DndOverview::default();
        dnd.sync(true, 0, false);
        let t0 = Instant::now();
        dnd.motion(BUTTON, 0, t0);
        assert_eq!(dnd.deadline(), Some(t0 + DND_ACTIVITIES_TIMEOUT));
        // Moving on the button restarts the wait.
        let t1 = t0 + Duration::from_millis(200);
        dnd.motion(BUTTON, 0, t1);
        assert!(dnd
            .poll(BUTTON, false, t0 + DND_ACTIVITIES_TIMEOUT)
            .is_empty());
        assert_eq!(
            dnd.poll(BUTTON, false, t1 + DND_ACTIVITIES_TIMEOUT),
            [DndAction::OpenOverview]
        );
        assert_eq!(dnd.deadline(), None);
    }

    #[test]
    fn leaving_the_button_cancels_the_open() {
        let mut dnd = DndOverview::default();
        dnd.sync(true, 0, false);
        let t0 = Instant::now();
        dnd.motion(BUTTON, 0, t0);
        dnd.motion(Hover::default(), 0, t0 + Duration::from_millis(100));
        assert_eq!(dnd.deadline(), None);
        assert!(dnd
            .poll(BUTTON, false, t0 + Duration::from_secs(1))
            .is_empty());
    }

    #[test]
    fn an_open_overview_is_left_open_by_the_button() {
        let mut dnd = DndOverview::default();
        dnd.sync(true, 0, true);
        let t0 = Instant::now();
        dnd.motion(BUTTON, 0, t0);
        assert!(dnd
            .poll(BUTTON, true, t0 + DND_ACTIVITIES_TIMEOUT)
            .is_empty());
    }

    #[test]
    fn a_window_preview_activates_after_750_ms_of_rest() {
        let mut dnd = DndOverview::default();
        dnd.sync(true, 0, true);
        let t0 = Instant::now();
        dnd.motion(over(7), 0, t0);
        // Moving within the same preview keeps the timer it has.
        dnd.motion(over(7), 0, t0 + Duration::from_millis(400));
        assert_eq!(dnd.deadline(), Some(t0 + DND_WINDOW_SWITCH_TIMEOUT));
        assert!(dnd
            .poll(
                over(7),
                true,
                t0 + DND_WINDOW_SWITCH_TIMEOUT - Duration::from_millis(1)
            )
            .is_empty());
        assert_eq!(
            dnd.poll(over(7), true, t0 + DND_WINDOW_SWITCH_TIMEOUT),
            [DndAction::ActivateWindow(7)]
        );
        assert_eq!(dnd.deadline(), None);
    }

    #[test]
    fn another_preview_or_none_restarts_or_disarms_the_window_timer() {
        let mut dnd = DndOverview::default();
        dnd.sync(true, 0, true);
        let t0 = Instant::now();
        dnd.motion(over(7), 0, t0);
        let t1 = t0 + Duration::from_millis(500);
        dnd.motion(over(8), 0, t1);
        assert_eq!(dnd.deadline(), Some(t1 + DND_WINDOW_SWITCH_TIMEOUT));
        dnd.motion(Hover::default(), 0, t1 + Duration::from_millis(10));
        assert_eq!(dnd.deadline(), None);
    }

    #[test]
    fn a_timer_fires_only_onto_what_it_was_armed_for() {
        let mut dnd = DndOverview::default();
        dnd.sync(true, 0, true);
        let t0 = Instant::now();
        dnd.motion(over(7), 0, t0);
        // The scene moved under the pointer: preview 9 is there now.
        assert!(dnd
            .poll(over(9), true, t0 + DND_WINDOW_SWITCH_TIMEOUT)
            .is_empty());
        assert_eq!(dnd.deadline(), None);
    }

    #[test]
    fn entering_a_thumbnail_activates_its_workspace_once() {
        let mut dnd = DndOverview::default();
        dnd.sync(true, 1, true);
        let t0 = Instant::now();
        let thumb = |ws| Hover {
            thumbnail: Some(ws),
            ..Hover::default()
        };
        assert_eq!(dnd.motion(thumb(2), 1, t0), [DndAction::SwitchWorkspace(2)]);
        // Moving inside it does not switch again; neither does the
        // thumbnail of the workspace already active.
        assert!(dnd.motion(thumb(2), 2, t0).is_empty());
        assert!(dnd.motion(Hover::default(), 2, t0).is_empty());
        assert_eq!(dnd.motion(thumb(0), 2, t0), [DndAction::SwitchWorkspace(0)]);
        assert!(dnd.motion(Hover::default(), 0, t0).is_empty());
        assert!(dnd.motion(thumb(0), 0, t0).is_empty());
    }

    #[test]
    fn the_end_returns_an_open_overview_to_where_the_drag_began() {
        let mut dnd = DndOverview::default();
        assert!(dnd.sync(true, 1, false).is_empty());
        assert!(dnd.active());
        // Still running, elsewhere and open: nothing yet.
        assert!(dnd.sync(true, 3, true).is_empty());
        assert_eq!(
            dnd.sync(false, 3, true),
            [DndAction::SwitchWorkspace(1), DndAction::CloseOverview]
        );
        assert!(!dnd.active());
    }

    #[test]
    fn an_end_on_the_starting_workspace_only_closes_the_overview() {
        let mut dnd = DndOverview::default();
        dnd.sync(true, 1, false);
        assert_eq!(dnd.sync(false, 1, true), [DndAction::CloseOverview]);
    }

    #[test]
    fn an_end_after_the_overview_closed_leaves_the_workspace_alone() {
        // A window activated by the drag closed the overview and moved to
        // its workspace: the drop was meant for it (`_onDragEnd` only
        // acts while the overview shows).
        let mut dnd = DndOverview::default();
        dnd.sync(true, 1, true);
        assert!(dnd.sync(false, 3, false).is_empty());
        assert!(!dnd.active());
    }
}
