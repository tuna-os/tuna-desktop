//! GNOME Shell's polkit authentication agent (components/polkitAgent.js):
//! when an app asks for administrator rights, polkit asks the session's
//! agent, and the agent shows GNOME's "Authentication Required" dialog.
//! Without an agent every such request fails.
//!
//! The agent registers itself with org.freedesktop.PolicyKit1.Authority
//! on the system bus for this session, answers BeginAuthentication with
//! the dialog, and checks the password through polkit's own helper, the
//! way libpolkit-agent-1's `PolkitAgentSession` reaches it (polkit 128
//! src/polkitagent/polkitagentsession.c, `polkit_agent_session_initiate`):
//! first through `/run/polkit/agent-helper.socket`, which socket-activates
//! `polkit-agent-helper@.service` (since polkit 127 the helper binary is
//! no longer setuid), and only when that socket is missing or refuses the
//! connection by running the setuid `polkit-agent-helper-1 USER`.
//! Harnesses point `TUNA_POLKIT_HELPER` at a stand-in helper, which then
//! replaces both.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

const AGENT_PATH: &str = "/org/tuna/PolkitAgent";
const AUTHORITY: &str = "org.freedesktop.PolicyKit1";
const AUTHORITY_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";
const AUTHORITY_IFACE: &str = "org.freedesktop.PolicyKit1.Authority";
/// GNOME's failure line (polkitAgent.js).
const FAILED: &str = "Sorry, that didn’t work. Please try again.";

const XML: &str = r#"<node>
  <interface name="org.freedesktop.PolicyKit1.AuthenticationAgent">
    <method name="BeginAuthentication">
      <arg type="s" name="action_id" direction="in"/>
      <arg type="s" name="message" direction="in"/>
      <arg type="s" name="icon_name" direction="in"/>
      <arg type="a{ss}" name="details" direction="in"/>
      <arg type="s" name="cookie" direction="in"/>
      <arg type="a(sa{sv})" name="identities" direction="in"/>
    </method>
    <method name="CancelAuthentication">
      <arg type="s" name="cookie" direction="in"/>
    </method>
  </interface>
</node>"#;

/// One line from polkit's helper (its PAM conversation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperLine {
    /// PAM asks for a secret (a password): the prompt text.
    Secret(String),
    /// PAM asks for visible text.
    Text(String),
    /// PAM reports an error, or information.
    Error(String),
    Info(String),
    Success,
    /// FAILURE, or a line polkit does not know (which libpolkit-agent-1
    /// also ends the session on as a failure).
    Failure,
}

/// GLib's `g_strcompress`: the helper `g_strescape`s every message
/// (polkitagenthelper-pam.c `send_to_helper`), and polkit unescapes it.
fn strcompress(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&c) = b.get(i) else { break };
        match c {
            b'0'..=b'7' => {
                let mut v: u32 = 0;
                let mut n = 0;
                while n < 3 && i < b.len() && (b'0'..=b'7').contains(&b[i]) {
                    v = v * 8 + u32::from(b[i] - b'0');
                    i += 1;
                    n += 1;
                }
                out.push(v as u8);
                continue;
            }
            b'b' => out.push(b'\x08'),
            b'f' => out.push(b'\x0c'),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(b'\x0b'),
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse one helper output line as libpolkit-agent-1 does
/// (polkitagentsession.c `io_watch_have_data`).
pub fn parse_helper_line(line: &str) -> HelperLine {
    let line = strcompress(line.strip_suffix('\n').unwrap_or(line));
    let line = line.trim_end_matches('\r');
    let rest = |prefix: &str| line.strip_prefix(prefix).map(|r| r.trim().to_owned());
    if let Some(p) = rest("PAM_PROMPT_ECHO_OFF ") {
        HelperLine::Secret(p)
    } else if let Some(p) = rest("PAM_PROMPT_ECHO_ON ") {
        HelperLine::Text(p)
    } else if let Some(m) = rest("PAM_ERROR_MSG ") {
        HelperLine::Error(m)
    } else if let Some(m) = rest("PAM_TEXT_INFO ") {
        HelperLine::Info(m)
    } else if line.starts_with("SUCCESS") {
        HelperLine::Success
    } else {
        HelperLine::Failure
    }
}

/// GNOME's prompt text for a PAM prompt: "Password: " shows as "Password".
pub fn prompt_hint(prompt: &str) -> String {
    let p = prompt.trim().trim_end_matches([':', '：']).trim();
    if p.is_empty() {
        "Password".to_owned()
    } else {
        p.to_owned()
    }
}

/// Which identity to authenticate as, as GNOME picks it: the session's
/// user when polkit offers it, else root, else the first offered.
/// `identities` are the unix-user uids polkit offered.
pub fn pick_identity(identities: &[u32], own_uid: u32) -> Option<u32> {
    if identities.contains(&own_uid) {
        Some(own_uid)
    } else if identities.contains(&0) {
        Some(0)
    } else {
        identities.first().copied()
    }
}

/// (login name, display name) for `uid`.
fn user_names(uid: u32) -> Option<(String, String)> {
    // SAFETY: getpwuid returns static storage, read at once.
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return None;
        }
        let login = std::ffi::CStr::from_ptr((*pw).pw_name)
            .to_string_lossy()
            .into_owned();
        let gecos = if (*pw).pw_gecos.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr((*pw).pw_gecos)
                .to_string_lossy()
                .into_owned()
        };
        let display = crate::logic::display_name(&gecos, &login);
        Some((login, display))
    }
}

