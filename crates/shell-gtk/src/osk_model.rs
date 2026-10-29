//! On-screen keyboard state, ported from GNOME Shell 51.0's
//! `js/ui/keyboard.js` (`Keyboard` key handling, `open`/`close` and their
//! rest timer) and `js/misc/inputMethod.js` (the focus-out hide delay).
//!
//! Pure state machines: [`Keyboard`] turns key presses into [`Effect`]s the
//! view forwards to the compositor; [`Presence`] decides when the keyboard
//! is shown on an explicit millisecond clock.

use crate::osk_layout::{self, hints, Action, Key, Layout, Level, Purpose};

/// `KEYBOARD_REST_TIME` (`KEYBOARD_ANIMATION_TIME * 2`).
pub const KEYBOARD_REST_TIME_MS: u64 = 300;
/// inputMethod.js `HIDE_PANEL_TIME`.
pub const HIDE_PANEL_TIME_MS: u64 = 50;
/// `Clutter.KEY_BackSpace`.
pub const KEYVAL_BACKSPACE: u32 = 0xff08;

/// What a key interaction asks of the outside world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// `KeyboardController.commit(str, modifiers)`.
    Commit {
        text: String,
        modifiers: Vec<u32>,
    },
    /// `keyvalPress` then `keyvalRelease`.
    Keyval(u32),
    KeyvalPress(u32),
    KeyvalRelease(u32),
    /// The hide key: `close(true)`.
    Hide,
    /// The language key: `_popupLanguageMenu`.
    LanguageMenu,
    /// The emoji key: `_toggleEmoji`. [`Keyboard::emoji_active`] has
    /// already been flipped when this is returned.
    ToggleEmoji,
}

/// A key of the current level: `level().rows[row][index]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyRef {
    pub row: usize,
    pub index: usize,
}

/// `Keyboard`'s layout and key state.
pub struct Keyboard {
    layout: Layout,
    group: String,
    purpose: Purpose,
    hints: u32,
    /// `_emojiVisible`, fixed when the keys were built.
    emoji_visible: bool,
    level: String,
    latched: bool,
    /// `_modifiers`: a JS Set, so insertion-ordered without duplicates.
    modifiers: Vec<u32>,
    emoji_active: bool,
    long_pressed: bool,
    /// The delete key's long press sent a BackSpace press not yet released
    /// (`KeyboardController._backspacePressed`).
    delete_held: bool,
}

fn should_show_emoji(purpose: Purpose, hints: u32) -> bool {
    hints & hints::NO_EMOJI == 0
        && matches!(
            purpose,
            Purpose::Normal | Purpose::Alpha | Purpose::Password | Purpose::Terminal
        )
}

impl Keyboard {
    /// Load the layout for `group` and `purpose` (`_updateKeys`); `None`
    /// when no candidate layout loads.
    pub fn new(group: &str, purpose: Purpose, hints: u32) -> Option<Self> {
        let layout = osk_layout::resolve(group, purpose)?;
        Some(Self::with_layout(layout, group, purpose, hints))
    }

