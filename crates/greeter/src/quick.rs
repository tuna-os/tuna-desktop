//! The login screen's quick settings (#1164): gnome-shell 51.1's
//! `panel.js` `QuickSettings` as the `gdm` mode shows it, named "System"
//! (`C_('System menu in the top bar', 'System')`), with:
//!
//! - the battery (`status/system.js` `PowerToggle`, UPower's
//!   `DisplayDevice`): shown when `IsPresent`, titled with the
//!   percentage, its icon `battery-level-N[-charging]-symbolic` or
//!   `battery-level-100-charged-symbolic`; the panel shows the same icon,
//!   or `system-shutdown-symbolic` without a battery;
//! - the network (`status/network.js`, NetworkManager): the primary
//!   connection with its name, `network-wired-symbolic` or
//!   `network-wireless-symbolic` in the panel (`network-wired-no-route-
//!   symbolic` without full connectivity), hidden when nothing is
//!   connected;
//! - the power button and its menu (`power.rs`).
//!
//! Deviations (docs/greeter.md): the network row shows the primary
//! connection only (no device toggles, Wi-Fi list or VPN), and there is
//! no volume, brightness or Bluetooth at the login screen yet.

use std::rc::Rc;

use gio::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

use crate::i18n::{ctr, tr};
use crate::power::Power;

const UPOWER: &str = "org.freedesktop.UPower";
const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const UPOWER_DEVICE: &str = "org.freedesktop.UPower.Device";
const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
/// `UPower.DeviceState`.
const CHARGING: u32 = 1;
const FULLY_CHARGED: u32 = 4;
const PENDING_CHARGE: u32 = 5;
/// `NM.ConnectivityState.FULL`, `NM.State.CONNECTED_GLOBAL`.
const CONNECTIVITY_FULL: u32 = 4;
const CONNECTED_GLOBAL: u32 = 70;

/// PowerToggle's icon for a battery state.
pub fn battery_icon(state: u32, percentage: f64, threshold: bool) -> String {
    let fill = (10.0 * (percentage / 10.0).floor()).clamp(0.0, 100.0) as u32;
    let charging = match state {
        CHARGING => "-charging",
        PENDING_CHARGE if threshold => "-plugged-in",
        _ => "",
    };
    if state == FULLY_CHARGED || (state == CHARGING && fill == 100) {
        "battery-level-100-charged-symbolic".to_owned()
    } else {
        format!("battery-level-{fill}{charging}-symbolic")
    }
}

/// PowerToggle's title (`Intl.NumberFormat` percent, English).
pub fn battery_title(percentage: f64) -> String {
    format!("{}%", percentage.round().clamp(0.0, 100.0) as u32)
}

/// The network indicator's icon for the primary connection type.
pub fn network_icon(kind: &str, connectivity: u32) -> &'static str {
    match kind {
        "802-11-wireless" => "network-wireless-symbolic",
        _ if connectivity == CONNECTIVITY_FULL => "network-wired-symbolic",
        _ => "network-wired-no-route-symbolic",
    }
}

/// The network menu header for a connection type (`status/network.js`).
fn network_header(kind: &str) -> String {
    match kind {
        "802-11-wireless" => tr("Wi–Fi"),
        "gsm" | "cdma" => tr("Mobile Connections"),
        "vpn" | "wireguard" => tr("VPN"),
        _ => tr("Wired Connections"),
    }
}

pub struct QuickSettings {
    pub button: gtk::MenuButton,
    battery_icon: gtk::Image,
    battery_row: gtk::Box,
    battery_label: gtk::Label,
    battery_row_icon: gtk::Image,
    network_icon: gtk::Image,
    network_row: gtk::Box,
    network_header: gtk::Label,
    network_name: gtk::Label,
    network_row_icon: gtk::Image,
    power: Rc<Power>,
    upower: std::cell::RefCell<Option<gio::DBusProxy>>,
    nm: std::cell::RefCell<Option<gio::DBusProxy>>,
}

