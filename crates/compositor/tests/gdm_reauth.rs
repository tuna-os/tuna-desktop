//! Lock-screen verification through GDM's reauthentication channel
//! (`tuna_compositor::gdm_reauth`) against a GDM 51 fixture: a manager
//! serving `OpenReauthenticationChannel` and a session server carrying
//! `UserVerifier`, both peer to peer as GDM's are. The fixture runs the
//! PAM decision itself, as GDM's session worker does.
//!
//! `gdm_reauthentication_applies_pam_faillock` runs a real PAM stack with
//! `pam_faillock` through pam_wrapper (libpam-wrapper) and re-runs itself
//! with LD_PRELOAD set; without pam_wrapper it is skipped with a note
//! unless TUNA_REQUIRE_PAM_WRAPPER is set (CI's check job sets it).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use gio::glib;
use gio::prelude::*;
use tuna_compositor::gdm_reauth::{Reauth, Source};
use tuna_compositor::pam_backend::{Backend, ConversationTicket, Initial, Verification};
use tuna_compositor::pam_cancel::Cancellation;
use tuna_compositor::pam_conversation::{Binding, Challenge, Origin, Policy};
use tuna_compositor::pam_helper::{Answer, AnswerText, Refusal, Style};
use tuna_shell_control::Secret;

const WRAPPER: &str = "/usr/lib/x86_64-linux-gnu/libpam_wrapper.so";
const MATRIX: &str = "/usr/lib/x86_64-linux-gnu/pam_wrapper/pam_matrix.so";
const FAILLOCK: &str = "/usr/lib/x86_64-linux-gnu/security/pam_faillock.so";
const CHILD: &str = "TUNA_GDM_REAUTH_TEST_CHILD";
/// GDM 51's generic `gdm-password` failure (`get_generic_error_message`).
const PASSWORD_FAILED: &str = "Sorry, password authentication didn’t work. Please try again.";

const XML: &str = r#"<node>
  <interface name="org.gnome.DisplayManager.Manager">
    <method name="OpenReauthenticationChannel">
      <arg name="username" direction="in" type="s"/>
      <arg name="address" direction="out" type="s"/>
    </method>
  </interface>
  <interface name="org.gnome.DisplayManager.UserVerifier">
    <method name="BeginVerificationForUser">
      <arg name="service_name" direction="in" type="s"/>
      <arg name="username" direction="in" type="s"/>
    </method>
    <method name="AnswerQuery">
      <arg name="service_name" direction="in" type="s"/>
      <arg name="answer" direction="in" type="s"/>
    </method>
    <method name="Cancel"/>
  </interface>
</node>"#;

/// Decides an answer for (service, user, answer), as GDM's worker runs PAM.
type Check = Arc<dyn Fn(&str, &str, &str) -> bool + Send + Sync>;

/// A GDM 51 fixture on its own GLib thread. Every call it receives is
/// logged (answers never are).
struct FakeGdm {
    manager: String,
    log: Arc<Mutex<Vec<String>>>,
    channels: Arc<AtomicUsize>,
    _dir: tempfile::TempDir,
}

