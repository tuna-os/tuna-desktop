//! Built-in privacy screens (#356, ledger P-ST-04):
//! `org.gnome.desktop.privacy` `privacy-screen`, consumed by the
//! compositor as mutter 51 consumes it.
//!
//! GNOME 51 sources (mutter 51.0, `138a14fb`):
//!
//! - KMS: a connector has a privacy screen when it exposes the
//!   `privacy-screen hw-state` enum (`Enabled`, `Disabled`,
//!   `Enabled-locked`, `Disabled-locked`); it is software-switchable when
//!   it also exposes `privacy-screen sw-state`, and locked otherwise
//!   (`meta-kms-connector.c` `privacy_screen_state_hw`,
//!   `has_privacy_screen_software_toggle`, `state_set_properties`). A
//!   requested sw-state is assumed applied unless the screen is locked
//!   (`meta_kms_connector_update_state`'s connector-update branch).
//! - Policy: the global state of the active monitors is disabled when
//!   any is disabled, else enabled when any is enabled, else unavailable
//!   (`meta-monitor-manager.c` `get_global_privacy_screen_state`). A
//!   settings change that differs from it switches every unlocked
//!   privacy screen (`apply_privacy_screen_settings`,
//!   `ensure_privacy_screen_settings`); every monitor rebuild switches
//!   them again (`meta_monitor_manager_rebuild`). A hardware change the
//!   compositor did not request (the firmware hotkey) writes the setting
//!   back so Settings follows it
//!   (`meta-monitor-manager-native.c` `on_kms_resources_changed`,
//!   `meta_monitor_manager_maybe_emit_privacy_screen_change`,
//!   `meta-settings.c` `meta_settings_set_privacy_screen_enabled`).
//! - Publication: `GetCurrentState` gives each monitor with a privacy
//!   screen `privacy-screen-state` `(bb)`, enabled and hardware-locked,
//!   and `MonitorsChanged` follows every change
//!   (`meta-monitor-manager.c` `meta_monitor_manager_handle_get_current_state`).
//!   GNOME Settings' Screen Lock page shows its Privacy Screen row only
//!   when a monitor reports one and makes it insensitive when all are
//!   locked (gnome-control-center 51.0
//!   `panels/privacy/screen/cc-screen-page.c` `update_display_config`).
//!
//! This module is the pure policy: [`PrivacyScreens`] over any
//! [`PrivacyOutput`]. The DRM backend implements the outputs
//! (`drm/privacy.rs`); the setting comes from the owned GIO worker in
//! `privacy_screen_settings.rs`.

/// GSettings schema holding the key.
pub const SCHEMA: &str = "org.gnome.desktop.privacy";
/// Whether the privacy screen is on.
pub const KEY: &str = "privacy-screen";
/// The KMS connector property that switches the privacy screen.
pub const SW_STATE_PROPERTY: &str = "privacy-screen sw-state";
/// The KMS connector property reporting the privacy screen's state.
pub const HW_STATE_PROPERTY: &str = "privacy-screen hw-state";

/// A privacy screen's state (`MetaPrivacyScreenState` without
/// `UNAVAILABLE`, which is `None`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenState {
    pub enabled: bool,
    /// Hardware-locked, or without a software toggle: the setting
    /// cannot change it.
    pub locked: bool,
}

/// `privacy_screen_state_hw` plus the locked rule of
/// `state_set_properties`: the state a connector reports from its
/// `privacy-screen hw-state` enum name (`None` without the property).
/// An unknown name reads as disabled, as mutter warns and does.
pub fn connector_state(hw_state: Option<&str>, has_sw_toggle: bool) -> Option<ScreenState> {
    let (enabled, locked) = match hw_state? {
        "Enabled" => (true, false),
        "Disabled" => (false, false),
        "Enabled-locked" => (true, true),
        "Disabled-locked" => (false, true),
        other => {
            eprintln!("tuna-compositor: unknown privacy screen state: {other:?}");
            (false, false)
        }
    };
    Some(ScreenState {
        enabled,
        locked: locked || !has_sw_toggle,
    })
}

/// One KMS property as a connector lists it: name, enum entries
/// (value, name) and current value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumProperty<H> {
    pub handle: H,
    pub name: String,
    pub entries: Vec<(u64, String)>,
    pub value: u64,
}

impl<H> EnumProperty<H> {
    fn entry_name(&self, value: u64) -> &str {
        self.entries
            .iter()
            .find(|(v, _)| *v == value)
            .map(|(_, name)| name.as_str())
            // Matches no known state: `connector_state` warns.
            .unwrap_or("")
    }

