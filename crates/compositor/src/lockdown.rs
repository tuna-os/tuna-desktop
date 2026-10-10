//! Administrative lockdown policy (`org.gnome.desktop.lockdown`, P-ST-05, #357).
//!
//! Pure policy engine: one snapshot of the eleven lockdown keys plus the
//! allow/deny decision for every desktop entry point each key constrains.
//! The compositor owns enforcement for session-scoped requests (lock);
//! the shell owns its menus and dialogs; applications and system daemons
//! own the rest. There is no I/O here: the shell reads GSettings (it
//! owns the `gio` dependency) and ships a snapshot over the control
//! channel; tests inject values through [`LockdownPolicy::from_reader`].
//!
//! Key inventory (verified against the installed
//! `gsettings-desktop-schemas` 46.1 `org.gnome.desktop.lockdown`
//! schema on this machine; the GNOME 51.0 upstream text is unconfirmed
//! from here, so key parity with 51.0 stays an open question):
//!
//! | Key | Owner | Entry points constrained |
//! |---|---|---|
//! | `disable-command-line` | shell (future) | run-command dialog (Alt+F2); Tuna ships none yet, so the restriction holds by absence and [`LockdownPolicy::command_line_allowed`] gates the dialog when it lands |
//! | `disable-log-out` | shell | power menu Log Out row; nested sessions lose their only row and show no menu |
//! | `disable-user-switching` | shell (future) | user-switch row; Tuna ships none yet, gated by [`LockdownPolicy::user_switching_allowed`] |
//! | `disable-lock-screen` | compositor + shell | `Lock` control command, idle lock, shell lock button; denied, never weakened (see below) |
//! | `disable-show-password` | shell | password reveal toggles (network secret dialog) |
//! | `disable-application-handlers` | shell | shell-opened URLs/handlers (settings buttons today); in-app links stay app-owned |
//! | `disable-printing` | app-owned | applications' Print dialogs; the shell exposes no print path of its own |
//! | `disable-print-setup` | app-owned | applications' Print Setup dialogs |
//! | `disable-save-to-disk` | app-owned | applications' Save As dialogs; shell-owned savers (screenshots) are out of the key's scope, as in GNOME |
//! | `user-administration-disabled` | daemon-owned | user account administration (accountsservice/polkit); Tuna ships no Users panel |
//! | `mount-removable-storage-devices-as-read-only` | daemon-owned | removable mounts (udisks2); Tuna ships no mount agent |
//!
//! Secure-lock invariant: `disable-lock-screen` removes the lock entry
//! points; it never changes verification. Unlock still requires session
//! auth, and no policy state auto-unlocks: a session locked before the
//! policy arrived stays locked.
//!
//! Refresh rule (same as the shell snapshot): an unreadable key keeps
//! its current value, never resets. A first load with no bus reads
//! GNOME defaults (everything allowed), exactly like GNOME with an
//! unreachable dconf.

/// GSettings schema holding the lockdown keys.
pub const SCHEMA: &str = "org.gnome.desktop.lockdown";
/// Run-command dialog gate (Alt+F2).
pub const KEY_DISABLE_COMMAND_LINE: &str = "disable-command-line";
/// Applications' Save As dialogs (app-owned).
pub const KEY_DISABLE_SAVE_TO_DISK: &str = "disable-save-to-disk";
/// Applications' Print dialogs (app-owned).
pub const KEY_DISABLE_PRINTING: &str = "disable-printing";
/// Applications' Print Setup dialogs (app-owned).
pub const KEY_DISABLE_PRINT_SETUP: &str = "disable-print-setup";
/// User-switch entry points.
pub const KEY_DISABLE_USER_SWITCHING: &str = "disable-user-switching";
/// Lock entry points (button, `Lock` command, idle lock).
pub const KEY_DISABLE_LOCK_SCREEN: &str = "disable-lock-screen";
/// Shell-opened URL/MIME handlers.
pub const KEY_DISABLE_APPLICATION_HANDLERS: &str = "disable-application-handlers";
/// Log Out entry points.
pub const KEY_DISABLE_LOG_OUT: &str = "disable-log-out";
/// User account administration (daemon-owned).
pub const KEY_USER_ADMINISTRATION_DISABLED: &str = "user-administration-disabled";
/// Removable mounts writable (daemon-owned; `true` mounts read-only).
pub const KEY_MOUNT_REMOVABLE_READ_ONLY: &str = "mount-removable-storage-devices-as-read-only";
/// Password reveal toggles.
pub const KEY_DISABLE_SHOW_PASSWORD: &str = "disable-show-password";

