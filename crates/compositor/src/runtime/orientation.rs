//! Sensor-driven orientation in the running session (#345, P-DS-04), as
//! Mutter 51's monitor manager drives it (`src/backends/meta-monitor-manager.c`):
//! `PanelOrientationManaged` follows touch mode, the accelerometer and
//! the built-in panel (`update_panel_orientation_managed`); tracking is
//! inhibited by the orientation lock or an unmanaged panel
//! (`orientation_lock_changed`, `sync_orientation_unmanaged_inhibit`);
//! an orientation change rotates the built-in panel with a temporary
//! monitors configuration (`handle_orientation_change`), never
//! rewriting monitors.xml, and the lock keeps the rotation it has.

use super::Runtime;
use crate::display_config::Method;

impl Runtime {
    /// Whether a built-in panel is connected, and whether its preferred
    /// mode is portrait (`None`: no built-in panel).
    fn builtin_portrait(&self) -> Option<bool> {
        let state = self.display_state();
        let panel = state.monitors.iter().find(|m| m.builtin)?;
        let preferred = panel.modes.get(panel.preferred)?;
        Some(preferred.width <= preferred.height)
    }

    /// Publish `PanelOrientationManaged` and the tracking inhibition to
    /// the D-Bus thread (which emits the property change and claims or
    /// releases the accelerometer).
    pub(super) fn sync_orientation(&mut self) {
        let has_builtin = self.builtin_portrait().is_some();
        self.orientation_share
            .set_managed(self.orientation.managed(has_builtin));
        self.orientation_share
            .set_inhibited(self.orientation.inhibited(has_builtin));
    }

    /// The seat's touch mode moved (a touchscreen, pointer or tablet-mode
    /// switch came or went): Mutter recomputes the managed state.
    pub(super) fn sync_orientation_seat(&mut self) {
        #[cfg(feature = "drm")]
        if let super::Backend::Drm(drm) = &self.backend {
            let mode = drm.touch_mode();
            let (touch_mode, has_touchscreen) = (mode.touch_mode(), mode.has_touchscreen);
            if (
                self.orientation.touch_mode,
                self.orientation.has_touchscreen,
            ) != (touch_mode, has_touchscreen)
            {
                self.orientation.touch_mode = touch_mode;
                self.orientation.has_touchscreen = has_touchscreen;
                self.sync_orientation();
            }
        }
    }

    /// The orientation lock changed (`orientation_lock_changed`): tracking
    /// stops or resumes; the panel keeps its rotation either way.
    pub(super) fn set_orientation_lock(&mut self, lock: bool) {
        if self.orientation.lock != lock {
            self.orientation.lock = lock;
            self.sync_orientation();
        }
    }

    /// An accelerometer sample from the D-Bus thread: presence and the
    /// orientation (frozen while untracked). A changed orientation, or a
    /// fresh claim (`::sensor-active`), runs `orientation_changed`.
    pub(super) fn on_sensor_sample(
        &mut self,
        present: bool,
        orientation: Option<crate::orientation::SensorOrientation>,
        claimed: bool,
    ) {
        self.orientation.sensor_present = present;
        let changed = self.orientation.sensor != orientation;
        self.orientation.sensor = orientation;
        if changed || claimed {
            let portrait = self.builtin_portrait();
            if let Some(transform) = self.orientation.orientation_changed(portrait) {
                self.rotate_builtin(transform);
            }
        }
        self.sync_orientation();
    }

    /// `handle_orientation_change`: the lit built-in panel's logical
    /// monitor takes `transform` through a temporary configuration.
    fn rotate_builtin(&mut self, transform: u32) {
        let state = self.display_state();
        let Some(panel) = state
            .monitors
            .iter()
            .find(|m| m.builtin && m.current.is_some())
        else {
            return;
        };
        let live = state.live_config();
        let mut config = live.clone();
        let Some(logical) = config.logical.iter_mut().find(|l| {
            l.monitors
                .iter()
                .any(|m| m.spec.connector == panel.spec.connector)
        }) else {
            return;
        };
        if logical.transform == transform {
            return;
        }
        logical.transform = transform;
        self.display_configs.ensure_current(live);
        match self.apply_monitors(&config) {
            Ok(()) => {
                self.display_configs.applied(config, Method::Temporary);
                eprintln!(
                    "tuna-compositor: orientation: {} rotated to transform {transform}",
                    panel.spec.connector
                );
            }
            Err(error) => {
                eprintln!("tuna-compositor: orientation: rotation refused: {error}")
            }
        }
        self.publish_display_state();
    }

    /// The orientation state for the compositor state file.
    pub(super) fn orientation_diagnostics(&self) -> serde_json::Value {
        use crate::orientation::SensorOrientation;
        serde_json::json!({
            "sensor_present": self.orientation.sensor_present,
            "sensor": self.orientation.sensor.map(|s| match s {
                SensorOrientation::Normal => "normal",
                SensorOrientation::BottomUp => "bottom-up",
                SensorOrientation::LeftUp => "left-up",
                SensorOrientation::RightUp => "right-up",
            }),
            "locked": self.orientation.lock,
            "managed": self.orientation_share.is_managed(),
            "touch_mode": self.orientation.touch_mode,
            "tracking": !self.orientation_share.is_inhibited(),
        })
    }
}
