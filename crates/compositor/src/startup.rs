//! Startup notification: GNOME's busy cursor while an app launch is
//! pending (#1193).
//!
//! Mutter 51 keeps a list of startup sequences
//! (`src/core/startup-notification.c`) and, while any is not complete,
//! shows `CLUTTER_CURSOR_WAIT` everywhere the compositor picks a theme
//! cursor (`src/compositor/compositor.c:1132-1146`,
//! `meta_compositor_update_global_cursor`, applied in
//! `meta_backend_get_cursor`, `src/backends/meta-backend.c:2164-2177`).
//! Sequences come from:
//!
//! - every committed xdg-activation token (`token_commit`,
//!   `src/wayland/meta-wayland-activation.c:154-196`), completed when a
//!   window activates with it (`maybe_activate`, `:328-375`);
//! - the shell's app launches (`meta_launch_context_get_startup_notify_id`,
//!   `src/core/meta-launch-context.c:129-189`), which Tuna's shell reports
//!   with `BeginStartupSequence`. Tuna does not hand those launches a token,
//!   so they complete when a window of the app maps, as mutter completes a
//!   sequence whose WM class a mapping window carries
//!   (`find_startup_sequence_by_wmclass`, `src/core/display.c:2620-2688`).
//!
//! A sequence still pending after `STARTUP_TIMEOUT_MS` (15 s,
//! `startup-notification.c:39,466-512`) times out. Mutter polls that once a
//! second; Tuna expires it at its exact deadline.

use std::time::{Duration, Instant};

/// Mutter 51's `STARTUP_TIMEOUT_MS`.
pub const STARTUP_TIMEOUT: Duration = Duration::from_millis(15_000);

/// Sequences kept at once; the oldest is dropped past this. Tokens are
/// minted only for the focused client with an input serial, and the
/// shell's launches follow clicks, so this bound is never reached in
/// practice; it caps what a misbehaving peer can hold.
pub const MAX_SEQUENCES: usize = 64;

/// One pending launch.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sequence {
    /// The activation token, or `None` for a shell launch.
    token: Option<String>,
    /// Desktop id (shell launches) or the token's `set_app_id`.
    app_id: Option<String>,
    /// The launched app's `StartupWMClass`.
    wm_class: Option<String>,
    started: Instant,
}

/// Mutter's `MetaStartupNotification`: the pending startup sequences.
#[derive(Debug, Default)]
pub struct StartupNotification {
    sequences: Vec<Sequence>,
}

impl StartupNotification {
    /// `token_commit`: a committed xdg-activation token starts a sequence.
    pub fn add_token(&mut self, token: String, app_id: Option<String>, now: Instant) {
        self.push(Sequence {
            token: Some(token),
            app_id,
            wm_class: None,
            started: now,
        });
    }

    /// A shell app launch starts a sequence for `app_id` (and its
    /// `StartupWMClass`).
    pub fn add_launch(&mut self, app_id: String, wm_class: Option<String>, now: Instant) {
        self.push(Sequence {
            token: None,
            app_id: Some(app_id),
            wm_class,
            started: now,
        });
    }

    fn push(&mut self, sequence: Sequence) {
        if self.sequences.len() >= MAX_SEQUENCES {
            self.sequences.remove(0);
        }
        self.sequences.push(sequence);
    }

    /// `maybe_activate`: a window activated with `token` completes its
    /// sequence. Whether one was pending.
    pub fn complete_token(&mut self, token: &str) -> bool {
        let before = self.sequences.len();
        self.sequences
            .retain(|sequence| sequence.token.as_deref() != Some(token));
        self.sequences.len() != before
    }