    fn entry_value(&self, name: &str) -> Option<u64> {
        self.entries
            .iter()
            .find(|(_, n)| n == name)
            .map(|(v, _)| *v)
    }
}

/// A connector's privacy screen properties (`MetaKmsConnector`'s
/// `PRIVACY_SCREEN_SW_STATE`/`HW_STATE`) and its cached state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KmsPrivacyScreen<H> {
    sw: Option<EnumProperty<H>>,
    hw: Option<EnumProperty<H>>,
    state: Option<ScreenState>,
}

impl<H> Default for KmsPrivacyScreen<H> {
    fn default() -> Self {
        Self {
            sw: None,
            hw: None,
            state: None,
        }
    }
}

impl<H: Copy + PartialEq> KmsPrivacyScreen<H> {
    /// Discover the properties among a connector's enum properties.
    pub fn discover(properties: Vec<EnumProperty<H>>) -> Self {
        let mut screen = Self::default();
        let mut hw = None;
        for property in properties {
            match property.name.as_str() {
                SW_STATE_PROPERTY => screen.sw = Some(property),
                HW_STATE_PROPERTY => hw = Some(property),
                _ => {}
            }
        }
        if let Some(hw) = hw {
            screen.state = connector_state(Some(hw.entry_name(hw.value)), screen.sw.is_some());
            screen.hw = Some(hw);
        }
        screen
    }

    /// The cached state (`meta_output_kms_get_privacy_screen_state`).
    pub fn state(&self) -> Option<ScreenState> {
        self.state
    }

    /// The hw-state property, to re-read its value.
    pub fn hw_handle(&self) -> Option<H> {
        self.hw.as_ref().map(|hw| hw.handle)
    }

    /// Re-read state from the hw-state property's current `value`
    /// (`meta_kms_connector_update_state` comparing
    /// `privacy_screen_state`); returns whether it changed.
    pub fn refresh(&mut self, value: u64) -> bool {
        let Some(hw) = self.hw.as_mut() else {
            return false;
        };
        hw.value = value;
        let state = connector_state(Some(hw.entry_name(value)), self.sw.is_some());
        std::mem::replace(&mut self.state, state) != state
    }

    /// Switch the privacy screen through `program` (writing `value` to
    /// the sw-state property); on success the cache assumes it applied,
    /// as `meta_kms_connector_update_state` does for a connector update.
    pub fn switch(
        &mut self,
        enabled: bool,
        program: impl FnOnce(H, u64) -> Result<(), String>,
    ) -> Result<(), String> {
        let state = self.state.ok_or("the connector has no privacy screen")?;
        if state.locked {
            return Err("the privacy screen is locked at hardware level".into());
        }
        let sw = self.sw.as_ref().ok_or("no privacy-screen sw-state")?;
        let name = if enabled { "Enabled" } else { "Disabled" };
        let value = sw
            .entry_value(name)
            .ok_or_else(|| format!("privacy-screen sw-state has no {name:?}"))?;
        program(sw.handle, value)?;
        self.state = Some(ScreenState {
            enabled,
            locked: false,
        });
        Ok(())
    }
}

/// One monitor as the privacy screen policy sees it.
pub trait PrivacyOutput {
    /// Connector name, for warnings.
    fn label(&self) -> &str;
    /// Lit (`meta_monitor_is_active`).
    fn active(&self) -> bool;
    /// Its privacy screen; `None` when it has none.
    fn privacy_state(&self) -> Option<ScreenState>;
    /// Switch an unlocked privacy screen.
    fn switch_privacy(&mut self, enabled: bool) -> Result<(), String>;
}

/// `MetaPrivacyScreenChangeState`: why the next hardware change happens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Change {
    #[default]
    None,
    PendingSetting,
    PendingHotkey,
}

/// What the runtime does after a policy step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[must_use]
pub struct Outcome {
    /// Write `privacy-screen` to this value
    /// (`meta_settings_set_privacy_screen_enabled`).
    pub write_setting: Option<bool>,
    /// A privacy screen changed: republish the display state so
    /// `MonitorsChanged` reaches GNOME Settings.
    pub monitors_changed: bool,
}

/// `get_global_privacy_screen_state`: `None` when no active monitor
/// has a privacy screen.
pub fn global_state<O: PrivacyOutput>(outputs: &[O]) -> Option<bool> {
    let mut global = None;
    for output in outputs.iter().filter(|o| o.active()) {
        match output.privacy_state() {
            None => continue,
            Some(state) if !state.enabled => return Some(false),
            Some(_) => global = Some(true),
        }
    }
    global
}

