//! `org.gnome.Shell.Wacom.PadOsd` on the session bus (gnome-shell 51
//! `js/ui/padOsd.js` `PadOsdService`, interface
//! `data/dbus-interfaces/org.gnome.Shell.Wacom.PadOsd.xml`): GNOME
//! Settings' "Map Buttons" calls `Show` with the pad's device node. The
//! compositor resolves the node (`meta_display_request_pad_osd`); an
//! unknown pad answers `G_IO_ERROR_CANCELLED` "Invalid params" as GNOME.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::gio;
use gtk4::glib;
use tuna_shell_control::PadOsdTarget;

pub const NAME: &str = "org.gnome.Shell.Wacom.PadOsd";
pub const PATH: &str = "/org/gnome/Shell/Wacom";
const INTERFACE: &str = "org.gnome.Shell.Wacom.PadOsd";

const XML: &str = r#"<node>
  <interface name="org.gnome.Shell.Wacom.PadOsd">
    <method name="Show">
      <arg name="device_node" direction="in" type="o"/>
      <arg name="edition_mode" direction="in" type="b"/>
    </method>
  </interface>
</node>"#;

/// Send `RequestPadOsd`: the command's request id, or `None` when there
/// is no compositor to ask.
pub type Request = Rc<dyn Fn(PadOsdTarget, bool) -> Option<u64>>;

/// `Show(o device_node, b edition_mode)` as the command it sends.
pub fn show_command(params: &glib::Variant) -> Option<(PadOsdTarget, bool)> {
    let (node, edition_mode) = params.get::<(glib::variant::ObjectPath, bool)>()?;
    Some((
        PadOsdTarget::DeviceNode(node.as_str().to_owned()),
        edition_mode,
    ))
}

/// `ShowAsync`'s answer once the compositor decided: nothing, or
/// `Gio.IOErrorEnum.CANCELLED` "Invalid params" for a pad it does not know.
pub fn show_reply(applied: bool) -> Result<Option<glib::Variant>, glib::Error> {
    if applied {
        Ok(None)
    } else {
        Err(glib::Error::new(
            gio::IOErrorEnum::Cancelled,
            "Invalid params",
        ))
    }
}

/// The exported service: `Show` calls waiting for their command result.
#[derive(Default)]
pub struct Service {
    pending: RefCell<Vec<(u64, gio::DBusMethodInvocation)>>,
}

impl Service {
    /// A `CommandResult`: answer the `Show` call that sent `id`, if any.
    pub fn command_result(&self, id: u64, applied: bool) {
        let invocation = {
            let mut pending = self.pending.borrow_mut();
            let Some(index) = pending.iter().position(|(pending, _)| *pending == id) else {
                return;
            };
            pending.swap_remove(index).1
        };
        invocation.return_result(show_reply(applied));
    }
}

/// Own `org.gnome.Shell.Wacom.PadOsd` (replacing another owner, as
/// gnome-shell does) and serve `Show` through `request`.
pub fn start(request: Request) -> Rc<Service> {
    let service = Rc::new(Service::default());
    let node = match gio::DBusNodeInfo::for_xml(XML) {
        Ok(node) => node,
        Err(e) => {
            eprintln!("tuna-shell-gtk: {INTERFACE} interface: {e}");
            return service;
        }
    };
    let Some(info) = node.lookup_interface(INTERFACE) else {
        return service;
    };
    let svc = service.clone();
    let id = gio::bus_own_name(
        gio::BusType::Session,
        NAME,
        gio::BusNameOwnerFlags::REPLACE,
        move |conn, _| {
            let (svc, request) = (svc.clone(), request.clone());
            let registered = conn
                .register_object(PATH, &info)
                .method_call(move |_, _, _, _, method, params, invocation| {
                    if method != "Show" {
                        invocation
                            .return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method);
                        return;
                    }
                    let Some((target, edition_mode)) = show_command(&params) else {
                        invocation
                            .return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", method);
                        return;
                    };
                    match request(target, edition_mode) {
                        Some(id) => svc.pending.borrow_mut().push((id, invocation)),
                        None => invocation.return_result(show_reply(false)),
                    }
                })
                .build();
            if let Err(e) = registered {
                eprintln!("tuna-shell-gtk: {INTERFACE} object: {e}");
            }
        },
        |_, _| {},
        |_, _| eprintln!("tuna-shell-gtk: {NAME} is owned elsewhere"),
    );
    // Held for the session, like org.gnome.Shell.
    let _ = id;
    service
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk4::prelude::*;

    #[test]
    fn show_arguments_become_a_device_node_request() {
        let params = (
            glib::variant::ObjectPath::try_from("/dev/input/event7".to_owned()).unwrap(),
            true,
        )
            .to_variant();
        assert_eq!(params.type_().as_str(), "(ob)");
        assert_eq!(
            show_command(&params),
            Some((PadOsdTarget::DeviceNode("/dev/input/event7".into()), true))
        );
        let normal = (
            glib::variant::ObjectPath::try_from("/dev/input/event12".to_owned()).unwrap(),
            false,
        )
            .to_variant();
        assert_eq!(
            show_command(&normal),
            Some((PadOsdTarget::DeviceNode("/dev/input/event12".into()), false))
        );
        // A string instead of an object path is not this method's call.
        assert_eq!(
            show_command(&("/dev/input/event7", true).to_variant()),
            None
        );
    }

    #[test]
    fn denied_requests_answer_invalid_params() {
        assert!(matches!(show_reply(true), Ok(None)));
        let error = show_reply(false).unwrap_err();
        assert_eq!(
            error.kind::<gio::IOErrorEnum>(),
            Some(gio::IOErrorEnum::Cancelled)
        );
        assert_eq!(error.message(), "Invalid params");
    }
}
