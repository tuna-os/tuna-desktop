//! The top bar's camera-in-use indicator: gnome-shell 51
//! `js/ui/status/camera.js` over `Shell.CameraMonitor`.
//!
//! GNOME adds a `camera-web-symbolic` icon with the `privacy-indicator`
//! style class and binds its visibility to `cameras-in-use`. The monitor
//! (`src/shell-camera-monitor.c`) turns that on at once when a camera node
//! runs, and off only [`HOLD_MS`] after the last one stops
//! (`DISABLE_DELAY_MS`), so an app that reopens its camera does not flicker
//! the icon; a lost PipeWire connection turns it off at once. What counts
//! as in use is [`Snapshot::camera_in_use`](crate::audio_state::Snapshot).
use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

pub const ICON: &str = "camera-web-symbolic";
pub const LABEL: &str = "Camera in use";
/// `DISABLE_DELAY_MS` in `shell-camera-monitor.c`.
pub const HOLD_MS: u64 = 500;

pub struct CameraIndicator {
    image: gtk::Image,
    /// The pending hide after the last camera stopped.
    hold: RefCell<Option<glib::SourceId>>,
}

impl CameraIndicator {
    /// Adopt the panel icon: hidden, named, styled as a privacy indicator.
    pub fn new(image: gtk::Image) -> Rc<Self> {
        image.set_icon_name(Some(ICON));
        image.add_css_class("privacy-indicator");
        image.update_property(&[gtk::accessible::Property::Label(LABEL)]);
        image.set_visible(false);
        Rc::new(Self {
            image,
            hold: RefCell::new(None),
        })
    }

    /// A camera is (not) in use now. In use shows the icon at once and
    /// cancels a pending hide; not in use hides it after [`HOLD_MS`].
    pub fn set_in_use(self: &Rc<Self>, in_use: bool) {
        if in_use {
            if let Some(hold) = self.hold.borrow_mut().take() {
                hold.remove();
            }
            self.image.set_visible(true);
        } else if self.image.is_visible() && self.hold.borrow().is_none() {
            let weak = Rc::downgrade(self);
            let id = glib::timeout_add_local_once(Duration::from_millis(HOLD_MS), move || {
                if let Some(this) = weak.upgrade() {
                    this.hold.borrow_mut().take();
                    this.image.set_visible(false);
                }
            });
            *self.hold.borrow_mut() = Some(id);
        }
    }

    /// The monitor lost PipeWire: nothing is in use, at once.
    pub fn reset(&self) {
        if let Some(hold) = self.hold.borrow_mut().take() {
            hold.remove();
        }
        self.image.set_visible(false);
    }
}

impl Drop for CameraIndicator {
    fn drop(&mut self) {
        if let Some(hold) = self.hold.get_mut().take() {
            hold.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn pump_until(limit: Duration, mut done: impl FnMut() -> bool) -> Option<Duration> {
        let start = Instant::now();
        let context = glib::MainContext::default();
        while start.elapsed() < limit {
            while context.iteration(false) {}
            if done() {
                return Some(start.elapsed());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    /// gnome-shell 51 `camera.js` + `shell-camera-monitor.c`: shown at
    /// once, hidden `DISABLE_DELAY_MS` after the last camera stops, kept
    /// by a camera that returns inside the delay, hidden at once when
    /// PipeWire goes.
    #[test]
    #[ignore = "requires owned Xvfb/private D-Bus and explicit camera indicator admission"]
    fn actual_gtk_camera_icon_follows_the_monitor_with_a_hold_off() {
        assert_eq!(
            std::env::var("TUNA_REQUIRE_CAMERA_INDICATOR").as_deref(),
            Ok("1")
        );
        gtk::init().expect("actual GTK display required");
        let image = gtk::Image::new();
        let indicator = CameraIndicator::new(image.clone());
        assert!(!image.is_visible(), "hidden until a camera runs");
        assert_eq!(image.icon_name().as_deref(), Some("camera-web-symbolic"));
        assert!(image.has_css_class("privacy-indicator"));

        indicator.set_in_use(true);
        assert!(image.is_visible(), "a camera in use shows the icon at once");

        indicator.set_in_use(false);
        assert!(
            image.is_visible(),
            "the icon outlives the stop for a moment"
        );
        // Back before the delay: the icon stays and nothing hides it later.
        indicator.set_in_use(true);
        assert!(indicator.hold.borrow().is_none());
        assert!(
            pump_until(Duration::from_millis(HOLD_MS + 300), || !image.is_visible()).is_none(),
            "a returning camera must keep the icon"
        );

        indicator.set_in_use(false);
        let hidden = pump_until(Duration::from_secs(3), || !image.is_visible())
            .expect("the icon never hid after the camera stopped");
        assert!(
            hidden >= Duration::from_millis(HOLD_MS - 50),
            "hid after only {hidden:?}"
        );

        indicator.set_in_use(true);
        indicator.set_in_use(false);
        indicator.reset();
        assert!(!image.is_visible(), "a lost connection hides it at once");
        assert!(indicator.hold.borrow().is_none());
        println!(
            "CAMERA-INDICATOR pass hid {} ms after the last camera stopped",
            hidden.as_millis()
        );
    }
}