/// Mutter's privacy screen policy: the setting and the pending change.
#[derive(Debug, Default)]
pub struct PrivacyScreens {
    /// The setting as last read or written; `None` before the first read.
    setting: Option<bool>,
    change: Change,
}

impl PrivacyScreens {
    /// The setting as the policy knows it.
    pub fn setting(&self) -> Option<bool> {
        self.setting
    }

    /// A read of `privacy-screen` (`privacy_settings_changed`; the first
    /// read is `meta_monitor_manager_post_init`): switch the privacy
    /// screens when the global state differs.
    pub fn setting_read<O: PrivacyOutput>(&mut self, value: bool, outputs: &mut [O]) -> Outcome {
        if self.setting == Some(value) {
            return Outcome::default();
        }
        self.setting = Some(value);
        let needs_update = global_state(outputs).is_some_and(|global| global != value);
        if !needs_update {
            return Outcome::default();
        }
        let (any_set, changed) = self.ensure(outputs);
        if any_set {
            self.change = Change::PendingSetting;
        }
        if changed {
            self.hardware_changed(outputs)
        } else {
            Outcome::default()
        }
    }

    /// The monitors were rebuilt (a configuration applied, the session
    /// reactivated): `meta_monitor_manager_rebuild` switches every
    /// privacy screen to the setting again, unconditionally.
    pub fn rebuild<O: PrivacyOutput>(&mut self, outputs: &mut [O]) -> Outcome {
        if self.setting.is_none() {
            return Outcome::default();
        }
        let (_, changed) = self.ensure(outputs);
        if changed {
            self.hardware_changed(outputs)
        } else {
            Outcome::default()
        }
    }

    /// A privacy screen's state changed (`on_kms_resources_changed` with
    /// `META_KMS_RESOURCE_CHANGE_PRIVACY_SCREEN`). Unrequested, it was the
    /// hotkey: the setting follows the hardware.
    pub fn hardware_changed<O: PrivacyOutput>(&mut self, outputs: &[O]) -> Outcome {
        if self.change == Change::None {
            self.change = Change::PendingHotkey;
        }
        let mut outcome = Outcome {
            write_setting: None,
            monitors_changed: true,
        };
        if self.change != Change::PendingSetting {
            let enabled = global_state(outputs) == Some(true);
            if self.setting != Some(enabled) {
                self.setting = Some(enabled);
                outcome.write_setting = Some(enabled);
            }
        }
        self.change = Change::None;
        outcome
    }

