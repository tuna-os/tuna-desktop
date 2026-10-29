//! Colour management in the running session (P-DS-03): each frame's
//! CRTC white point from Night Light, the assigned colord profile and
//! calibration sessions, and the proof state of both.

use super::{Backend, Runtime};

impl Runtime {
    /// Program the CRTC white points for `temperature` (Kelvin) and
    /// return whether this frame still needs the renderer's Night Light
    /// tint (nested outputs, or a CRTC that takes neither LUT nor CTM).
    pub(super) fn update_white_points(&mut self, temperature: u32) -> bool {
        match &mut self.backend {
            Backend::Winit(_) => true,
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => drm.update_white_points(&self.color, temperature),
        }
    }

    /// colord registration, assignment and the kernel's CRTC colour
    /// state for the proof state file.
    pub(super) fn color_diagnostics(&self) -> serde_json::Value {
        let status = match self.color.colord_status() {
            crate::color_manager::ColordStatus::Idle => serde_json::json!("idle"),
            crate::color_manager::ColordStatus::Connected => serde_json::json!("connected"),
            crate::color_manager::ColordStatus::Unavailable(why) => {
                serde_json::json!({ "unavailable": why })
            }
        };
        let profile = |p: &crate::color_manager::ProfileInfo| {
            serde_json::json!({
                "id": p.id, "object_path": p.object_path,
                "filename": p.filename, "title": p.title,
            })
        };
        let devices: Vec<serde_json::Value> = self
            .color
            .devices()
            .iter()
            .map(|d| {
                serde_json::json!({
                    "connector": d.connector,
                    "device_id": d.device_id,
                    "object_path": d.object_path,
                    "ready": d.ready,
                    "device_profile": d.device_profile.as_ref().map(profile),
                    "assigned": d.assigned.as_ref().map(profile),
                    "has_vcgt": d.calibration.is_some(),
                    "error": d.error,
                })
            })
            .collect();
        let sessions: Vec<serde_json::Value> = self
            .color
            .sessions()
            .iter()
            .map(|s| serde_json::json!({ "connector": s.connector, "object_path": s.object_path }))
            .collect();
        #[cfg(feature = "drm")]
        let crtcs = match &self.backend {
            Backend::Drm(drm) => drm.color_state(),
            Backend::Winit(_) => Vec::new(),
        };
        #[cfg(not(feature = "drm"))]
        let crtcs: Vec<serde_json::Value> = Vec::new();
        serde_json::json!({
            "colord": status,
            "devices": devices,
            "calibration_sessions": sessions,
            "temperature": self.night_light.applied_kelvin(),
            "crtcs": crtcs,
        })
    }

    /// Delete the colord devices and profiles this session registered.
    pub(super) fn shutdown_colord(&mut self) {
        if let Some(client) = self.colord.take() {
            client.shutdown();
        }
    }
}