impl FakeGdm {
    fn start(refuse_channel: bool, check: Check) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_owned();
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let channels = Arc::new(AtomicUsize::new(0));
        let (ready, address) = mpsc::channel();
        let (thread_log, thread_channels) = (log.clone(), channels.clone());
        std::thread::spawn(move || {
            let context = glib::MainContext::new();
            context
                .with_thread_default(|| {
                    let node = gio::DBusNodeInfo::for_xml(XML).unwrap();
                    let verifier = node
                        .lookup_interface("org.gnome.DisplayManager.UserVerifier")
                        .unwrap();
                    let manager = node
                        .lookup_interface("org.gnome.DisplayManager.Manager")
                        .unwrap();
                    let server = |name: &str| {
                        let dir = path.join(name);
                        std::fs::create_dir(&dir).unwrap();
                        gio::DBusServer::new_sync(
                            &format!("unix:dir={}", dir.display()),
                            gio::DBusServerFlags::NONE,
                            &gio::dbus_generate_guid(),
                            None,
                            None::<&gio::Cancellable>,
                        )
                        .unwrap()
                    };
                    let session = server("session");
                    let peers: Rc<RefCell<Vec<gio::DBusConnection>>> = Rc::default();
                    let (session_log, session_peers) = (thread_log.clone(), peers.clone());
                    session.connect_new_connection(move |_, connection| {
                        session_peers.borrow_mut().push(connection.clone());
                        let (log, check) = (session_log.clone(), check.clone());
                        connection
                            .register_object(tuna_gdm::SESSION_PATH, &verifier)
                            .method_call(move |connection, _, _, _, method, params, invocation| {
                                verify(&connection, &log, &check, method, &params);
                                invocation.return_value(None);
                            })
                            .build()
                            .unwrap();
                        true
                    });
                    session.start();
                    let session_address = session.client_address().to_string();
                    let managers = server("manager");
                    let (manager_log, manager_peers) = (thread_log.clone(), peers.clone());
                    managers.connect_new_connection(move |_, connection| {
                        // An accepted peer is kept, as GDM keeps its clients.
                        manager_peers.borrow_mut().push(connection.clone());
                        let (log, address, channels) = (
                            manager_log.clone(),
                            session_address.clone(),
                            thread_channels.clone(),
                        );
                        connection
                            .register_object(tuna_gdm::MANAGER_PATH, &manager)
                            .method_call(move |_, _, _, _, method, params, invocation| {
                                let (user,) = params.get::<(String,)>().unwrap();
                                log.lock().unwrap().push(format!("{method}({user})"));
                                if refuse_channel {
                                    invocation.return_dbus_error(
                                        "org.gnome.DisplayManager.Manager.Error",
                                        "No session available",
                                    );
                                } else {
                                    channels.fetch_add(1, Ordering::SeqCst);
                                    invocation
                                        .return_value(Some(&(address.as_str(),).to_variant()));
                                }
                            })
                            .build()
                            .unwrap();
                        true
                    });
                    managers.start();
                    ready.send(managers.client_address().to_string()).unwrap();
                    let _keep = (session, managers, peers);
                    loop {
                        context.iteration(true);
                    }
                })
                .unwrap();
        });
        Self {
            manager: address.recv_timeout(Duration::from_secs(10)).unwrap(),
            log,
            channels,
            _dir: dir,
        }
    }

    fn logged(&self, entry: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.log.lock().unwrap().iter().any(|seen| seen == entry) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

/// GDM's `UserVerifier` for one call: password asks for a secret,
/// fingerprint only informs; an answer is decided by `check`.
fn verify(
    connection: &gio::DBusConnection,
    log: &Mutex<Vec<String>>,
    check: &Check,
    method: &str,
    params: &glib::Variant,
) {
    let emit = |member: &str, args: glib::Variant| {
        connection
            .emit_signal(
                None,
                tuna_gdm::SESSION_PATH,
                tuna_gdm::USER_VERIFIER,
                member,
                Some(&args),
            )
            .unwrap();
    };
    match method {
        "BeginVerificationForUser" => {
            let (service, user) = params.get::<(String, String)>().unwrap();
            log.lock()
                .unwrap()
                .push(format!("{method}({service}, {user})"));
            *USER.lock().unwrap() = user;
            if service == "gdm-fingerprint" {
                emit(
                    "Info",
                    (
                        service.as_str(),
                        "Place your finger on the fingerprint reader",
                    )
                        .to_variant(),
                );
            } else {
                emit("Info", (service.as_str(), "Welcome").to_variant());
                emit(
                    "SecretInfoQuery",
                    (service.as_str(), "Password: ").to_variant(),
                );
            }
        }
        "AnswerQuery" => {
            let (service, answer) = params.get::<(String, String)>().unwrap();
            log.lock().unwrap().push(format!("{method}({service})"));
            let user = USER.lock().unwrap().clone();
            if check(&service, &user, &answer) {
                emit("VerificationComplete", (service.as_str(),).to_variant());
            } else {
                emit("Problem", (service.as_str(), PASSWORD_FAILED).to_variant());
                emit("VerificationFailed", (service.as_str(),).to_variant());
                emit("ConversationStopped", (service.as_str(),).to_variant());
            }
        }
        "Cancel" => {
            log.lock().unwrap().push(method.to_owned());
            emit("Reset", ().to_variant());
        }
        _ => {}
    }
}

static USER: Mutex<String> = Mutex::new(String::new());

fn user() -> String {
    tuna_compositor::unlock::session_user().expect("kernel session account")
}

/// A backend whose private helper must not run: its factory counts calls.
fn backend(reauth: Reauth, helpers: &Arc<AtomicUsize>) -> Backend {
    let helpers = helpers.clone();
    Backend::with_trusted_factory_and_reauth(
        move |_, _, _, _, _| {
            helpers.fetch_add(1, Ordering::SeqCst);
            Err(std::io::Error::other("no private helper in this fixture"))
        },
        reauth,
    )
    .unwrap()
}

fn binding(request: u64) -> Binding {
    Binding {
        origin: Origin {
            connection: 1,
            request,
        },
        attempt: request,
        epoch: 1,
    }
}

const POLICY: Policy = Policy {
    backend_phase: Duration::from_secs(30),
    total: Duration::from_secs(900),
};

fn conversation(backend: &Backend, request: u64) -> ConversationTicket {
    backend
        .begin_conversation(
            "tuna-lock".into(),
            user(),
            binding(request),
            POLICY,
            Cancellation::default(),
        )
        .unwrap()
}

fn challenge(ticket: &ConversationTicket) -> Challenge {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match ticket.try_challenge() {
            Ok(challenge) => return challenge,
            Err(mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(error) => panic!("no challenge: {error:?}"),
        }
    }
}

fn outcome(ticket: &mut ConversationTicket) -> Verification {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(outcome) = ticket.try_outcome() {
            return outcome;
        }
        assert!(Instant::now() < deadline, "no outcome");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Answer the welcome message, then the password question with `answer`.
fn answer_password(ticket: &ConversationTicket, request: u64, answer: &str) {
    let info = challenge(ticket);
    assert_eq!(info.messages.len(), 1);
    assert_eq!(info.messages[0].style, Style::Info);
    assert_eq!(info.messages[0].text, "Welcome");
    let sequence = info.sequence;
    info.response
        .submit(
            binding(request),
            sequence,
            vec![Answer {
                index: 0,
                text: None,
            }],
        )
        .unwrap();
    let query = challenge(ticket);
    assert_eq!(query.messages[0].style, Style::Secret);
    assert_eq!(query.messages[0].text, "Password: ");
    let sequence = query.sequence;
    query
        .response
        .submit(
            binding(request),
            sequence,
            vec![Answer {
                index: 0,
                text: Some(AnswerText::new(answer.into())),
            }],
        )
        .unwrap();
}

fn right_password() -> Check {
    Arc::new(|service, _, answer| service == "gdm-password" && answer == "right")
}

#[test]
fn gdm_reauthentication_relays_the_conversation_and_unlocks_only_on_completion() {
    let gdm = FakeGdm::start(false, right_password());
    let helpers = Arc::new(AtomicUsize::new(0));
    let source = Source::with_manager(&gdm.manager, true);
    let backend = backend(Reauth::password(source), &helpers);

    let mut wrong = conversation(&backend, 1);
    answer_password(&wrong, 1, "wrong");
    assert_eq!(
        outcome(&mut wrong),
        Verification::Retired {
            authenticated: false
        }
    );
    // GDM's failure keeps its class; its generic text is the runtime's to show.
    assert_eq!(wrong.refusal(), Some(Refusal::Failed));
    drop(wrong);

    let mut right = conversation(&backend, 2);
    answer_password(&right, 2, "right");
    assert_eq!(
        outcome(&mut right),
        Verification::Retired {
            authenticated: true
        }
    );
    drop(right);

    // The single-password path answers GDM's one question itself.
    for (password, authenticated) in [("right", true), ("wrong", false)] {
        let mut ticket = backend
            .begin(
                "tuna-lock".into(),
                user(),
                Secret(password.into()),
                Cancellation::default(),
            )
            .unwrap();
        match ticket.initial() {
            Initial::Finished(result) => {
                assert_eq!(
                    result,
                    Verification::Retired { authenticated },
                    "{password}"
                )
            }
            Initial::Pending => panic!("{password}: no result in time"),
        }
    }
    assert!(gdm.logged(&format!("OpenReauthenticationChannel({})", user())));
    assert!(gdm.logged(&format!(
        "BeginVerificationForUser(gdm-password, {})",
        user()
    )));
    assert!(gdm.logged("AnswerQuery(gdm-password)"));
    assert_eq!(helpers.load(Ordering::SeqCst), 0, "the private helper ran");
    backend.shutdown();
}

#[test]
fn without_gdm_the_private_helper_runs_and_a_gdm_session_never_does() {
    let missing = tempfile::tempdir().unwrap();
    let nowhere = format!("unix:path={}", missing.path().join("none").display());
    let refusing = FakeGdm::start(true, right_password());
    let single = |source: Arc<Source>, helpers: &Arc<AtomicUsize>| {
        let backend = backend(Reauth::password(source), helpers);
        let mut ticket = backend
            .begin(
                "tuna-lock".into(),
                user(),
                Secret("right".into()),
                Cancellation::default(),
            )
            .unwrap();
        let result = match ticket.initial() {
            Initial::Finished(result) => result,
            Initial::Pending => panic!("no result in time"),
        };
        backend.shutdown();
        result
    };
    // No GDM at all, or a GDM that did not start this session: the
    // private helper runs (this fixture's factory fails it closed).
    for source in [
        Source::with_manager(&nowhere, false),
        Source::with_manager(&refusing.manager, false),
    ] {
        let helpers = Arc::new(AtomicUsize::new(0));
        assert_eq!(single(source, &helpers), Verification::Unretired);
        assert_eq!(helpers.load(Ordering::SeqCst), 1);
    }
    assert!(refusing.logged(&format!("OpenReauthenticationChannel({})", user())));
    // A session GDM started is refused when its channel does not open.
    for source in [
        Source::with_manager(&nowhere, true),
        Source::with_manager(&refusing.manager, true),
    ] {
        let helpers = Arc::new(AtomicUsize::new(0));
        assert_eq!(
            single(source, &helpers),
            Verification::Retired {
                authenticated: false
            }
        );
        assert_eq!(helpers.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn dismissing_the_lock_dialog_cancels_gdm_verification() {
    let gdm = FakeGdm::start(false, right_password());
    let helpers = Arc::new(AtomicUsize::new(0));
    let backend = backend(
        Reauth::password(Source::with_manager(&gdm.manager, true)),
        &helpers,
    );
    let mut ticket = conversation(&backend, 1);
    let info = challenge(&ticket);
    let sequence = info.sequence;
    info.response
        .submit(
            binding(1),
            sequence,
            vec![Answer {
                index: 0,
                text: None,
            }],
        )
        .unwrap();
    let query = challenge(&ticket);
    assert_eq!(query.messages[0].style, Style::Secret);
    // The shell's CancelVerification: the runtime cancels the ticket and
    // drops the unanswered question.
    ticket.cancel();
    drop(query);
    assert_eq!(
        outcome(&mut ticket),
        Verification::Retired {
            authenticated: false
        }
    );
    assert!(gdm.logged("Cancel"), "GDM's verification was not cancelled");
    assert!(!gdm
        .log
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.starts_with("AnswerQuery")));
    // The admission is free again: a new conversation begins.
    let mut again = conversation(&backend, 2);
    answer_password(&again, 2, "right");
    assert_eq!(
        outcome(&mut again),
        Verification::Retired {
            authenticated: true
        }
    );
    backend.shutdown();
}

#[test]
fn password_and_fingerprint_share_one_reauthentication_channel() {
    let gdm = FakeGdm::start(false, right_password());
    let helpers = Arc::new(AtomicUsize::new(0));
    let source = Source::with_manager(&gdm.manager, true);
    let password = backend(Reauth::password(source.clone()), &helpers);
    let fingerprint = backend(Reauth::fingerprint(source), &helpers);
    let mut scan = conversation(&fingerprint, 7);
    let cue = challenge(&scan);
    assert_eq!(cue.messages[0].style, Style::Info);
    assert_eq!(
        cue.messages[0].text,
        "Place your finger on the fingerprint reader"
    );
    let sequence = cue.sequence;
    cue.response
        .submit(
            binding(7),
            sequence,
            vec![Answer {
                index: 0,
                text: None,
            }],
        )
        .unwrap();
    let mut typed = conversation(&password, 8);
    answer_password(&typed, 8, "right");
    assert_eq!(
        outcome(&mut typed),
        Verification::Retired {
            authenticated: true
        }
    );
    // One channel carried both services; the fingerprint one never saw
    // the password conversation's completion as its own.
    assert_eq!(gdm.channels.load(Ordering::SeqCst), 1);
    assert!(gdm.logged(&format!(
        "BeginVerificationForUser(gdm-fingerprint, {})",
        user()
    )));
    assert!(scan.try_outcome().is_none());
    scan.cancel();
    assert_eq!(
        outcome(&mut scan),
        Verification::Retired {
            authenticated: false
        }
    );
    assert_eq!(helpers.load(Ordering::SeqCst), 0);
    password.shutdown();
    fingerprint.shutdown();
}

/// Entries `faillock` lists for `user` in `dir`.
fn tally(dir: &Path, user: &str) -> usize {
    let output = Command::new("faillock")
        .args(["--dir", &dir.display().to_string(), "--user", user])
        .output()
        .expect("faillock (libpam-modules-bin)");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with(|c: char| c.is_ascii_digit()))
        .count()
}

#[test]
fn gdm_reauthentication_applies_pam_faillock() {
    if std::env::var_os(CHILD).is_some() {
        return faillock_child();
    }
    if ![WRAPPER, MATRIX, FAILLOCK]
        .iter()
        .all(|path| Path::new(path).exists())
    {
        assert!(
            std::env::var_os("TUNA_REQUIRE_PAM_WRAPPER").is_none(),
            "pam_wrapper and pam_faillock are required here but missing"
        );
        eprintln!("gdm_reauth: pam_wrapper or pam_faillock not installed, skipped");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let passdb = dir.path().join("passdb");
    std::fs::write(&passdb, format!("{}:right:gdm-password\n", user())).unwrap();
    let tally_dir = dir.path().join("faillock");
    std::fs::create_dir(&tally_dir).unwrap();
    let services = dir.path().join("services");
    std::fs::create_dir(&services).unwrap();
    // The stack GDM's worker runs for gdm-password with faillock enabled
    // (authselect with-faillock, Arch's system-auth): deny=3. The tally is
    // writable, as GDM's root worker finds /var/run/faillock.
    let faillock = |phase: &str| {
        format!(
            "{FAILLOCK} {phase} dir={} deny=3 unlock_time=600",
            tally_dir.display()
        )
    };
    std::fs::write(
        services.join("gdm-password"),
        format!(
            "auth required {}\n\
             auth [success=1 default=bad] {MATRIX} passdb={p}\n\
             auth [default=die] {}\n\
             auth sufficient {}\n\
             account required {FAILLOCK} dir={t}\n\
             account required {MATRIX} passdb={p}\n",
            faillock("preauth"),
            faillock("authfail"),
            faillock("authsucc"),
            p = passdb.display(),
            t = tally_dir.display(),
        ),
    )
    .unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "gdm_reauthentication_applies_pam_faillock",
            "--nocapture",
        ])
        .env(CHILD, &tally_dir)
        .env("LD_PRELOAD", WRAPPER)
        .env("PAM_WRAPPER", "1")
        .env("PAM_WRAPPER_SERVICE_DIR", &services)
        .status()
        .unwrap();
    assert!(status.success(), "child faillock checks failed");
}

fn faillock_child() {
    let tally_dir = PathBuf::from(std::env::var_os(CHILD).unwrap());
    // The fixture's worker runs the real stack, as GDM's root worker does.
    let gdm = FakeGdm::start(false, Arc::new(tuna_compositor::pam::authenticate));
    let helpers = Arc::new(AtomicUsize::new(0));
    let backend = backend(
        Reauth::password(Source::with_manager(&gdm.manager, true)),
        &helpers,
    );
    let attempt = |request: u64, password: &str| {
        let mut ticket = conversation(&backend, request);
        answer_password(&ticket, request, password);
        outcome(&mut ticket).authenticated()
    };
    for request in 1..=3 {
        assert!(!attempt(request, "wrong"));
    }
    // deny=3: the right password no longer unlocks, and the three
    // failures are on record where faillock reads them.
    assert!(
        !attempt(4, "right"),
        "the right password unlocked after deny=3"
    );
    assert_eq!(tally(&tally_dir, &user()), 3);
    assert_eq!(helpers.load(Ordering::SeqCst), 0);
    // An administrator's reset lets the right password unlock again.
    let reset = Command::new("faillock")
        .args([
            "--dir",
            &tally_dir.display().to_string(),
            "--user",
            &user(),
            "--reset",
        ])
        .status()
        .unwrap();
    assert!(reset.success());
    assert!(attempt(5, "right"));
    backend.shutdown();
}