    /// `ensure_privacy_screen_settings`: switch every monitor's privacy
    /// screen to the setting. Returns whether any monitor took it (also
    /// when it already matched) and whether any hardware changed.
    fn ensure<O: PrivacyOutput>(&self, outputs: &mut [O]) -> (bool, bool) {
        let Some(enabled) = self.setting else {
            return (false, false);
        };
        let (mut any_set, mut changed) = (false, false);
        for output in outputs.iter_mut() {
            let Some(state) = output.privacy_state() else {
                continue;
            };
            if state.locked {
                eprintln!(
                    "tuna-compositor: privacy screen on {} is locked at hardware level, impossible to set it",
                    output.label()
                );
                continue;
            }
            if state.enabled == enabled {
                any_set = true;
                continue;
            }
            match output.switch_privacy(enabled) {
                Ok(()) => {
                    any_set = true;
                    changed = true;
                }
                Err(error) => eprintln!(
                    "tuna-compositor: failed to set privacy screen on {}: {error}",
                    output.label()
                ),
            }
        }
        (any_set, changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENUM: [(u64, &str); 4] = [
        (0, "Enabled"),
        (1, "Disabled"),
        (2, "Enabled-locked"),
        (3, "Disabled-locked"),
    ];

    fn property(handle: u32, name: &str, value: u64) -> EnumProperty<u32> {
        EnumProperty {
            handle,
            name: name.to_owned(),
            entries: ENUM.iter().map(|(v, n)| (*v, (*n).to_owned())).collect(),
            value,
        }
    }

    /// A connector as the kernel lists it: its privacy screen properties
    /// (a ThinkPad eDP panel exposes both) and one unrelated enum.
    fn props(sw: Option<u64>, hw: Option<u64>) -> Vec<EnumProperty<u32>> {
        let mut list = vec![property(1, "DPMS", 0)];
        if let Some(value) = sw {
            list.push(property(10, SW_STATE_PROPERTY, value));
        }
        if let Some(value) = hw {
            list.push(property(11, HW_STATE_PROPERTY, value));
        }
        list
    }

    /// A fake KMS connector: its kernel hw-state value, the sw-state
    /// writes it received and whether the kernel refuses them. An
    /// accepted write moves the kernel's hw-state with it, as the
    /// kernel's privacy screen providers report.
    struct FakeConnector {
        name: &'static str,
        active: bool,
        kernel_hw: Option<u64>,
        screen: KmsPrivacyScreen<u32>,
        writes: Vec<(u32, u64)>,
        refuse: bool,
    }

    impl FakeConnector {
        fn new(name: &'static str, sw: Option<u64>, hw: Option<u64>) -> Self {
            Self {
                name,
                active: true,
                kernel_hw: hw,
                screen: KmsPrivacyScreen::discover(props(sw, hw)),
                writes: Vec::new(),
                refuse: false,
            }
        }

        /// The firmware hotkey: the kernel's hw-state flips; the udev
        /// change event makes the backend re-read.
        fn hotkey(&mut self, value: u64) -> bool {
            self.kernel_hw = Some(value);
            self.reread()
        }

        /// A udev change re-read of the kernel's current value.
        fn reread(&mut self) -> bool {
            self.kernel_hw
                .is_some_and(|value| self.screen.refresh(value))
        }
    }

    impl PrivacyOutput for FakeConnector {
        fn label(&self) -> &str {
            self.name
        }
        fn active(&self) -> bool {
            self.active
        }
        fn privacy_state(&self) -> Option<ScreenState> {
            self.screen.state()
        }
        fn switch_privacy(&mut self, enabled: bool) -> Result<(), String> {
            let refuse = self.refuse;
            let (writes, kernel_hw) = (&mut self.writes, &mut self.kernel_hw);
            self.screen.switch(enabled, |handle, value| {
                if refuse {
                    return Err("EINVAL".into());
                }
                writes.push((handle, value));
                *kernel_hw = Some(value);
                Ok(())
            })
        }
    }

    const ON: ScreenState = ScreenState {
        enabled: true,
        locked: false,
    };
    const OFF: ScreenState = ScreenState {
        enabled: false,
        locked: false,
    };

    #[test]
    fn connector_states_follow_the_kms_enum_and_the_toggle() {
        assert_eq!(connector_state(None, true), None);
        assert_eq!(connector_state(Some("Enabled"), true), Some(ON));
        assert_eq!(connector_state(Some("Disabled"), true), Some(OFF));
        let locked = |enabled| ScreenState {
            enabled,
            locked: true,
        };
        assert_eq!(
            connector_state(Some("Enabled-locked"), true),
            Some(locked(true))
        );
        assert_eq!(
            connector_state(Some("Disabled-locked"), true),
            Some(locked(false))
        );
        // No sw-state: hardware-only, so locked.
        assert_eq!(connector_state(Some("Enabled"), false), Some(locked(true)));
        // An unknown value reads as disabled.
        assert_eq!(connector_state(Some(""), true), Some(OFF));
    }

    #[test]
    fn discovery_reads_the_properties_and_ignores_connectors_without() {
        let panel = KmsPrivacyScreen::discover(props(Some(1), Some(1)));
        assert_eq!(panel.state(), Some(OFF));
        assert_eq!(panel.hw_handle(), Some(11));
        // vkms, virtio and external monitors: no property, no state.
        let plain = KmsPrivacyScreen::discover(props(None, None));
        assert_eq!(plain.state(), None);
        // sw-state alone is not a privacy screen (mutter reads hw-state).
        assert_eq!(
            KmsPrivacyScreen::discover(props(Some(0), None)).state(),
            None
        );
        // A hw-state value outside the enum reads as disabled.
        assert_eq!(
            KmsPrivacyScreen::discover(props(Some(0), Some(9))).state(),
            Some(OFF)
        );
    }

    #[test]
    fn switching_writes_the_sw_state_enum_value_and_refuses_locked_screens() {
        let mut panel = FakeConnector::new("eDP-1", Some(1), Some(1));
        panel.switch_privacy(true).unwrap();
        assert_eq!(panel.writes, vec![(10, 0)]);
        assert_eq!(panel.privacy_state(), Some(ON));
        // The kernel's re-read agrees with the assumed state.
        assert!(!panel.reread());
        panel.switch_privacy(false).unwrap();
        assert_eq!(panel.writes, vec![(10, 0), (10, 1)]);

        let mut locked = FakeConnector::new("eDP-1", Some(3), Some(3));
        assert!(locked.switch_privacy(true).is_err());
        let mut hardware_only = FakeConnector::new("eDP-1", None, Some(1));
        assert!(hardware_only.switch_privacy(true).is_err());
        assert!(locked.writes.is_empty() && hardware_only.writes.is_empty());

        // A refused write leaves the cached state alone.
        let mut refused = FakeConnector::new("eDP-1", Some(1), Some(1));
        refused.refuse = true;
        assert!(refused.switch_privacy(true).is_err());
        assert_eq!(refused.privacy_state(), Some(OFF));
    }

    #[test]
    fn refresh_reports_only_real_hw_state_changes() {
        let mut panel = FakeConnector::new("eDP-1", Some(1), Some(1));
        assert!(!panel.hotkey(1));
        assert!(panel.hotkey(0));
        assert_eq!(panel.privacy_state(), Some(ON));
        assert!(panel.hotkey(2));
        assert_eq!(
            panel.privacy_state(),
            Some(ScreenState {
                enabled: true,
                locked: true
            })
        );
        let mut plain = FakeConnector::new("DP-1", None, None);
        assert!(!plain.hotkey(0));
        assert_eq!(plain.privacy_state(), None);
    }

    #[test]
    fn global_state_is_disabled_when_any_active_monitor_is() {
        let mut outputs = vec![
            FakeConnector::new("eDP-1", Some(0), Some(0)),
            FakeConnector::new("DP-1", None, None),
        ];
        assert_eq!(global_state(&outputs), Some(true));
        outputs.push(FakeConnector::new("eDP-2", Some(1), Some(1)));
        assert_eq!(global_state(&outputs), Some(false));
        // Inactive monitors do not count.
        outputs[2].active = false;
        assert_eq!(global_state(&outputs), Some(true));
        outputs[0].active = false;
        assert_eq!(global_state(&outputs), None);
    }

    #[test]
    fn the_setting_switches_unlocked_screens_without_writing_itself_back() {
        let mut outputs = vec![
            FakeConnector::new("eDP-1", Some(1), Some(1)),
            FakeConnector::new("DP-1", None, None),
        ];
        let mut policy = PrivacyScreens::default();
        // First read with the screen already off: nothing to do.
        assert_eq!(policy.setting_read(false, &mut outputs), Outcome::default());
        assert!(outputs[0].writes.is_empty());
        // Settings turns it on: sw-state Enabled, MonitorsChanged, and
        // no write back of the setting that caused it.
        assert_eq!(
            policy.setting_read(true, &mut outputs),
            Outcome {
                write_setting: None,
                monitors_changed: true
            }
        );
        assert_eq!(outputs[0].writes, vec![(10, 0)]);
        assert_eq!(outputs[0].privacy_state(), Some(ON));
        // The same value again (our own echo) changes nothing.
        assert_eq!(policy.setting_read(true, &mut outputs), Outcome::default());
        assert_eq!(outputs[0].writes.len(), 1);
        // Off again.
        let outcome = policy.setting_read(false, &mut outputs);
        assert!(outcome.monitors_changed && outcome.write_setting.is_none());
        assert_eq!(outputs[0].writes, vec![(10, 0), (10, 1)]);
    }

    #[test]
    fn first_read_applies_the_stored_setting_at_startup() {
        // The panel comes up off (firmware default); the user left it on.
        let mut outputs = vec![FakeConnector::new("eDP-1", Some(1), Some(1))];
        let mut policy = PrivacyScreens::default();
        assert_eq!(policy.setting(), None);
        let outcome = policy.setting_read(true, &mut outputs);
        assert!(outcome.monitors_changed);
        assert_eq!(outcome.write_setting, None);
        assert_eq!(outputs[0].privacy_state(), Some(ON));
    }

    #[test]
    fn locked_and_absent_screens_are_never_written() {
        let mut outputs = vec![
            FakeConnector::new("eDP-1", Some(3), Some(3)),
            FakeConnector::new("DP-1", None, None),
        ];
        let mut policy = PrivacyScreens::default();
        assert_eq!(policy.setting_read(true, &mut outputs), Outcome::default());
        assert!(outputs.iter().all(|o| o.writes.is_empty()));
        assert_eq!(
            outputs[0].privacy_state(),
            Some(ScreenState {
                enabled: false,
                locked: true
            })
        );
        // No privacy screen anywhere: nothing happens either way.
        let mut plain = vec![FakeConnector::new("Virtual-1", None, None)];
        let mut policy = PrivacyScreens::default();
        assert_eq!(policy.setting_read(true, &mut plain), Outcome::default());
        assert_eq!(policy.rebuild(&mut plain), Outcome::default());
    }

    #[test]
    fn a_refused_write_changes_nothing_and_publishes_nothing() {
        let mut outputs = vec![FakeConnector::new("eDP-1", Some(1), Some(1))];
        outputs[0].refuse = true;
        let mut policy = PrivacyScreens::default();
        assert_eq!(policy.setting_read(true, &mut outputs), Outcome::default());
        assert_eq!(outputs[0].privacy_state(), Some(OFF));
    }

    #[test]
    fn the_hotkey_writes_the_setting_back() {
        let mut outputs = vec![FakeConnector::new("eDP-1", Some(1), Some(1))];
        let mut policy = PrivacyScreens::default();
        assert_eq!(policy.setting_read(false, &mut outputs), Outcome::default());
        // Fn+D on a ThinkPad: the kernel turns it on behind our back.
        assert!(outputs[0].hotkey(0));
        assert_eq!(
            policy.hardware_changed(&outputs),
            Outcome {
                write_setting: Some(true),
                monitors_changed: true
            }
        );
        assert_eq!(policy.setting(), Some(true));
        // GSettings echoes the write: no switch, no second write.
        assert_eq!(policy.setting_read(true, &mut outputs), Outcome::default());
        assert!(outputs[0].writes.is_empty());
        // Off by hotkey.
        assert!(outputs[0].hotkey(1));
        assert_eq!(policy.hardware_changed(&outputs).write_setting, Some(false));
        // Locked on by the firmware: the setting follows the hardware.
        assert!(outputs[0].hotkey(2));
        assert_eq!(policy.hardware_changed(&outputs).write_setting, Some(true));
    }

    #[test]
    fn a_change_while_a_setting_is_pending_is_not_a_hotkey() {
        // A locked-off panel keeps the global state disabled, so turning
        // the setting on needs an update; the unlocked panel already
        // matches, so the setting is pending with no hardware change.
        let mut outputs = vec![
            FakeConnector::new("eDP-1", Some(3), Some(3)),
            FakeConnector::new("eDP-2", Some(0), Some(0)),
        ];
        let mut policy = PrivacyScreens::default();
        assert_eq!(policy.setting_read(true, &mut outputs), Outcome::default());
        assert!(outputs.iter().all(|o| o.writes.is_empty()));
        // The firmware then unlocks the first panel to off: the pending
        // setting owns the change, so the setting is not rewritten.
        assert!(outputs[0].hotkey(1));
        assert_eq!(
            policy.hardware_changed(&outputs),
            Outcome {
                write_setting: None,
                monitors_changed: true
            }
        );
        assert_eq!(policy.setting(), Some(true));
        // The next unrequested change is a hotkey again.
        assert!(outputs[1].hotkey(1));
        assert_eq!(policy.hardware_changed(&outputs).write_setting, Some(false));
    }

    #[test]
    fn rebuild_reapplies_the_setting_after_another_owner_changed_it() {
        let mut outputs = vec![FakeConnector::new("eDP-1", Some(1), Some(1))];
        let mut policy = PrivacyScreens::default();
        let _ = policy.setting_read(true, &mut outputs);
        assert_eq!(outputs[0].privacy_state(), Some(ON));
        // A VT switch: the greeter's compositor turned it off.
        assert!(outputs[0].hotkey(1));
        let outcome = policy.rebuild(&mut outputs);
        assert!(outcome.monitors_changed);
        // The rebuild restored the setting; writing it back is a no-op.
        assert_eq!(outcome.write_setting, None);
        assert_eq!(outputs[0].privacy_state(), Some(ON));
        assert_eq!(outputs[0].writes, vec![(10, 0), (10, 0)]);
        // Already matching: a rebuild writes nothing.
        assert_eq!(policy.rebuild(&mut outputs), Outcome::default());
        assert_eq!(outputs[0].writes.len(), 2);
    }

    #[test]
    fn no_rebuild_before_the_setting_is_known() {
        let mut outputs = vec![FakeConnector::new("eDP-1", Some(1), Some(1))];
        let mut policy = PrivacyScreens::default();
        assert_eq!(policy.rebuild(&mut outputs), Outcome::default());
        assert!(outputs[0].writes.is_empty());
    }
}
