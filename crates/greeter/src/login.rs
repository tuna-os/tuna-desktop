//! GTK-owned login controller. Blocking socket exchanges run on Gio workers;
//! only request completions wake GTK. Widget callbacks hold weak references.
use crate::{
    client::GreeterClient,
    controller::{Command, Controller, Effect},
    model::Screen,
    session::SessionEntry,
};
use gtk4::{gio, glib, prelude::*};
use libadwaita::prelude::*;
use std::{
    cell::RefCell,
    net::Shutdown,
    os::unix::net::UnixStream,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

struct State {
    controller: Controller,
    sessions: Vec<SessionEntry>,
    socket: String,
    client: Option<GreeterClient>,
    interrupt: Arc<Mutex<Option<UnixStream>>>,
    closed: Arc<AtomicBool>,
    widgets: Widgets,
}
#[derive(Clone)]
pub struct Widgets {
    pub window: libadwaita::ApplicationWindow,
    pub username: gtk4::Entry,
    pub answer: gtk4::Entry,
    pub prompt: gtk4::Label,
    pub notice: gtk4::Label,
    pub sessions: libadwaita::ComboRow,
    pub submit: gtk4::Button,
    pub cancel: gtk4::Button,
}
pub struct LoginWindow {
    state: Rc<RefCell<State>>,
}
impl LoginWindow {
    pub fn widgets(&self) -> Widgets {
        self.state.borrow().widgets.clone()
    }
    pub fn shutdown(&self) {
        let state = self.state.borrow();
        state.closed.store(true, Ordering::Release);
        if let Some(stream) = state.interrupt.lock().expect("interrupt lock").take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
}
impl Drop for LoginWindow {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn render(state: &State) {
    let model = &state.controller.model;
    let w = &state.widgets;
    let prompt = model.current_prompt();
    w.prompt
        .set_label(prompt.map_or("Username", |p| p.text.as_str()));
    w.answer.set_visible(prompt.is_some());
    w.answer.set_visibility(!prompt.is_some_and(|p| p.secret));
    w.answer
        .set_input_purpose(if prompt.is_some_and(|p| p.secret) {
            gtk4::InputPurpose::Password
        } else {
            gtk4::InputPurpose::FreeForm
        });
    let idle = !state.controller.busy() && model.screen != Screen::Launching;
    w.username.set_sensitive(idle && prompt.is_none());
    w.sessions.set_sensitive(idle && prompt.is_none());
    w.answer.set_sensitive(idle && prompt.is_some());
    w.submit.set_sensitive(idle && !state.sessions.is_empty());
    w.cancel.set_sensitive(
        model.user.is_some() && model.screen != Screen::Launching && !state.controller.cancelling(),
    );
    w.notice.set_label(if state.controller.cancelling() {
        "Canceling…"
    } else {
        model.notice.as_deref().unwrap_or("")
    });
    w.notice.set_visible(!w.notice.text().is_empty());
    if idle {
        if prompt.is_some() {
            w.answer.grab_focus();
        } else {
            w.username.grab_focus();
        }
    }
}

fn apply(state: &Rc<RefCell<State>>, effect: Effect) {
    render(&state.borrow());
    match effect {
        Effect::Render => {}
        Effect::Exit => {
            if let Some(app) = state.borrow().widgets.window.application() {
                app.quit();
            }
        }
        Effect::Send(command) => {
            let (client, socket, interrupt, closed) = {
                let mut state = state.borrow_mut();
                (
                    state.client.take(),
                    state.socket.clone(),
                    state.interrupt.clone(),
                    state.closed.clone(),
                )
            };
            let weak = Rc::downgrade(state);
            glib::MainContext::default().spawn_local(async move {
                let result = gio::spawn_blocking(move || {
                    let mut client = match client {
                        Some(client) => client,
                        None => match GreeterClient::connect(&socket) {
                            Ok(client) => client,
                            Err(_) => return (None, Err(())),
                        },
                    };
                    *interrupt.lock().expect("interrupt lock") = client.shutdown_handle().ok();
                    if closed.load(Ordering::Acquire) {
                        return (None, Err(()));
                    }
                    let cancel = matches!(&command, Command::Cancel);
                    let response = match command {
                        Command::Create(user) => client.create_session(&user),
                        Command::Answer(answer) => client.answer(answer),
                        Command::Start(cmd, env) => client.start_session_response(cmd, env),
                        Command::Cancel => client.cancel_response(),
                    }
                    .map_err(|_| ());
                    // A rejected cancellation cannot leave a reusable conversation.
                    let reusable = response.is_ok()
                        && (!cancel || matches!(&response, Ok(greetd_ipc::Response::Success)));
                    if !reusable {
                        interrupt.lock().expect("interrupt lock").take();
                    }
                    (reusable.then_some(client), response)
                })
                .await;
                let Some(state) = weak.upgrade() else {
                    return;
                };
                if state.borrow().closed.load(Ordering::Acquire) {
                    return;
                }
                let (client, response) = result.unwrap_or((None, Err(())));
                let effect = {
                    let mut state = state.borrow_mut();
                    state.client = client;
                    // Clear every answer after its exchange; never retain secrets for retry.
                    state.widgets.answer.set_text("");
                    state.controller.receive(response)
                };
                apply(&state, effect);
            });
        }
    }
}

pub fn build_login(
    app: &libadwaita::Application,
    entries: Vec<SessionEntry>,
    socket: String,
) -> LoginWindow {
    let window = libadwaita::ApplicationWindow::builder()
        .application(app)
        .title("Sign in to Tuna Desktop")
        .default_width(420)
        .default_height(480)
        .build();
    let contents = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
    contents.set_margin_top(32);
    contents.set_margin_bottom(32);
    contents.set_margin_start(32);
    contents.set_margin_end(32);
    let username = gtk4::Entry::builder().placeholder_text("Username").build();
    username.set_max_length(256);
    username.update_property(&[gtk4::accessible::Property::Label("Username")]);
    let prompt = gtk4::Label::new(Some("Username"));
    prompt.set_wrap(true);
    let answer = gtk4::Entry::new();
    answer.set_max_length(4096);
    answer.update_property(&[gtk4::accessible::Property::Label("Authentication answer")]);
    let notice = gtk4::Label::new(None);
    notice.set_wrap(true);
    let names = gtk4::StringList::new(&[]);
    for entry in &entries {
        names.append(&entry.name);
    }
    let sessions = libadwaita::ComboRow::builder()
        .title("Session")
        .model(&names)
        .selected(entries.iter().position(|e| e.is_default).unwrap_or(0) as u32)
        .build();
    let submit = gtk4::Button::with_label("Sign in");
    submit.add_css_class("suggested-action");
    let cancel = gtk4::Button::with_label("Cancel");
    for child in [
        notice.clone().upcast::<gtk4::Widget>(),
        username.clone().upcast(),
        prompt.clone().upcast(),
        answer.clone().upcast(),
        sessions.clone().upcast(),
        submit.clone().upcast(),
        cancel.clone().upcast(),
    ] {
        contents.append(&child);
    }
    window.set_content(Some(&contents));
    let state = Rc::new(RefCell::new(State {
        controller: Controller::default(),
        sessions: entries,
        socket,
        client: None,
        interrupt: Arc::new(Mutex::new(None)),
        closed: Arc::new(AtomicBool::new(false)),
        widgets: Widgets {
            window: window.clone(),
            username: username.clone(),
            answer: answer.clone(),
            prompt,
            notice,
            sessions,
            submit: submit.clone(),
            cancel: cancel.clone(),
        },
    }));
    let weak = Rc::downgrade(&state);
    submit.connect_clicked(move |_| {
        let Some(state) = weak.upgrade() else {
            return;
        };
        let effect = {
            let mut state = state.borrow_mut();
            let w = &state.widgets;
            let user = w.username.text().to_string();
            let answer = w.answer.text().to_string();
            let selected = state.sessions.get(w.sessions.selected() as usize).cloned();
            state.controller.submit(&user, answer, selected.as_ref())
        };
        apply(&state, effect);
    });
    for entry in [username, answer] {
        let button = submit.clone();
        entry.connect_activate(move |_| button.emit_clicked());
    }
    let weak = Rc::downgrade(&state);
    cancel.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            let effect = state.borrow_mut().controller.cancel();
            apply(&state, effect);
        }
    });
    let keys = gtk4::EventControllerKey::new();
    let button = cancel;
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk4::gdk::Key::Escape && button.is_sensitive() {
            button.emit_clicked();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    window.add_controller(keys);
    let weak = Rc::downgrade(&state);
    window.connect_close_request(move |window| {
        if let Some(state) = weak.upgrade() {
            let state = state.borrow();
            state.closed.store(true, Ordering::Release);
            if let Some(stream) = state.interrupt.lock().expect("interrupt lock").take() {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
        if let Some(app) = window.application() {
            app.quit();
        }
        glib::Propagation::Proceed
    });
    render(&state.borrow());
    window.present();
    LoginWindow { state }
}
