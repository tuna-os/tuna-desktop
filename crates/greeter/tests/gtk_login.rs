//! Actual GTK widgets and real greetd wire. Run in the dedicated Xvfb CI lane.
#![cfg(feature = "gtk-ui")]
use greetd_ipc::{codec::SyncCodec, AuthMessageType, ErrorType, Request, Response};
use gtk4::{gio, glib, prelude::*};
use std::{
    cell::Cell,
    os::unix::net::UnixListener,
    path::PathBuf,
    rc::Rc,
    thread,
    time::{Duration, Instant},
};
use tuna_greeter::{login, session::SessionEntry};

fn auth(kind: AuthMessageType, message: &str) -> Response {
    Response::AuthMessage {
        auth_message_type: kind,
        auth_message: message.into(),
    }
}
fn error() -> Response {
    Response::Error {
        error_type: ErrorType::AuthError,
        description: "private PAM detail".into(),
    }
}
fn exchange(stream: &mut std::os::unix::net::UnixStream, kind: &str, reply: Response) {
    let request = Request::read_from(stream).unwrap();
    match (kind, request) {
        ("create", Request::CreateSession { username }) => assert_eq!(username, "ada"),
        (
            "answer",
            Request::PostAuthMessageResponse {
                response: Some(answer),
            },
        ) => {
            assert_eq!(answer, "fixture-secret");
        }
        (
            "token",
            Request::PostAuthMessageResponse {
                response: Some(answer),
            },
        ) => {
            assert_eq!(answer, "fixture-token");
        }
        ("ack", Request::PostAuthMessageResponse { response: None }) => {}
        ("cancel", Request::CancelSession) => {}
        ("start", Request::StartSession { cmd, env }) => {
            assert_eq!(cmd, vec!["tuna-session", "--fixture"]);
            assert!(env.contains(&"XDG_SESSION_TYPE=wayland".into()));
            assert!(env.contains(&"XDG_CURRENT_DESKTOP=Tuna:GNOME".into()));
        }
        _ => panic!("unexpected greetd request kind (payload deliberately omitted)"),
    }
    // Keep GTK busy long enough to observe notice and responsiveness.
    thread::sleep(Duration::from_millis(180));
    reply.write_to(stream).unwrap();
}
#[test]
#[ignore = "requires dedicated Xvfb/DBus; scripts/tuna-greeter-proof runs it in CI"]
fn live_greetd_gtk_prompt_cancel_retry_and_handoff() {
    libadwaita::init().unwrap();
    let artifacts =
        PathBuf::from(std::env::var_os("TUNA_GREETER_ARTIFACTS").expect("owned artifacts"));
    std::fs::create_dir_all(&artifacts).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let socket = scratch.path().join("greetd.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // Failed password, acknowledged reset, then prompt cancellation.
        exchange(
            &mut stream,
            "create",
            auth(AuthMessageType::Secret, "Password:"),
        );
        exchange(&mut stream, "answer", error());
        exchange(&mut stream, "cancel", Response::Success);
        exchange(
            &mut stream,
            "create",
            auth(AuthMessageType::Visible, "Cancel token:"),
        );
        exchange(&mut stream, "cancel", Response::Success);
        // Cancel while CreateSession is still in flight: its reply must be
        // drained before the controller writes CancelSession.
        exchange(
            &mut stream,
            "create",
            auth(AuthMessageType::Secret, "Pending password:"),
        );
        exchange(&mut stream, "cancel", Response::Success);
        // A marked device prompt cannot receive a typed answer, including
        // programmatic activation of the real GTK callbacks.
        exchange(
            &mut stream,
            "create",
            auth(
                AuthMessageType::Secret,
                "Place your finger on the fingerprint reader",
            ),
        );
        exchange(&mut stream, "cancel", Response::Success);
        // PAM informational/error messages acknowledge without a password.
        exchange(
            &mut stream,
            "create",
            auth(AuthMessageType::Info, "Welcome notice"),
        );
        exchange(
            &mut stream,
            "ack",
            auth(AuthMessageType::Error, "Service notice"),
        );
        exchange(
            &mut stream,
            "ack",
            auth(AuthMessageType::Secret, "Password:"),
        );
        exchange(
            &mut stream,
            "answer",
            auth(AuthMessageType::Visible, "Token:"),
        );
        exchange(&mut stream, "token", Response::Success);
        exchange(&mut stream, "start", error());
        exchange(&mut stream, "cancel", Response::Success);
        // Transport failure must reconnect before another real retry.
        assert!(matches!(
            Request::read_from(&mut stream).unwrap(),
            Request::CreateSession { .. }
        ));
        drop(stream);
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // Retry after rejected start; exit only after real StartSession Success.
        exchange(
            &mut stream,
            "create",
            auth(AuthMessageType::Secret, "Password:"),
        );
        exchange(&mut stream, "answer", Response::Success);
        exchange(&mut stream, "start", Response::Success);
    });
    let app = libadwaita::Application::builder()
        .application_id("asia.reilly.tuna.greeter.walkthrough")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let timer = Rc::new(std::cell::RefCell::new(None));
    let timer_owner = timer.clone();
    let owner = Rc::new(std::cell::RefCell::new(None::<login::LoginWindow>));
    let saved = owner.clone();
    let progress = Rc::new(Cell::new(0));
    let result = progress.clone();
    app.connect_activate(move |app| {
        let _hold = app.hold();
        // Actual widget lifetime checks before the wire walkthrough. Detach
        // the close fixture from this app so its close handler cannot quit it.
        let shutdown = login::build_login(app, vec![], String::new());
        let shutdown_widgets = shutdown.widgets();
        shutdown_widgets.answer.set_text("shutdown-widget-fixture");
        shutdown.shutdown();
        assert!(shutdown_widgets.answer.text().is_empty());
        shutdown_widgets.window.destroy();
        drop(shutdown);
        let close = login::build_login(app, vec![], String::new());
        let close_widgets = close.widgets();
        close_widgets.answer.set_text("close-widget-fixture");
        close_widgets
            .window
            .set_application(None::<&gtk4::Application>);
        close_widgets.window.close();
        assert!(close_widgets.answer.text().is_empty());
        drop(close);
        let window = login::build_login(
            app,
            vec![SessionEntry {
                name: "Tuna Desktop".into(),
                command: vec!["tuna-session".into(), "--fixture".into()],
                source: PathBuf::new(),
                is_default: true,
            }],
            socket.to_string_lossy().into_owned(),
        );
        let widgets = window.widgets();
        *saved.borrow_mut() = Some(window);
        widgets.username.set_text("ada");
        let start = Instant::now();
        let ticks = Cell::new(0);
        let visual_state = std::cell::RefCell::new((String::new(), Instant::now()));
        let stage = result.clone();
        let artifacts = artifacts.clone();
        let source = glib::timeout_add_local(Duration::from_millis(10), move || {
            assert!(
                start.elapsed() < Duration::from_secs(20),
                "GTK walkthrough timed out at stage {}",
                stage.get()
            );
            ticks.set(ticks.get() + 1);
            let ready = widgets.submit.is_sensitive();
            let prompt = widgets.prompt.text();
            let notice = widgets.notice.text();
            // Require the observed widget state to survive multiple GTK frame
            // ticks before capturing or submitting the next answer.
            let key = format!("{}:{}:{}:{}", stage.get(), ready, prompt, notice);
            {
                let mut visual = visual_state.borrow_mut();
                if visual.0 != key {
                    *visual = (key, Instant::now());
                    return glib::ControlFlow::Continue;
                }
                if visual.1.elapsed() < Duration::from_millis(40) {
                    return glib::ControlFlow::Continue;
                }
            }
            let capture = |name: &str| {
                assert!(std::process::Command::new("scrot")
                    .arg(artifacts.join(format!("{name}.png")))
                    .status()
                    .unwrap()
                    .success());
            };
            match stage.get() {
                0 if ready => {
                    capture("username");
                    widgets.submit.emit_clicked();
                    assert!(!widgets.submit.is_sensitive());
                    stage.set(1);
                }
                1 if ready && prompt == "Password:" => {
                    assert!(!gtk4::prelude::EntryExt::is_visible(&widgets.answer));
                    assert!(gtk4::prelude::WidgetExt::is_visible(&widgets.answer));
                    capture("secret");
                    widgets.answer.set_text("fixture-secret");
                    stage.set(15);
                }
                15 if ready && prompt == "Password:" => {
                    assert!(!gtk4::prelude::EntryExt::is_visible(&widgets.answer));
                    capture("secret-entered");
                    widgets.submit.emit_clicked();
                    assert!(
                        widgets.answer.text().is_empty(),
                        "clear before delayed reply"
                    );
                    // Programmatic activation while busy must not erase input
                    // or send another answer. Peer still receives fixture-secret.
                    widgets.answer.set_text("busy-refusal-fixture");
                    widgets.submit.emit_clicked();
                    assert_eq!(widgets.answer.text(), "busy-refusal-fixture");
                    widgets.answer.set_text("");
                    stage.set(2);
                }
                2 if ready && notice == "Sign-in failed. Try again." => {
                    assert!(widgets.answer.text().is_empty());
                    assert!(!notice.contains("PAM"));
                    capture("failed");
                    widgets.submit.emit_clicked();
                    stage.set(3);
                }
                3 if ready && prompt == "Cancel token:" => {
                    assert!(gtk4::prelude::EntryExt::is_visible(&widgets.answer));
                    capture("visible-cancel");
                    widgets.answer.set_text("cancel-unsubmitted-fixture");
                    widgets.cancel.emit_clicked();
                    assert!(
                        widgets.answer.text().is_empty(),
                        "cancel clears before reply"
                    );
                    assert!(!widgets.cancel.is_sensitive());
                    stage.set(4);
                }
                4 if ready && prompt == "Username" && notice.is_empty() => {
                    capture("cancelled");
                    widgets.username.set_text("ada");
                    widgets.submit.emit_clicked();
                    assert!(widgets.cancel.is_sensitive());
                    widgets.cancel.emit_clicked();
                    assert!(!widgets.cancel.is_sensitive());
                    stage.set(30);
                }
                30 if ready && prompt == "Username" && notice.is_empty() => {
                    capture("pending-cancelled");
                    widgets.username.set_text("ada");
                    widgets.submit.emit_clicked();
                    stage.set(32);
                }
                32 if prompt == "Place your finger on the fingerprint reader"
                    && notice == tuna_greeter::model::FINGERPRINT_TYPED_NOTICE =>
                {
                    assert!(!ready);
                    assert!(!gtk4::prelude::WidgetExt::is_visible(&widgets.answer));
                    assert!(!widgets.answer.is_sensitive());
                    assert!(widgets.cancel.is_sensitive());
                    capture("fingerprint-typed-blocked");
                    widgets.answer.set_text("must-not-send");
                    widgets.submit.emit_clicked();
                    assert!(widgets.answer.text().is_empty());
                    widgets.answer.set_text("must-not-send");
                    widgets.answer.emit_activate();
                    assert!(widgets.answer.text().is_empty());
                    assert!(!widgets.submit.is_sensitive());
                    widgets.cancel.emit_clicked();
                    stage.set(33);
                }
                33 if ready && prompt == "Username" && notice.is_empty() => {
                    widgets.username.set_text("ada");
                    widgets.submit.emit_clicked();
                    stage.set(5);
                }
                5 if notice == "Welcome notice" => {
                    assert!(!ready);
                    capture("info");
                    stage.set(6);
                }
                6 if notice == "Service notice" => {
                    assert!(!ready);
                    capture("error-notice");
                    stage.set(7);
                }
                7 if ready && prompt == "Password:" => {
                    assert!(!gtk4::prelude::EntryExt::is_visible(&widgets.answer));
                    widgets.answer.set_text("fixture-secret");
                    widgets.submit.emit_clicked();
                    stage.set(8);
                }
                8 if ready && prompt == "Token:" => {
                    assert!(gtk4::prelude::EntryExt::is_visible(&widgets.answer));
                    assert!(widgets.answer.text().is_empty());
                    capture("multi-visible");
                    widgets.answer.set_text("fixture-token");
                    widgets.submit.emit_clicked();
                    stage.set(9);
                }
                9 if !ready && prompt == "Username" && !widgets.cancel.is_sensitive() => {
                    capture("launch-wait");
                    stage.set(10);
                }
                10 if ready && notice == "Sign-in failed. Try again." => {
                    capture("start-rejected");
                    widgets.username.set_text("ada");
                    widgets.submit.emit_clicked();
                    stage.set(40);
                }
                40 if ready && notice == "Sign-in failed. Try again." && prompt == "Username" => {
                    capture("transport-failure");
                    widgets.username.set_text("ada");
                    widgets.submit.emit_clicked();
                    stage.set(11);
                }
                11 if ready && prompt == "Password:" => {
                    widgets.answer.set_text("fixture-secret");
                    widgets.submit.emit_clicked();
                    stage.set(12);
                }
                12 if !ready && prompt == "Username" => {
                    capture("handoff-wait");
                    assert!(
                        ticks.get() > 40,
                        "GTK must keep dispatching during blocking peer waits"
                    );
                    stage.set(13);
                }
                _ => {}
            }
            glib::ControlFlow::Continue
        });
        *timer_owner.borrow_mut() = Some(source);
    });
    let cleanup = owner.clone();
    app.connect_shutdown(move |_| {
        cleanup.borrow_mut().take();
    });
    assert_eq!(app.run_with_args::<&str>(&[]), glib::ExitCode::SUCCESS);
    if let Some(source) = timer.borrow_mut().take() {
        source.remove();
    }
    assert_eq!(
        progress.get(),
        13,
        "quit requires successful actual StartSession reply"
    );
    peer.join().unwrap();
    std::fs::write(PathBuf::from(std::env::var_os("TUNA_GREETER_ARTIFACTS").unwrap()).join("assertions.txt"),"G-GREETER-GTK-PROMPTS PASS\nG-GREETER-CANCEL-RETRY PASS\nG-GREETER-START-REPLY PASS\nG-GREETER-WORKER-RESPONSIVE PASS\nG-GREETER-SECRET-WIDGET-CLEAR PASS\nG-GREETER-FPRINT-TYPED-DENIAL PASS\n").unwrap();
}
