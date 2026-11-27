//! GDM 51's greeter D-Bus API, as libgdm's `GdmClient` speaks it.
//!
//! The greeter asks GDM's daemon for its private channel on the system
//! bus (`org.gnome.DisplayManager.Manager.OpenSession`, allowed only to
//! the greeter session's own user), then connects peer to peer to the
//! returned address (`unix:path=/run/gdm/dbus/dbus-…`, no message bus,
//! EXTERNAL authentication). On that connection the session object
//! `/org/gnome/DisplayManager/Session` carries
//! `org.gnome.DisplayManager.UserVerifier` (the PAM conversations) and
//! `org.gnome.DisplayManager.Greeter` (session choice and start).
//!
//! Sources: GDM 51.1.1 `daemon/gdm-manager.xml`, `daemon/gdm-session.xml`,
//! `daemon/gdm-session.c`, `libgdm/gdm-client.c`
//! (<https://gitlab.gnome.org/GNOME/gdm/-/tree/51.1.1>).

use gio::prelude::*;

pub const MANAGER_NAME: &str = "org.gnome.DisplayManager";
pub const MANAGER_PATH: &str = "/org/gnome/DisplayManager/Manager";
pub const MANAGER_INTERFACE: &str = "org.gnome.DisplayManager.Manager";
pub const SESSION_PATH: &str = "/org/gnome/DisplayManager/Session";
pub const USER_VERIFIER: &str = "org.gnome.DisplayManager.UserVerifier";
pub const GREETER: &str = "org.gnome.DisplayManager.Greeter";
/// The UserVerifier extensions gnome-shell 51 enables (loginDialog.js
/// `set_enabled_extensions`); GDM exports them on the channel once asked.
pub const CHOICE_LIST: &str = "org.gnome.DisplayManager.UserVerifier.ChoiceList";
pub const CUSTOM_JSON: &str = "org.gnome.DisplayManager.UserVerifier.CustomJSON";
/// The extensions, in gnome-shell's order.
pub const EXTENSIONS: [&str; 2] = [CHOICE_LIST, CUSTOM_JSON];

/// One signal of the session object, parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    ConversationStarted {
        service: String,
    },
    ConversationStopped {
        service: String,
    },
    Info {
        service: String,
        text: String,
    },
    Problem {
        service: String,
        text: String,
    },
    InfoQuery {
        service: String,
        query: String,
    },
    SecretInfoQuery {
        service: String,
        query: String,
    },
    /// GDM reset the session (after a `Cancel`, or on its own): every
    /// conversation stops and the selected session is forgotten.
    Reset,
    ServiceUnavailable {
        service: String,
        message: String,
    },
    VerificationFailed {
        service: String,
    },
    VerificationComplete {
        service: String,
    },
    DefaultSessionNameChanged {
        session: String,
    },
    SessionOpened {
        service: String,
        session_id: String,
    },
    /// `ChoiceList.ChoiceQuery`: a PAM module offers choices (GDM's
    /// `GDM_PAM_EXTENSION_CHOICE_LIST`), in the module's order.
    ChoiceQuery {
        service: String,
        prompt: String,
        choices: Vec<(String, String)>,
    },
    /// `CustomJSON.Request`: a JSON query (SSSD's `auth-mechanisms`).
    JsonRequest {
        service: String,
        protocol: String,
        version: u32,
        json: String,
    },
    /// `Greeter.TimedLoginRequested`, the answer to
    /// `GetTimedLoginDetails` when timed login is configured.
    TimedLoginRequested {
        user: String,
        delay: i32,
    },
}