/// polkit's helper socket (polkit-agent-helper.socket).
const HELPER_SOCKET: &str = "/run/polkit/agent-helper.socket";

/// Where polkit's helper is reached.
#[derive(Debug, Clone, Default)]
pub struct Endpoints {
    /// The socket-activated helper, tried first.
    pub socket: Option<std::path::PathBuf>,
    /// The setuid helper binary, the fallback.
    pub helper: Option<std::path::PathBuf>,
}

impl Endpoints {
    fn system() -> Self {
        if let Some(p) = std::env::var_os("TUNA_POLKIT_HELPER") {
            return Self {
                socket: None,
                helper: Some(p.into()),
            };
        }
        let helper = [
            "/usr/lib/polkit-1/polkit-agent-helper-1",
            "/usr/libexec/polkit-agent-helper-1",
            "/usr/lib/policykit-1/polkit-agent-helper-1",
        ]
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.exists());
        Self {
            socket: Some(HELPER_SOCKET.into()),
            helper,
        }
    }
}

/// What the helper says, as `PolkitAgentSession`'s signals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Request { prompt: String, echo: bool },
    ShowError(String),
    ShowInfo(String),
    Completed(bool),
}

/// Why a session could not start at all.
#[derive(Debug)]
pub enum StartError {
    NoHelper,
    Spawn(glib::Error),
}

struct SessionInner {
    output: gio::OutputStream,
    /// Held so the socket stays open.
    conn: Option<gio::SocketConnection>,
    proc: Option<gio::Subprocess>,
    cancel: gio::Cancellable,
    done: Cell<bool>,
    on_event: Box<dyn Fn(SessionEvent)>,
}

impl SessionInner {
    fn stop(&self) {
        self.done.set(true);
        self.cancel.cancel();
        if let Some(c) = &self.conn {
            let _ = c.close(gio::Cancellable::NONE);
        }
        if let Some(p) = &self.proc {
            p.force_exit();
        }
    }
}

/// One conversation with polkit's helper: libpolkit-agent-1's
/// `PolkitAgentSession`. Dropping it ends the helper.
pub struct HelperSession(Rc<SessionInner>);

impl HelperSession {
    /// Connect to the helper for `user` and hand it `cookie`, as
    /// `polkit_agent_session_initiate` does: on the socket the first line
    /// is the user name, then the cookie; the setuid helper gets the user
    /// as its argument and the cookie on stdin. Events arrive on the
    /// thread-default main context.
    pub fn start(
        endpoints: &Endpoints,
        user: &str,
        cookie: &str,
        on_event: impl Fn(SessionEvent) + 'static,
    ) -> Result<Self, StartError> {
        let mut conn = None;
        if let Some(path) = endpoints.socket.as_ref().filter(|p| p.exists()) {
            match SocketClientExt::connect(
                &gio::SocketClient::new(),
                &gio::UnixSocketAddress::new(path),
                gio::Cancellable::NONE,
            ) {
                Ok(c) => conn = Some(c),
                Err(e) => eprintln!(
                    "tuna-shell-gtk: polkit helper socket {}: {e}; falling back to the setuid helper",
                    path.display()
                ),
            }
        }
        let (input, output, proc) = if let Some(c) = &conn {
            let output = c.output_stream();
            let _ = output.write_all(format!("{user}\n").as_bytes(), gio::Cancellable::NONE);
            (c.input_stream(), output, None)
        } else {
            let path = endpoints.helper.as_ref().ok_or(StartError::NoHelper)?;
            let argv = [path.as_os_str(), std::ffi::OsStr::new(user)];
            let proc = gio::Subprocess::newv(
                &argv,
                gio::SubprocessFlags::STDIN_PIPE | gio::SubprocessFlags::STDOUT_PIPE,
            )
            .map_err(StartError::Spawn)?;
            let (Some(stdin), Some(stdout)) = (proc.stdin_pipe(), proc.stdout_pipe()) else {
                proc.force_exit();
                return Err(StartError::NoHelper);
            };
            (stdout, stdin, Some(proc))
        };
        // The cookie goes on the stream so no other process can see it.
        let _ = output.write_all(format!("{cookie}\n").as_bytes(), gio::Cancellable::NONE);
        let inner = Rc::new(SessionInner {
            output,
            conn,
            proc,
            cancel: gio::Cancellable::new(),
            done: Cell::new(false),
            on_event: Box::new(on_event),
        });
        read_helper(&inner, gio::DataInputStream::new(&input));
        Ok(Self(inner))
    }

