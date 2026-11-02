//! Owner-only state-file evidence for actual server keyboard emissions.
//!
//! This records neither keys nor application consumption. The root fixture must
//! independently join socket credentials to process start/ELF and app content.

use std::os::unix::fs::MetadataExt;

use smithay::reexports::wayland_server::Resource;

use crate::{windows::WindowManager, State};

pub(super) fn enable(state: &State) {
    let private_path = std::env::var_os("TUNA_COMPOSITOR_STATE")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .and_then(|path| path.parent().and_then(|parent| parent.metadata().ok()))
        .is_some_and(|metadata| {
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0
        });
    if private_path {
        if let Some(keyboard) = state.seat.get_keyboard() {
            keyboard.enable_delivery_observer();
        }
    }
}

// Called only from the already opt-in publish_state path, outside input dispatch
// and all keyboard locks. No backend borrow or callback is involved.
pub(super) fn publish(manager: &WindowManager, state: &State, doc: &mut serde_json::Value) {
    let keyboard = state.seat.get_keyboard();
    if keyboard
        .as_ref()
        .is_none_or(|keyboard| keyboard.delivery_observer_status() == Err("observer-disabled"))
    {
        return;
    }
    let status = keyboard
        .as_ref()
        .ok_or("seat-keyboard-unavailable")
        .and_then(|keyboard| keyboard.delivery_observer_status());
    doc["keyboard_delivery_observer"] = serde_json::json!({
        "enabled": keyboard.as_ref().is_some_and(|keyboard|
            keyboard.delivery_observer_status() != Err("observer-disabled")),
        "evaluable": status.is_ok(),
        "error": status.err(),
    });
    let focus = keyboard
        .as_ref()
        .and_then(|keyboard| keyboard.current_focus());
    doc["native_keyboard_focus"] = match focus.as_ref() {
        Some(surface) => {
            let client = surface.client();
            let credentials =
                client
                    .as_ref()
                    .ok_or("focus-client-unavailable")
                    .and_then(|client| {
                        client
                            .get_credentials(&state.dh)
                            .map_err(|_| "focus-credentials-unavailable")
                    });
            serde_json::json!({
                "surface_object_id": surface.id().protocol_id(),
                "client_pid": credentials.as_ref().ok().map(|credentials| credentials.pid),
                "client_uid": credentials.as_ref().ok().map(|credentials| credentials.uid),
                "evaluable": credentials.is_ok(),
                "error": credentials.err(),
            })
        }
        None => serde_json::json!({"evaluable": false, "error": "no-keyboard-focus"}),
    };
    let mut windows = Vec::new();
    for window in manager.model().windows() {
        let Some(surface) = manager.surface_of(window.id) else {
            continue;
        };
        // Xwayland may also have a wl_surface, but it is not an original native
        // application's connection and cannot qualify this observer's proof.
        if !state
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .any(|toplevel| toplevel.wl_surface() == &surface)
        {
            continue;
        }
        let client = surface.client();
        let credentials = client
            .as_ref()
            .ok_or("window-client-unavailable")
            .and_then(|client| {
                client
                    .get_credentials(&state.dh)
                    .map_err(|_| "window-credentials-unavailable")
            });
        let delivery = client
            .as_ref()
            .ok_or("window-client-unavailable")
            .and_then(|client| {
                keyboard
                    .as_ref()
                    .ok_or("seat-keyboard-unavailable")
                    .and_then(|keyboard| keyboard.delivery_snapshot(client))
            });
        let error = credentials
            .as_ref()
            .err()
            .copied()
            .or_else(|| delivery.as_ref().err().copied());
        let resources =
            delivery.as_ref().ok().map(|delivery| {
                delivery.resources.iter()
            .map(|resource| serde_json::json!({
                "binding_generation": resource.binding_generation,
                "object_id": resource.resource_id.protocol_id(),
                "resource_id": format!("observer-binding-{}", resource.binding_generation),
                "emitted_key_events": resource.emitted_key_events,
            })).collect::<Vec<_>>()
            });
        windows.push(serde_json::json!({
            "window_id": window.id,
            "surface_object_id": surface.id().protocol_id(),
            "client_pid": credentials.as_ref().ok().map(|credentials| credentials.pid),
            "client_uid": credentials.as_ref().ok().map(|credentials| credentials.uid),
            "connection_id": delivery.as_ref().ok().map(|delivery|
                format!("observer-connection-{}", delivery.connection_generation)),
            "connection_generation": delivery.as_ref().ok().map(|delivery| delivery.connection_generation),
            "resources": resources,
            "actual_keyboard_focus": focus.as_ref() == Some(&surface),
            "evaluable": error.is_none(),
            "error": error,
        }));
    }
    windows.sort_unstable_by_key(|window| window["window_id"].as_u64());
    doc["native_keyboard_windows"] = windows.into();
}

#[cfg(test)]
#[path = "native_keyboard_observer_tests.rs"]
mod tests;
