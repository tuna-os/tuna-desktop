//! GNOME Shell 51's audio device selection (`audioDeviceSelection.js`):
//! `org.gnome.Shell.AudioDeviceSelection` at
//! `/org/gnome/Shell/AudioDeviceSelection`. gnome-settings-daemon's
//! media-keys plugin opens it when a combined jack reports a plug it
//! cannot classify, with the candidate devices (`headphones`, `headset`,
//! `microphone`); the choice goes back to that caller as `DeviceSelected`
//! and gsd switches the jack's port. Like GNOME, a request naming fewer
//! than two known devices shows nothing, a second `Open` while one is
//! up is ignored, Cancel and Escape close, and Sound Settings opens the
//! Sound panel. Choosing a device also leaves the overview.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

pub const NAME: &str = "org.gnome.Shell.AudioDeviceSelection";
pub const PATH: &str = "/org/gnome/Shell/AudioDeviceSelection";
const XML: &str = r#"<node>
  <interface name="org.gnome.Shell.AudioDeviceSelection">
    <method name="Open">
      <arg name="devices" direction="in" type="as"/>
    </method>
    <method name="Close">
    </method>
    <signal name="DeviceSelected">
      <arg name="device" type="s"/>
    </signal>
  </interface>
</node>"#;

/// One selectable device, in GNOME's order (`AudioDevice`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Headphones,
    Headset,
    Microphone,
}

impl Device {
    const ALL: [Device; 3] = [Device::Headphones, Device::Headset, Device::Microphone];

    /// The name gsd sends and receives back.
    pub fn name(self) -> &'static str {
        match self {
            Device::Headphones => "headphones",
            Device::Headset => "headset",
            Device::Microphone => "microphone",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Device::Headphones => "Headphones",
            Device::Headset => "Headset",
            Device::Microphone => "Microphone",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Device::Headphones => "audio-headphones-symbolic",
            Device::Headset => "audio-headset-symbolic",
            Device::Microphone => "audio-input-microphone-symbolic",
        }
    }
}

/// The devices a request offers, deduplicated in GNOME's order; `None`
/// when fewer than two are known ("Too few devices for a selection").
/// Names match case-insensitively, as `AudioDevice[n.toUpperCase()]`.
pub fn choices(names: &[String]) -> Option<Vec<Device>> {
    let chosen: Vec<Device> = Device::ALL
        .into_iter()
        .filter(|device| names.iter().any(|n| n.eq_ignore_ascii_case(device.name())))
        .collect();
    (chosen.len() >= 2).then_some(chosen)
}

struct Dialog {
    window: gtk::ApplicationWindow,
    fade: crate::transient::ModalFade,
    devices: gtk::Box,
    /// The `Open` caller, which alone receives `DeviceSelected`.
    caller: RefCell<Option<String>>,
    conn: RefCell<Option<gio::DBusConnection>>,
    leave_overview: Rc<dyn Fn()>,
}

impl Dialog {
    fn is_open(&self) -> bool {
        self.caller.borrow().is_some()
    }

    fn open(self: &Rc<Self>, devices: &[Device], caller: String) {
        while let Some(child) = self.devices.first_child() {
            self.devices.remove(&child);
        }
        for device in devices.iter().copied() {
            let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
            content.add_css_class("audio-selection-device-box");
            let icon = gtk::Image::from_icon_name(device.icon());
            icon.add_css_class("audio-selection-device-icon");
            icon.set_pixel_size(64);
            content.append(&icon);
            let label = gtk::Label::new(Some(device.label()));
            label.add_css_class("audio-selection-device-label");
            content.append(&label);
            let button = gtk::Button::new();
            button.add_css_class("audio-selection-device");
            button.set_child(Some(&content));
            let me = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(me) = me.upgrade() {
                    me.select(device);
                }
            });
            self.devices.append(&button);
        }
        *self.caller.borrow_mut() = Some(caller);
        self.fade.present();
    }

    fn close(&self) {
        if self.caller.borrow_mut().take().is_some() {
            self.fade.hide();
        }
    }

    fn select(&self, device: Device) {
        let caller = self.caller.borrow().clone();
        if let (Some(caller), Some(conn)) = (caller, self.conn.borrow().as_ref()) {
            let _ = conn.emit_signal(
                Some(&caller),
                PATH,
                NAME,
                "DeviceSelected",
                Some(&(device.name(),).to_variant()),
            );
        }
        self.close();
        (self.leave_overview)();
    }
}

