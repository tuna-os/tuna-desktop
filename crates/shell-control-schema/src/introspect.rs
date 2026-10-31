//! `org.gnome.Shell.Introspect` names shared by the compositor, which serves
//! the window list, and the GTK shell, which owns `org.gnome.Shell`.
//!
//! GNOME Shell 51 exports one `/org/gnome/Shell/Introspect` object on the
//! connection that owns both `org.gnome.Shell` and
//! `org.gnome.Shell.Introspect` (`js/misc/introspect.js`), and
//! xdg-desktop-portal-gnome 51 reaches it through either name: its
//! screencast picker uses `org.gnome.Shell.Introspect`, its Background
//! portal `org.gnome.Shell`. In Tuna the two names belong to two processes,
//! so the shell relays the object it receives on `org.gnome.Shell` to the
//! compositor through [`FORWARDED_INTERFACE`], naming the original caller;
//! the compositor admits that caller exactly as it admits a direct one.

/// The compositor's bus name for the window list.
pub const NAME: &str = "org.gnome.Shell.Introspect";
/// The object path, the same under both names.
pub const PATH: &str = "/org/gnome/Shell/Introspect";
/// GNOME's interface.
pub const INTERFACE: &str = "org.gnome.Shell.Introspect";
/// Tuna's addition on the compositor's object: calls the shell received on
/// `org.gnome.Shell`, each taking the original caller's unique bus name.
/// Only the supervised shell may make them.
pub const FORWARDED_INTERFACE: &str = "org.tuna.Introspect";
/// `(caller) -> a{sa{sv}}`: GetRunningApplications for `caller`.
pub const GET_RUNNING_APPLICATIONS_FOR: &str = "GetRunningApplicationsFor";
/// `(caller) -> a{ta{sv}}`: GetWindows for `caller`.
pub const GET_WINDOWS_FOR: &str = "GetWindowsFor";
