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
        _ => None,
    }
}

/// A call on the session object. Answers are never in `Debug` output.
#[derive(Clone, PartialEq, Eq)]
pub enum Call {
    BeginVerificationForUser { service: String, user: String },
    AnswerQuery { service: String, answer: String },
    Cancel,
    SelectSession { session: String },
    StartSessionWhenReady { service: String },
}

impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BeginVerificationForUser { service, user } => {
                write!(f, "BeginVerificationForUser({service}, {user})")
            }
            Self::AnswerQuery { service, .. } => write!(f, "AnswerQuery({service}, <redacted>)"),
            Self::Cancel => write!(f, "Cancel"),
            Self::SelectSession { session } => write!(f, "SelectSession({session})"),
            Self::StartSessionWhenReady { service } => {
                write!(f, "StartSessionWhenReady({service}, true)")
            }
        }
    }
}

impl Call {
    /// Interface, method and arguments on the wire.
    pub fn wire(&self) -> (&'static str, &'static str, glib::Variant) {
        match self {
            Self::BeginVerificationForUser { service, user } => (
                USER_VERIFIER,
                "BeginVerificationForUser",
                (service.as_str(), user.as_str()).to_variant(),
            ),
            Self::AnswerQuery { service, answer } => (
                USER_VERIFIER,
                "AnswerQuery",
                (service.as_str(), answer.as_str()).to_variant(),
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
        }
    }
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
        // Signals the greeter does not act on yet.
        assert_eq!(
            parse(GREETER, "TimedLoginRequested", ("a", 5i32).to_variant()),
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
}
