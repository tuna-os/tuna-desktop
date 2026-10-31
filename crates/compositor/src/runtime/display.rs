//! GNOME Settings' display configuration in the running session (#340):
//! the state `org.gnome.Mutter.DisplayConfig` reports, applying a
//! verified configuration to the backend, and Mutter's confirmation of
//! persistent changes (GNOME Shell's "Keep these display settings?",
//! reverted after 20 seconds unless kept).

use std::time::Duration;

use smithay::output::Scale;

use super::{clamp_scale, logical_size, Backend, Runtime};
use crate::display_config::{
    self, Confirmation, DisplayState, LogicalState, Method, ModeSpec, MonitorState,
};
use crate::monitors::{MonitorSpec, MonitorsConfig};
use crate::State;

impl Runtime {
    /// What GetCurrentState reports. The nested output is one monitor
    /// with one mode, the host window's size: a window cannot change
    /// the host's mode, be mirrored or switched off.
    pub(super) fn display_state(&self) -> DisplayState {
        match &self.backend {
            Backend::Winit(backend) => {
                let Some(output) = self.state.primary_output() else {
                    return DisplayState::default();
                };
                let name = output.name();
                let properties = output.physical_properties();
                let (size, refresh) = match output.current_mode() {
                    Some(mode) => (mode.size, mode.refresh),
                    None => (backend.window_size(), 60_000),
                };
                DisplayState {
                    serial: 0,
                    monitors: vec![MonitorState {
                        spec: MonitorSpec {
                            connector: name.clone(),
                            vendor: properties.make,
                            product: properties.model.clone(),
                            serial: name.clone(),
                        },
                        display_name: properties.model,
                        builtin: false,
                        width_mm: 0,
                        height_mm: 0,
                        modes: vec![ModeSpec {
                            width: size.w,
                            height: size.h,
                            refresh: refresh as f32 / 1000.0,
                            interlaced: false,
                        }],
                        preferred: 0,
                        current: Some(0),
                    }],
                    logical: vec![LogicalState {
                        x: 0,
                        y: 0,
                        scale: self.scale,
                        transform: 0,
                        primary: true,
                        monitors: vec![name],
                    }],
                }
            }
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => drm.display_state(),
        }
    }

    /// Republish the display state when the outputs may have changed
    /// (cheap otherwise); a new state bumps the serial and announces
    /// MonitorsChanged.
    pub(super) fn publish_display_state(&mut self) {
        let key = match &mut self.backend {
            Backend::Winit(backend) => {
                let size = self
                    .state
                    .primary_output()
                    .and_then(|o| o.current_mode())
                    .map(|m| m.size)
                    .unwrap_or_else(|| backend.window_size());
                (size.w, size.h, self.scale.to_bits())
            }
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => {
                if drm.take_mode_reverted() {
                    self.display_published = None;
                }
                (0, 0, drm.layout_generation())
            }
        };
        #[cfg(feature = "drm")]
        if self.display_published.is_none() {
            // A refused commit changed an output's size.
            self.sync_drm_outputs();
        }
        if self.display_published == Some(key) {
            return;
        }
        self.display_published = Some(key);
        self.display_share.publish(self.display_state());
    }

    /// `ApplyMonitorsConfig` with a verified configuration (method
    /// temporary or persistent), as Mutter's monitor manager applies it:
    /// a pending confirmation is cancelled first, the backend applies
    /// the configuration (refusing it whole if the hardware does), and
    /// a persistent one asks GNOME Shell to confirm.
    pub(super) fn on_apply_monitors(
        &mut self,
        config: MonitorsConfig,
        method: Method,
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    ) {
        let live = self.display_state().live_config();
        self.display_configs.ensure_current(live);
        self.display_configs.cancel_confirmation();
        let result = self.apply_monitors(&config);
        match &result {
            Ok(()) => {
                if let Some(generation) = self.display_configs.applied(config, method) {
                    self.request_display_confirmation(generation);
                }
            }
            Err(error) => eprintln!("tuna-compositor: display configuration refused: {error}"),
        }
        self.publish_display_state();
        let _ = reply.send(result);
    }