impl QuickSettings {
    pub fn new(overlay: &gtk::Overlay, beneath: &gtk::Widget) -> Rc<Self> {
        let power = Power::new(overlay, beneath);
        let network_icon = gtk::Image::from_icon_name("network-wired-symbolic");
        network_icon.set_visible(false);
        let battery_icon = gtk::Image::from_icon_name("system-shutdown-symbolic");
        let indicators = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        indicators.add_css_class("panel-status-indicators-box");
        indicators.append(&network_icon);
        indicators.append(&battery_icon);

        // The top row: the battery, then the power button at the end.
        let battery_row_icon = gtk::Image::new();
        let battery_label = gtk::Label::new(None);
        let battery_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        battery_row.add_css_class("power-item");
        battery_row.append(&battery_row_icon);
        battery_row.append(&battery_label);
        battery_row.set_visible(false);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        top.add_css_class("quick-settings-system-item");
        top.append(&battery_row);
        top.append(&spacer);
        top.append(&power.button);

        let network_row_icon = gtk::Image::new();
        let network_header = gtk::Label::new(None);
        network_header.add_css_class("heading");
        network_header.set_xalign(0.0);
        let network_name = gtk::Label::new(None);
        network_name.set_xalign(0.0);
        let network_text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        network_text.append(&network_header);
        network_text.append(&network_name);
        let network_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        network_row.add_css_class("quick-toggle");
        network_row.append(&network_row_icon);
        network_row.append(&network_text);
        network_row.set_visible(false);

        let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
        column.add_css_class("quick-settings-menu");
        column.append(&top);
        column.append(&power.menu);
        column.append(&network_row);
        let popover = gtk::Popover::new();
        popover.add_css_class("quick-settings");
        popover.set_child(Some(&column));
        let button = gtk::MenuButton::new();
        button.set_child(Some(&indicators));
        button.add_css_class("flat");
        button.add_css_class("panel-button");
        button.update_property(&[gtk::accessible::Property::Label(&ctr(
            "System menu in the top bar",
            "System",
        ))]);
        button.set_popover(Some(&popover));
        let quick = Rc::new(Self {
            button,
            battery_icon,
            battery_row,
            battery_label,
            battery_row_icon,
            network_icon,
            network_row,
            network_header,
            network_name,
            network_row_icon,
            power,
            upower: Default::default(),
            nm: Default::default(),
        });
        {
            let weak = Rc::downgrade(&quick);
            *quick.power.close.borrow_mut() = Some(Box::new(move || {
                if let Some(quick) = weak.upgrade() {
                    quick.button.popdown();
                }
            }));
        }
        {
            let weak = Rc::downgrade(&quick);
            popover.connect_map(move |_| {
                if let Some(quick) = weak.upgrade() {
                    quick.power.refresh();
                }
            });
        }
        {
            let power = Rc::downgrade(&quick.power);
            popover.connect_closed(move |_| {
                if let Some(power) = power.upgrade() {
                    power.button.set_active(false);
                }
            });
        }
        quick.watch();
        quick
    }

