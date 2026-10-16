//! Keyboard focus into the top bar (P-A11Y-02).
//!
//! GNOME 51 reaches panel controls without a pointer through
//! Ctrl+Alt+Tab ("Switch system controls", `switch-panels`): a group
//! switcher whose panel choice moves keyboard focus into the top bar
//! (gnome-shell 51.0 `js/ui/ctrlAltTab.js`, bound in
//! `js/ui/windowManager.js`). With a single bar the chord focuses it
//! directly: the panel surface takes exclusive keyboard, the first
//! (forward) or last (backward) control takes GTK focus, and
//! Tab/arrows walk every control — tray icons, the accessibility
//! menu and the screencast stop button included. Enter activates;
//! Escape closes the open panel menu, and a second Escape returns
//! focus to the client. A menu closing any other way (item
//! activation, outside click) also returns focus, as in GNOME.

/// Which end of the bar takes focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusEnd {
    First,
    Last,
}

/// Which end `switch-panels` (`backward`: `switch-panels-backward`)
/// focuses.
pub fn focus_end(backward: bool) -> FocusEnd {
    if backward {
        FocusEnd::Last
    } else {
        FocusEnd::First
    }
}

/// What Escape does on the panel window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeOutcome {
    /// A panel menu is open: close the menus, keep panel focus.
    CloseMenus,
    /// No menu open: give the keyboard back to the client.
    Release,
    /// Panel focus is not held: not our key.
    Ignore,
}

/// Escape with `panel_focus` held and `menu_open` reporting whether
/// any panel popover is up.
pub fn escape(panel_focus: bool, menu_open: bool) -> EscapeOutcome {
    match (panel_focus, menu_open) {
        (false, _) => EscapeOutcome::Ignore,
        (true, true) => EscapeOutcome::CloseMenus,
        (true, false) => EscapeOutcome::Release,
    }
}

/// Timing-independent guard matching our own Escape popdowns to the
/// popover `closed` signals they produce.
///
/// The old synchronous `escaping` flag only covered `closed` signals
/// that fired inside the Escape handler. Gtk can emit `closed` after
/// the handler returns; that late signal then dropped the whole panel
/// hold, which both fired a genuine NORMAL chord later (journey
/// G-PANEL-FOCUS) and cleared AT-SPI focus early (gtk-shell
/// G-PANEL-FOCUS). Counting one credit per popdown of a visible
/// popover makes the match independent of signal timing: each
/// `closed` spends at most one credit, and a `show` spends stale
/// credit so an unconsumed close can never swallow a later genuine
/// close.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EscapeGuard {
    outstanding: u32,
}

impl EscapeGuard {
    /// One of our popdowns is about to close a visible popover: its
    /// `closed` signal must not release the hold, whenever it lands.
    pub fn note_escape_close(&mut self) {
        self.outstanding = self.outstanding.saturating_add(1);
    }

    /// A popover became visible: any credit older than this show is
    /// stale (its close never arrived), so drop one.
    pub fn note_shown(&mut self) {
        self.outstanding = self.outstanding.saturating_sub(1);
    }

    /// Whether a popover `closed` signal returns focus to the client:
    /// only when panel focus is held and no Escape credit covers it.
    pub fn closed(&mut self, panel_focus: bool) -> bool {
        if self.outstanding > 0 {
            self.outstanding -= 1;
            return false;
        }
        panel_focus
    }
}

/// Whether Left/Right walk the bar's controls: panel focus held, no
/// menu open (an open menu owns the arrows), and no text entry
/// focused (entries live in popovers, but stay safe).
pub fn arrows_walk_bar(panel_focus: bool, menu_open: bool) -> bool {
    panel_focus && !menu_open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ends_follow_direction() {
        assert_eq!(focus_end(false), FocusEnd::First);
        assert_eq!(focus_end(true), FocusEnd::Last);
    }

    #[test]
    fn escape_unwinds_menus_then_releases() {
        assert_eq!(escape(false, false), EscapeOutcome::Ignore);
        assert_eq!(escape(false, true), EscapeOutcome::Ignore);
        assert_eq!(escape(true, true), EscapeOutcome::CloseMenus);
        assert_eq!(escape(true, false), EscapeOutcome::Release);
    }

    #[test]
    fn escape_close_credit_survives_a_late_closed_signal() {
        // The G-PANEL-FOCUS regression, both ways: on the old code
        // `release_on_popdown(true, false)` returned true (the hold
        // dropped); the guard spends the Escape credit instead.
        let mut guard = EscapeGuard::default();
        guard.note_escape_close();
        assert!(!guard.closed(true));
        // Credit spent: a further close with the hold live releases.
        assert!(guard.closed(true));
    }

    #[test]
    fn close_without_hold_never_releases() {
        let mut guard = EscapeGuard::default();
        assert!(!guard.closed(false));
    }

    #[test]
    fn foreign_close_releases_a_held_bar() {
        // Item activation / outside click: no Escape credit, hold live.
        let mut guard = EscapeGuard::default();
        assert!(guard.closed(true));
    }

    #[test]
    fn shown_bounds_stale_credit() {
        let mut guard = EscapeGuard::default();
        guard.note_escape_close();
        guard.note_shown();
        assert!(guard.closed(true));
    }

    #[test]
    fn two_popdowns_spend_two_credits() {
        let mut guard = EscapeGuard::default();
        guard.note_escape_close();
        guard.note_escape_close();
        assert!(!guard.closed(true));
        assert!(!guard.closed(true));
        assert!(guard.closed(true));
    }

    #[test]
    fn arrows_walk_only_a_menu_free_bar() {
        assert!(!arrows_walk_bar(false, false));
        assert!(!arrows_walk_bar(false, true));
        assert!(!arrows_walk_bar(true, true));
        assert!(arrows_walk_bar(true, false));
    }
}