    /// Answer the helper's request (`polkit_agent_session_response`).
    pub fn response(&self, text: &str) {
        if self.0.done.get() {
            return;
        }
        let mut line = Vec::with_capacity(text.len() + 1);
        line.extend_from_slice(text.as_bytes());
        line.push(b'\n');
        let _ = self.0.output.write_all(&line, gio::Cancellable::NONE);
        line.fill(0);
    }
}

impl Drop for HelperSession {
    fn drop(&mut self) {
        self.0.stop();
    }
}

fn read_helper(inner: &Rc<SessionInner>, reader: gio::DataInputStream) {
    let inner2 = inner.clone();
    reader
        .clone()
        .read_line_utf8_async(glib::Priority::DEFAULT, Some(&inner.cancel), move |res| {
            if inner2.done.get() {
                return;
            }
            // The helper going away without a verdict is a failure.
            let line = match res {
                Ok(Some(line)) => parse_helper_line(&line),
                _ => HelperLine::Failure,
            };
            let event = match line {
                HelperLine::Secret(prompt) => SessionEvent::Request {
                    prompt,
                    echo: false,
                },
                HelperLine::Text(prompt) => SessionEvent::Request { prompt, echo: true },
                HelperLine::Error(m) => SessionEvent::ShowError(m),
                HelperLine::Info(m) => SessionEvent::ShowInfo(m),
                HelperLine::Success => SessionEvent::Completed(true),
                HelperLine::Failure => SessionEvent::Completed(false),
            };
            if let SessionEvent::Completed(_) = event {
                inner2.stop();
            } else {
                read_helper(&inner2, reader);
            }
            (inner2.on_event)(event);
        });
}

/// After a failed session, whether to start the next one at once.
/// GNOME restarts after every failure (polkitAgent.js
/// `_onSessionCompleted`); that is bounded by the user only when the
/// failed session had their answer. A helper that fails before taking
/// one would be restarted forever, so it waits for Authenticate instead.
pub fn restart_at_once(answered: bool) -> bool {
    answered
}

/// One authentication in progress.
struct Request {
    cookie: String,
    invocation: gio::DBusMethodInvocation,
    user: String,
    session: Option<HelperSession>,
    /// The current session has the user's answer.
    answered: bool,
}

pub struct PolkitAgent {
    window: gtk::Window,
    /// The modal fade in and out (`ModalDialog`, 100 ms).
    fade: crate::transient::ModalFade,
    message: gtk::Label,
    user_label: gtk::Label,
    entry: gtk::PasswordEntry,
    error: gtk::Label,
    info: gtk::Label,
    ok: gtk::Button,
    request: RefCell<Option<Request>>,
    /// The helper is waiting for the password.
    awaiting: Cell<bool>,
}