    /// Mutter's `request_persistent_confirmation`: revert after the
    /// timeout unless GNOME Shell's dialog keeps the change first.
    fn request_display_confirmation(&mut self, generation: u64) {
        let timeout = display_config::CONFIRMATION_TIMEOUT_SECS;
        let timer = calloop::timer::Timer::from_duration(Duration::from_secs(timeout.into()));
        if let Err(error) = self.loop_handle.insert_source(timer, move |_, _, rt| {
            rt.display_confirmation_timed_out(generation);
            calloop::timer::TimeoutAction::Drop
        }) {
            eprintln!("tuna-compositor: display confirmation timer: {error}");
        }
        self.control
            .queue_message(tuna_shell_control::Message::ConfirmDisplayChange {
                timeout_secs: timeout,
            });
    }

    fn display_confirmation_timed_out(&mut self, generation: u64) {
        if self.display_configs.timed_out(generation) {
            eprintln!("tuna-compositor: display change not kept; reverting");
            self.restore_previous_display_config();
        }
    }

    /// GNOME Shell's answer (`meta_monitor_manager_confirm_configuration`):
    /// "Keep Changes" saves the configuration, "Revert Settings"
    /// restores the previous one.
    pub(super) fn complete_display_change(&mut self, keep: bool) {
        match self.display_configs.confirm(keep) {
            Confirmation::Ignored => {}
            Confirmation::Save(config) => match crate::monitors::save(&config) {
                Ok(path) => eprintln!(
                    "tuna-compositor: display settings saved to {}",
                    path.display()
                ),
                Err(e) => eprintln!("tuna-compositor: display settings not saved: {e}"),
            },
            Confirmation::Restore => self.restore_previous_display_config(),
        }
    }

    /// Mutter's `restore_previous_config`: the configuration before the
    /// unconfirmed one returns, as a temporary configuration.
    fn restore_previous_display_config(&mut self) {
        let Some(previous) = self.display_configs.pop_previous() else {
            return;
        };
        match self.apply_monitors(&previous) {
            Ok(()) => {
                self.display_configs.applied(previous, Method::Temporary);
            }
            Err(error) => {
                eprintln!("tuna-compositor: the previous display configuration failed: {error}")
            }
        }
        self.publish_display_state();
    }

    /// A connector was plugged in or unplugged (Mutter's
    /// `meta_monitor_manager_reload`): the backend lights or drops the
    /// outputs, the new monitor set is configured as
    /// `meta_monitor_manager_ensure_configured` does (stored, then the
    /// current or previous configuration that covers it, then linear),
    /// and wl_outputs, windows and DisplayConfig follow.
    #[cfg(feature = "drm")]
    pub(super) fn on_drm_hotplug(&mut self) {
        let live = self.display_state().live_config();
        let Backend::Drm(drm) = &mut self.backend else {
            return;
        };
        let changes = drm.rescan_connectors();
        if changes.is_empty() {
            return;
        }
        eprintln!(
            "tuna-compositor: drm: hotplug: connected [{}], disconnected [{}]",
            changes.added.join(", "),
            changes.removed.join(", ")
        );
        self.display_configs.ensure_current(live);
        let connectors = drm.startup_connectors();
        if !connectors.is_empty() {
            let names: Vec<String> = connectors.iter().map(|c| c.name.clone()).collect();
            let stored = crate::monitors::load_exact(&names);
            let existing: Vec<&MonitorsConfig> = [
                self.display_configs.current(),
                self.display_configs.previous(),
            ]
            .into_iter()
            .flatten()
            .collect();
            let primary = self.state.primary_output_name();
            let fallback = crate::drm::fallback_scale();
            let mut layouts = display_config::hotplug_layouts(
                stored.as_ref(),
                &existing,
                &connectors,
                primary.as_deref(),
                fallback,
            );
            // The sensor wins on unlocked built-in panels; the
            // configured transform stays in manual_transforms.
            for layout in &mut layouts {
                if let Some(enabled) = &mut layout.enabled {
                    self.manual_transforms
                        .insert(layout.connector.clone(), enabled.transform);
                    enabled.transform = self
                        .orientation
                        .effective(&layout.connector, enabled.transform);
                }
            }
            if let Err(error) = drm.apply_layout(&layouts) {
                eprintln!("tuna-compositor: drm: hotplug configuration not applied: {error}");
                let linear = display_config::hotplug_layouts(
                    None,
                    &[],
                    &connectors,
                    primary.as_deref(),
                    fallback,
                );
                if let Err(error) = drm.apply_layout(&linear) {
                    eprintln!("tuna-compositor: drm: linear configuration not applied: {error}");
                }
            }
        }
        drm.log_outputs();
        self.color.set_gamma_sizes(drm.gamma_sizes());
        // meta_monitor_manager_notify_monitors_changed →
        // meta_color_manager_monitors_changed: colord devices follow.
        if let Some(colord) = &self.colord {
            colord.monitors_changed(drm.colord_monitors());
        }
        let primary_scale = drm.outputs.first().map(|o| o.scale);
        self.sync_drm_outputs();
        if let Some(scale) = primary_scale {
            self.state.set_preferred_scale(scale);
        }
        let live = self.display_state().live_config();
        self.display_configs.configured(live);
        self.publish_display_state();
    }

