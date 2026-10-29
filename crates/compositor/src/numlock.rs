//! GNOME's remembered Num Lock (#346, ledger P-IN-06):
//! `org.gnome.desktop.peripherals.keyboard` `remember-numlock-state` and
//! `numlock-state`, owned by the compositor as mutter 51 owns them
//! (`src/backends/meta-input-settings.c`
//! `meta_input_settings_maybe_restore_numlock_state` and
//! `_maybe_save_numlock_state`, called from `meta-seat-impl.c`).
//!
//! - Restore, once when the seat's keyboard starts: Num Lock goes on when
//!   `remember-numlock-state` is set and `numlock-state` is true;
//!   otherwise it stays as the keymap starts (off).
//! - Save, on every change of the Num Lock LED: while
//!   `remember-numlock-state` is set, `numlock-state` follows it (written
//!   only when it differs).
//!
//! The compositor has no `gio`; like the lockdown backstop it talks to
//! GSettings through the `gsettings` CLI, always off the frame path: a
//! startup reader thread and one save worker that coalesces to the
//! newest state. Unreadable settings read as GNOME's defaults (nothing
//! remembered), so a broken backend never turns Num Lock on or writes.

use std::sync::mpsc;

use crate::lockdown::{read_bool_via_command, LOCKDOWN_READ_TIMEOUT};

/// Mutter's keyboard settings schema.
pub const SCHEMA: &str = "org.gnome.desktop.peripherals.keyboard";
/// Whether the Num Lock state is remembered across sessions.
pub const KEY_REMEMBER: &str = "remember-numlock-state";
/// The remembered Num Lock state.
pub const KEY_STATE: &str = "numlock-state";

/// `maybe_restore_numlock_state`: the remembered state when remembering,
/// else off. Unreadable keys read their GNOME default (`false`).
pub fn restored_state(remember: Option<bool>, saved: Option<bool>) -> bool {
    remember == Some(true) && saved == Some(true)
}

/// `maybe_save_numlock_state`: write `current` only while remembering
/// and when the stored value differs. An unreadable `remember` writes
/// nothing; an unreadable stored value is rewritten.
pub fn needs_save(remember: Option<bool>, saved: Option<bool>, current: bool) -> bool {
    remember == Some(true) && saved != Some(current)
}

/// GSettings access, injectable for tests.
pub trait Store: Send + 'static {
    fn get(&self, key: &str) -> Option<bool>;
    fn set(&self, key: &str, value: bool);
}

/// The live `gsettings` CLI, bounded like the lockdown reads.
#[derive(Clone, Copy)]
pub struct CliStore;

impl Store for CliStore {
    fn get(&self, key: &str) -> Option<bool> {
        if !crate::lockdown::session_bus_present() {
            return None;
        }
        read_bool_via_command("gsettings", &["get", SCHEMA, key], LOCKDOWN_READ_TIMEOUT)
    }

    fn set(&self, key: &str, value: bool) {
        if !crate::lockdown::session_bus_present() {
            return;
        }
        let status = std::process::Command::new("gsettings")
            .args(["set", SCHEMA, key, if value { "true" } else { "false" }])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if !status.is_ok_and(|s| s.success()) {
            eprintln!("tuna-compositor: could not save {SCHEMA} {key}");
        }
    }
}

/// The runtime's half: what to restore, and which changes to save.
pub struct NumlockMemory {
    restore: Option<mpsc::Receiver<bool>>,
    /// Last observed Num Lock state; `None` until the keyboard exists.
    last: Option<bool>,
    /// A change seen before the restore landed: the user's choice wins.
    changed_before_restore: bool,
    save: mpsc::Sender<bool>,
}

impl NumlockMemory {
    /// Start the restore read and the save worker against `store`;
    /// `landed` runs once the restore read finishes (the runtime wakes
    /// its loop so the result applies without waiting for input).
    pub fn start(store: impl Store + Clone, landed: impl FnOnce() + Send + 'static) -> Self {
        let (restore_tx, restore_rx) = mpsc::channel();
        let reader = store.clone();
        let _ = std::thread::Builder::new()
            .name("tuna-numlock-restore".into())
            .spawn(move || {
                let remember = reader.get(KEY_REMEMBER);
                let saved = if remember == Some(true) {
                    reader.get(KEY_STATE)
                } else {
                    None
                };
                let _ = restore_tx.send(restored_state(remember, saved));
                landed();
            });
        let (save_tx, save_rx) = mpsc::channel::<bool>();
        let _ = std::thread::Builder::new()
            .name("tuna-numlock-save".into())
            .spawn(move || {
                while let Ok(mut current) = save_rx.recv() {
                    // Coalesce a burst of toggles to the newest state.
                    while let Ok(newer) = save_rx.try_recv() {
                        current = newer;
                    }
                    let remember = store.get(KEY_REMEMBER);
                    if remember != Some(true) {
                        continue;
                    }
                    if needs_save(remember, store.get(KEY_STATE), current) {
                        store.set(KEY_STATE, current);
                    }
                }
            });
        Self {
            restore: Some(restore_rx),
            last: None,
            changed_before_restore: false,
            save: save_tx,
        }
    }