impl PolkitAgent {
    fn new(app: &gtk::Application) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("tuna-end-session");
        window.add_css_class("tuna-polkit");
        window.set_title(Some("Authentication Required"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::POLKIT_NAMESPACE));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);

        // GNOME's prompt-dialog: a 28em modal card.
        let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
        card.add_css_class("modal-dialog");
        card.add_css_class("prompt-dialog");
        card.set_halign(gtk::Align::Center);
        card.set_valign(gtk::Align::Center);
        card.set_size_request(411, -1);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.add_css_class("modal-dialog-content-box");
        let header = gtk::Box::new(gtk::Orientation::Vertical, 18);
        let title = gtk::Label::new(Some("Authentication Required"));
        title.add_css_class("message-dialog-title");
        let message = gtk::Label::new(None);
        message.add_css_class("message-dialog-description");
        message.set_wrap(true);
        message.set_max_width_chars(1);
        message.set_hexpand(true);
        message.set_justify(gtk::Justification::Center);
        header.append(&title);
        header.append(&message);
        content.append(&header);
        // The user (avatar and name) and the password.
        let user_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        user_box.add_css_class("polkit-dialog-user-layout");
        let avatar = gtk::Image::from_icon_name("avatar-default-symbolic");
        avatar.add_css_class("polkit-user-icon");
        avatar.set_halign(gtk::Align::Center);
        let user_label = gtk::Label::new(None);
        user_label.add_css_class("polkit-dialog-user-label");
        user_box.append(&avatar);
        user_box.append(&user_label);
        content.append(&user_box);
        let entry = gtk::PasswordEntry::new();
        entry.add_css_class("prompt-dialog-password-entry");
        entry.set_halign(gtk::Align::Center);
        entry.set_property("placeholder-text", "Password");
        // GNOME's polkit dialog is an StPasswordEntry: peek icon shown
        // unless `disable-show-password` denies, live (#357).
        crate::lockdown::bind_password_entry(&entry);
        entry.update_property(&[gtk::accessible::Property::Label("Password")]);
        content.append(&entry);
        let error = gtk::Label::new(None);
        error.add_css_class("prompt-dialog-error-label");
        error.set_wrap(true);
        error.set_visible(false);
        let info = gtk::Label::new(None);
        info.add_css_class("prompt-dialog-info-label");
        info.set_wrap(true);
        info.set_visible(false);
        content.append(&error);
        content.append(&info);
        card.append(&content);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.add_css_class("modal-dialog-button-box");
        buttons.set_homogeneous(true);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("modal-dialog-button");
        let ok = gtk::Button::with_label("Authenticate");
        ok.add_css_class("modal-dialog-button");
        ok.set_sensitive(false);
        buttons.append(&cancel);
        buttons.append(&ok);
        card.append(&buttons);
        window.set_child(Some(&card));

        let agent = Rc::new(Self {
            fade: crate::transient::ModalFade::new(&window),
            window,
            message,
            user_label,
            entry,
            error,
            info,
            ok,
            request: RefCell::new(None),
            awaiting: Cell::new(false),
        });
        {
            let weak = Rc::downgrade(&agent);
            cancel.connect_clicked(move |_| {
                if let Some(a) = weak.upgrade() {
                    a.finish(false);
                }
            });
        }
        {
            let weak = Rc::downgrade(&agent);
            agent.ok.connect_clicked(move |_| {
                if let Some(a) = weak.upgrade() {
                    a.submit();
                }
            });
        }
        {
            let weak = Rc::downgrade(&agent);
            agent.entry.connect_activate(move |_| {
                if let Some(a) = weak.upgrade() {
                    a.submit();
                }
            });
        }
        {
            let ok = agent.ok.clone();
            agent
                .entry
                .connect_changed(move |e| ok.set_sensitive(!e.text().is_empty()));
        }
        let keys = gtk::EventControllerKey::new();
        {
            let weak = Rc::downgrade(&agent);
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    if let Some(a) = weak.upgrade() {
                        a.finish(false);
                    }
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
        }
        agent.window.add_controller(keys);
        agent
    }

    /// polkit's BeginAuthentication.
    fn begin(
        self: &Rc<Self>,
        message: &str,
        cookie: &str,
        identities: &[u32],
        invocation: gio::DBusMethodInvocation,
    ) {
        if self.request.borrow().is_some() {
            invocation.return_dbus_error(
                "org.freedesktop.PolicyKit1.Error.Failed",
                "an authentication is already in progress",
            );
            return;
        }
        // SAFETY: getuid never fails.
        let own = unsafe { libc::getuid() };
        let Some(uid) = pick_identity(identities, own) else {
            invocation.return_dbus_error("org.freedesktop.PolicyKit1.Error.Failed", "no identity");
            return;
        };
        let Some((login, display)) = user_names(uid) else {
            invocation.return_dbus_error("org.freedesktop.PolicyKit1.Error.Failed", "unknown user");
            return;
        };
        self.message.set_label(message);
        if uid == 0 {
            self.user_label.set_label("Administrator");
            self.user_label
                .add_css_class("polkit-dialog-user-root-label");
        } else {
            self.user_label.set_label(&display);
            self.user_label
                .remove_css_class("polkit-dialog-user-root-label");
        }
        self.error.set_visible(false);
        self.info.set_visible(false);
        self.entry.set_text("");
        *self.request.borrow_mut() = Some(Request {
            cookie: cookie.to_owned(),
            invocation,
            user: login,
            session: None,
            answered: false,
        });
        self.fade.present();
        self.entry.grab_focus();
        self.start_helper();
    }

    /// Start a session with polkit's helper for the request's user.
    fn start_helper(self: &Rc<Self>) {
        let (user, cookie) = match self.request.borrow_mut().as_mut() {
            Some(r) => {
                r.session = None;
                r.answered = false;
                (r.user.clone(), r.cookie.clone())
            }
            None => return,
        };
        let weak = Rc::downgrade(self);
        let started = HelperSession::start(&Endpoints::system(), &user, &cookie, move |ev| {
            if let Some(agent) = weak.upgrade() {
                agent.on_session_event(ev);
            }
        });
        match started {
            Ok(session) => {
                self.ok.set_sensitive(!self.entry.text().is_empty());
                if let Some(r) = self.request.borrow_mut().as_mut() {
                    r.session = Some(session);
                }
            }
            Err(e) => {
                if let StartError::Spawn(e) = &e {
                    eprintln!("tuna-shell-gtk: polkit helper: {e}");
                }
                self.error.set_label(match e {
                    StartError::NoHelper => "No authentication helper is installed.",
                    StartError::Spawn(_) => FAILED,
                });
                self.error.set_visible(true);
                // Authenticate tries again.
                self.ok.set_sensitive(true);
            }
        }
    }

    fn on_session_event(self: &Rc<Self>, ev: SessionEvent) {
        match ev {
            SessionEvent::Request { prompt, .. } => {
                self.entry
                    .set_property("placeholder-text", prompt_hint(&prompt));
                self.entry.set_text("");
                self.entry.set_sensitive(true);
                self.entry.grab_focus();
                self.awaiting.set(true);
            }
            SessionEvent::ShowError(m) => {
                self.entry.set_text("");
                self.error.set_label(&m);
                self.error.set_visible(true);
                self.info.set_visible(false);
            }
            SessionEvent::ShowInfo(m) => {
                self.entry.set_text("");
                self.info.set_label(&m);
                self.info.set_visible(true);
                self.error.set_visible(false);
            }
            SessionEvent::Completed(true) => self.finish(true),
            SessionEvent::Completed(false) => {
                let answered = match self.request.borrow_mut().as_mut() {
                    Some(r) => {
                        r.session = None;
                        r.answered
                    }
                    None => return,
                };
                self.awaiting.set(false);
                self.entry.set_text("");
                self.entry.set_sensitive(true);
                // GNOME keeps a PAM error that is already showing.
                if !self.error.is_visible() {
                    self.error.set_label(FAILED);
                    self.error.set_visible(true);
                }
                self.info.set_visible(false);
                if restart_at_once(answered) {
                    self.start_helper();
                } else {
                    eprintln!("tuna-shell-gtk: polkit helper ended before asking for a password");
                    self.ok.set_sensitive(true);
                }
            }
        }
    }

    fn submit(self: &Rc<Self>) {
        let idle = self
            .request
            .borrow()
            .as_ref()
            .is_some_and(|r| r.session.is_none());
        if idle {
            // The last helper failed without asking: start over.
            self.entry.set_text("");
            self.error.set_visible(false);
            self.start_helper();
            return;
        }
        if !self.awaiting.get() {
            return;
        }
        let password = self.entry.text();
        if password.is_empty() {
            return;
        }
        let mut req = self.request.borrow_mut();
        let Some(r) = req.as_mut() else { return };
        let Some(session) = r.session.as_ref() else {
            return;
        };
        self.awaiting.set(false);
        r.answered = true;
        // Let GtkText leave focus before making its entry insensitive.
        if let Some(root) = self.entry.root() {
            root.set_focus(gtk::Widget::NONE);
        }
        self.entry.set_sensitive(false);
        self.ok.set_sensitive(false);
        self.error.set_visible(false);
        self.info.set_visible(false);
        session.response(&password);
        drop(req);
        self.entry.set_text("");
    }

    /// End the request: authorized, or cancelled by the user.
    fn finish(&self, authorized: bool) {
        self.fade.hide();
        self.entry.set_text("");
        self.awaiting.set(false);
        let Some(request) = self.request.borrow_mut().take() else {
            return;
        };
        drop(request.session);
        if authorized {
            request.invocation.return_value(None);
        } else {
            request.invocation.return_dbus_error(
                "org.freedesktop.PolicyKit1.Error.Cancelled",
                "The user cancelled the authentication",
            );
        }
    }

    /// polkit's CancelAuthentication.
    fn cancel(&self, cookie: &str) {
        let ours = self
            .request
            .borrow()
            .as_ref()
            .is_some_and(|r| r.cookie == cookie);
        if ours {
            self.finish(false);
        }
    }
}

