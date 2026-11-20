//! GNOME 51's on-screen keyboard manager (gnome-shell 51.0
//! `js/ui/keyboard.js` `KeyboardManager`, #349). The keyboard itself is
//! `tuna-osk`'s.
//!
//! [`Osk`] follows `screen-keyboard-enabled` and the compositor's
//! touch-mode auto-show, the current input source (the layout group) and
//! the compositor's input-method focus. The session keyboard is a layer
//! surface that never takes the keyboard, so the text field keeps focus
//! while keys are pressed; it types through the compositor (`OskInput`,
//! `KeyboardController`). On the lock screen each lock window embeds its
//! own [`PromptKeyboard`], opened by the unlock prompt's focus
//! (`_onKeyFocusChanged`), which edits only that prompt: nothing typed
//! there reaches the compositor or the session, and nothing of the
//! session is drawn.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Instant;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tuna_osk::layout::Purpose;
use tuna_osk::model::{Effect, Keyboard, Presence};
use tuna_osk::prompt::PromptKeyboard;
use tuna_osk::view::{
    keyboard_height, KeyboardView, Languages, Typing, KEYBOARD_ANIMATION_TIME_MS,
};
use tuna_shell_control::{OskInput, TextFieldFocus};

use crate::input_sources::InputSources;

const A11Y_APPLICATIONS_SCHEMA: &str = "org.gnome.desktop.a11y.applications";
const SHOW_KEYBOARD: &str = "screen-keyboard-enabled";

/// The session keyboard's typing: GNOME's `KeyboardController` through
/// the compositor.
pub fn compositor_input(effect: Effect) -> Vec<OskInput> {
    match effect {
        Effect::Commit { text, modifiers } => vec![OskInput::Commit { text, modifiers }],
        Effect::Keyval(keysym) => vec![
            OskInput::Keyval {
                keysym,
                pressed: true,
            },
            OskInput::Keyval {
                keysym,
                pressed: false,
            },
        ],
        Effect::KeyvalPress(keysym) => vec![OskInput::Keyval {
            keysym,
            pressed: true,
        }],
        Effect::KeyvalRelease(keysym) => vec![OskInput::Keyval {
            keysym,
            pressed: false,
        }],
        Effect::Hide | Effect::LanguageMenu | Effect::ToggleEmoji => Vec::new(),
    }
}

/// The session keyboard: a layer surface at the bottom of the monitor
/// that never takes the keyboard (`Main.layoutManager.keyboardBox`).
struct Session {
    window: gtk::Window,
    revealer: gtk::Revealer,
    view: KeyboardView,
    presence: RefCell<Presence>,
    timer: RefCell<Option<glib::SourceId>>,
}

/// `KeyboardManager`.
pub struct Osk {
    settings: Option<gio::Settings>,
    setting: Cell<bool>,
    touch_auto: Cell<bool>,
    locked: Cell<bool>,
    focus: Cell<Option<TextFieldFocus>>,
    sources: Rc<InputSources>,
    send: Rc<dyn Fn(OskInput)>,
    epoch: Instant,
    session: RefCell<Option<Rc<Session>>>,
    locks: RefCell<Vec<Weak<PromptKeyboard>>>,
}

impl Osk {
    /// `send` types into the focused client through the compositor.
    pub fn new(sources: Rc<InputSources>, send: Rc<dyn Fn(OskInput)>) -> Rc<Self> {
        let settings = crate::settings(A11Y_APPLICATIONS_SCHEMA).filter(|s| {
            s.settings_schema()
                .is_some_and(|schema| schema.has_key(SHOW_KEYBOARD))
        });
        let osk = Rc::new(Self {
            setting: Cell::new(settings.as_ref().is_some_and(|s| s.boolean(SHOW_KEYBOARD))),
            settings,
            touch_auto: Cell::new(false),
            locked: Cell::new(false),
            focus: Cell::new(None),
            sources: sources.clone(),
            send,
            epoch: Instant::now(),
            session: RefCell::new(None),
            locks: RefCell::new(Vec::new()),
        });
        if let Some(settings) = &osk.settings {
            let weak = Rc::downgrade(&osk);
            settings.connect_changed(Some(SHOW_KEYBOARD), move |settings, key| {
                if let Some(osk) = weak.upgrade() {
                    osk.setting.set(settings.boolean(key));
                    osk.sync_enabled();
                }
            });
        }
        {
            let weak = Rc::downgrade(&osk);
            sources.connect_changed(Rc::new(move || {
                if let Some(osk) = weak.upgrade() {
                    osk.group_changed();
                }
            }));
        }
        osk.sync_enabled();
        osk
    }