/// Build the dialog and own `org.gnome.Shell.AudioDeviceSelection`
/// (GNOME replaces an earlier owner).
pub fn start(app: &gtk::Application, leave_overview: Rc<dyn Fn()>) {
    let window = gtk::ApplicationWindow::new(app);
    window.add_css_class("tuna-audio-device-selection");
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some("tuna-audio-device-selection"));
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        window.set_anchor(edge, true);
    }
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.set_title(Some("Select Audio Device"));
    let card = gtk::Box::new(gtk::Orientation::Vertical, 18);
    card.add_css_class("modal-dialog");
    card.add_css_class("audio-device-selection-dialog");
    card.set_halign(gtk::Align::Center);
    card.set_valign(gtk::Align::Center);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
    content.add_css_class("modal-dialog-content-box");
    let title = gtk::Label::new(Some("Select Audio Device"));
    title.add_css_class("message-dialog-title");
    content.append(&title);
    let devices = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    devices.add_css_class("audio-selection-box");
    devices.set_halign(gtk::Align::Center);
    devices.set_homogeneous(true);
    content.append(&devices);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    buttons.add_css_class("modal-dialog-button-box");
    buttons.set_homogeneous(true);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("modal-dialog-button");
    let settings = gtk::Button::with_label("Sound Settings");
    settings.add_css_class("modal-dialog-button");
    buttons.append(&cancel);
    buttons.append(&settings);
    card.append(&content);
    card.append(&buttons);
    window.set_child(Some(&card));

    let dialog = Rc::new(Dialog {
        fade: crate::transient::ModalFade::new(&window),
        window,
        devices,
        caller: RefCell::new(None),
        conn: RefCell::new(None),
        leave_overview,
    });
    {
        let me = Rc::downgrade(&dialog);
        cancel.connect_clicked(move |_| {
            if let Some(me) = me.upgrade() {
                me.close();
            }
        });
    }
    {
        // `Main.sessionMode.allowSettings` holds in the user session.
        let me = Rc::downgrade(&dialog);
        settings.connect_clicked(move |_| {
            if let Some(me) = me.upgrade() {
                me.close();
                (me.leave_overview)();
                crate::open_settings_panel("sound");
            }
        });
    }
    {
        let keys = gtk::EventControllerKey::new();
        let me = Rc::downgrade(&dialog);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(me) = me.upgrade() {
                    me.close();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        dialog.window.add_controller(keys);
    }

    let Some(info) = gio::DBusNodeInfo::for_xml(XML)
        .ok()
        .and_then(|node| node.lookup_interface(NAME))
    else {
        eprintln!("tuna-shell-gtk: audio device selection interface did not parse");
        return;
    };
    // Held for the session: an OwnerId releases nothing when it drops.
    let _ = gio::bus_own_name(
        gio::BusType::Session,
        NAME,
        gio::BusNameOwnerFlags::REPLACE,
        move |conn, _| {
            *dialog.conn.borrow_mut() = Some(conn.clone());
            let me = Rc::downgrade(&dialog);
            let registered = conn
                .register_object(PATH, &info)
                .method_call(move |_, sender, _, _, method, params, invocation| {
                    let Some(me) = me.upgrade() else {
                        invocation.return_value(None);
                        return;
                    };
                    match method {
                        "Open" => {
                            let names = params.get::<(Vec<String>,)>().map(|(n,)| n);
                            if let (false, Some(devices)) =
                                (me.is_open(), names.as_deref().and_then(choices))
                            {
                                me.open(&devices, sender.unwrap_or_default().to_owned());
                            }
                            invocation.return_value(None);
                        }
                        "Close" => {
                            me.close();
                            invocation.return_value(None);
                        }
                        _ => invocation
                            .return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
                    }
                })
                .build();
            if let Err(e) = registered {
                eprintln!("tuna-shell-gtk: audio device selection object: {e}");
            }
        },
        |_, _| {},
        |_, _| eprintln!("tuna-shell-gtk: {NAME} is owned elsewhere"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn choices_follow_gnome_order_and_need_two_devices() {
        assert_eq!(
            choices(&names(&["microphone", "headphones", "headset"])),
            Some(vec![
                Device::Headphones,
                Device::Headset,
                Device::Microphone
            ])
        );
        assert_eq!(
            choices(&names(&["headset", "MICROPHONE"])),
            Some(vec![Device::Headset, Device::Microphone])
        );
        // "Too few devices for a selection": nothing opens.
        assert_eq!(choices(&names(&["headphones"])), None);
        assert_eq!(choices(&names(&["headphones", "headphones"])), None);
        assert_eq!(choices(&names(&["headphones", "speaker"])), None);
        assert_eq!(choices(&[]), None);
    }

    #[test]
    fn devices_round_trip_gsd_names_labels_and_icons() {
        // gsd-media-keys-manager.c audio_selection_choices and
        // audioDeviceSelection.js labels and icons.
        assert_eq!(Device::Headphones.name(), "headphones");
        assert_eq!(Device::Headset.name(), "headset");
        assert_eq!(Device::Microphone.name(), "microphone");
        assert_eq!(Device::Headset.label(), "Headset");
        assert_eq!(Device::Microphone.icon(), "audio-input-microphone-symbolic");
        let node = gio::DBusNodeInfo::for_xml(XML).unwrap();
        let iface = node.lookup_interface(NAME).unwrap();
        assert!(iface.lookup_method("Open").is_some());
        assert!(iface.lookup_method("Close").is_some());
        assert!(iface.lookup_signal("DeviceSelected").is_some());
    }
}
