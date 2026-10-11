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

/// Whether a panel popover closing returns focus to the client: only
/// when panel focus is held and the close did not come from our own
/// Escape handling (which keeps focus on the owning button).
pub fn release_on_popdown(panel_focus: bool, escape_closing: bool) -> bool {
    panel_focus && !escape_closing
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
    fn popdown_releases_unless_escape_is_closing() {
        assert!(!release_on_popdown(false, false));
        assert!(!release_on_popdown(false, true));
        assert!(!release_on_popdown(true, true));
        assert!(release_on_popdown(true, false));
    }

    #[test]
    fn arrows_walk_only_a_menu_free_bar() {
        assert!(!arrows_walk_bar(false, false));
        assert!(!arrows_walk_bar(false, true));
        assert!(!arrows_walk_bar(true, true));
        assert!(arrows_walk_bar(true, false));
    }
}
