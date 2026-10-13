//! Literal indexed lock prompts. Transport authority lives in ControlClient.
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk4 as gtk;
use tuna_shell_control::auth::{Answer, AnswerText, Phase, Prompt, Style};
use tuna_shell_host::control::{AuthContext, AuthEvent};

pub type StartHook = Rc<dyn Fn() -> Option<AuthContext>>;
pub type AnswerHook = Rc<dyn Fn(AuthContext, Vec<Answer>) -> bool>;
pub type CancelHook = Rc<dyn Fn(AuthContext) -> bool>;
pub struct Hooks {
    pub start: StartHook,
    pub answer: AnswerHook,
    pub cancel: CancelHook,
}
struct Field {
    index: u32,
    entry: Option<gtk::Entry>,
}
pub struct View {
    pub root: gtk::Box,
    rows: RefCell<Vec<Field>>,
}
impl View {
    fn clear(&self) {
        for field in self.rows.borrow().iter() {
            if let Some(entry) = &field.entry {
                entry.set_text("");
            }
        }
    }
    fn remove(&self) {
        self.clear();
        self.rows.borrow_mut().clear();
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
    }
}
impl Drop for View {
    fn drop(&mut self) {
        self.clear();
    }
}
pub struct AuthUi {
    hooks: RefCell<Option<Hooks>>,
    generation: Cell<Option<u64>>,
    available: Cell<bool>,
    start_ready: Cell<bool>,
    context: Cell<Option<AuthContext>>,
    phase: Cell<Phase>,
    stopped: Cell<bool>,
    serial: Cell<u64>,
    views: RefCell<Vec<Weak<View>>>,
}
impl AuthUi {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            hooks: RefCell::new(None),
            generation: Cell::new(None),
            available: Cell::new(false),
            start_ready: Cell::new(false),
            context: Cell::new(None),
            phase: Cell::new(Phase::Unavailable),
            stopped: Cell::new(false),
            serial: Cell::new(0),
            views: RefCell::new(Vec::new()),
        })
    }
    pub fn install(&self, hooks: Hooks) {
        *self.hooks.borrow_mut() = Some(hooks);
    }
    pub fn view(self: &Rc<Self>) -> Rc<View> {
        let view = Rc::new(View {
            root: gtk::Box::new(gtk::Orientation::Vertical, 8),
            rows: RefCell::new(Vec::new()),
        });
        view.root.set_visible(false);
        self.views.borrow_mut().push(Rc::downgrade(&view));
        view
    }
    pub fn active(&self) -> bool {
        self.context.get().is_some()
    }
    pub fn available(&self) -> bool {
        self.available.get()
    }
    fn views(&self) -> Vec<Rc<View>> {
        self.views
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }
    fn advance(&self) -> Option<u64> {
        let serial = self.serial.get().checked_add(1)?;
        self.serial.set(serial);
        Some(serial)
    }
    fn clear_fields(&self) {
        for view in self.views() {
            view.clear();
        }
    }
    fn status(self: &Rc<Self>, text: &str) {
        for view in self.views() {
            view.remove();
            let label = gtk::Label::new(Some(text));
            label.set_wrap(true);
            view.root.append(&label);
            view.root.set_visible(self.active() || self.available());
            if let Some(context) = self.context.get().filter(|_| !self.stopped.get()) {
                let cancel = gtk::Button::with_label("Cancel");
                let weak = Rc::downgrade(self);
                cancel.connect_clicked(move |_| {
                    if let Some(ui) = weak
                        .upgrade()
                        .filter(|ui| ui.context.get() == Some(context))
                    {
                        ui.cancel();
                    }
                });
                view.root.append(&cancel);
            } else if self.context.get().is_none() && self.available() {
                let retry = gtk::Button::with_label("Sign In");
                let weak = Rc::downgrade(self);
                let serial = self.serial.get();
                retry.connect_clicked(move |_| {
                    if let Some(ui) = weak.upgrade().filter(|ui| ui.serial.get() == serial) {
                        ui.start();
                    }
                });
                view.root.append(&retry);
            }
        }
    }
    pub fn invalidate(self: &Rc<Self>) {
        self.clear_fields();
        self.context.set(None);
        self.available.set(false);
        self.start_ready.set(false);
        self.stopped.set(true);
        let _ = self.advance();
        for view in self.views() {
            view.remove();
            view.root.set_visible(false);
        }
    }
    pub fn sync(self: &Rc<Self>, generation: Option<u64>, available: bool, start_ready: bool) {
        let changed = self.generation.replace(generation) != generation;
        let was_available = self.available.get();
        if changed || generation.is_none() {
            self.invalidate();
        }
        self.available.set(generation.is_some() && available);
        self.start_ready.set(generation.is_some() && start_ready);
        if self.available.get() && (changed || !was_available) && !self.active() {
            self.status("Sign In");
        }
    }
    /// Normal curtain dismissal starts the admitted session stack. No typed
    /// credential is cached before the first owned challenge.
    pub fn start(self: &Rc<Self>) -> bool {
        if self.active() {
            return true;
        }
        if !self.available.get() {
            return false;
        }
        if !self.start_ready.get() {
            self.status("Please wait…");
            return false;
        }
        let start = self
            .hooks
            .borrow()
            .as_ref()
            .map(|hooks| hooks.start.clone());
        let Some(context) = start.and_then(|start| start()) else {
            self.invalidate();
            return false;
        };
        if self.generation.get() != Some(context.generation) {
            self.invalidate();
            return false;
        }
        self.context.set(Some(context));
        self.phase.set(Phase::Starting);
        self.stopped.set(false);
        self.start_ready.set(false);
        if self.advance().is_none() {
            self.invalidate();
            return false;
        }
        self.status("Signing in…");
        true
    }
    fn eligible(&self, context: AuthContext, serial: u64) -> bool {
        self.context.get() == Some(context)
            && self.generation.get() == Some(context.generation)
            && self.serial.get() == serial
            && self.phase.get() == Phase::Prompt
            && !self.stopped.get()
    }
    fn submit(self: &Rc<Self>, context: AuthContext, serial: u64, fields: &[Field]) {
        if !self.eligible(context, serial) {
            return;
        }
        let answers: Vec<Answer> = fields
            .iter()
            .map(|field| Answer {
                index: field.index,
                text: field
                    .entry
                    .as_ref()
                    .map(|entry| AnswerText(entry.text().to_string())),
            })
            .collect();
        if !tuna_shell_control::auth::valid_answers(&answers) {
            return;
        }
        self.phase.set(Phase::Verifying);
        let _ = self.advance();
        // Clear every monitor immediately, before awaiting any daemon reply.
        self.clear_fields();
        self.status("Signing in…");
        let send = self
            .hooks
            .borrow()
            .as_ref()
            .map(|hooks| hooks.answer.clone());
        if !send.is_some_and(|send| send(context, answers)) {
            self.invalidate();
        }
    }
    pub fn cancel(self: &Rc<Self>) {
        let Some(context) = self.context.get() else {
            return;
        };
        if self.stopped.replace(true) {
            return;
        }
        self.phase.set(Phase::Cancelling);
        let _ = self.advance();
        self.clear_fields();
        self.status("Cancelling…");
        let cancel = self
            .hooks
            .borrow()
            .as_ref()
            .map(|hooks| hooks.cancel.clone());
        if !cancel.is_some_and(|cancel| cancel(context)) {
            self.invalidate();
        }
    }
    pub fn event(self: &Rc<Self>, event: AuthEvent) {
        match event {
            AuthEvent::Challenge { context, messages } => {
                let matching = self.context.get().is_some_and(|old| {
                    old.generation == context.generation
                        && old.start_request == context.start_request
                        && old.epoch == context.epoch
                        && old.sequence.checked_add(1) == Some(context.sequence)
                        && context.key.is_some()
                        && old.key.is_none_or(|key| Some(key) == context.key)
                });
                if !matching
                    || self.stopped.get()
                    || self.generation.get() != Some(context.generation)
                    || !tuna_shell_control::auth::valid_prompts(&messages)
                {
                    return;
                }
                self.context.set(Some(context));
                self.phase.set(Phase::Prompt);
                let Some(serial) = self.advance() else {
                    self.invalidate();
                    return;
                };
                self.render(context, serial, &messages);
            }
            AuthEvent::State { context, phase } => {
                let matching = self.context.get().is_some_and(|old| {
                    old.generation == context.generation
                        && old.start_request == context.start_request
                        && old.epoch == context.epoch
                        && old.key.is_none_or(|key| Some(key) == context.key)
                });
                if !matching {
                    return;
                }
                if self.stopped.get()
                    && matches!(phase, Phase::Starting | Phase::Prompt | Phase::Verifying)
                {
                    return;
                }
                self.context.set(Some(context));
                self.phase.set(phase);
                if phase == Phase::Denied
                    || (context.key.is_none()
                        && matches!(phase, Phase::Unavailable | Phase::Blocked))
                {
                    self.context.set(None);
                    self.stopped.set(true);
                    let _ = self.advance();
                    self.status("Sorry, that didn't work. Please try again.");
                } else if matches!(phase, Phase::Starting | Phase::Verifying) {
                    self.status("Signing in…");
                } else if matches!(
                    phase,
                    Phase::Cancelling | Phase::Blocked | Phase::Unavailable
                ) {
                    self.stopped.set(true);
                    let _ = self.advance();
                    self.status("Signing in is unavailable.");
                }
            }
            AuthEvent::AnswerResult {
                context,
                applied: false,
            } => {
                if self.context.get() == Some(context) && !self.stopped.get() {
                    // Fresh fields require a fresh challenge; never repopulate
                    // or automatically replay values from a rejected answer.
                    self.cancel();
                }
            }
            AuthEvent::Availability(_)
            | AuthEvent::RootResult { .. }
            | AuthEvent::AnswerResult { .. }
            | AuthEvent::CancelResult { .. }
            | AuthEvent::Ignored => {}
        }
    }
    fn render(self: &Rc<Self>, context: AuthContext, serial: u64, messages: &[Prompt]) {
        for view in self.views() {
            view.remove();
            let mut fields = Vec::new();
            for prompt in messages {
                let label = gtk::Label::new(Some(&prompt.text));
                label.set_wrap(true);
                label.set_max_width_chars(60);
                label.set_xalign(0.0);
                view.root.append(&label);
                let entry = if matches!(prompt.style, Style::Visible | Style::Secret) {
                    let entry = gtk::Entry::new();
                    entry.set_visibility(prompt.style == Style::Visible);
                    entry.set_hexpand(true);
                    entry.update_property(&[gtk::accessible::Property::Label(&prompt.text)]);
                    view.root.append(&entry);
                    Some(entry)
                } else {
                    None
                };
                fields.push(Field {
                    index: prompt.index,
                    entry,
                });
            }
            // Widgets are shared clones only, never stored answer strings.
            *view.rows.borrow_mut() = fields
                .iter()
                .map(|field| Field {
                    index: field.index,
                    entry: field.entry.clone(),
                })
                .collect();
            let fields = Rc::new(fields);
            let submit = gtk::Button::with_label("Sign In");
            let cancel = gtk::Button::with_label("Cancel");
            let weak = Rc::downgrade(self);
            let captured = fields.clone();
            submit.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.submit(context, serial, &captured);
                }
            });
            for field in fields.iter() {
                if let Some(entry) = &field.entry {
                    let submit = submit.downgrade();
                    entry.connect_activate(move |_| {
                        if let Some(submit) = submit.upgrade() {
                            submit.emit_clicked();
                        }
                    });
                }
            }
            let weak = Rc::downgrade(self);
            cancel.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade().filter(|ui| ui.eligible(context, serial)) {
                    ui.cancel();
                }
            });
            view.root.append(&submit);
            view.root.append(&cancel);
            view.root.set_visible(true);
            if let Some(entry) = fields.iter().find_map(|field| field.entry.as_ref()) {
                entry.grab_focus();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::auth::Key;
    fn button(view: &View, text: &str) -> gtk::Button {
        let mut child = view.root.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Ok(button) = widget.downcast::<gtk::Button>() {
                if button.label().as_deref() == Some(text) {
                    return button;
                }
            }
        }
        panic!("required actual GTK action missing");
    }
    #[test]
    #[ignore = "requires admitted Xvfb GTK proof"]
    fn actual_gtk_indexed_prompts_clear_and_reject_stale_callbacks() {
        assert!(
            std::env::var_os("TUNA_REQUIRE_AUTH_TRANSPORT_GTK").is_some(),
            "dedicated admission required"
        );
        gtk::init().expect("actual GTK display required");
        let ui = AuthUi::new();
        let left = ui.view();
        let right = ui.view();
        let starts = Rc::new(Cell::new(0));
        let sends = Rc::new(Cell::new(0));
        let cancels = Rc::new(Cell::new(0));
        let clearing = Rc::new(RefCell::new(Vec::<gtk::Entry>::new()));
        let root = AuthContext {
            generation: 1,
            start_request: 10,
            key: None,
            sequence: 0,
            epoch: 7,
        };
        let seen = Rc::new(RefCell::new(Vec::<Vec<(u32, Option<String>)>>::new()));
        ui.install(Hooks {
            start: {
                let starts = starts.clone();
                Rc::new(move || {
                    starts.set(starts.get() + 1);
                    Some(root)
                })
            },
            answer: {
                let sends = sends.clone();
                let seen = seen.clone();
                let clearing = clearing.clone();
                Rc::new(move |_, answers| {
                    assert!(
                        !clearing.borrow().is_empty(),
                        "actual entries observed at send boundary"
                    );
                    assert!(
                        clearing
                            .borrow()
                            .iter()
                            .all(|entry| entry.text().is_empty()),
                        "all monitor fields cleared before transport callback"
                    );
                    sends.set(sends.get() + 1);
                    seen.borrow_mut().push(
                        answers
                            .iter()
                            .map(|answer| {
                                (
                                    answer.index,
                                    answer.text.as_ref().map(|text| text.0.clone()),
                                )
                            })
                            .collect(),
                    );
                    true
                })
            },
            cancel: {
                let cancels = cancels.clone();
                Rc::new(move |_| {
                    cancels.set(cancels.get() + 1);
                    true
                })
            },
        });
        ui.sync(Some(1), false, false);
        assert!(!ui.start());
        assert_eq!(starts.get(), 0, "minor/transport alone is no policy");
        ui.sync(Some(1), true, true);
        assert!(ui.start());
        assert_eq!(starts.get(), 1);
        let first = AuthContext {
            key: Some(Key {
                start_request: 10,
                attempt_serial: 42,
                epoch: 7,
            }),
            sequence: 1,
            ..root
        };
        let mixed = vec![
            Prompt {
                index: 0,
                style: Style::Visible,
                text: "Account".into(),
            },
            Prompt {
                index: 1,
                style: Style::Secret,
                text: "Password".into(),
            },
            Prompt {
                index: 2,
                style: Style::Info,
                text: "Literal <b>notice</b>".into(),
            },
            Prompt {
                index: 3,
                style: Style::Error,
                text: "Another literal message".into(),
            },
        ];
        ui.event(AuthEvent::Challenge {
            context: first,
            messages: mixed,
        });
        let mut labels = Vec::new();
        let mut child = left.root.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Ok(label) = widget.downcast::<gtk::Label>() {
                labels.push(label);
            }
        }
        assert_eq!(labels.len(), 4);
        assert!(!labels[2].uses_markup());
        assert_eq!(labels[2].text(), "Literal <b>notice</b>");
        assert_eq!(labels[3].text(), "Another literal message");
        let visible = left.rows.borrow()[0].entry.clone().unwrap();
        let secret = left.rows.borrow()[1].entry.clone().unwrap();
        assert!(gtk::prelude::EntryExt::is_visible(&visible));
        assert!(!gtk::prelude::EntryExt::is_visible(&secret));
        assert!(left.rows.borrow()[2].entry.is_none());
        assert!(left.rows.borrow()[3].entry.is_none());
        visible.set_text("controlled-account");
        secret.set_text("controlled-password");
        let right_secret = right.rows.borrow()[1].entry.clone().unwrap();
        right_secret.set_text("other-monitor-field");
        *clearing.borrow_mut() = vec![visible.clone(), secret.clone(), right_secret.clone()];
        let stale_first = button(&left, "Sign In");
        stale_first.emit_clicked();
        assert_eq!(sends.get(), 1);
        assert_eq!(visible.text(), "");
        assert_eq!(secret.text(), "");
        assert_eq!(right_secret.text(), "");
        assert_eq!(
            seen.borrow()[0],
            vec![
                (0, Some("controlled-account".into())),
                (1, Some("controlled-password".into())),
                (2, None),
                (3, None)
            ]
        );
        stale_first.emit_clicked();
        assert_eq!(sends.get(), 1);
        let second = AuthContext {
            sequence: 2,
            ..first
        };
        ui.event(AuthEvent::Challenge {
            context: second,
            messages: vec![Prompt {
                index: 0,
                style: Style::Secret,
                text: "One-time code".into(),
            }],
        });
        let otp = left.rows.borrow()[0].entry.clone().unwrap();
        assert_eq!(otp.text(), "", "next prompt never copies the password");
        otp.set_text("controlled-otp");
        let right_otp = right.rows.borrow()[0].entry.clone().unwrap();
        right_otp.set_text("other-monitor-otp");
        *clearing.borrow_mut() = vec![otp.clone(), right_otp.clone()];
        stale_first.emit_clicked();
        assert_eq!(sends.get(), 1);
        assert_eq!(otp.text(), "controlled-otp");
        let stale_second = button(&left, "Sign In");
        stale_second.emit_clicked();
        assert_eq!(sends.get(), 2);
        assert_eq!(seen.borrow()[1], vec![(0, Some("controlled-otp".into()))]);
        assert_eq!(otp.text(), "");
        let third = AuthContext {
            sequence: 3,
            ..second
        };
        ui.event(AuthEvent::Challenge {
            context: third,
            messages: vec![Prompt {
                index: 0,
                style: Style::Secret,
                text: "Cancelable fresh prompt".into(),
            }],
        });
        let cancelled_field = left.rows.borrow()[0].entry.clone().unwrap();
        cancelled_field.set_text("controlled-cancelled-value");
        let stale_third = button(&left, "Sign In");
        let cancel = button(&left, "Cancel");
        cancel.emit_clicked();
        assert_eq!(cancels.get(), 1);
        assert_eq!(
            cancelled_field.text(),
            "",
            "cancel immediately clears typed field"
        );
        assert!(ui.active(), "cancel callback is not retirement");
        ui.event(AuthEvent::CancelResult {
            context: third,
            applied: true,
        });
        stale_second.emit_clicked();
        assert_eq!(sends.get(), 2);
        ui.event(AuthEvent::Challenge {
            context: AuthContext {
                sequence: 4,
                ..third
            },
            messages: vec![Prompt {
                index: 0,
                style: Style::Secret,
                text: "late prompt".into(),
            }],
        });
        assert!(left.rows.borrow().is_empty());
        assert!(ui.active());
        ui.sync(None, false, false);
        assert!(!ui.active());
        stale_second.emit_clicked();
        stale_third.emit_clicked();
        cancel.emit_clicked();
        assert_eq!(sends.get(), 2);
        assert_eq!(cancels.get(), 1);
        // Explicit new generation needs fresh Start and fields. Old closures
        // remain inert; unavailable policy cannot expose a permanent entry.
        ui.sync(Some(2), false, false);
        assert!(!ui.start());
        assert_eq!(starts.get(), 1);
        assert!(left.rows.borrow().is_empty());
        assert!(!left.root.is_visible());
        let lost = AuthUi::new();
        let lost_view = lost.view();
        let lost_sends = Rc::new(Cell::new(0));
        lost.install(Hooks {
            start: Rc::new(move || Some(root)),
            answer: {
                let lost_sends = lost_sends.clone();
                Rc::new(move |_, _| {
                    lost_sends.set(lost_sends.get() + 1);
                    true
                })
            },
            cancel: Rc::new(|_| true),
        });
        lost.sync(Some(1), true, true);
        assert!(lost.start());
        lost.event(AuthEvent::Challenge {
            context: first,
            messages: vec![Prompt {
                index: 0,
                style: Style::Secret,
                text: "Fresh field before EOF".into(),
            }],
        });
        let lost_entry = lost_view.rows.borrow()[0].entry.clone().unwrap();
        lost_entry.set_text("controlled-before-EOF");
        let stale_lost = button(&lost_view, "Sign In");
        lost.sync(None, false, false);
        assert_eq!(
            lost_entry.text(),
            "",
            "generation loss clears actual typed widget"
        );
        stale_lost.emit_clicked();
        assert_eq!(lost_sends.get(), 0);
        assert!(!lost_view.root.is_visible());
        println!("interactive GTK controls: mixed_styles fresh_password_otp all_monitor_clear stale_callbacks cancel_ack generation_loss policy_absent");
    }
}
