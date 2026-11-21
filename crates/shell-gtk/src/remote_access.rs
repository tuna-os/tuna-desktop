//! GNOME 51's remote access indicators (`js/ui/status/remoteAccess.js`,
//! #1162 F2) over the compositor's active grants (mutter's
//! `MetaRemoteAccessHandle`s, wire 0.54 `RemoteAccess`):
//!
//! - `ScreenSharingIndicator`: an orange panel button ("Stop Screen
//!   Sharing", `screen-shared-symbolic` and `screencast-stop-symbolic`)
//!   in the panel's right box after the screen recording indicator, shown
//!   while any grant that is not a recording is active (portal screen
//!   casts, remote desktop and its linked cast, an input capture while it
//!   captures). Pressing it stops every one of them (`_stopSharing`); the
//!   compositor revokes each for good. Once shown it stays at least
//!   `MIN_SHARED_INDICATOR_VISIBLE_TIME_US` (5 s), so a short share is
//!   still seen.
//! - `RemoteAccessApplet`: an orange `media-record-symbolic` privacy icon,
//!   first among the system indicators, shown while a recording grant is
//!   active. The screenshot UI's own recording has its red panel button
//!   instead (`ScreenRecordingIndicator`, `screencast.rs`): GNOME leaves it
//!   out by counting it while `screencast_in_progress`; here the
//!   compositor names the shell as its owner, and only the shell's
//!   recorder records through the shell.
//!
//! GNOME 51 adds no quick settings item for either (the applet's
//! `quickSettingsItems` stays empty); the panel button is the Stop.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk4 as gtk;
use tuna_shell_control::{RemoteAccessHandle, RemoteAccessKind, RemoteAccessOwner};

/// The sharing button's accessible name and role (GNOME's
/// `accessible_name`, `Atk.Role.PUSH_BUTTON`).
pub const STOP_SHARING: &str = "Stop Screen Sharing";

/// `MIN_SHARED_INDICATOR_VISIBLE_TIME_US`.
pub const MIN_SHARED_VISIBLE: Duration = Duration::from_secs(5);

/// The grants `ScreenSharingIndicator` holds: every one that is not a
/// recording (`if (handle.isRecording) return;`).
pub fn sharing(handles: &[RemoteAccessHandle]) -> Vec<(RemoteAccessKind, u64)> {
    handles
        .iter()
        .filter(|handle| !handle.recording)
        .map(|handle| (handle.kind, handle.id))
        .collect()
}

/// Whether `RemoteAccessApplet` shows: a recording grant other than the
/// shell's own recorder, which has its own indicator.
pub fn recording(handles: &[RemoteAccessHandle]) -> bool {
    handles
        .iter()
        .any(|handle| handle.recording && handle.owner != RemoteAccessOwner::Shell)
}

/// What `ScreenSharingIndicator._sync` does to the button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sync {
    /// Show it (or keep it shown).
    Show,
    /// Hide it now.
    Hide,
    /// Hide it once it has been shown for the minimum time.
    HideIn(Duration),
    /// Hidden and nothing to share: leave it.
    Stay,
}

/// `_sync` for `sharing` grants, given since when the button shows.
pub fn sync(sharing: bool, shown_since: Option<Instant>, now: Instant) -> Sync {
    match (sharing, shown_since) {
        (true, _) => Sync::Show,
        (false, None) => Sync::Stay,
        (false, Some(since)) => {
            let shown = now.saturating_duration_since(since);
            if shown >= MIN_SHARED_VISIBLE {
                Sync::Hide
            } else {
                Sync::HideIn(MIN_SHARED_VISIBLE - shown)
            }
        }
    }
}

/// Stops one grant (the compositor's `StopRemoteAccess`).
pub type Stop = Rc<dyn Fn(RemoteAccessKind, u64)>;

/// GNOME's two remote access indicators.
pub struct RemoteAccessUi {
    /// `ScreenSharingIndicator`, for the panel's right box.
    pub sharing: gtk::Button,
    /// `RemoteAccessApplet`'s icon, first among the system indicators.
    pub recording: gtk::Image,
    state: Rc<State>,
}

#[derive(Default)]
struct State {
    /// The grants the sharing button stops.
    handles: RefCell<Vec<(RemoteAccessKind, u64)>>,
    /// Since when the sharing button shows (`_visibleTimeUs`).
    shown_since: Cell<Option<Instant>>,
    /// `_hideIndicatorId`.
    hide: RefCell<Option<gtk::glib::SourceId>>,
    /// The control connection the grants came over.
    generation: Cell<Option<u64>>,
}