/// Parse a signal of [`USER_VERIFIER`] or [`GREETER`]; `None` for
/// anything else (other interfaces, unused signals, wrong types).
pub fn parse_signal(interface: &str, member: &str, params: &glib::Variant) -> Option<Signal> {
    let one = || params.get::<(String,)>().map(|(a,)| a);
    let two = || params.get::<(String, String)>();
    match (interface, member) {
        (USER_VERIFIER, "ConversationStarted") => {
            one().map(|service| Signal::ConversationStarted { service })
        }
        (USER_VERIFIER, "ConversationStopped") => {
            one().map(|service| Signal::ConversationStopped { service })
        }
        (USER_VERIFIER, "Info") => two().map(|(service, text)| Signal::Info { service, text }),
        (USER_VERIFIER, "Problem") => {
            two().map(|(service, text)| Signal::Problem { service, text })
        }
        (USER_VERIFIER, "InfoQuery") => {
            two().map(|(service, query)| Signal::InfoQuery { service, query })
        }
        (USER_VERIFIER, "SecretInfoQuery") => {
            two().map(|(service, query)| Signal::SecretInfoQuery { service, query })
        }
        (USER_VERIFIER, "Reset") => params.get::<()>().map(|()| Signal::Reset),
        (USER_VERIFIER, "ServiceUnavailable") => {
            two().map(|(service, message)| Signal::ServiceUnavailable { service, message })
        }
        (USER_VERIFIER, "VerificationFailed") => {
            one().map(|service| Signal::VerificationFailed { service })
        }
        (USER_VERIFIER, "VerificationComplete") => {
            one().map(|service| Signal::VerificationComplete { service })
        }
        (GREETER, "DefaultSessionNameChanged") => {
            one().map(|session| Signal::DefaultSessionNameChanged { session })
        }
        (GREETER, "SessionOpened") => two().map(|(service, session_id)| Signal::SessionOpened {
            service,
            session_id,
        }),
        (GREETER, "TimedLoginRequested") => params
            .get::<(String, i32)>()
            .map(|(user, delay)| Signal::TimedLoginRequested { user, delay }),
        (CHOICE_LIST, "ChoiceQuery") => {
            if params.type_().as_str() != "(ssa{ss})" {
                return None;
            }
            let text = |i: usize| params.child_value(i).str().map(str::to_owned);
            let choices = params
                .child_value(2)
                .iter()
                .filter_map(|entry| {
                    Some((
                        entry.child_value(0).str()?.to_owned(),
                        entry.child_value(1).str()?.to_owned(),
                    ))
                })
                .collect();
            Some(Signal::ChoiceQuery {
                service: text(0)?,
                prompt: text(1)?,
                choices,
            })
        }
        (CUSTOM_JSON, "Request") => params.get::<(String, String, u32, String)>().map(
            |(service, protocol, version, json)| Signal::JsonRequest {
                service,
                protocol,
                version,
                json,
            },
        ),
        _ => None,
    }
}

/// A call on the session object. Answers are never in `Debug` output.
#[derive(Clone, PartialEq, Eq)]
pub enum Call {
    /// `UserVerifier.EnableExtensions`, once per channel (libgdm does it
    /// when it makes the channel's UserVerifier proxy).
    EnableExtensions,
    BeginVerificationForUser {
        service: String,
        user: String,
    },
    /// A conversation that finds its user itself (a smartcard's
    /// certificate names it).
    BeginVerification {
        service: String,
    },
    AnswerQuery {
        service: String,
        answer: String,
    },
    /// `ChoiceList.SelectChoice`.
    SelectChoice {
        service: String,
        choice: String,
    },
    /// `CustomJSON.Reply`; the JSON may carry a password or PIN.
    JsonReply {
        service: String,
        json: String,
    },
    Cancel,
    SelectSession {
        session: String,
    },
    StartSessionWhenReady {
        service: String,
    },
    /// libgdm asks for the timed login details whenever it makes the
    /// Greeter proxy; GDM answers with `TimedLoginRequested`.
    GetTimedLoginDetails,
    BeginAutoLogin {
        user: String,
    },
    StopConflictingSession,
}

impl Call {
    /// Whether the call belongs to the conversation's channel (GDM's
    /// UserVerifier and its extensions) rather than the greeter's.
    pub fn is_verifier(&self) -> bool {
        self.wire().0 != GREETER
    }
}

impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnableExtensions => write!(f, "EnableExtensions({})", EXTENSIONS.join(", ")),
            Self::BeginVerificationForUser { service, user } => {
                write!(f, "BeginVerificationForUser({service}, {user})")
            }
            Self::BeginVerification { service } => write!(f, "BeginVerification({service})"),
            Self::AnswerQuery { service, .. } => write!(f, "AnswerQuery({service}, <redacted>)"),
            Self::SelectChoice { service, choice } => {
                write!(f, "SelectChoice({service}, {choice})")
            }
            Self::JsonReply { service, .. } => write!(f, "Reply({service}, <redacted>)"),
            Self::Cancel => write!(f, "Cancel"),
            Self::SelectSession { session } => write!(f, "SelectSession({session})"),
            Self::StartSessionWhenReady { service } => {
                write!(f, "StartSessionWhenReady({service}, true)")
            }
            Self::GetTimedLoginDetails => write!(f, "GetTimedLoginDetails"),
            Self::BeginAutoLogin { user } => write!(f, "BeginAutoLogin({user})"),
            Self::StopConflictingSession => write!(f, "StopConflictingSession"),
        }
    }
}

impl Call {
    /// Interface, method and arguments on the wire.
    pub fn wire(&self) -> (&'static str, &'static str, glib::Variant) {
        match self {
            Self::EnableExtensions => (
                USER_VERIFIER,
                "EnableExtensions",
                (EXTENSIONS.to_vec(),).to_variant(),
            ),
            Self::BeginVerificationForUser { service, user } => (
                USER_VERIFIER,
                "BeginVerificationForUser",
                (service.as_str(), user.as_str()).to_variant(),
            ),
            Self::BeginVerification { service } => (
                USER_VERIFIER,
                "BeginVerification",
                (service.as_str(),).to_variant(),
            ),
            Self::AnswerQuery { service, answer } => (
                USER_VERIFIER,
                "AnswerQuery",
                (service.as_str(), answer.as_str()).to_variant(),
            ),
            Self::SelectChoice { service, choice } => (
                CHOICE_LIST,
                "SelectChoice",
                (service.as_str(), choice.as_str()).to_variant(),
            ),
            Self::JsonReply { service, json } => (
                CUSTOM_JSON,
                "Reply",
                (service.as_str(), json.as_str()).to_variant(),
            ),
            Self::Cancel => (USER_VERIFIER, "Cancel", ().to_variant()),
            Self::SelectSession { session } => {
                (GREETER, "SelectSession", (session.as_str(),).to_variant())
            }
            Self::StartSessionWhenReady { service } => (
                GREETER,
                "StartSessionWhenReady",
                (service.as_str(), true).to_variant(),
            ),
            Self::GetTimedLoginDetails => (GREETER, "GetTimedLoginDetails", ().to_variant()),
            Self::BeginAutoLogin { user } => {
                (GREETER, "BeginAutoLogin", (user.as_str(),).to_variant())
            }
            Self::StopConflictingSession => (GREETER, "StopConflictingSession", ().to_variant()),
        }
    }
}

/// `Manager.OpenReauthenticationChannel` (libgdm
/// `gdm_client_open_reauthentication_channel`): on the login screen GDM
/// answers only for a user whose session runs, with the address of that
/// session's reauthentication channel; otherwise `AccessDenied`
/// (`gdm-manager.c`: "Login screen only allowed to open
/// reauthentication channels for running sessions").
pub async fn open_reauthentication_channel(
    system: &gio::DBusConnection,
    user: &str,
) -> Result<String, glib::Error> {
    let reply = system
        .call_future(
            Some(MANAGER_NAME),
            MANAGER_PATH,
            MANAGER_INTERFACE,
            "OpenReauthenticationChannel",
            Some(&(user,).to_variant()),
            glib::VariantTy::new("(s)").ok(),
            gio::DBusCallFlags::NONE,
            -1,
        )
        .await?;
    Ok(reply.get::<(String,)>().map(|(a,)| a).unwrap_or_default())
}

