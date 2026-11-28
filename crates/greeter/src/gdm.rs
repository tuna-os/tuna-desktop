//! GDM 51's greeter channel: the shared client (`tuna-gdm`) plus the
//! greeter's own registration with GDM's manager.

pub use tuna_gdm::*;

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