impl RemoteAccessUi {
    pub fn new(stop: Stop) -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&gtk::Image::from_icon_name("screen-shared-symbolic"));
        row.append(&gtk::Image::from_icon_name("screencast-stop-symbolic"));
        let sharing = gtk::Button::builder().child(&row).build();
        sharing.add_css_class("panel-button");
        sharing.add_css_class("screen-sharing-indicator");
        sharing.update_property(&[gtk::accessible::Property::Label(STOP_SHARING)]);
        sharing.set_visible(false);
        let state = Rc::new(State::default());
        {
            let state = state.clone();
            sharing.connect_clicked(move |_| {
                let handles = state.handles.borrow().clone();
                for (kind, id) in handles {
                    stop(kind, id);
                }
            });
        }
        let recording = gtk::Image::from_icon_name("media-record-symbolic");
        recording.add_css_class("system-status-icon");
        recording.add_css_class("privacy-indicator");
        recording.set_visible(false);
        Self {
            sharing,
            recording,
            state,
        }
    }

    /// A new control connection replays the grants still active, so
    /// those of the last one go.
    pub fn sync_connection(&self, generation: Option<u64>) {
        if self.state.generation.replace(generation) != generation {
            self.update(&[]);
        }
    }

    /// The compositor's whole list of active grants.
    pub fn update(&self, handles: &[RemoteAccessHandle]) {
        self.recording.set_visible(recording(handles));
        let sharing = sharing(handles);
        let any = !sharing.is_empty();
        *self.state.handles.borrow_mut() = sharing;
        if let Some(pending) = self.state.hide.borrow_mut().take() {
            pending.remove();
        }
        let now = Instant::now();
        match sync(any, self.state.shown_since.get(), now) {
            Sync::Show => {
                if self.state.shown_since.get().is_none() {
                    self.state.shown_since.set(Some(now));
                }
                self.sharing.set_visible(true);
            }
            Sync::Hide => hide(&self.sharing, &self.state),
            Sync::HideIn(after) => {
                let (button, state) = (self.sharing.downgrade(), self.state.clone());
                *self.state.hide.borrow_mut() =
                    Some(gtk::glib::timeout_add_local_once(after, move || {
                        state.hide.borrow_mut().take();
                        if let Some(button) = button.upgrade() {
                            hide(&button, &state);
                        }
                    }));
            }
            Sync::Stay => {}
        }
    }
}