/// Whether a D-Bus error is GDM's refusal (`AccessDenied`), after which
/// userVerifier.js verifies on the login screen's own channel.
pub fn access_denied(error: &glib::Error) -> bool {
    gio::DBusError::remote_error(error).as_deref()
        == Some("org.freedesktop.DBus.Error.AccessDenied")
        || error.matches(gio::DBusError::AccessDenied)
}

/// `Manager.OpenSession` on the system bus: the private channel's address.
pub async fn open_session(system: &gio::DBusConnection) -> Result<String, glib::Error> {
    let reply = system
        .call_future(
            Some(MANAGER_NAME),
            MANAGER_PATH,
            MANAGER_INTERFACE,
            "OpenSession",
            None,
            Some(glib::VariantTy::new("(s)").unwrap()),
            gio::DBusCallFlags::NONE,
            -1,
        )
        .await?;
    Ok(reply.get::<(String,)>().map(|(a,)| a).unwrap_or_default())
}

/// The peer-to-peer connection to GDM's session server, as
/// `gdm_client_get_connection` makes it.
pub async fn connect(address: &str) -> Result<gio::DBusConnection, glib::Error> {
    gio::DBusConnection::for_address_future(
        address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT,
        None,
    )
    .await
}

/// A subscription to the session object's signals; dropping it
/// unsubscribes.
pub struct Subscription {
    connection: gio::DBusConnection,
    id: std::ffi::c_uint,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        use glib::translate::ToGlibPtr;
        // SAFETY: `id` came from `g_dbus_connection_signal_subscribe` on
        // this connection and is unsubscribed only here, once.
        unsafe {
            gio::ffi::g_dbus_connection_signal_unsubscribe(
                self.connection.to_glib_none().0,
                self.id,
            );
        }
    }
}

type Handler = Box<dyn Fn(Signal)>;

/// Every signal of the session object, parsed, until the returned
/// subscription is dropped. The handler runs in the calling thread's
/// default main context.
///
/// Not gio's `subscribe_to_signal`: gio-rs 0.22 reads the sender as a
/// non-null string, but signals on a peer-to-peer connection have no
/// sender (GDBus passes NULL), so its trampoline panics across the FFI
/// boundary and the process aborts on GDM's first signal.
pub fn subscribe(
    connection: &gio::DBusConnection,
    handler: impl Fn(Signal) + 'static,
) -> Subscription {
    use glib::translate::{from_glib_none, ToGlibPtr};
    use std::ffi::{c_char, CStr};

    unsafe extern "C" fn trampoline(
        _connection: *mut gio::ffi::GDBusConnection,
        _sender: *const c_char,
        _object_path: *const c_char,
        interface: *const c_char,
        member: *const c_char,
        parameters: *mut glib::ffi::GVariant,
        data: glib::ffi::gpointer,
    ) {
        // SAFETY: GDBus passes a signal's interface and member as
        // non-null strings and its parameters as a non-null tuple, valid
        // for the call; `data` is the `Handler` boxed below, freed only by
        // `free` after the last call.
        let (interface, member, parameters, handler) = unsafe {
            (
                CStr::from_ptr(interface),
                CStr::from_ptr(member),
                from_glib_none::<_, glib::Variant>(parameters),
                &*(data as *const Handler),
            )
        };
        let (Ok(interface), Ok(member)) = (interface.to_str(), member.to_str()) else {
            return;
        };
        if let Some(signal) = parse_signal(interface, member, &parameters) {
            handler(signal);
        }
    }
    unsafe extern "C" fn free(data: glib::ffi::gpointer) {
        // SAFETY: `data` is the box leaked below; GDBus frees it once.
        drop(unsafe { Box::from_raw(data as *mut Handler) });
    }

    let data: Box<Handler> = Box::new(Box::new(handler));
    let path = std::ffi::CString::new(SESSION_PATH).expect("no NUL in the session path");
    // SAFETY: every pointer is valid for the call (the path is copied),
    // and `trampoline`/`free` match GDBusSignalCallback/GDestroyNotify.
    let id = unsafe {
        gio::ffi::g_dbus_connection_signal_subscribe(
            connection.to_glib_none().0,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            path.as_ptr(),
            std::ptr::null(),
            gio::ffi::G_DBUS_SIGNAL_FLAGS_NONE,
            Some(trampoline),
            Box::into_raw(data) as glib::ffi::gpointer,
            Some(free),
        )
    };
    Subscription {
        connection: connection.clone(),
        id,
    }
}