/// Every lockdown key, in schema order.
pub const ALL_KEYS: [&str; 11] = [
    KEY_DISABLE_COMMAND_LINE,
    KEY_DISABLE_SAVE_TO_DISK,
    KEY_DISABLE_PRINTING,
    KEY_DISABLE_PRINT_SETUP,
    KEY_DISABLE_USER_SWITCHING,
    KEY_DISABLE_LOCK_SCREEN,
    KEY_DISABLE_APPLICATION_HANDLERS,
    KEY_DISABLE_LOG_OUT,
    KEY_USER_ADMINISTRATION_DISABLED,
    KEY_MOUNT_REMOVABLE_READ_ONLY,
    KEY_DISABLE_SHOW_PASSWORD,
];

/// One user-visible action a lockdown key can restrict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RestrictedAction {
    /// Open the run-command dialog.
    RunCommand,
    /// Offer Log Out.
    LogOut,
    /// Offer user switching.
    SwitchUser,
    /// Lock the session (button, command, idle).
    LockScreen,
    /// Reveal a typed password.
    ShowPassword,
    /// Open a URL or MIME handler from the shell.
    OpenHandler,
    /// Print (app-owned).
    Print,
    /// Change print setup (app-owned).
    PrintSetup,
    /// Save to disk (app-owned).
    SaveToDisk,
    /// Administer user accounts (daemon-owned).
    UserAdmin,
    /// Write to removable storage (daemon-owned).
    WriteRemovable,
}

impl RestrictedAction {
    /// The lockdown key governing this action.
    pub fn key(self) -> &'static str {
        match self {
            RestrictedAction::RunCommand => KEY_DISABLE_COMMAND_LINE,
            RestrictedAction::LogOut => KEY_DISABLE_LOG_OUT,
            RestrictedAction::SwitchUser => KEY_DISABLE_USER_SWITCHING,
            RestrictedAction::LockScreen => KEY_DISABLE_LOCK_SCREEN,
            RestrictedAction::ShowPassword => KEY_DISABLE_SHOW_PASSWORD,
            RestrictedAction::OpenHandler => KEY_DISABLE_APPLICATION_HANDLERS,
            RestrictedAction::Print => KEY_DISABLE_PRINTING,
            RestrictedAction::PrintSetup => KEY_DISABLE_PRINT_SETUP,
            RestrictedAction::SaveToDisk => KEY_DISABLE_SAVE_TO_DISK,
            RestrictedAction::UserAdmin => KEY_USER_ADMINISTRATION_DISABLED,
            RestrictedAction::WriteRemovable => KEY_MOUNT_REMOVABLE_READ_ONLY,
        }
    }

    /// Short denial message for shell UX ("restricted by your administrator").
    pub fn denial(self) -> &'static str {
        match self {
            RestrictedAction::RunCommand => "Running commands is disabled by your administrator.",
            RestrictedAction::LogOut => "Logging out is disabled by your administrator.",
            RestrictedAction::SwitchUser => "Switching users is disabled by your administrator.",
            RestrictedAction::LockScreen => "Locking the screen is disabled by your administrator.",
            RestrictedAction::ShowPassword => {
                "Showing passwords is disabled by your administrator."
            }
            RestrictedAction::OpenHandler => "Opening links is disabled by your administrator.",
            RestrictedAction::Print => "Printing is disabled by your administrator.",
            RestrictedAction::PrintSetup => {
                "Changing print settings is disabled by your administrator."
            }
            RestrictedAction::SaveToDisk => "Saving files is disabled by your administrator.",
            RestrictedAction::UserAdmin => "Managing users is disabled by your administrator.",
            RestrictedAction::WriteRemovable => {
                "Removable storage is read-only by administrator policy."
            }
        }
    }
}