    /// A window mapped: the oldest shell launch whose app id or WM class
    /// is one of `names` (a Wayland app id; an X11 window's class, instance
    /// and derived app id) completes. Desktop ids and app ids are compared
    /// without ASCII case, as X11 classes are often capitalised. Whether one
    /// completed.
    pub fn window_mapped<'a>(&mut self, names: impl IntoIterator<Item = &'a str> + Clone) -> bool {
        let matches = |wanted: &Option<String>| {
            wanted.as_deref().is_some_and(|wanted| {
                names
                    .clone()
                    .into_iter()
                    .any(|name| !name.is_empty() && name.eq_ignore_ascii_case(wanted))
            })
        };
        let Some(index) = self.sequences.iter().position(|sequence| {
            sequence.token.is_none() && (matches(&sequence.app_id) || matches(&sequence.wm_class))
        }) else {
            return false;
        };
        self.sequences.remove(index);
        true
    }

    /// `startup_sequence_timeout`: drop sequences older than
    /// [`STARTUP_TIMEOUT`]. Whether any timed out.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.sequences.len();
        self.sequences
            .retain(|sequence| now.saturating_duration_since(sequence.started) <= STARTUP_TIMEOUT);
        self.sequences.len() != before
    }

    /// `meta_startup_notification_has_pending_sequences`, as of `now`.
    pub fn pending(&self, now: Instant) -> bool {
        self.sequences
            .iter()
            .any(|sequence| now.saturating_duration_since(sequence.started) <= STARTUP_TIMEOUT)
    }

    /// When the oldest pending sequence times out.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.sequences
            .iter()
            .map(|sequence| sequence.started + STARTUP_TIMEOUT)
            .min()
    }

    /// For the state snapshot: pending sequences' app ids (empty when
    /// unknown), oldest first.
    pub fn snapshot(&self, now: Instant) -> serde_json::Value {
        let pending: Vec<_> = self
            .sequences
            .iter()
            .filter(|sequence| now.saturating_duration_since(sequence.started) <= STARTUP_TIMEOUT)
            .map(|sequence| {
                serde_json::json!({
                    "app_id": sequence.app_id.clone().unwrap_or_default(),
                    "source": if sequence.token.is_some() { "activation" } else { "launch" },
                })
            })
            .collect();
        serde_json::json!({ "busy": !pending.is_empty(), "pending": pending })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_is_pending_until_its_window_maps() {
        let now = Instant::now();
        let mut sn = StartupNotification::default();
        assert!(!sn.pending(now));
        sn.add_launch("org.gnome.TextEditor".into(), None, now);
        assert!(sn.pending(now));
        assert!(!sn.window_mapped(["org.gnome.Nautilus"]), "another app");
        assert!(sn.pending(now));
        assert!(sn.window_mapped(["org.gnome.texteditor"]));
        assert!(!sn.pending(now));
    }

    #[test]
    fn startup_wm_class_and_x11_names_complete_a_launch() {
        let now = Instant::now();
        let mut sn = StartupNotification::default();
        sn.add_launch("firefox".into(), Some("Navigator".into()), now);
        sn.add_launch("xterm".into(), None, now);
        assert!(sn.window_mapped(["XTerm", "xterm"]));
        assert!(sn.window_mapped(["Firefox-esr", "Navigator"]));
        assert!(!sn.pending(now));
    }

    #[test]
    fn tokens_complete_on_activation_not_on_map() {
        let now = Instant::now();
        let mut sn = StartupNotification::default();
        sn.add_token("t1".into(), Some("org.gnome.Calculator".into()), now);
        assert!(!sn.window_mapped(["org.gnome.Calculator"]));
        assert!(sn.pending(now));
        assert!(!sn.complete_token("t2"));
        assert!(sn.complete_token("t1"));
        assert!(!sn.pending(now));
    }

    /// `STARTUP_TIMEOUT_MS` is 15000 in mutter 51.
    #[test]
    fn sequences_time_out_after_fifteen_seconds() {
        let now = Instant::now();
        let mut sn = StartupNotification::default();
        sn.add_launch("org.gnome.Maps".into(), None, now);
        assert_eq!(sn.next_deadline(), Some(now + Duration::from_secs(15)));
        let edge = now + STARTUP_TIMEOUT;
        assert!(sn.pending(edge));
        assert!(!sn.expire(edge));
        let past = edge + Duration::from_millis(1);
        assert!(
            !sn.pending(past),
            "busy ends at the deadline even before expire runs"
        );
        assert!(sn.expire(past));
        assert_eq!(sn.next_deadline(), None);
        assert_eq!(sn.snapshot(past)["busy"], false);
    }

    #[test]
    fn the_list_is_bounded() {
        let now = Instant::now();
        let mut sn = StartupNotification::default();
        for n in 0..MAX_SEQUENCES + 5 {
            sn.add_token(format!("t{n}"), None, now);
        }
        assert_eq!(sn.sequences.len(), MAX_SEQUENCES);
        assert!(!sn.complete_token("t0"), "the oldest went first");
        assert!(sn.complete_token(&format!("t{}", MAX_SEQUENCES + 4)));
    }
}
