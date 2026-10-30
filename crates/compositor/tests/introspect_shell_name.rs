//! `/org/gnome/Shell/Introspect` under `org.gnome.Shell` (#410).
//!
//! xdg-desktop-portal-gnome 51's Background portal reads the running apps
//! through `org.gnome.Shell` and listens for RunningApplicationsChanged
//! there; GNOME Shell serves the object under both of its names. These tests
//! run the GTK shell's actual relay (compiled from its source: it needs only
//! gio) against the compositor's actual Introspect service and capture
//! `Authority` on a private dbus-daemon.
//!
//! The admitted caller is the supervised shell's own process (this test
//! process is published as the shell): the installed-portal identity needs
//! a root-owned `/usr/libexec/xdg-desktop-portal-gnome` and is qualified in
//! the VM. The impostor is a separate process that owns the portal's bus
//! name without being the installed binary.

#[path = "../../shell-gtk/src/introspect_forward.rs"]
mod introspect_forward;

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use gio::glib;
use gio::prelude::*;
use tuna_compositor::capture_security::Authority;
use tuna_compositor::introspect;
use tuna_compositor::mutter::WindowSnapshot;
use tuna_shell_control::introspect::{
    FORWARDED_INTERFACE, GET_RUNNING_APPLICATIONS_FOR, GET_WINDOWS_FOR, INTERFACE, PATH,
};
use zbus::zvariant::OwnedValue;

const SHELL: &str = "org.gnome.Shell";
const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";
const IMPOSTOR_ADDRESS: &str = "TUNA_INTROSPECT_TEST_IMPOSTOR_ADDRESS";
const IMPOSTOR_VICTIM: &str = "TUNA_INTROSPECT_TEST_IMPOSTOR_VICTIM";

type Apps = HashMap<String, HashMap<String, OwnedValue>>;
type WindowList = HashMap<u64, HashMap<String, OwnedValue>>;

fn window(id: u64, app: &str, sandboxed: Option<&str>, focused: bool) -> WindowSnapshot {
    WindowSnapshot {
        id,
        title: format!("window {id}"),
        app_id: Some(app.into()),
        application_id: None,
        sandboxed_app_id: sandboxed.map(str::to_owned),
        width: 640,
        height: 480,
        focused,
        hidden: false,
        x11: false,
        standalone: true,
        introspect_eligible: true,
    }
}