    /// Apply a configuration live, with GNOME's ScreenTransition: the
    /// old layout is pictured first when the layout changes.
    fn apply_monitors(&mut self, config: &MonitorsConfig) -> Result<(), String> {
        let before = (self.state.hot_corner_outputs(), self.scale.to_bits());
        let snapshots = if self.transitions.wants_screen_snapshot() {
            self.screen_snapshots()
        } else {
            Vec::new()
        };
        let result = self.arrange_monitors(config);
        if (self.state.hot_corner_outputs(), self.scale.to_bits()) != before {
            self.transitions.screen_changed(snapshots);
        }
        result
    }

    /// Each monitor's mode, scale, position and rotation; mirrors and
    /// switched-off monitors on hardware. Layout, rendering and the
    /// per-window client scale follow on the next frame. The sensor
    /// may immediately override a built-in panel's rotation while
    /// orientation lock is off (#345).
    fn arrange_monitors(&mut self, config: &MonitorsConfig) -> Result<(), String> {
        let primary = config
            .logical
            .iter()
            .find(|l| l.primary)
            .or_else(|| config.logical.first())
            .ok_or_else(|| "at least one output must stay on".to_owned())?;
        let primary_scale = clamp_scale(primary.scale);
        match &mut self.backend {
            Backend::Winit(backend) => {
                // One nested output: the verified configuration holds
                // exactly it, in its only mode.
                self.scale = primary_scale;
                if let Some(output) = self.state.primary_output() {
                    let scale = if self.scale == 1.0 {
                        Scale::Integer(1)
                    } else {
                        Scale::Fractional(self.scale)
                    };
                    output.change_current_state(None, None, Some(scale), None);
                }
                if primary.transform != 0 {
                    eprintln!(
                        "tuna-compositor: nested output cannot rotate; ignoring transform {}",
                        primary.transform
                    );
                }
                let size = backend.window_size();
                let (lw, lh) = logical_size(size.w, size.h, self.scale);
                self.state.set_output_size(lw, lh);
                // Anchored layer surfaces (the panel) follow the new
                // size now, not on their next commit.
                crate::layer::arrange_after_commit(&self.state);
            }
            #[cfg(feature = "drm")]
            Backend::Drm(drm) => {
                let mut layouts = display_config::output_layouts(config);
                // The sensor wins on unlocked built-in panels; the
                // configured transform stays in manual_transforms.
                for layout in &mut layouts {
                    if let Some(enabled) = &mut layout.enabled {
                        enabled.transform = self
                            .orientation
                            .effective(&layout.connector, enabled.transform);
                    }
                }
                drm.apply_layout(&layouts)?;
            }
        }
        for logical in &config.logical {
            for monitor in &logical.monitors {
                self.manual_transforms
                    .insert(monitor.spec.connector.clone(), logical.transform);
            }
        }
        #[cfg(feature = "drm")]
        self.sync_drm_outputs();
        self.state.set_preferred_scale(primary_scale);
        Ok(())
    }