/// GNOME Shell's `registerSessionWithGDM` and `registerDisplayWithGDM`
/// (`js/misc/loginManager.js`): tell GDM the login screen is up, so it
/// hands the display over from plymouth. Best effort, as in GNOME.
pub async fn register(system: &gio::DBusConnection) {
    for method in ["RegisterSession", "RegisterDisplay"] {
        if let Err(error) = system
            .call_future(
                Some(MANAGER_NAME),
                MANAGER_PATH,
                MANAGER_INTERFACE,
                method,
                None,
                None,
                gio::DBusCallFlags::NONE,
                -1,
            )
            .await
        {
            eprintln!("tuna-greeter: {method}: {}", error.message());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(interface: &str, member: &str, params: glib::Variant) -> Option<Signal> {
        parse_signal(interface, member, &params)
    }

    #[test]
    fn user_verifier_signals_parse_with_their_service() {
        let s = |a: &str| a.to_owned();
        assert_eq!(
            parse(
                USER_VERIFIER,
                "SecretInfoQuery",
                ("gdm-password", "Password:").to_variant()
            ),
            Some(Signal::SecretInfoQuery {
                service: s("gdm-password"),
                query: s("Password:")
            })
        );
        assert_eq!(
            parse(
                USER_VERIFIER,
                "InfoQuery",
                ("gdm-password", "Code:").to_variant()
            ),
            Some(Signal::InfoQuery {
                service: s("gdm-password"),
                query: s("Code:")
            })
        );
        assert_eq!(
            parse(
                USER_VERIFIER,
                "Problem",
                ("gdm-password", "Nope").to_variant()
            ),
            Some(Signal::Problem {
                service: s("gdm-password"),
                text: s("Nope")
            })
        );
        assert_eq!(
            parse(USER_VERIFIER, "Info", ("gdm-password", "Hi").to_variant()),
            Some(Signal::Info {
                service: s("gdm-password"),
                text: s("Hi")
            })
        );
        assert_eq!(
            parse(
                USER_VERIFIER,
                "VerificationFailed",
                ("gdm-password",).to_variant()
            ),
            Some(Signal::VerificationFailed {
                service: s("gdm-password")
            })
        );
        assert_eq!(
            parse(
                USER_VERIFIER,
                "VerificationComplete",
                ("gdm-password",).to_variant()
            ),
            Some(Signal::VerificationComplete {
                service: s("gdm-password")
            })
        );
        assert_eq!(
            parse(
                USER_VERIFIER,
                "ServiceUnavailable",
                ("gdm-smartcard", "No card").to_variant()
            ),
            Some(Signal::ServiceUnavailable {
                service: s("gdm-smartcard"),
                message: s("No card")
            })
        );
        assert_eq!(
            parse(
                USER_VERIFIER,
                "ConversationStopped",
                ("gdm-password",).to_variant()
            ),
            Some(Signal::ConversationStopped {
                service: s("gdm-password")
            })
        );
        assert_eq!(
            parse(USER_VERIFIER, "Reset", ().to_variant()),
            Some(Signal::Reset)
        );
    }

    #[test]
    fn greeter_signals_parse() {
        assert_eq!(
            parse(
                GREETER,
                "SessionOpened",
                ("gdm-password", "c2").to_variant()
            ),
            Some(Signal::SessionOpened {
                service: "gdm-password".into(),
                session_id: "c2".into()
            })
        );
        assert_eq!(
            parse(GREETER, "DefaultSessionNameChanged", ("tuna",).to_variant()),
            Some(Signal::DefaultSessionNameChanged {
                session: "tuna".into()
            })
        );
    }

    #[test]
    fn foreign_wrongly_typed_and_unused_signals_are_ignored() {
        // A signal of the right name on another interface.
        assert_eq!(
            parse("org.example.X", "Problem", ("a", "b").to_variant()),
            None
        );
        // Wrong argument types or arity.
        assert_eq!(parse(USER_VERIFIER, "Problem", ("a",).to_variant()), None);
        assert_eq!(
            parse(USER_VERIFIER, "VerificationFailed", (1u32,).to_variant()),
            None
        );
        // Signals the greeter does not act on.
        assert_eq!(
            parse(GREETER, "TimedLoginRequested", ("a", 5u32).to_variant()),
            None
        );
        assert_eq!(
            parse(
                USER_VERIFIER,
                "ReauthenticationStarted",
                (5i32,).to_variant()
            ),
            None
        );
    }

    #[test]
    fn calls_have_gdm_signatures_and_never_debug_the_answer() {
        let answer = Call::AnswerQuery {
            service: "gdm-password".into(),
            answer: "hunter2".into(),
        };
        let (interface, method, args) = answer.wire();
        assert_eq!((interface, method), (USER_VERIFIER, "AnswerQuery"));
        assert_eq!(args.type_().as_str(), "(ss)");
        assert!(!format!("{answer:?}").contains("hunter2"));
        let begin = Call::BeginVerificationForUser {
            service: "gdm-password".into(),
            user: "alice".into(),
        };
        assert_eq!(begin.wire().2.type_().as_str(), "(ss)");
        assert_eq!(Call::Cancel.wire().2.type_().as_str(), "()");
        let select = Call::SelectSession {
            session: "tuna".into(),
        };
        assert_eq!(select.wire().0, GREETER);
        assert_eq!(select.wire().2.type_().as_str(), "(s)");
        let start = Call::StartSessionWhenReady {
            service: "gdm-password".into(),
        };
        assert_eq!(start.wire().1, "StartSessionWhenReady");
        assert_eq!(start.wire().2.type_().as_str(), "(sb)");
        assert!(start.wire().2.get::<(String, bool)>().unwrap().1);
    }

    #[test]
    fn extension_and_timed_login_signals_parse() {
        assert_eq!(
            parse(GREETER, "TimedLoginRequested", ("ada", 30i32).to_variant()),
            Some(Signal::TimedLoginRequested {
                user: "ada".into(),
                delay: 30
            })
        );
        let list = glib::Variant::array_from_iter_with_type(
            glib::VariantTy::new("{ss}").unwrap(),
            [("k1", "Card one"), ("k2", "Card two")]
                .into_iter()
                .map(|(k, v)| glib::Variant::from_dict_entry(&k.to_variant(), &v.to_variant())),
        );
        let params = glib::Variant::tuple_from_iter([
            "gdm-smartcard".to_variant(),
            "Select a card".to_variant(),
            list,
        ]);
        assert_eq!(
            parse(CHOICE_LIST, "ChoiceQuery", params),
            Some(Signal::ChoiceQuery {
                service: "gdm-smartcard".into(),
                prompt: "Select a card".into(),
                choices: vec![
                    ("k1".into(), "Card one".into()),
                    ("k2".into(), "Card two".into())
                ]
            })
        );
        assert_eq!(
            parse(CHOICE_LIST, "ChoiceQuery", ("a", "b", "c").to_variant()),
            None
        );
        assert_eq!(
            parse(
                CUSTOM_JSON,
                "Request",
                ("gdm-switchable-auth", "auth-mechanisms", 1u32, "{}").to_variant()
            ),
            Some(Signal::JsonRequest {
                service: "gdm-switchable-auth".into(),
                protocol: "auth-mechanisms".into(),
                version: 1,
                json: "{}".into()
            })
        );
        // The extensions' signals on the base interface are not theirs.
        assert_eq!(
            parse(
                USER_VERIFIER,
                "Request",
                ("s", "p", 1u32, "{}").to_variant()
            ),
            None
        );
    }

    #[test]
    fn extension_calls_go_to_their_interfaces_and_redact_json() {
        let reply = Call::JsonReply {
            service: "gdm-switchable-auth".into(),
            json: r#"{"password":"hunter2"}"#.into(),
        };
        assert_eq!(reply.wire().0, CUSTOM_JSON);
        assert_eq!(reply.wire().2.type_().as_str(), "(ss)");
        assert!(!format!("{reply:?}").contains("hunter2"));
        let choice = Call::SelectChoice {
            service: "gdm-smartcard".into(),
            choice: "k1".into(),
        };
        assert_eq!(
            (choice.wire().0, choice.wire().1),
            (CHOICE_LIST, "SelectChoice")
        );
        assert_eq!(Call::EnableExtensions.wire().2.type_().as_str(), "(as)");
        assert_eq!(
            Call::EnableExtensions
                .wire()
                .2
                .get::<(Vec<String>,)>()
                .unwrap()
                .0,
            vec![CHOICE_LIST.to_owned(), CUSTOM_JSON.to_owned()]
        );
        assert!(choice.is_verifier() && reply.is_verifier() && Call::Cancel.is_verifier());
        assert!(!Call::GetTimedLoginDetails.is_verifier());
        assert_eq!(
            Call::BeginAutoLogin { user: "ada".into() }.wire(),
            (GREETER, "BeginAutoLogin", ("ada",).to_variant())
        );
        assert_eq!(Call::StopConflictingSession.wire().2.type_().as_str(), "()");
        assert_eq!(
            Call::BeginVerification {
                service: "gdm-smartcard".into()
            }
            .wire()
            .2
            .type_()
            .as_str(),
            "(s)"
        );
    }

    /// GDM's channel is peer to peer: its signals carry no sender. They
    /// reach the handler (gio-rs's own subscription aborts on them).
    #[test]
    fn peer_signals_without_a_sender_reach_the_handler() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let dir = tempfile::tempdir().unwrap();
        let context = glib::MainContext::new();
        context
            .with_thread_default(|| {
                let server = gio::DBusServer::new_sync(
                    &format!("unix:dir={}", dir.path().display()),
                    gio::DBusServerFlags::NONE,
                    &gio::dbus_generate_guid(),
                    None,
                    None::<&gio::Cancellable>,
                )
                .unwrap();
                let gdm_side: Rc<RefCell<Option<gio::DBusConnection>>> = Rc::default();
                let keep = gdm_side.clone();
                server.connect_new_connection(move |_, connection| {
                    *keep.borrow_mut() = Some(connection.clone());
                    true
                });
                server.start();
                // The server authenticates in this context: connect without
                // blocking it.
                let greeter_side = context
                    .block_on(gio::DBusConnection::for_address_future(
                        &server.client_address(),
                        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT,
                        None,
                    ))
                    .unwrap();
                let seen: Rc<RefCell<Vec<Signal>>> = Rc::default();
                let record = seen.clone();
                let subscription = subscribe(&greeter_side, move |signal| {
                    record.borrow_mut().push(signal)
                });
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                let mut sent = false;
                while seen.borrow().is_empty() {
                    assert!(std::time::Instant::now() < deadline, "no signal arrived");
                    if !sent {
                        if let Some(gdm) = gdm_side.borrow().as_ref() {
                            gdm.emit_signal(
                                None,
                                SESSION_PATH,
                                USER_VERIFIER,
                                "SecretInfoQuery",
                                Some(&("gdm-password", "Password: ").to_variant()),
                            )
                            .unwrap();
                            sent = true;
                        }
                    }
                    context.iteration(false);
                }
                assert_eq!(
                    seen.borrow()[0],
                    Signal::SecretInfoQuery {
                        service: "gdm-password".into(),
                        query: "Password: ".into()
                    }
                );
                drop(subscription);
                server.stop();
            })
            .unwrap();
    }

    #[test]
    fn access_denied_is_gdms_refusal_only() {
        assert!(access_denied(&gio::DBusError::new_for_dbus_error(
            "org.freedesktop.DBus.Error.AccessDenied",
            "Login screen only allowed to open reauthentication channels for running sessions",
        )));
        assert!(access_denied(&glib::Error::new(
            gio::DBusError::AccessDenied,
            "denied"
        )));
        assert!(!access_denied(&gio::DBusError::new_for_dbus_error(
            "org.gnome.DisplayManager.Manager.Error.NotSupported",
            "no"
        )));
        assert!(!access_denied(&gio::DBusError::new_for_dbus_error(
            "org.freedesktop.DBus.Error.ServiceUnknown",
            "GDM is not running"
        )));
        assert!(!access_denied(&glib::Error::new(
            gio::IOErrorEnum::NotConnected,
            "closed"
        )));
    }

    /// GDM's Manager on a private message bus: `OpenReauthenticationChannel`
    /// answers mary (a running session) with her channel's address and
    /// refuses ada as gdm-manager.c does.
    #[test]
    fn reauthentication_channels_are_opened_for_running_sessions_only() {
        use std::io::BufRead;
        let mut daemon = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("dbus-daemon starts");
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_owned();
        let context = glib::MainContext::new();
        let answers = context
            .with_thread_default(|| {
                context.block_on(async {
                    let flags = gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                        | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION;
                    let gdm = gio::DBusConnection::for_address_future(&address, flags, None)
                        .await
                        .unwrap();
                    let node = gio::DBusNodeInfo::for_xml(&format!(
                        r#"<node><interface name="{MANAGER_INTERFACE}">
                        <method name="OpenReauthenticationChannel">
                          <arg name="username" direction="in" type="s"/>
                          <arg name="address" direction="out" type="s"/>
                        </method></interface></node>"#
                    ))
                    .unwrap();
                    let _object = gdm
                        .register_object(
                            MANAGER_PATH,
                            &node.lookup_interface(MANAGER_INTERFACE).unwrap(),
                        )
                        .method_call(|_, _, _, _, _, params, invocation| {
                            match params.get::<(String,)>().map(|(user,)| user).as_deref() {
                                Some("mary") => invocation.return_value(Some(
                                    &("unix:path=/run/gdm/dbus/dbus-mary",).to_variant(),
                                )),
                                _ => invocation.return_dbus_error(
                                    "org.freedesktop.DBus.Error.AccessDenied",
                                    "Login screen only allowed to open reauthentication \
                                     channels for running sessions",
                                ),
                            }
                        })
                        .build()
                        .unwrap();
                    gdm.call_future(
                        Some("org.freedesktop.DBus"),
                        "/org/freedesktop/DBus",
                        "org.freedesktop.DBus",
                        "RequestName",
                        Some(&(MANAGER_NAME, 4u32).to_variant()),
                        None,
                        gio::DBusCallFlags::NONE,
                        -1,
                    )
                    .await
                    .unwrap();
                    let greeter = gio::DBusConnection::for_address_future(&address, flags, None)
                        .await
                        .unwrap();
                    (
                        open_reauthentication_channel(&greeter, "mary").await,
                        open_reauthentication_channel(&greeter, "ada").await,
                    )
                })
            })
            .unwrap();
        let _ = daemon.kill();
        let _ = daemon.wait();
        assert_eq!(answers.0.unwrap(), "unix:path=/run/gdm/dbus/dbus-mary");
        let refused = answers.1.unwrap_err();
        assert!(access_denied(&refused), "{refused:?}");
    }
}