/// The shell's side: its own thread and main context, a connection that
/// owns `org.gnome.Shell` with the relay registered, as `shell_dbus.rs`
/// sets it up.
struct Shell {
    main_loop: glib::MainLoop,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Shell {
    fn start(address: &str) -> Self {
        let address = address.to_owned();
        let (ready, started) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let context = glib::MainContext::new();
            context
                .with_thread_default(|| {
                    let connection = gio::DBusConnection::for_address_sync(
                        &address,
                        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
                        None,
                        gio::Cancellable::NONE,
                    )
                    .expect("shell connects");
                    let relay = introspect_forward::register(&connection).expect("relay registers");
                    let reply = connection
                        .call_sync(
                            Some("org.freedesktop.DBus"),
                            "/org/freedesktop/DBus",
                            "org.freedesktop.DBus",
                            "RequestName",
                            Some(&(SHELL, 4u32).to_variant()),
                            Some(glib::VariantTy::new("(u)").unwrap()),
                            gio::DBusCallFlags::NONE,
                            5000,
                            gio::Cancellable::NONE,
                        )
                        .expect("shell requests its name");
                    assert_eq!(reply.get::<(u32,)>(), Some((1,)), "primary owner");
                    let main_loop = glib::MainLoop::new(Some(&context), false);
                    ready.send(main_loop.clone()).unwrap();
                    main_loop.run();
                    drop(relay);
                })
                .unwrap();
        });
        let main_loop = started
            .recv_timeout(Duration::from_secs(10))
            .expect("shell owns org.gnome.Shell");
        Self {
            main_loop,
            thread: Some(thread),
        }
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        self.main_loop.quit();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A private bus with the compositor's service and the shell's relay.
struct Session {
    daemon: Child,
    address: String,
    handle: introspect::Handle,
    authority: Authority,
    shell: Option<Shell>,
    client: zbus::blocking::Connection,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shell.take();
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn connect(address: &str) -> zbus::blocking::Connection {
    zbus::blocking::connection::Builder::address(address)
        .unwrap()
        .method_timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

fn session(windows: Vec<WindowSnapshot>) -> Session {
    let mut daemon = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("private test bus starts");
    let mut address = String::new();
    BufReader::new(daemon.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let address = address.trim().to_owned();
    let authority = Authority::default();
    authority.publish(false, Some(std::process::id()));
    let handle = introspect::Handle::default();
    handle.publish(windows);
    let builder = zbus::blocking::connection::Builder::address(address.as_str()).unwrap();
    assert!(introspect::serve(builder, &handle, authority.clone()).unwrap());
    let shell = Shell::start(&address);
    let client = connect(&address);
    Session {
        daemon,
        address,
        handle,
        authority,
        shell: Some(shell),
        client,
    }
}

fn call_shell(client: &zbus::blocking::Connection, method: &str) -> zbus::Result<zbus::Message> {
    client.call_method(Some(SHELL), PATH, Some(INTERFACE), method, &())
}

fn error_name(result: zbus::Result<zbus::Message>) -> String {
    match result {
        Ok(_) => "answered".into(),
        Err(zbus::Error::MethodError(name, _, _)) => name.to_string(),
        Err(other) => format!("{other}"),
    }
}

#[test]
fn an_admitted_caller_reads_sandboxed_apps_and_windows_through_org_gnome_shell() {
    let session = session(vec![
        window(
            1,
            "org.gnome.TextEditor",
            Some("org.gnome.TextEditor"),
            true,
        ),
        window(2, "org.example.Native", None, false),
    ]);
    let apps: Apps = call_shell(&session.client, "GetRunningApplications")
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert_eq!(apps.len(), 2, "{apps:?}");
    let editor = &apps["org.gnome.TextEditor.desktop"];
    assert_eq!(
        String::try_from(editor["sandboxed-app-id"].clone()).unwrap(),
        "org.gnome.TextEditor"
    );
    assert_eq!(
        Vec::<String>::try_from(editor["active-on-seats"].clone()).unwrap(),
        vec!["seat0"]
    );
    assert!(!apps["org.example.Native.desktop"].contains_key("sandboxed-app-id"));

    let windows: WindowList = call_shell(&session.client, "GetWindows")
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert_eq!(windows.len(), 2);
    assert_eq!(
        String::try_from(windows[&1]["sandboxed-app-id"].clone()).unwrap(),
        "org.gnome.TextEditor"
    );

    // The portal's GDBus proxy loads the properties with GetAll.
    let properties = zbus::blocking::fdo::PropertiesProxy::builder(&session.client)
        .destination(SHELL)
        .unwrap()
        .path(PATH)
        .unwrap()
        .build()
        .unwrap();
    let interface = zbus::names::InterfaceName::try_from(INTERFACE).unwrap();
    let all = properties.get_all(interface.clone()).unwrap();
    assert_eq!(u32::try_from(all["version"].clone()).unwrap(), 3);
    assert!(bool::try_from(all["AnimationsEnabled"].clone()).unwrap());
    assert!(all.contains_key("ScreenSize"));
    let version = properties.get(interface, "version").unwrap();
    assert_eq!(u32::try_from(version).unwrap(), 3);
}

#[test]
fn an_unadmitted_caller_is_denied_through_org_gnome_shell() {
    if let Ok(address) = std::env::var(IMPOSTOR_ADDRESS) {
        return impostor(&address, &std::env::var(IMPOSTOR_VICTIM).unwrap());
    }
    let session = session(vec![window(
        1,
        "org.gnome.TextEditor",
        Some("org.gnome.TextEditor"),
        true,
    )]);
    // The admitted connection's name, which the impostor tries to borrow.
    let victim = session.client.unique_name().unwrap().to_string();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "an_unadmitted_caller_is_denied_through_org_gnome_shell",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(IMPOSTOR_ADDRESS, &session.address)
        .env(IMPOSTOR_VICTIM, &victim)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    // libtest may print its own "test … " prefix on the first line.
    let answers: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.split_once("impostor: ").map(|(_, answer)| answer))
        .collect();
    assert_eq!(
        answers,
        [
            format!("GetRunningApplications {ACCESS_DENIED}"),
            format!("GetWindows {ACCESS_DENIED}"),
            format!("{GET_RUNNING_APPLICATIONS_FOR} {ACCESS_DENIED}"),
            format!("{GET_WINDOWS_FOR} {ACCESS_DENIED}"),
        ],
        "{stdout}"
    );
    // The admitted caller still reads the list afterwards.
    assert!(call_shell(&session.client, "GetRunningApplications").is_ok());
}

/// A separate process that owns the portal backend's bus name but is not
/// the installed binary: it asks through `org.gnome.Shell`, then calls the
/// forwarded interface directly naming the admitted caller.
fn impostor(address: &str, victim: &str) {
    let conn = connect(address);
    conn.request_name("org.freedesktop.impl.portal.desktop.gnome")
        .unwrap();
    for method in ["GetRunningApplications", "GetWindows"] {
        let answer = error_name(call_shell(&conn, method));
        println!("impostor: {method} {answer}");
    }
    for method in [GET_RUNNING_APPLICATIONS_FOR, GET_WINDOWS_FOR] {
        let answer = error_name(conn.call_method(
            Some(tuna_shell_control::introspect::NAME),
            PATH,
            Some(FORWARDED_INTERFACE),
            method,
            &(victim,),
        ));
        println!("impostor: {method} {answer}");
    }
}

#[test]
fn a_locked_session_denies_the_list_through_org_gnome_shell() {
    let session = session(vec![window(
        1,
        "org.gnome.TextEditor",
        Some("org.gnome.TextEditor"),
        true,
    )]);
    session.authority.publish(true, Some(std::process::id()));
    for method in ["GetRunningApplications", "GetWindows"] {
        assert_eq!(
            error_name(call_shell(&session.client, method)),
            ACCESS_DENIED,
            "{method}"
        );
    }
    session.authority.publish(false, Some(std::process::id()));
    assert!(call_shell(&session.client, "GetRunningApplications").is_ok());
}

#[test]
fn compositor_change_signals_reach_subscribers_on_org_gnome_shell() {
    let session = session(vec![window(1, "org.gnome.TextEditor", None, true)]);
    let owner = zbus::blocking::fdo::DBusProxy::new(&session.client)
        .unwrap()
        .get_name_owner(SHELL.try_into().unwrap())
        .unwrap();
    // The portal's proxy matches on the org.gnome.Shell owner.
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(owner.as_str())
        .unwrap()
        .path(PATH)
        .unwrap()
        .interface(INTERFACE)
        .unwrap()
        .build();
    let signals =
        zbus::blocking::MessageIterator::for_match_rule(rule, &session.client, Some(16)).unwrap();
    let (seen, received) = mpsc::channel();
    std::thread::spawn(move || {
        for message in signals.flatten() {
            let member = message.header().member().map(|m| m.to_string());
            let sender = message.header().sender().map(|s| s.to_string());
            if seen.send((member, sender)).is_err() {
                break;
            }
        }
    });
    session.handle.publish(vec![
        window(1, "org.gnome.TextEditor", None, true),
        window(
            2,
            "org.gnome.Calculator",
            Some("org.gnome.Calculator"),
            false,
        ),
    ]);
    let mut members = Vec::new();
    while members.len() < 2 {
        let (member, sender) = received
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("relayed signals, got {members:?}"));
        assert_eq!(sender.as_deref(), Some(owner.as_str()));
        members.extend(member);
    }
    members.sort();
    assert_eq!(members, ["RunningApplicationsChanged", "WindowsChanged"]);
}