    /// Mirror the hardware outputs into the window manager's output
    /// inventory: each lit leader is a wl_output at its position. Clones
    /// are not (Mutter advertises one per monitor, clones included: a
    /// known difference, see the ledger); switched
    /// off outputs are not (their windows move first). The primary
    /// leader is primary.
    #[cfg(feature = "drm")]
    fn sync_drm_outputs(&mut self) {
        let Backend::Drm(drm) = &self.backend else {
            return;
        };
        // (connector, output, layout position, logical size)
        type Leader = (String, smithay::output::Output, (i32, i32), (i32, i32));
        let leaders: Vec<Leader> = drm
            .outputs
            .iter()
            .filter(|o| o.is_leader())
            .map(|o| (o.name.clone(), o.output.clone(), o.loc, o.logical_size()))
            .collect();
        let gone: Vec<String> = self
            .state
            .outputs
            .iter()
            .map(|entry| entry.name.clone())
            .filter(|name| !leaders.iter().any(|(leader, ..)| leader == name))
            .collect();
        let primary_before = self.state.primary_output_name();
        let before = self.state.output_slices();
        for name in &gone {
            self.state.remove_output(name);
        }
        self.destroy_retired_output_globals_later();
        for (name, output, loc, (lw, lh)) in &leaders {
            let known = self.state.outputs.iter().any(|entry| &entry.name == name);
            self.state.add_output(name, Some(output.clone()), *lw, *lh);
            self.state.set_output_location(name, *loc);
            if !known {
                let global = output.create_global::<State>(&self.state.dh);
                self.state.note_output_global(name, global);
            }
        }
        if let Some((primary, ..)) = leaders.first() {
            self.state.set_primary(primary);
        }
        // Mutter re-homes every window when the monitors change
        // (`meta_window_update_for_monitors_changed`).
        let after = self.state.output_slices();
        if after != before || self.state.primary_output_name() != primary_before {
            eprintln!("tuna-compositor: outputs changed from {before:?} to {after:?}");
            self.manager
                .relocate_for_monitors_changed(&self.state, &before);
            self.manager.reapply_derived_layouts(&mut self.state);
        }
        // Anchored layer surfaces (the panel) follow a new mode now,
        // not on their next commit.
        crate::layer::arrange_after_commit(&self.state);
    }

    /// Destroy the withdrawn wl_output globals of removed outputs once
    /// clients have long seen the removal: 10 seconds, as niri does
    /// (`Niri::remove_output`, GPL-3.0-or-later). A client binding one
    /// meanwhile gets an inert wl_output instead of a protocol error.
    #[cfg(feature = "drm")]
    fn destroy_retired_output_globals_later(&mut self) {
        let retired = self.state.take_retired_output_globals();
        if retired.is_empty() {
            return;
        }
        let timer = calloop::timer::Timer::from_duration(Duration::from_secs(10));
        if let Err(error) = self.loop_handle.insert_source(timer, move |_, _, rt| {
            for global in &retired {
                rt.state.dh.remove_global::<State>(global.clone());
            }
            calloop::timer::TimeoutAction::Drop
        }) {
            eprintln!("tuna-compositor: output global removal timer: {error}");
        }
    }

    /// The display configuration for the proof state file: the serial,
    /// whether a change awaits confirmation, and each connector's mode
    /// with the CRTC mode the kernel reports on hardware.
    pub(super) fn display_config_diagnostics(&self) -> serde_json::Value {
        let state = self.display_share.snapshot();
        #[cfg(feature = "drm")]
        let crtc_modes = match &self.backend {
            Backend::Drm(drm) => drm.crtc_modes(),
            Backend::Winit(_) => Vec::new(),
        };
        #[cfg(not(feature = "drm"))]
        let crtc_modes: Vec<(String, Option<String>)> = Vec::new();
        let monitors: Vec<serde_json::Value> = state
            .monitors
            .iter()
            .map(|m| {
                let logical = state.logical_of(&m.spec.connector);
                serde_json::json!({
                    "connector": m.spec.connector,
                    "mode": m.current.map(|index| m.modes[index].id()),
                    "modes": m.modes.len(),
                    "crtc_mode": crtc_modes
                        .iter()
                        .find(|(name, _)| *name == m.spec.connector)
                        .and_then(|(_, mode)| mode.clone()),
                    "scale": logical.map(|l| l.scale),
                    "position": logical.map(|l| [l.x, l.y]),
                    "mirrors": logical
                        .filter(|l| l.monitors.first() != Some(&m.spec.connector))
                        .and_then(|l| l.monitors.first().cloned()),
                })
            })
            .collect();
        serde_json::json!({
            "serial": state.serial,
            "awaiting_confirmation": self.display_configs.awaiting_confirmation(),
            "monitors": monitors,
        })
    }
}