/// The unix-user uids among polkit's identities.
fn unix_users(identities: &glib::Variant) -> Vec<u32> {
    (0..identities.n_children())
        .filter_map(|i| {
            let ident = identities.child_value(i);
            let kind: String = ident.child_value(0).get()?;
            if kind != "unix-user" {
                return None;
            }
            let details = glib::VariantDict::new(Some(&ident.child_value(1)));
            details.lookup::<u32>("uid").ok().flatten()
        })
        .collect()
}

/// This session's polkit subject: logind's session id.
fn session_subject() -> glib::Variant {
    let id = std::env::var("XDG_SESSION_ID").unwrap_or_default();
    let details: HashMap<String, glib::Variant> =
        HashMap::from([("session-id".to_owned(), id.to_variant())]);
    ("unix-session", details).to_variant()
}

/// Serve the agent and register it with polkit for this session.
pub fn start(app: &gtk::Application) {
    let agent = PolkitAgent::new(app);
    let node = match gio::DBusNodeInfo::for_xml(XML) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("tuna-shell-gtk: polkit agent interface: {e}");
            return;
        }
    };
    gio::bus_get(gio::BusType::System, gio::Cancellable::NONE, move |conn| {
        let Ok(conn) = conn else {
            eprintln!("tuna-shell-gtk: polkit agent: no system bus");
            return;
        };
        let Some(info) = node.lookup_interface("org.freedesktop.PolicyKit1.AuthenticationAgent")
        else {
            return;
        };
        let calls = agent.clone();
        let registered =
            conn.register_object(AGENT_PATH, &info)
                .method_call(move |_, _, _, _, method, params, invocation| match method {
                    "BeginAuthentication" => {
                        let message: String = params.child_value(1).get().unwrap_or_default();
                        let cookie: String = params.child_value(4).get().unwrap_or_default();
                        let users = unix_users(&params.child_value(5));
                        calls.begin(&message, &cookie, &users, invocation);
                    }
                    "CancelAuthentication" => {
                        let cookie: String = params.child_value(0).get().unwrap_or_default();
                        calls.cancel(&cookie);
                        invocation.return_value(None);
                    }
                    _ => invocation
                        .return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
                })
                .build();
        if let Err(e) = registered {
            eprintln!("tuna-shell-gtk: polkit agent object: {e}");
            return;
        }
        // Register now, and again whenever polkit (re)starts, as GNOME's
        // agent does. A NameOwnerChanged watch: gio's name watcher hands
        // its callbacks a NULL connection when the bus goes away, which
        // its Rust binding unwraps.
        let conn2 = conn.clone();
        crate::services::watch_name(&conn, AUTHORITY, move |owned| {
            if owned {
                register(&conn2);
            }
        });
    });
}