    fn watch(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let flags = gio::DBusProxyFlags::DO_NOT_AUTO_START;
            let upower = gio::DBusProxy::for_bus_future(
                gio::BusType::System,
                flags,
                None,
                UPOWER,
                DISPLAY_DEVICE,
                UPOWER_DEVICE,
            )
            .await;
            let nm =
                gio::DBusProxy::for_bus_future(gio::BusType::System, flags, None, NM, NM_PATH, NM)
                    .await;
            let Some(quick) = weak.upgrade() else {
                return;
            };
            if let Ok(upower) = upower {
                let weak = Rc::downgrade(&quick);
                upower.connect_local("g-properties-changed", false, move |_| {
                    if let Some(quick) = weak.upgrade() {
                        quick.sync_battery();
                    }
                    None
                });
                *quick.upower.borrow_mut() = Some(upower);
            }
            if let Ok(nm) = nm {
                let weak = Rc::downgrade(&quick);
                nm.connect_local("g-properties-changed", false, move |_| {
                    if let Some(quick) = weak.upgrade() {
                        quick.sync_network();
                    }
                    None
                });
                *quick.nm.borrow_mut() = Some(nm);
            }
            quick.sync_battery();
            quick.sync_network();
        });
    }

    fn sync_battery(&self) {
        let proxy = self.upower.borrow().clone();
        let prop = |name: &str| proxy.as_ref().and_then(|p| p.cached_property(name));
        let present = prop("IsPresent")
            .and_then(|v| v.get::<bool>())
            .unwrap_or(false);
        if !present {
            self.battery_row.set_visible(false);
            self.battery_icon
                .set_icon_name(Some("system-shutdown-symbolic"));
            crate::trace("greeter battery present=false");
            return;
        }
        let percentage = prop("Percentage")
            .and_then(|v| v.get::<f64>())
            .unwrap_or(0.0);
        let state = prop("State").and_then(|v| v.get::<u32>()).unwrap_or(0);
        let threshold = prop("ChargeThresholdEnabled")
            .and_then(|v| v.get::<bool>())
            .unwrap_or(false);
        let icon = battery_icon(state, percentage, threshold);
        let title = battery_title(percentage);
        self.battery_icon.set_icon_name(Some(&icon));
        self.battery_row_icon.set_icon_name(Some(&icon));
        self.battery_label.set_label(&title);
        self.battery_row.set_visible(true);
        crate::trace(&format!(
            "greeter battery present=true title={title} icon={icon}"
        ));
    }

    fn sync_network(self: &Rc<Self>) {
        let proxy = self.nm.borrow().clone();
        let prop = |name: &str| proxy.as_ref().and_then(|p| p.cached_property(name));
        let state = prop("State").and_then(|v| v.get::<u32>()).unwrap_or(0);
        let connectivity = prop("Connectivity")
            .and_then(|v| v.get::<u32>())
            .unwrap_or(0);
        let kind = prop("PrimaryConnectionType")
            .and_then(|v| v.get::<String>())
            .unwrap_or_default();
        let primary = prop("PrimaryConnection")
            .and_then(|v| v.get::<glib::variant::ObjectPath>())
            .map(|p| p.as_str().to_owned())
            .filter(|p| p != "/");
        let Some(primary) = primary else {
            self.network_row.set_visible(false);
            self.network_icon
                .set_icon_name(Some(network_icon("", connectivity)));
            self.network_icon.set_visible(state == CONNECTED_GLOBAL);
            crate::trace("greeter network primary=none");
            return;
        };
        let icon = network_icon(&kind, connectivity);
        self.network_icon.set_icon_name(Some(icon));
        self.network_icon.set_visible(true);
        self.network_row_icon.set_icon_name(Some(icon));
        self.network_header.set_label(&network_header(&kind));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(system) = gio::bus_get_future(gio::BusType::System).await else {
                return;
            };
            let id = system
                .call_future(
                    Some(NM),
                    &primary,
                    "org.freedesktop.DBus.Properties",
                    "Get",
                    Some(&(NM_ACTIVE, "Id").to_variant()),
                    Some(glib::VariantTy::new("(v)").expect("valid type")),
                    gio::DBusCallFlags::NONE,
                    -1,
                )
                .await
                .ok()
                .and_then(|reply| reply.child_value(0).as_variant())
                .and_then(|v| v.get::<String>())
                .unwrap_or_default();
            let Some(quick) = weak.upgrade() else {
                return;
            };
            quick.network_name.set_label(&id);
            quick.network_row.set_visible(true);
            quick
                .network_row
                .update_property(&[gtk::accessible::Property::Label(&format!(
                    "{} {id}",
                    quick.network_header.label()
                ))]);
            crate::trace(&format!("greeter network primary={id} type={kind}"));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_icons_are_power_toggles() {
        assert_eq!(battery_icon(2, 57.0, false), "battery-level-50-symbolic");
        assert_eq!(
            battery_icon(1, 57.0, false),
            "battery-level-50-charging-symbolic"
        );
        assert_eq!(
            battery_icon(4, 100.0, false),
            "battery-level-100-charged-symbolic"
        );
        assert_eq!(
            battery_icon(1, 100.0, false),
            "battery-level-100-charged-symbolic"
        );
        assert_eq!(
            battery_icon(5, 80.0, true),
            "battery-level-80-plugged-in-symbolic"
        );
        assert_eq!(battery_icon(5, 80.0, false), "battery-level-80-symbolic");
        assert_eq!(battery_title(57.4), "57%");
    }

    #[test]
    fn the_network_icon_follows_the_primary_connection() {
        assert_eq!(network_icon("802-3-ethernet", 4), "network-wired-symbolic");
        assert_eq!(
            network_icon("802-3-ethernet", 2),
            "network-wired-no-route-symbolic"
        );
        assert_eq!(
            network_icon("802-11-wireless", 4),
            "network-wireless-symbolic"
        );
    }
}
