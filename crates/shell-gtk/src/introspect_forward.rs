//! `/org/gnome/Shell/Introspect` on `org.gnome.Shell` (#410).
//!
//! GNOME Shell 51 exports its Introspect object on the connection that owns
//! both `org.gnome.Shell` and `org.gnome.Shell.Introspect`
//! (`js/misc/introspect.js`). xdg-desktop-portal-gnome 51's Background
//! portal asks through `org.gnome.Shell` (`background.c`), its screencast
//! picker through `org.gnome.Shell.Introspect`. Here the compositor serves
//! the window list under the second name; this relay makes the same object
//! answer under the shell's name, as one process would in GNOME.
//!
//! The relay holds no window state and no policy. Each list call goes to
//! the compositor's `org.tuna.Introspect` with the original caller's unique
//! name, and the compositor admits that caller exactly as it admits a direct
//! one (installed portal only, nothing while locked); its replies and
//! errors come back unchanged. Properties are read through to the
//! compositor (GDBus routes `org.freedesktop.DBus.Properties` to the method
//! handler when no getter is registered), and the compositor's change
//! signals are re-emitted here, where the portal subscribes.
//!
//! Also compiled into the compositor's `introspect_shell_name` test, so the
//! real relay is exercised against the real service; it uses `gio::glib`
//! only for that reason.

use gio::glib;
use gio::prelude::*;
use tuna_shell_control::introspect::{
    FORWARDED_INTERFACE, GET_RUNNING_APPLICATIONS_FOR, GET_WINDOWS_FOR, INTERFACE, NAME, PATH,
};

const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// GNOME Shell 51's `org.gnome.Shell.Introspect.xml`.
const XML: &str = r#"<node>
  <interface name="org.gnome.Shell.Introspect">
    <method name="GetRunningApplications">
      <arg name="apps" direction="out" type="a{sa{sv}}"/>
    </method>
    <method name="GetWindows">
      <arg name="windows" direction="out" type="a{ta{sv}}"/>
    </method>
    <signal name="RunningApplicationsChanged"/>
    <signal name="WindowsChanged"/>
    <property name="AnimationsEnabled" type="b" access="read"/>
    <property name="ScreenSize" type="(ii)" access="read"/>
    <property name="version" type="u" access="read"/>
  </interface>
</node>"#;

/// The registered object and its signal relay; dropping it removes both.
pub struct Relay {
    connection: gio::DBusConnection,
    registration: Option<gio::RegistrationId>,
    _signals: gio::SignalSubscription,
}

impl Drop for Relay {
    fn drop(&mut self) {
        if let Some(id) = self.registration.take() {
            let _ = self.connection.unregister_object(id);
        }
    }
}

/// Export the relay on `connection`, the one that owns `org.gnome.Shell`.
pub fn register(connection: &gio::DBusConnection) -> Result<Relay, glib::Error> {
    let node = gio::DBusNodeInfo::for_xml(XML)?;
    let info = node
        .lookup_interface(INTERFACE)
        .expect("the interface XML names the Introspect interface");
    let registration = connection
        .register_object(PATH, &info)
        .method_call(
            |connection, sender, _, interface, method, params, invocation| {
                forward(&connection, sender, interface, method, params, invocation);
            },
        )
        .build()?;
    let signals = connection.subscribe_to_signal(
        Some(NAME),
        None,
        None,
        Some(PATH),
        None,
        gio::DBusSignalFlags::NONE,
        |signal| {
            if relayed(signal.interface_name, signal.signal_name, signal.parameters) {
                let _ = signal.connection.emit_signal(
                    None,
                    PATH,
                    signal.interface_name,
                    signal.signal_name,
                    Some(signal.parameters),
                );
            }
        },
    );
    Ok(Relay {
        connection: connection.clone(),
        registration: Some(registration),
        _signals: signals,
    })
}

/// GNOME's change signals, and property changes of its interface only.
fn relayed(interface: &str, member: &str, params: &glib::Variant) -> bool {
    match (interface, member) {
        (INTERFACE, "WindowsChanged" | "RunningApplicationsChanged") => true,
        (PROPERTIES, "PropertiesChanged") => params
            .try_child_value(0)
            .and_then(|name| name.str().map(|name| name == INTERFACE))
            .unwrap_or(false),
        _ => false,
    }
}

/// Pass one call to the compositor and its answer back to `invocation`.
fn forward(
    connection: &gio::DBusConnection,
    sender: Option<&str>,
    interface: Option<&str>,
    method: &str,
    params: glib::Variant,
    invocation: gio::DBusMethodInvocation,
) {
    let Some(sender) = sender else {
        invocation.return_dbus_error("org.freedesktop.DBus.Error.AccessDenied", "no sender");
        return;
    };
    let (interface, method, params) = match (interface, method) {
        (Some(INTERFACE), "GetRunningApplications") => (
            FORWARDED_INTERFACE,
            GET_RUNNING_APPLICATIONS_FOR,
            (sender,).to_variant(),
        ),
        (Some(INTERFACE), "GetWindows") => {
            (FORWARDED_INTERFACE, GET_WINDOWS_FOR, (sender,).to_variant())
        }
        // Get and GetAll: the compositor's properties are public.
        (Some(PROPERTIES), method) => (PROPERTIES, method, params),
        (_, method) => {
            invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method);
            return;
        }
    };
    connection.call(
        Some(NAME),
        PATH,
        interface,
        method,
        Some(&params),
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        -1,
        gio::Cancellable::NONE,
        move |reply| match reply {
            Ok(reply) => invocation.return_value(Some(&reply)),
            Err(mut error) => match gio::DBusError::remote_error(&error) {
                Some(name) => {
                    gio::DBusError::strip_remote_error(&mut error);
                    invocation.return_dbus_error(&name, error.message());
                }
                None => invocation.return_gerror(error),
            },
        },
    );
}