/// Snapshot of the eleven lockdown keys. GNOME defaults are all `false`
/// (nothing restricted); a `true` bit restricts its entry points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LockdownPolicy {
    /// `disable-command-line`.
    pub disable_command_line: bool,
    /// `disable-save-to-disk` (app-owned).
    pub disable_save_to_disk: bool,
    /// `disable-printing` (app-owned).
    pub disable_printing: bool,
    /// `disable-print-setup` (app-owned).
    pub disable_print_setup: bool,
    /// `disable-user-switching`.
    pub disable_user_switching: bool,
    /// `disable-lock-screen`.
    pub disable_lock_screen: bool,
    /// `disable-application-handlers`.
    pub disable_application_handlers: bool,
    /// `disable-log-out`.
    pub disable_log_out: bool,
    /// `user-administration-disabled` (daemon-owned).
    pub user_administration_disabled: bool,
    /// `mount-removable-storage-devices-as-read-only` (daemon-owned).
    pub mount_removable_read_only: bool,
    /// `disable-show-password`.
    pub disable_show_password: bool,
}

impl LockdownPolicy {
    /// Read one key: `None` (absent/unreadable) keeps the current value.
    fn refresh_key(current: &mut bool, read: &dyn Fn(&str) -> Option<bool>, key: &str) {
        if let Some(value) = read(key) {
            *current = value;
        }
    }

    /// Snapshot from a key reader (GSettings today, control channel at
    /// union). Starts at GNOME defaults, so unreadable keys read allowed.
    pub fn from_reader(read: &dyn Fn(&str) -> Option<bool>) -> Self {
        let mut policy = Self::default();
        policy.refresh(read);
        policy
    }

    /// Re-read every key. Absent or unreadable keys keep their current
    /// value, never reset; a returned `false` re-allows its action.
    pub fn refresh(&mut self, read: &dyn Fn(&str) -> Option<bool>) {
        Self::refresh_key(
            &mut self.disable_command_line,
            read,
            KEY_DISABLE_COMMAND_LINE,
        );
        Self::refresh_key(
            &mut self.disable_save_to_disk,
            read,
            KEY_DISABLE_SAVE_TO_DISK,
        );
        Self::refresh_key(&mut self.disable_printing, read, KEY_DISABLE_PRINTING);
        Self::refresh_key(&mut self.disable_print_setup, read, KEY_DISABLE_PRINT_SETUP);
        Self::refresh_key(
            &mut self.disable_user_switching,
            read,
            KEY_DISABLE_USER_SWITCHING,
        );
        Self::refresh_key(&mut self.disable_lock_screen, read, KEY_DISABLE_LOCK_SCREEN);
        Self::refresh_key(
            &mut self.disable_application_handlers,
            read,
            KEY_DISABLE_APPLICATION_HANDLERS,
        );
        Self::refresh_key(&mut self.disable_log_out, read, KEY_DISABLE_LOG_OUT);
        Self::refresh_key(
            &mut self.user_administration_disabled,
            read,
            KEY_USER_ADMINISTRATION_DISABLED,
        );
        Self::refresh_key(
            &mut self.mount_removable_read_only,
            read,
            KEY_MOUNT_REMOVABLE_READ_ONLY,
        );
        Self::refresh_key(
            &mut self.disable_show_password,
            read,
            KEY_DISABLE_SHOW_PASSWORD,
        );
    }

    /// Nothing restricted.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether `action` is currently allowed.
    pub fn allows(&self, action: RestrictedAction) -> bool {
        match action {
            RestrictedAction::RunCommand => self.command_line_allowed(),
            RestrictedAction::LogOut => self.logout_allowed(),
            RestrictedAction::SwitchUser => self.user_switching_allowed(),
            RestrictedAction::LockScreen => self.lock_screen_allowed(),
            RestrictedAction::ShowPassword => self.password_reveal_allowed(),
            RestrictedAction::OpenHandler => self.application_handlers_allowed(),
            RestrictedAction::Print => self.printing_allowed(),
            RestrictedAction::PrintSetup => self.print_setup_allowed(),
            RestrictedAction::SaveToDisk => self.save_to_disk_allowed(),
            RestrictedAction::UserAdmin => self.user_administration_allowed(),
            RestrictedAction::WriteRemovable => self.removable_storage_writable(),
        }
    }