    /// A keyboard on an already loaded layout, starting on its `default`
    /// level with the content hints applied.
    pub fn with_layout(layout: Layout, group: &str, purpose: Purpose, hints: u32) -> Self {
        let mut keyboard = Self {
            layout,
            group: group.to_owned(),
            purpose,
            hints,
            emoji_visible: should_show_emoji(purpose, hints),
            level: String::new(),
            latched: false,
            modifiers: Vec::new(),
            emoji_active: false,
            long_pressed: false,
            delete_held: false,
        };
        keyboard.reset_level();
        keyboard.update_level_from_hints(false);
        keyboard
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn level_name(&self) -> &str {
        &self.level
    }

    pub fn level(&self) -> &Level {
        self.layout
            .level(&self.level)
            .unwrap_or(&self.layout.levels[0])
    }

    pub fn latched(&self) -> bool {
        self.latched
    }

    pub fn modifier_active(&self, keyval: u32) -> bool {
        self.modifiers.contains(&keyval)
    }

    /// The active modifiers, in the order they were enabled.
    pub fn modifiers(&self) -> &[u32] {
        &self.modifiers
    }

    pub fn emoji_active(&self) -> bool {
        self.emoji_active
    }

    /// `_emojiVisible` (`_shouldShowEmoji` when the keys were last built):
    /// pass it to [`osk_layout::place`].
    pub fn emoji_visible(&self) -> bool {
        self.emoji_visible
    }

    /// `_onGroupChanged` → `_updateKeys`. Returns whether the layout changed.
    pub fn set_group(&mut self, group: &str) -> bool {
        self.group = group.to_owned();
        let old = self.layout.clone();
        self.update_keys();
        self.layout != old
    }

    /// `purpose-changed` → `_updateKeys`, then `_onContentHintsChanged`
    /// without its INHIBIT_OSK close ([`Presence`] owns visibility): rebuild
    /// the keys if emoji visibility flips, else `_updateLevelFromHints(false)`.
    /// Returns whether the keys were rebuilt.
    pub fn set_content(&mut self, purpose: Purpose, hints: u32) -> bool {
        let mut rebuilt = false;
        if purpose != self.purpose {
            self.purpose = purpose;
            rebuilt = self.update_keys();
        }
        self.hints = hints;
        if should_show_emoji(self.purpose, self.hints) != self.emoji_visible {
            rebuilt |= self.update_keys();
        } else {
            self.update_level_from_hints(false);
        }
        rebuilt
    }

    /// A key of the current level released: the `released`/`keyval`/
    /// `commit` handlers `_addRowKeys` connects.
    pub fn release(&mut self, key: KeyRef) -> Vec<Effect> {
        let Some(key) = self.key(key).cloned() else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        if let Some(action) = &key.action {
            match action {
                Action::Hide => {
                    effects.push(Effect::Hide);
                    self.update_level_from_hints(true);
                }
                Action::LanguageMenu => effects.push(Effect::LanguageMenu),
                Action::Emoji => {
                    self.emoji_active = !self.emoji_active;
                    effects.push(Effect::ToggleEmoji);
                }
                Action::Modifier => {
                    if let Some(keyval) = key.keyval {
                        self.toggle_modifier(keyval);
                    }
                }
                Action::Delete => {
                    // toggleDelete(true); toggleDelete(false) on the
                    // no-IM-focus keypress fallback: a held delete only
                    // releases, otherwise a full BackSpace click.
                    if self.delete_held {
                        self.delete_held = false;
                        effects.push(Effect::KeyvalRelease(KEYVAL_BACKSPACE));
                    } else {
                        effects.push(Effect::Keyval(KEYVAL_BACKSPACE));
                    }
                    self.update_level_from_hints(true);
                }
                Action::LevelSwitch if !self.long_pressed => {
                    if let Some(level) = &key.level {
                        self.set_active_level(level);
                    }
                    // GNOME 51: `_setLatched(key.level === 1 && iconName ===
                    // 'osk-caps-lock-symbolic')`. `level` is a string, so this
                    // is always false: a level switch click always unlatches.
                    self.latched = false;
                }
                Action::LevelSwitch | Action::Other(_) => {}
            }
            self.long_pressed = false;
        } else if let Some(keyval) = key.keyval {
            effects.push(Effect::Keyval(keyval));
            self.update_level_from_hints(true);
        } else if let Some(text) = key.commit_string() {
            effects = self.commit(text);
        }
        effects
    }

    /// A key of the current level long-pressed (the `long-press` handlers
    /// `_addRowKeys` connects). Other keys do nothing here: the view shows
    /// their extended keys.
    pub fn long_press(&mut self, key: KeyRef) -> Vec<Effect> {
        let Some(key) = self.key(key).cloned() else {
            return Vec::new();
        };
        match key.action {
            Some(Action::LevelSwitch)
                if key.icon_name.as_deref() == Some("osk-shift-symbolic")
                    && key.level.as_deref() == Some("shift") =>
            {
                self.set_active_level("shift");
                self.latched = true;
                self.long_pressed = true;
                Vec::new()
            }
            Some(Action::Delete) if !self.delete_held => {
                // toggleDelete(true)
                self.delete_held = true;
                vec![Effect::KeyvalPress(KEYVAL_BACKSPACE)]
            }
            _ => Vec::new(),
        }
    }

    /// An extended (long-press popup) key clicked: commits like a string key.
    pub fn extended(&mut self, text: &str) -> Vec<Effect> {
        self.commit(text)
    }

    /// EmojiSelection `emoji-selected`: commit with no modifiers, nothing else.
    pub fn emoji(&mut self, text: &str) -> Vec<Effect> {
        vec![Effect::Commit {
            text: text.to_owned(),
            modifiers: Vec::new(),
        }]
    }

    /// `_setEmojiActive`.
    pub fn set_emoji_active(&mut self, active: bool) {
        self.emoji_active = active;
    }

    /// `_open`: the keyboard always opens on the keys, not the emoji.
    pub fn opened(&mut self) {
        self.set_emoji_active(false);
    }

    /// `_close`: `_disableAllModifiers`, and release a delete still held.
    pub fn closed(&mut self) -> Vec<Effect> {
        self.modifiers.clear();
        if self.delete_held {
            self.delete_held = false;
            vec![Effect::KeyvalRelease(KEYVAL_BACKSPACE)]
        } else {
            Vec::new()
        }
    }

    fn key(&self, key: KeyRef) -> Option<&Key> {
        self.level().rows.get(key.row)?.get(key.index)
    }

    /// The commit handler: commit with the active modifiers, then
    /// `_disableAllModifiers` and `_updateLevelFromHints(true)`.
    fn commit(&mut self, text: &str) -> Vec<Effect> {
        let effect = Effect::Commit {
            text: text.to_owned(),
            modifiers: self.modifiers.clone(),
        };
        self.modifiers.clear();
        self.update_level_from_hints(true);
        vec![effect]
    }

    /// `_toggleModifier`.
    fn toggle_modifier(&mut self, keyval: u32) {
        if let Some(i) = self.modifiers.iter().position(|&m| m == keyval) {
            self.modifiers.remove(i);
        } else {
            self.modifiers.push(keyval);
        }
    }

    /// `_updateKeys`: reload the layout for the group and purpose, then
    /// `_setActiveLevel('default')`. When nothing loads GNOME keeps the old
    /// keys; so does this. Returns whether the keys were rebuilt.
    fn update_keys(&mut self) -> bool {
        let Some(layout) = osk_layout::resolve(&self.group, self.purpose) else {
            self.set_active_level("default");
            return false;
        };
        self.layout = layout;
        self.emoji_visible = should_show_emoji(self.purpose, self.hints);
        // The new level is always a different page in GNOME, so
        // `_setActiveLevel` unlatches and clears the modifiers.
        self.reset_level();
        true
    }

    fn reset_level(&mut self) {
        self.latched = false;
        self.modifiers.clear();
        self.level = if self.layout.level("default").is_some() {
            "default".to_owned()
        } else {
            self.layout.levels[0].level.clone()
        };
    }

    /// `_setActiveLevel`: switching unlatches the level and disables all
    /// modifiers. Unknown levels are ignored (GNOME would throw).
    ///
    /// GNOME only clears the latch *display* here and leaves `_latched` to
    /// the caller's `_setLatched`; every caller but `_updateKeys` sets it
    /// right after, so clearing the flag here only differs there, where
    /// GNOME would leave a stale latch on the new default level.
    fn set_active_level(&mut self, name: &str) {
        if self.level == name || self.layout.level(name).is_none() {
            return;
        }
        self.latched = false;
        self.modifiers.clear();
        self.level = name.to_owned();
    }

    /// `_updateLevelFromHints`.
    ///
    /// Deviation: for AUTO_CAPITALIZATION/TITLECASE GNOME requests the
    /// surrounding text and picks shift at the start of a sentence/word.
    /// Tuna does not forward surrounding text to the shell, so the level is
    /// left unchanged for those hints.
    fn update_level_from_hints(&mut self, user_input: bool) {
        if self.latched {
            return;
        }
        if self.hints & hints::LOWERCASE != 0 {
            self.set_active_level("default");
            return;
        }
        if self.layout.level("shift").is_none() {
            return;
        }
        if self.hints & hints::UPPERCASE != 0 {
            self.set_active_level("shift");
            return;
        }
        if self.hints & (hints::AUTO_CAPITALIZATION | hints::TITLECASE) != 0 {
            return;
        }
        if user_input && self.level == "shift" {
            self.set_active_level("default");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingKind {
    Show,
    Hide,
}

/// When the keyboard is on screen: `Keyboard.open`/`close` with their rest
/// timer, `KeyboardManager._syncEnabled`, and inputMethod.js's delayed
/// panel hide on focus out.
#[derive(Debug, Clone)]
pub struct Presence {
    enabled: bool,
    /// `_keyboardRequested`.
    requested: bool,
    /// `_keyboardVisible`.
    visible: bool,
    /// `_keyboardRestingId`.
    pending: Option<(u64, PendingKind)>,
    /// `InputMethod._hidePanelId`.
    hide_panel_at: Option<u64>,
}

impl Default for Presence {
    fn default() -> Self {
        Self::new()
    }
}

impl Presence {
    /// Enabled and hidden.
    pub fn new() -> Self {
        Self {
            enabled: true,
            requested: false,
            visible: false,
            pending: None,
            hide_panel_at: None,
        }
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    /// The next time [`Presence::tick`] has work to do.
    pub fn deadline(&self) -> Option<u64> {
        match (self.pending.map(|(t, _)| t), self.hide_panel_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// `KeyboardManager._syncEnabled`: disabling destroys the keyboard,
    /// hiding it at once and dropping every timer.
    pub fn set_enabled(&mut self, enabled: bool, _now: u64) {
        self.enabled = enabled;
        if !enabled {
            self.requested = false;
            self.visible = false;
            self.pending = None;
            self.hide_panel_at = None;
        }
    }

    /// Text input focus changed or was updated. `focused`: `vfunc_focus_in`
    /// cancels a pending panel hide; `show` (the client asked for the panel)
    /// opens unless INHIBIT_OSK; INHIBIT_OSK while visible closes
    /// (`_onContentHintsChanged`). Losing focus (`vfunc_focus_out`) hides
    /// the panel after [`HIDE_PANEL_TIME_MS`].
    pub fn focus(&mut self, focused: bool, show: bool, hints: u32, now: u64) {
        if !self.enabled {
            return;
        }
        let inhibit = hints & hints::INHIBIT_OSK != 0;
        if focused {
            self.hide_panel_at = None;
            if inhibit {
                if self.visible {
                    self.close(false, now);
                }
            } else if show {
                self.open(false, now);
            }
        } else if self.hide_panel_at.is_none() {
            self.hide_panel_at = Some(now + HIDE_PANEL_TIME_MS);
        }
    }

    /// The hide key: `close(true)`.
    pub fn hide_key(&mut self, now: u64) {
        if self.enabled {
            self.close(true, now);
        }
    }

    /// `open(true)`, e.g. for the lock screen prompt.
    pub fn open_now(&mut self, now: u64) {
        if self.enabled {
            self.open(true, now);
        }
    }

    /// `_onKeyFocusChanged` for the shell's own text (the unlock prompt):
    /// a focused text field opens after the rest time, anything else
    /// closes after it.
    pub fn key_focus(&mut self, text: bool, now: u64) {
        if !self.enabled {
            return;
        }
        if text {
            self.open(false, now);
        } else {
            self.close(false, now);
        }
    }

    /// Fire due timers; returns whether visibility changed.
    pub fn tick(&mut self, now: u64) -> bool {
        let was = self.visible;
        if self.hide_panel_at.is_some_and(|t| t <= now) {
            // set_input_panel_state(OFF) → _onKeyboardStateChanged → close(true)
            self.hide_panel_at = None;
            self.close(true, now);
        }
        if let Some((t, kind)) = self.pending {
            if t <= now {
                self.pending = None;
                match kind {
                    // _open
                    PendingKind::Show if self.requested => self.visible = true,
                    // _close
                    PendingKind::Hide if !self.requested => self.visible = false,
                    _ => {}
                }
            }
        }
        self.visible != was
    }

    /// `Keyboard.open`.
    fn open(&mut self, immediate: bool, now: u64) {
        self.requested = true;
        if self.visible {
            return;
        }
        self.pending = None;
        if immediate {
            self.visible = true;
        } else {
            self.pending = Some((now + KEYBOARD_REST_TIME_MS, PendingKind::Show));
        }
    }

    /// `Keyboard.close`.
    fn close(&mut self, immediate: bool, now: u64) {
        self.requested = false;
        if !self.visible {
            // GNOME leaves a pending show to find `_keyboardRequested`
            // false; dropping it is equivalent.
            self.pending = None;
            return;
        }
        self.pending = None;
        if immediate {
            self.visible = false;
        } else {
            self.pending = Some((now + KEYBOARD_REST_TIME_MS, PendingKind::Hide));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osk_layout::parse;

    fn us(hints: u32) -> Keyboard {
        let layout = parse(include_str!("../osk-layouts/us.json")).unwrap();
        Keyboard::with_layout(layout, "us", Purpose::Normal, hints)
    }

    fn find(kb: &Keyboard, pred: impl Fn(&Key) -> bool) -> KeyRef {
        for (row, keys) in kb.level().rows.iter().enumerate() {
            if let Some(index) = keys.iter().position(&pred) {
                return KeyRef { row, index };
            }
        }
        panic!("key not found on level {}", kb.level_name());
    }

    fn text(kb: &Keyboard, s: &str) -> KeyRef {
        find(kb, |k| k.commit_string() == Some(s) && k.action.is_none())
    }

    fn label(kb: &Keyboard, l: &str) -> KeyRef {
        find(kb, |k| k.label.as_deref() == Some(l))
    }

    fn action(kb: &Keyboard, a: Action) -> KeyRef {
        find(kb, |k| k.action.as_ref() == Some(&a))
    }

    fn shift(kb: &Keyboard) -> KeyRef {
        find(kb, |k| k.icon_name.as_deref() == Some("osk-shift-symbolic"))
    }

    fn commit(s: &str) -> Vec<Effect> {
        vec![Effect::Commit {
            text: s.to_owned(),
            modifiers: Vec::new(),
        }]
    }

    #[test]
    fn typing_commits() {
        let mut kb = us(0);
        let q = text(&kb, "q");
        assert_eq!(kb.release(q), commit("q"));
        assert_eq!(kb.level_name(), "default");
    }

    #[test]
    fn shift_applies_once() {
        let mut kb = us(0);
        let s = shift(&kb);
        assert!(kb.release(s).is_empty());
        assert_eq!(kb.level_name(), "shift");
        assert!(!kb.latched());
        let q = text(&kb, "Q");
        assert_eq!(kb.release(q), commit("Q"));
        assert_eq!(kb.level_name(), "default");
    }

    #[test]
    fn long_press_shift_latches() {
        let mut kb = us(0);
        let s = shift(&kb);
        assert!(kb.long_press(s).is_empty());
        assert!(kb.release(s).is_empty());
        assert_eq!(kb.level_name(), "shift");
        assert!(kb.latched());
        let q = text(&kb, "Q");
        assert_eq!(kb.release(q), commit("Q"));
        let w = text(&kb, "W");
        assert_eq!(kb.release(w), commit("W"));
        assert_eq!(kb.level_name(), "shift");
        let s = shift(&kb);
        kb.release(s);
        assert_eq!(kb.level_name(), "default");
        assert!(!kb.latched());
    }

    #[test]
    fn symbol_levels() {
        let mut kb = us(0);
        let k = label(&kb, "?123");
        kb.release(k);
        assert_eq!(kb.level_name(), "opt");
        let k = text(&kb, "1");
        assert_eq!(kb.release(k), commit("1"));
        assert_eq!(kb.level_name(), "opt");
        let k = label(&kb, "=/<");
        kb.release(k);
        assert_eq!(kb.level_name(), "opt+shift");
        let k = label(&kb, "ABC");
        kb.release(k);
        assert_eq!(kb.level_name(), "default");
    }

    #[test]
    fn modifiers_apply_to_next_commit() {
        let layout = parse(include_str!("../osk-layouts/us-extended.json")).unwrap();
        let mut kb = Keyboard::with_layout(layout, "us", Purpose::Terminal, 0);
        let ctrl = label(&kb, "Ctrl");
        assert!(kb.release(ctrl).is_empty());
        assert!(kb.modifier_active(0xffe3));
        let c = text(&kb, "c");
        assert_eq!(
            kb.release(c),
            vec![Effect::Commit {
                text: "c".to_owned(),
                modifiers: vec![0xffe3],
            }]
        );
        assert!(!kb.modifier_active(0xffe3));
        assert!(kb.modifiers().is_empty());
    }

    #[test]
    fn enter_sends_keyval() {
        let mut kb = us(0);
        let enter = find(&kb, |k| k.keyval == Some(0xff0d));
        assert_eq!(kb.release(enter), vec![Effect::Keyval(0xff0d)]);
    }

    #[test]
    fn delete_click_and_hold() {
        let mut kb = us(0);
        let del = action(&kb, Action::Delete);
        assert_eq!(kb.release(del), vec![Effect::Keyval(KEYVAL_BACKSPACE)]);
        assert_eq!(
            kb.long_press(del),
            vec![Effect::KeyvalPress(KEYVAL_BACKSPACE)]
        );
        assert_eq!(
            kb.release(del),
            vec![Effect::KeyvalRelease(KEYVAL_BACKSPACE)]
        );
        assert_eq!(
            kb.long_press(del),
            vec![Effect::KeyvalPress(KEYVAL_BACKSPACE)]
        );
        assert_eq!(kb.closed(), vec![Effect::KeyvalRelease(KEYVAL_BACKSPACE)]);
    }

    #[test]
    fn hide_and_emoji_keys() {
        let mut kb = us(0);
        let hide = action(&kb, Action::Hide);
        assert_eq!(kb.release(hide), vec![Effect::Hide]);
        let emoji = action(&kb, Action::Emoji);
        assert_eq!(kb.release(emoji), vec![Effect::ToggleEmoji]);
        assert!(kb.emoji_active());
        kb.opened();
        assert!(!kb.emoji_active());
        assert_eq!(kb.emoji("😀"), commit("😀"));
    }

    #[test]
    fn no_emoji_hint_drops_emoji_key() {
        let mut kb = us(0);
        assert!(kb.emoji_visible());
        let (placed, _) = osk_layout::place(kb.level(), kb.emoji_visible());
        let has_emoji = |placed: &[osk_layout::PlacedKey], kb: &Keyboard| {
            placed
                .iter()
                .any(|p| kb.level().rows[p.row][p.index].action == Some(Action::Emoji))
        };
        assert!(has_emoji(&placed, &kb));

        crate::osk_layout::tests::register();
        assert!(kb.set_content(Purpose::Normal, hints::NO_EMOJI));
        assert!(!kb.emoji_visible());
        let (placed, _) = osk_layout::place(kb.level(), kb.emoji_visible());
        assert!(!has_emoji(&placed, &kb));
    }

    #[test]
    fn case_hints() {
        let kb = us(hints::UPPERCASE);
        assert_eq!(kb.level_name(), "shift");
        let mut kb = us(hints::LOWERCASE);
        assert_eq!(kb.level_name(), "default");
        let s = shift(&kb);
        kb.release(s);
        let q = text(&kb, "Q");
        kb.release(q);
        assert_eq!(kb.level_name(), "default");
    }

    #[test]
    fn extended_key_commits() {
        let mut kb = us(0);
        let s = shift(&kb);
        kb.release(s);
        assert_eq!(kb.extended("É"), commit("É"));
        assert_eq!(kb.level_name(), "default");
    }

    #[test]
    fn group_change_reloads() {
        crate::osk_layout::tests::register();
        let mut kb = Keyboard::new("us", Purpose::Normal, 0).unwrap();
        assert!(kb.set_group("de"));
        assert_eq!(kb.layout().locale, "de");
        assert_eq!(kb.level_name(), "default");
        assert!(!kb.set_group("de"));
    }

    fn shown_after(p: &mut Presence, from: u64, to: u64) -> Option<u64> {
        (from..=to).find(|&t| {
            p.tick(t);
            p.visible()
        })
    }

    #[test]
    fn presence_opens_after_rest_time() {
        let mut p = Presence::new();
        p.focus(true, true, 0, 1000);
        assert!(!p.visible());
        assert_eq!(p.deadline(), Some(1300));
        assert!(!p.tick(1299));
        assert!(p.tick(1300));
        assert!(p.visible());
    }

    #[test]
    fn presence_focus_out_hides_after_delay() {
        let mut p = Presence::new();
        p.open_now(0);
        assert!(p.visible());
        p.focus(false, false, 0, 100);
        assert!(!p.tick(149));
        assert!(p.tick(150));
        assert!(!p.visible());
    }

    #[test]
    fn presence_refocus_keeps_open() {
        let mut p = Presence::new();
        p.open_now(0);
        p.focus(false, false, 0, 100);
        p.focus(true, true, 0, 120);
        assert!(!p.tick(1000));
        assert!(p.visible());
    }

    #[test]
    fn presence_hide_key_is_immediate() {
        let mut p = Presence::new();
        p.open_now(0);
        p.hide_key(10);
        assert!(!p.visible());
        assert_eq!(p.deadline(), None);
    }

    #[test]
    fn presence_inhibit_and_disabled() {
        let mut p = Presence::new();
        p.focus(true, true, hints::INHIBIT_OSK, 0);
        assert_eq!(shown_after(&mut p, 0, 1000), None);

        p.open_now(0);
        p.focus(true, true, hints::INHIBIT_OSK, 10);
        assert!(p.visible());
        assert!(p.tick(310));
        assert!(!p.visible());

        let mut p = Presence::new();
        p.set_enabled(false, 0);
        p.focus(true, true, 0, 0);
        p.open_now(0);
        assert_eq!(shown_after(&mut p, 0, 1000), None);
    }
}