    /// One runtime tick with the keyboard's Num Lock state (`None`
    /// before the seat has a keyboard). Returns `Some(true)` when the
    /// remembered state must be applied now; queues a save on every
    /// observed change.
    pub fn tick(&mut self, current: Option<bool>) -> Option<bool> {
        let current = current?;
        if self
            .last
            .replace(current)
            .is_some_and(|previous| previous != current)
        {
            if self.restore.is_some() {
                self.changed_before_restore = true;
            }
            let _ = self.save.send(current);
        }
        let restore = self.restore.as_ref()?;
        let wanted = match restore.try_recv() {
            Ok(wanted) => wanted,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => false,
        };
        self.restore = None;
        (wanted && !current && !self.changed_before_restore).then_some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    #[test]
    fn restore_and_save_follow_mutter() {
        // maybe_restore: only a remembered `true` turns Num Lock on.
        assert!(restored_state(Some(true), Some(true)));
        assert!(!restored_state(Some(true), Some(false)));
        assert!(!restored_state(Some(false), Some(true)));
        assert!(!restored_state(None, Some(true)));
        assert!(!restored_state(Some(true), None));
        // maybe_save: only while remembering, only when it differs.
        assert!(needs_save(Some(true), Some(false), true));
        assert!(!needs_save(Some(true), Some(true), true));
        assert!(!needs_save(Some(false), Some(false), true));
        assert!(!needs_save(None, Some(false), true));
        assert!(needs_save(Some(true), None, false));
    }

    /// `remember-numlock-state`, `numlock-state`, and every write.
    type Keys = (Option<bool>, Option<bool>, Vec<bool>);

    #[derive(Clone, Default)]
    struct Fake(Arc<Mutex<Keys>>);

    impl Store for Fake {
        fn get(&self, key: &str) -> Option<bool> {
            let inner = self.0.lock().unwrap();
            match key {
                KEY_REMEMBER => inner.0,
                KEY_STATE => inner.1,
                _ => None,
            }
        }
        fn set(&self, key: &str, value: bool) {
            assert_eq!(key, KEY_STATE);
            let mut inner = self.0.lock().unwrap();
            inner.1 = Some(value);
            inner.2.push(value);
        }
    }

    fn until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn remembered_on_restores_then_changes_are_saved() {
        let store = Fake::default();
        *store.0.lock().unwrap() = (Some(true), Some(true), Vec::new());
        let mut memory = NumlockMemory::start(store.clone(), || {});
        // No keyboard yet: nothing happens.
        assert_eq!(memory.tick(None), None);
        // The keyboard starts off; the remembered `true` comes back.
        let mut restored = None;
        assert!(until(|| {
            restored = memory.tick(Some(false));
            restored.is_some()
        }));
        assert_eq!(restored, Some(true));
        // Applying it is a change, but equal to the stored value: no write.
        assert_eq!(memory.tick(Some(true)), None);
        // The user turns it off: saved.
        assert_eq!(memory.tick(Some(false)), None);
        assert!(until(|| store.0.lock().unwrap().1 == Some(false)));
        assert_eq!(store.0.lock().unwrap().2, vec![false]);
    }

    #[test]
    fn nothing_remembered_restores_nothing_and_writes_nothing() {
        let store = Fake::default();
        *store.0.lock().unwrap() = (Some(false), Some(true), Vec::new());
        let mut memory = NumlockMemory::start(store.clone(), || {});
        assert!(until(|| {
            memory.tick(Some(false));
            memory.restore.is_none()
        }));
        memory.tick(Some(true));
        memory.tick(Some(false));
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            store.0.lock().unwrap().2.is_empty(),
            "remember off never writes"
        );
    }

    /// A backend slow enough that input arrives before the restore read.
    #[derive(Clone)]
    struct Slow(Fake);

    impl Store for Slow {
        fn get(&self, key: &str) -> Option<bool> {
            std::thread::sleep(Duration::from_millis(300));
            self.0.get(key)
        }
        fn set(&self, key: &str, value: bool) {
            self.0.set(key, value);
        }
    }

    #[test]
    fn a_toggle_before_the_restore_lands_wins() {
        let store = Fake::default();
        *store.0.lock().unwrap() = (Some(true), Some(true), Vec::new());
        let mut memory = NumlockMemory::start(Slow(store), || {});
        assert_eq!(memory.tick(Some(false)), None);
        assert_eq!(memory.tick(Some(true)), None);
        assert_eq!(memory.tick(Some(false)), None);
        let mut applied = false;
        assert!(until(|| {
            applied |= memory.tick(Some(false)).is_some();
            memory.restore.is_none()
        }));
        assert!(!applied, "the user's toggle beats a late restore");
    }
}