/// `_hideIndicator`.
fn hide(button: &gtk::Button, state: &State) {
    button.set_visible(false);
    state.shown_since.set(None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::RemoteAccessTarget;

    fn handle(
        kind: RemoteAccessKind,
        id: u64,
        owner: RemoteAccessOwner,
        recording: bool,
    ) -> RemoteAccessHandle {
        RemoteAccessHandle {
            kind,
            id,
            owner,
            owner_name: ":1.7".into(),
            recording,
            targets: vec![RemoteAccessTarget::Input],
        }
    }

    #[test]
    fn sharing_holds_every_grant_but_recordings() {
        let handles = [
            handle(
                RemoteAccessKind::ScreenCast,
                3,
                RemoteAccessOwner::Portal,
                false,
            ),
            handle(
                RemoteAccessKind::ScreenCast,
                4,
                RemoteAccessOwner::Shell,
                true,
            ),
            handle(
                RemoteAccessKind::RemoteDesktop,
                5,
                RemoteAccessOwner::RemoteDesktopDaemon,
                false,
            ),
            handle(
                RemoteAccessKind::InputCapture,
                1,
                RemoteAccessOwner::Portal,
                false,
            ),
        ];
        assert_eq!(
            sharing(&handles),
            vec![
                (RemoteAccessKind::ScreenCast, 3),
                (RemoteAccessKind::RemoteDesktop, 5),
                (RemoteAccessKind::InputCapture, 1),
            ]
        );
        assert!(sharing(&[]).is_empty());
    }

    #[test]
    fn the_record_icon_leaves_out_the_shells_own_recorder() {
        let own = handle(
            RemoteAccessKind::ScreenCast,
            4,
            RemoteAccessOwner::Shell,
            true,
        );
        let other = handle(
            RemoteAccessKind::ScreenCast,
            6,
            RemoteAccessOwner::Portal,
            true,
        );
        let share = handle(
            RemoteAccessKind::ScreenCast,
            3,
            RemoteAccessOwner::Portal,
            false,
        );
        assert!(
            !recording(std::slice::from_ref(&own)),
            "the red button shows it"
        );
        assert!(recording(&[own, other.clone()]));
        assert!(recording(&[other]));
        assert!(!recording(&[share]), "a share is the sharing button's");
        assert!(!recording(&[]));
    }

    #[test]
    fn the_sharing_button_stays_shown_five_seconds() {
        let now = Instant::now();
        assert_eq!(sync(true, None, now), Sync::Show);
        assert_eq!(
            sync(true, Some(now), now + Duration::from_secs(9)),
            Sync::Show
        );
        assert_eq!(sync(false, None, now), Sync::Stay);
        assert_eq!(
            sync(false, Some(now), now + Duration::from_secs(2)),
            Sync::HideIn(Duration::from_secs(3))
        );
        assert_eq!(sync(false, Some(now), now + MIN_SHARED_VISIBLE), Sync::Hide);
    }

    /// The real widgets: the button shows with its accessible name while
    /// a share is active, Stop asks the compositor to stop every shared
    /// grant, and the button hides once none is left and it has shown five
    /// seconds; the record icon follows recordings.
    #[test]
    #[ignore = "requires owned Xvfb/private D-Bus and explicit remote access indicator admission"]
    fn actual_gtk_indicator_shows_stops_and_hides() {
        assert_eq!(
            std::env::var("TUNA_REQUIRE_REMOTE_ACCESS_INDICATOR").as_deref(),
            Ok("1")
        );
        assert!(std::env::var_os("WAYLAND_DISPLAY").is_none());
        gtk::init().expect("actual GTK display required");
        let stopped: Rc<RefCell<Vec<(RemoteAccessKind, u64)>>> = Rc::default();
        let ui = {
            let stopped = stopped.clone();
            RemoteAccessUi::new(Rc::new(move |kind, id| {
                stopped.borrow_mut().push((kind, id))
            }))
        };
        let window = gtk::Window::new();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.append(&ui.recording);
        row.append(&ui.sharing);
        window.set_child(Some(&row));
        window.present();
        let drain = |seconds: f64| {
            let end = Instant::now() + Duration::from_secs_f64(seconds);
            while Instant::now() < end {
                while gtk::glib::MainContext::default().iteration(false) {}
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        assert!(!ui.sharing.is_visible() && !ui.recording.is_visible());
        let share = handle(
            RemoteAccessKind::ScreenCast,
            3,
            RemoteAccessOwner::Portal,
            false,
        );
        let remote = handle(
            RemoteAccessKind::RemoteDesktop,
            5,
            RemoteAccessOwner::RemoteDesktopDaemon,
            false,
        );
        let own = handle(
            RemoteAccessKind::ScreenCast,
            4,
            RemoteAccessOwner::Shell,
            true,
        );
        ui.update(&[share.clone(), remote.clone(), own.clone()]);
        drain(0.1);
        assert!(ui.sharing.is_visible() && ui.sharing.is_mapped());
        assert!(
            !ui.recording.is_visible(),
            "the shell's recorder has its own"
        );
        assert_eq!(ui.sharing.accessible_role(), gtk::AccessibleRole::Button);
        ui.sharing.emit_clicked();
        assert_eq!(
            *stopped.borrow(),
            vec![
                (RemoteAccessKind::ScreenCast, 3),
                (RemoteAccessKind::RemoteDesktop, 5)
            ]
        );
        // The compositor revoked them; the button stays its five seconds.
        ui.update(&[own]);
        drain(0.5);
        assert!(ui.sharing.is_visible(), "shown under five seconds");
        drain(4.8);
        assert!(!ui.sharing.is_visible(), "hidden after five seconds");
        // A recording by another owner shows the record icon only.
        ui.update(&[handle(
            RemoteAccessKind::ScreenCast,
            6,
            RemoteAccessOwner::Portal,
            true,
        )]);
        drain(0.1);
        assert!(ui.recording.is_visible() && !ui.sharing.is_visible());
        // A new share shows the button again.
        ui.update(&[share]);
        drain(0.1);
        assert!(ui.sharing.is_visible() && !ui.recording.is_visible());
        // A new control connection drops what the old one said: the
        // button goes after its five seconds.
        ui.sync_connection(Some(1));
        ui.sync_connection(Some(1));
        drain(5.2);
        assert!(!ui.sharing.is_visible());
        println!("REMOTE-ACCESS-INDICATOR pass Stop Screen Sharing shows for shares and remote control, stops each, stays five seconds; the record icon follows other owners' recordings");
    }
}