/// Register the agent with polkit's Authority for this session.
fn register(conn: &gio::DBusConnection) {
    let locale = std::env::var("LANG").unwrap_or_else(|_| "C".to_owned());
    conn.call(
        Some(AUTHORITY),
        AUTHORITY_PATH,
        AUTHORITY_IFACE,
        "RegisterAuthenticationAgent",
        Some(&glib::Variant::tuple_from_iter([
            session_subject(),
            locale.to_variant(),
            AGENT_PATH.to_variant(),
        ])),
        None,
        gio::DBusCallFlags::NONE,
        10_000,
        gio::Cancellable::NONE,
        |res| match res {
            Ok(_) => eprintln!("tuna-shell-gtk: polkit agent registered"),
            Err(e) => eprintln!("tuna-shell-gtk: polkit agent not registered: {e}"),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_lines_parse() {
        assert_eq!(
            parse_helper_line("PAM_PROMPT_ECHO_OFF Password: \n"),
            HelperLine::Secret("Password:".into())
        );
        assert_eq!(
            parse_helper_line("PAM_ERROR_MSG nope"),
            HelperLine::Error("nope".into())
        );
        assert_eq!(parse_helper_line("SUCCESS"), HelperLine::Success);
        assert_eq!(parse_helper_line("FAILURE\n"), HelperLine::Failure);
        assert_eq!(prompt_hint("Password: "), "Password");
        assert_eq!(prompt_hint(""), "Password");
    }

    #[test]
    fn registration_matches_polkits_signature() {
        let args = glib::Variant::tuple_from_iter([
            session_subject(),
            "C".to_variant(),
            AGENT_PATH.to_variant(),
        ]);
        assert_eq!(args.type_().as_str(), "((sa{sv})ss)");
    }

    #[test]
    fn identities_are_picked_like_gnome() {
        assert_eq!(pick_identity(&[0, 1000], 1000), Some(1000));
        assert_eq!(pick_identity(&[1001, 0], 1000), Some(0));
        assert_eq!(pick_identity(&[1001], 1000), Some(1001));
        assert_eq!(pick_identity(&[], 1000), None);
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    const PASSWORD: &str = "s3cret-admin-pw";

    /// Run one session to completion on a private main context, answering
    /// every request with `answer`; returns its events.
    fn run(endpoints: &Endpoints, answer: Option<&str>) -> Result<Vec<SessionEvent>, StartError> {
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let events = Rc::new(RefCell::new(Vec::new()));
            let session: Rc<RefCell<Option<HelperSession>>> = Rc::new(RefCell::new(None));
            let (ev2, s2) = (events.clone(), session.clone());
            let answer = answer.map(str::to_owned);
            let started = HelperSession::start(endpoints, "alice", "cookie-42", move |ev| {
                if let (SessionEvent::Request { .. }, Some(a)) = (&ev, &answer) {
                    if let Some(s) = s2.borrow().as_ref() {
                        s.response(a);
                    }
                }
                ev2.borrow_mut().push(ev);
            })?;
            *session.borrow_mut() = Some(started);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !events
                .borrow()
                .iter()
                .any(|e| matches!(e, SessionEvent::Completed(_)))
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "session never completed"
                );
                ctx.iteration(true);
            }
            // Nothing more arrives after the verdict.
            while ctx.iteration(false) {}
            drop(session.borrow_mut().take());
            let out = events.borrow().clone();
            Ok(out)
        })
        .unwrap()
    }

    /// A fake polkit-agent-helper@.service behind a socket: records every
    /// line it is sent and plays `script` (None = read a line).
    fn fake_socket(
        dir: &Path,
        script: Vec<Option<&'static str>>,
    ) -> (PathBuf, std::thread::JoinHandle<Vec<String>>) {
        let path = dir.join("agent-helper.socket");
        let listener = UnixListener::bind(&path).unwrap();
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut out = stream.try_clone().unwrap();
            let mut input = BufReader::new(stream);
            let mut got = Vec::new();
            for step in script {
                match step {
                    None => {
                        let mut l = String::new();
                        input.read_line(&mut l).unwrap();
                        got.push(l);
                    }
                    Some(line) => {
                        let _ = out.write_all(line.as_bytes());
                    }
                }
            }
            got
        });
        (path, handle)
    }

    /// A fake setuid helper: logs its argv and stdin lines to `log`.
    fn fake_helper(dir: &Path, body: &str) -> (PathBuf, PathBuf) {
        // Never open the executable for writing in this multithreaded test
        // process: a concurrent fork can retain that fd even after we close
        // it, making exec fail with ETXTBSY. Only the writer child opens the
        // inode, and it must exit before any caller executes the fixture.
        let log = dir.join("helper.log");
        let path = dir.join("helper");
        let script = format!(
            "#!/bin/sh\necho \"argv:$*\" >> '{log}'\n{body}\n",
            log = log.display()
        );
        let mut writer = Command::new("/bin/sh")
            .args([
                "-c",
                "cat > \"$1\" && chmod 755 \"$1\"",
                "fake-helper-writer",
            ])
            .arg(&path)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        writer
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        assert!(writer.wait().unwrap().success(), "helper writer failed");
        (path, log)
    }

    const CONVERSE: &str = "read c; echo \"cookie:$c\" >> \"$(dirname \"$0\")/helper.log\"\n\
        echo 'PAM_PROMPT_ECHO_OFF Password: '\n\
        read p; echo \"pw:$p\" >> \"$(dirname \"$0\")/helper.log\"\n\
        if [ \"$p\" = s3cret-admin-pw ]; then echo SUCCESS; else echo FAILURE; fi";

    #[test]
    fn fake_helpers_are_ready_to_execute_under_parallel_process_creation() {
        const WORKERS: usize = 4;
        let start = std::sync::Barrier::new(WORKERS);
        std::thread::scope(|scope| {
            for _ in 0..WORKERS {
                let start = &start;
                scope.spawn(move || {
                    start.wait();
                    for _ in 0..16 {
                        let dir = tempfile::tempdir().unwrap();
                        let (helper, log) = fake_helper(
                            dir.path(),
                            "printf '%s\\n' 'literal $(not-expanded) \\backslash %s'; exit 7",
                        );
                        assert!(!log.exists(), "the writer must not execute the helper");
                        // Other workers keep spawning while fixtures are
                        // written. Every execution must work on its first try.
                        let output = Command::new(helper).arg("alice").output().unwrap();
                        assert_eq!(output.status.code(), Some(7));
                        assert_eq!(output.stdout, b"literal $(not-expanded) \\backslash %s\n");
                        assert!(output.stderr.is_empty());
                        assert_eq!(std::fs::read_to_string(log).unwrap(), "argv:alice\n");
                    }
                });
            }
        });
    }

    #[test]
    fn socket_comes_first_and_speaks_polkits_wire_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, server) = fake_socket(
            dir.path(),
            vec![
                None,
                None,
                Some("PAM_PROMPT_ECHO_OFF Password: \n"),
                None,
                Some("SUCCESS\n"),
            ],
        );
        let (helper, log) = fake_helper(dir.path(), CONVERSE);
        let events = run(
            &Endpoints {
                socket: Some(socket),
                helper: Some(helper),
            },
            Some(PASSWORD),
        )
        .unwrap();
        assert_eq!(
            server.join().unwrap(),
            ["alice\n", "cookie-42\n", &format!("{PASSWORD}\n")]
        );
        assert_eq!(
            events,
            [
                SessionEvent::Request {
                    prompt: "Password:".into(),
                    echo: false
                },
                SessionEvent::Completed(true)
            ]
        );
        assert!(
            !log.exists(),
            "the setuid helper must not run when the socket answers"
        );
    }

    #[test]
    fn wrong_password_over_the_socket_is_denied() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, server) = fake_socket(
            dir.path(),
            vec![
                None,
                None,
                Some("PAM_PROMPT_ECHO_OFF Password: \n"),
                None,
                Some("PAM_ERROR_MSG Authentication\\040failure\n"),
                Some("FAILURE\n"),
            ],
        );
        let events = run(
            &Endpoints {
                socket: Some(socket),
                helper: None,
            },
            Some("wrong"),
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(
            events[1],
            SessionEvent::ShowError("Authentication failure".into())
        );
        assert_eq!(events.last(), Some(&SessionEvent::Completed(false)));
    }

    #[test]
    fn refused_socket_falls_back_to_the_setuid_helper_without_leaking_the_password() {
        let dir = tempfile::tempdir().unwrap();
        // A stale socket: the path exists but nobody listens.
        let socket = dir.path().join("agent-helper.socket");
        drop(UnixListener::bind(&socket).unwrap());
        assert!(socket.exists());
        let (helper, log) = fake_helper(dir.path(), CONVERSE);
        let events = run(
            &Endpoints {
                socket: Some(socket),
                helper: Some(helper),
            },
            Some(PASSWORD),
        )
        .unwrap();
        assert_eq!(events.last(), Some(&SessionEvent::Completed(true)));
        let log = std::fs::read_to_string(log).unwrap();
        // The user is the only argument; cookie and password go on stdin.
        assert_eq!(
            log,
            format!("argv:alice\ncookie:cookie-42\npw:{PASSWORD}\n")
        );
    }

    #[test]
    fn absent_socket_and_helper_cannot_start() {
        let dir = tempfile::tempdir().unwrap();
        let r = run(
            &Endpoints {
                socket: Some(dir.path().join("missing.socket")),
                helper: None,
            },
            Some(PASSWORD),
        );
        assert!(matches!(r, Err(StartError::NoHelper)));
    }

    #[test]
    fn crashing_helper_fails_without_ever_taking_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let (helper, log) = fake_helper(
            dir.path(),
            "echo 'polkit-agent-helper-1: needs to be setuid root' >&2; exit 1",
        );
        let events = run(
            &Endpoints {
                socket: None,
                helper: Some(helper),
            },
            Some(PASSWORD),
        )
        .unwrap();
        assert_eq!(events, [SessionEvent::Completed(false)]);
        assert!(!std::fs::read_to_string(log).unwrap().contains(PASSWORD));
        // GNOME would restart it at once; with no answer taken, that is
        // the respawn loop, so it waits for the user instead.
        assert!(!restart_at_once(false));
        assert!(restart_at_once(true));
    }

    #[test]
    fn spoofed_or_unknown_lines_end_the_session_as_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, server) = fake_socket(
            dir.path(),
            vec![None, None, Some("YES_REALLY_SUCCESS\n"), Some("SUCCESS\n")],
        );
        let events = run(
            &Endpoints {
                socket: Some(socket),
                helper: None,
            },
            Some(PASSWORD),
        )
        .unwrap();
        // No prompt was asked, so the password was never sent.
        assert_eq!(server.join().unwrap(), ["alice\n", "cookie-42\n"]);
        assert_eq!(events, [SessionEvent::Completed(false)]);
    }

    #[test]
    fn helper_messages_are_unescaped_like_g_strcompress() {
        assert_eq!(strcompress(r"a\tb\\c\042\n"), "a\tb\\c\"\n");
        assert_eq!(
            parse_helper_line("PAM_TEXT_INFO Touch\\040the\\040key\n"),
            HelperLine::Info("Touch the key".into())
        );
        assert_eq!(parse_helper_line("garbage"), HelperLine::Failure);
    }
}