    /// Denial message when `action` is restricted, `None` when allowed.
    pub fn denial_reason(&self, action: RestrictedAction) -> Option<&'static str> {
        (!self.allows(action)).then(|| action.denial())
    }

    /// Every currently restricted action, in [`ALL_KEYS`] order.
    pub fn restricted_actions(&self) -> Vec<RestrictedAction> {
        ALL_KEYS
            .iter()
            .filter_map(|key| {
                let action = match *key {
                    KEY_DISABLE_COMMAND_LINE => RestrictedAction::RunCommand,
                    KEY_DISABLE_SAVE_TO_DISK => RestrictedAction::SaveToDisk,
                    KEY_DISABLE_PRINTING => RestrictedAction::Print,
                    KEY_DISABLE_PRINT_SETUP => RestrictedAction::PrintSetup,
                    KEY_DISABLE_USER_SWITCHING => RestrictedAction::SwitchUser,
                    KEY_DISABLE_LOCK_SCREEN => RestrictedAction::LockScreen,
                    KEY_DISABLE_APPLICATION_HANDLERS => RestrictedAction::OpenHandler,
                    KEY_DISABLE_LOG_OUT => RestrictedAction::LogOut,
                    KEY_USER_ADMINISTRATION_DISABLED => RestrictedAction::UserAdmin,
                    KEY_MOUNT_REMOVABLE_READ_ONLY => RestrictedAction::WriteRemovable,
                    KEY_DISABLE_SHOW_PASSWORD => RestrictedAction::ShowPassword,
                    _ => return None,
                };
                (!self.allows(action)).then_some(action)
            })
            .collect()
    }

    /// Run-command dialog (Alt+F2) allowed.
    pub fn command_line_allowed(&self) -> bool {
        !self.disable_command_line
    }
    /// Log Out offered.
    pub fn logout_allowed(&self) -> bool {
        !self.disable_log_out
    }
    /// User switching offered.
    pub fn user_switching_allowed(&self) -> bool {
        !self.disable_user_switching
    }
    /// Lock entry points live. Denying lock never weakens verification
    /// and never unlocks; it only removes the button, the `Lock`
    /// command, and idle lock.
    pub fn lock_screen_allowed(&self) -> bool {
        !self.disable_lock_screen
    }
    /// Password reveal toggles shown.
    pub fn password_reveal_allowed(&self) -> bool {
        !self.disable_show_password
    }
    /// Shell-opened URL/MIME handlers run.
    pub fn application_handlers_allowed(&self) -> bool {
        !self.disable_application_handlers
    }
    /// Printing allowed (app-owned; exposed for admin UX text).
    pub fn printing_allowed(&self) -> bool {
        !self.disable_printing
    }
    /// Print setup allowed (app-owned).
    pub fn print_setup_allowed(&self) -> bool {
        !self.disable_print_setup
    }
    /// Saving to disk allowed (app-owned).
    pub fn save_to_disk_allowed(&self) -> bool {
        !self.disable_save_to_disk
    }
    /// User administration allowed (daemon-owned).
    pub fn user_administration_allowed(&self) -> bool {
        !self.user_administration_disabled
    }
    /// Removable storage writable (daemon-owned).
    pub fn removable_storage_writable(&self) -> bool {
        !self.mount_removable_read_only
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Reader over an explicit key set; missing keys read absent.
    fn reader(values: &HashMap<String, bool>) -> impl Fn(&str) -> Option<bool> + '_ {
        move |key: &str| values.get(key).copied()
    }

    #[test]
    fn defaults_allow_everything() {
        let policy = LockdownPolicy::default();
        assert!(policy.is_default());
        assert!(policy.restricted_actions().is_empty());
        for key in ALL_KEYS {
            let action = match key {
                KEY_DISABLE_COMMAND_LINE => RestrictedAction::RunCommand,
                KEY_DISABLE_SAVE_TO_DISK => RestrictedAction::SaveToDisk,
                KEY_DISABLE_PRINTING => RestrictedAction::Print,
                KEY_DISABLE_PRINT_SETUP => RestrictedAction::PrintSetup,
                KEY_DISABLE_USER_SWITCHING => RestrictedAction::SwitchUser,
                KEY_DISABLE_LOCK_SCREEN => RestrictedAction::LockScreen,
                KEY_DISABLE_APPLICATION_HANDLERS => RestrictedAction::OpenHandler,
                KEY_DISABLE_LOG_OUT => RestrictedAction::LogOut,
                KEY_USER_ADMINISTRATION_DISABLED => RestrictedAction::UserAdmin,
                KEY_MOUNT_REMOVABLE_READ_ONLY => RestrictedAction::WriteRemovable,
                KEY_DISABLE_SHOW_PASSWORD => RestrictedAction::ShowPassword,
                _ => unreachable!("ALL_KEYS covers every action"),
            };
            assert!(policy.allows(action), "{key} must default to allowed");
            assert_eq!(policy.denial_reason(action), None);
        }
    }

    #[test]
    fn each_key_denies_exactly_its_action() {
        let actions = [
            (KEY_DISABLE_COMMAND_LINE, RestrictedAction::RunCommand),
            (KEY_DISABLE_LOG_OUT, RestrictedAction::LogOut),
            (KEY_DISABLE_USER_SWITCHING, RestrictedAction::SwitchUser),
            (KEY_DISABLE_LOCK_SCREEN, RestrictedAction::LockScreen),
            (KEY_DISABLE_SHOW_PASSWORD, RestrictedAction::ShowPassword),
            (
                KEY_DISABLE_APPLICATION_HANDLERS,
                RestrictedAction::OpenHandler,
            ),
            (KEY_DISABLE_PRINTING, RestrictedAction::Print),
            (KEY_DISABLE_PRINT_SETUP, RestrictedAction::PrintSetup),
            (KEY_DISABLE_SAVE_TO_DISK, RestrictedAction::SaveToDisk),
            (
                KEY_USER_ADMINISTRATION_DISABLED,
                RestrictedAction::UserAdmin,
            ),
            (
                KEY_MOUNT_REMOVABLE_READ_ONLY,
                RestrictedAction::WriteRemovable,
            ),
        ];
        assert_eq!(actions.len(), ALL_KEYS.len());
        for (key, action) in actions {
            let values = HashMap::from([(key.to_owned(), true)]);
            let policy = LockdownPolicy::from_reader(&reader(&values));
            assert!(!policy.is_default());
            assert_eq!(policy.restricted_actions(), vec![action], "{key}");
            assert_eq!(policy.denial_reason(action), Some(action.denial()));
            assert_eq!(action.key(), key);
        }
    }

    #[test]
    fn absent_keys_keep_current_values_never_reset() {
        let values = HashMap::from([
            (KEY_DISABLE_LOG_OUT.to_owned(), true),
            (KEY_DISABLE_LOCK_SCREEN.to_owned(), true),
        ]);
        let mut policy = LockdownPolicy::from_reader(&reader(&values));
        assert!(!policy.logout_allowed());
        assert!(!policy.lock_screen_allowed());
        // A later unreadable bus changes nothing.
        policy.refresh(&|_| None);
        assert!(!policy.logout_allowed());
        assert!(!policy.lock_screen_allowed());
        // Unknown keys are ignored.
        policy.refresh(&|key| (key == "bogus").then_some(true));
        assert!(!policy.logout_allowed());
    }

    #[test]
    fn policy_unlock_restores_allowed_actions() {
        let locked = HashMap::from([
            (KEY_DISABLE_LOG_OUT.to_owned(), true),
            (KEY_DISABLE_LOCK_SCREEN.to_owned(), true),
            (KEY_DISABLE_COMMAND_LINE.to_owned(), true),
        ]);
        let mut policy = LockdownPolicy::from_reader(&reader(&locked));
        assert_eq!(policy.restricted_actions().len(), 3);
        let unlocked = HashMap::from([
            (KEY_DISABLE_LOG_OUT.to_owned(), false),
            (KEY_DISABLE_LOCK_SCREEN.to_owned(), false),
            (KEY_DISABLE_COMMAND_LINE.to_owned(), false),
        ]);
        policy.refresh(&reader(&unlocked));
        assert!(policy.is_default());
        assert!(policy.logout_allowed());
        assert!(policy.lock_screen_allowed());
        assert!(policy.command_line_allowed());
    }

    #[test]
    fn denial_messages_name_the_restriction() {
        assert!(RestrictedAction::LogOut.denial().contains("Logging out"));
        assert!(RestrictedAction::LockScreen.denial().contains("Locking"));
        assert!(RestrictedAction::WriteRemovable
            .denial()
            .contains("read-only"));
        for key in ALL_KEYS {
            let policy = LockdownPolicy::from_reader(&|k| (k == key).then_some(true));
            assert_eq!(policy.restricted_actions().len(), 1, "{key}");
        }
    }
}