    fn now(&self) -> u64 {
        self.epoch
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    /// `_syncEnabled`: `screen-keyboard-enabled` or touch-mode auto-show.
    pub fn enabled(&self) -> bool {
        self.setting.get() || self.touch_auto.get()
    }

    fn group(&self) -> String {
        self.sources.current_id()
    }

    fn sync_enabled(self: &Rc<Self>) {
        let enabled = self.enabled();
        if enabled && !self.locked.get() {
            self.ensure_session();
        }
        let now = self.now();
        let session = self.session.borrow().clone();
        if let Some(session) = session {
            session
                .presence
                .borrow_mut()
                .set_enabled(enabled && !self.locked.get(), now);
            self.session_changed(&session);
        }
        for lock in self.lock_keyboards() {
            lock.set_enabled(enabled);
        }
    }

    fn lock_keyboards(&self) -> Vec<Rc<PromptKeyboard>> {
        let mut locks = self.locks.borrow_mut();
        locks.retain(|lock| lock.strong_count() > 0);
        locks.iter().filter_map(Weak::upgrade).collect()
    }

    fn group_changed(&self) {
        let group = self.group();
        if let Some(session) = self.session.borrow().as_ref() {
            session.view.set_group(&group);
        }
        for lock in self.lock_keyboards() {
            lock.set_group(&group);
        }
    }

    /// The compositor's `OnScreenKeyboard`: input-method focus, the
    /// input-panel request and touch-mode auto-show.
    pub fn update(self: &Rc<Self>, focus: Option<TextFieldFocus>, show: bool, touch_auto: bool) {
        if touch_auto != self.touch_auto.get() {
            self.touch_auto.set(touch_auto);
            self.sync_enabled();
        }
        self.focus.set(focus);
        let Some(session) = self.session.borrow().clone() else {
            return;
        };
        if let Some(field) = focus {
            session
                .view
                .set_content(Purpose::from_wire(field.purpose), field.hints);
        }
        let now = self.now();
        session.presence.borrow_mut().focus(
            focus.is_some(),
            show,
            focus.map_or(0, |f| f.hints),
            now,
        );
        self.session_changed(&session);
    }

    /// The session locked or unlocked: the session keyboard never shows
    /// over the lock screen.
    pub fn set_locked(self: &Rc<Self>, locked: bool) {
        if self.locked.replace(locked) != locked {
            self.sync_enabled();
        }
    }

    fn ensure_session(self: &Rc<Self>) {
        if self.session.borrow().is_some() {
            return;
        }
        let Some(model) = Keyboard::new(&self.group(), Purpose::Normal, 0) else {
            eprintln!("tuna-shell-gtk: no on-screen keyboard layout loads");
            return;
        };
        let window = gtk::Window::new();
        window.add_css_class("tuna-osk");
        window.set_title(Some("On-Screen Keyboard"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::OSK_NAMESPACE));
        window.set_anchor(Edge::Left, true);
        window.set_anchor(Edge::Right, true);
        window.set_anchor(Edge::Bottom, true);
        // Never the keyboard: the text field keeps it (and its input
        // method) while keys are pressed.
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_focusable(false);
        let send = self.send.clone();
        let typing: Typing = Rc::new(move |effect| {
            for input in compositor_input(effect) {
                send(input);
            }
        });
        let weak = Rc::downgrade(self);
        let hide: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(osk) = weak.upgrade() {
                osk.hide_key();
            }
        });
        let languages: Rc<dyn Languages> = self.sources.clone();
        let view = KeyboardView::new(model, typing, hide, Some(languages));
        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
        revealer.set_transition_duration(KEYBOARD_ANIMATION_TIME_MS);
        revealer.set_valign(gtk::Align::End);
        revealer.set_child(Some(view.widget()));
        window.set_child(Some(&revealer));
        let session = Rc::new(Session {
            window,
            revealer,
            view,
            presence: RefCell::new(Presence::new()),
            timer: RefCell::new(None),
        });
        {
            // `_animateHideComplete`: the box goes once the slide ends.
            let weak = Rc::downgrade(&session);
            session
                .revealer
                .connect_child_revealed_notify(move |revealer| {
                    if let Some(session) = weak.upgrade() {
                        if !revealer.reveals_child() && !revealer.is_child_revealed() {
                            session.window.set_visible(false);
                        }
                    }
                });
        }
        *self.session.borrow_mut() = Some(session);
    }

    fn hide_key(self: &Rc<Self>) {
        let Some(session) = self.session.borrow().clone() else {
            return;
        };
        let now = self.now();
        session.presence.borrow_mut().hide_key(now);
        self.session_changed(&session);
    }

    /// Apply the session keyboard's presence, and arm its next timer.
    fn session_changed(self: &Rc<Self>, session: &Rc<Session>) {
        let visible = session.presence.borrow().visible();
        if visible && !session.revealer.reveals_child() {
            self.relayout(session);
            session.view.opened();
            session.window.set_visible(true);
            session.revealer.set_reveal_child(true);
        } else if !visible && session.revealer.reveals_child() {
            session.view.closed();
            session.revealer.set_reveal_child(false);
            if !crate::motion::current().animations_enabled() {
                session.window.set_visible(false);
            }
        }
        if let Some(id) = session.timer.borrow_mut().take() {
            id.remove();
        }
        let Some(deadline) = session.presence.borrow().deadline() else {
            return;
        };
        let delay = deadline.saturating_sub(self.now());
        let (weak, osk) = (Rc::downgrade(session), Rc::downgrade(self));
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
            let (Some(session), Some(osk)) = (weak.upgrade(), osk.upgrade()) else {
                return;
            };
            session.timer.borrow_mut().take();
            let now = osk.now();
            session.presence.borrow_mut().tick(now);
            osk.session_changed(&session);
        });
        *session.timer.borrow_mut() = Some(id);
    }

    /// `_relayout` on the keyboard's monitor.
    fn relayout(&self, session: &Session) {
        let monitor = gtk::gdk::Display::default().and_then(|display| {
            display
                .monitors()
                .item(0)
                .and_then(|m| m.downcast::<gtk::gdk::Monitor>().ok())
        });
        let Some(monitor) = monitor else { return };
        let geometry = monitor.geometry();
        let min = session
            .view
            .widget()
            .measure(gtk::Orientation::Vertical, geometry.width())
            .0;
        let height = keyboard_height(geometry.width(), geometry.height(), min);
        session.view.widget().set_size_request(-1, height);
        session.window.set_default_size(geometry.width(), height);
    }

    /// A lock window's keyboard for its unlock prompt: `prompt` yields the
    /// entry currently asking (hidden or visible, authPrompt.js
    /// `updateEntry`), or `None` once the prompt is gone.
    pub fn lock_keyboard(
        self: &Rc<Self>,
        prompt: Rc<dyn Fn() -> Option<gtk::Editable>>,
    ) -> Rc<PromptKeyboard> {
        let languages: Rc<dyn Languages> = self.sources.clone();
        let lock = PromptKeyboard::new(&self.group(), Some(languages), prompt, self.enabled());
        self.locks.borrow_mut().push(Rc::downgrade(&lock));
        lock
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_osk::view::{KEYVAL_BACKSPACE, KEYVAL_RETURN};

    #[test]
    fn effects_become_compositor_input_like_keyboard_controller() {
        assert_eq!(
            compositor_input(Effect::Commit {
                text: "Q".into(),
                modifiers: vec![0xffe3],
            }),
            vec![OskInput::Commit {
                text: "Q".into(),
                modifiers: vec![0xffe3],
            }]
        );
        assert_eq!(
            compositor_input(Effect::Keyval(KEYVAL_RETURN)),
            vec![
                OskInput::Keyval {
                    keysym: KEYVAL_RETURN,
                    pressed: true,
                },
                OskInput::Keyval {
                    keysym: KEYVAL_RETURN,
                    pressed: false,
                },
            ]
        );
        assert_eq!(
            compositor_input(Effect::KeyvalPress(KEYVAL_BACKSPACE)),
            vec![OskInput::Keyval {
                keysym: KEYVAL_BACKSPACE,
                pressed: true,
            }]
        );
        for local in [Effect::Hide, Effect::LanguageMenu, Effect::ToggleEmoji] {
            assert!(compositor_input(local).is_empty());
        }
    }
}
