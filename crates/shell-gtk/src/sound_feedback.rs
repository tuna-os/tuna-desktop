//! Serialized, bounded volume feedback. Playback state comes from the native
//! PipeWire event stream, so changing volume does not interrupt existing audio.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use gtk4::{gio, glib};

thread_local! {
    static RUNNING: Cell<Option<bool>> = const { Cell::new(None) };
    static OWN_PLAYING: Cell<bool> = const { Cell::new(false) };
    static PLAYER: RefCell<Option<gio::Subprocess>> = const { RefCell::new(None) };
    static PENDING: RefCell<Option<glib::SourceId>> = const { RefCell::new(None) };
    static SETTINGS: Option<gio::Settings> = {
        let settings = crate::media_keys::sound_settings();
        if let Some(settings) = settings.as_ref() {
            settings.connect_changed(Some("event-sounds"), |settings, _| {
                if !settings.boolean("event-sounds") {
                    PENDING.with_borrow_mut(|pending| {
                        if let Some(id) = pending.take() { id.remove(); }
                    });
                    PLAYER.with_borrow_mut(|player| {
                        if let Some(player) = player.take() { player.force_exit(); }
                    });
                    OWN_PLAYING.set(false);
                }
            });
        }
        settings
    };
}

pub fn set_sink_running(running: Option<bool>) {
    SETTINGS.with(|_| ());
    RUNNING.set(running);
}

pub fn feedback_process_id() -> Option<u32> {
    PLAYER.with_borrow(|player| player.as_ref()?.identifier()?.parse().ok())
}

pub fn set_own_playing(playing: bool) {
    OWN_PLAYING.set(playing);
}

fn allowed(
    enabled: bool,
    quiet: bool,
    changed: bool,
    muted: bool,
    running: Option<bool>,
    own_playing: bool,
) -> bool {
    enabled
        && !quiet
        && changed
        && !muted
        && running.is_some()
        && (running == Some(false) || own_playing)
}

/// Debounce repeated slider/key input, replacing the previous feedback rather
/// than accumulating players. Unknown/missing PipeWire state is conservative.
pub fn volume(quiet: bool, changed: bool, muted: bool) {
    PENDING.with_borrow_mut(|pending| {
        if let Some(id) = pending.take() {
            id.remove();
        }
    });
    let enabled = SETTINGS.with(|s| s.as_ref().is_some_and(|s| s.boolean("event-sounds")));
    if !allowed(
        enabled,
        quiet,
        changed,
        muted,
        RUNNING.get(),
        OWN_PLAYING.get(),
    ) {
        return;
    }
    PENDING.with_borrow_mut(|pending| {
        *pending = Some(glib::timeout_add_local_once(
            std::time::Duration::from_millis(30),
            move || {
                PENDING.with_borrow_mut(|pending| pending.take());
                let Some(settings) = SETTINGS.with(Clone::clone) else {
                    return;
                };
                // Recheck live policy/playback after the debounce interval.
                if !allowed(
                    settings.boolean("event-sounds"),
                    quiet,
                    changed,
                    muted,
                    RUNNING.get(),
                    OWN_PLAYING.get(),
                ) {
                    return;
                }
                PLAYER.with_borrow_mut(|old| {
                    if let Some(old) = old.take() {
                        old.force_exit();
                    }
                });
                OWN_PLAYING.set(false);
                let theme = format!("canberra.xdg-theme.name={}", settings.string("theme-name"));
                let launcher = gio::SubprocessLauncher::new(
                    gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE,
                );
                let parent = std::process::id() as libc::pid_t;
                launcher.set_child_setup(move || unsafe {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                        || libc::getppid() != parent
                    {
                        libc::_exit(127);
                    }
                    // The deadline survives exec and is independent of GTK's loop.
                    libc::signal(libc::SIGALRM, libc::SIG_DFL);
                    libc::alarm(5);
                });
                let Ok(process) = launcher.spawn(&[
                    "canberra-gtk-play".as_ref(),
                    "--id".as_ref(),
                    "audio-volume-change".as_ref(),
                    "--description".as_ref(),
                    "Volume changed".as_ref(),
                    "--property".as_ref(),
                    theme.as_ref(),
                ]) else {
                    return;
                };
                PLAYER.with_borrow_mut(|old| *old = Some(process.clone()));
                let timer = Rc::new(RefCell::new(None));
                let timeout = process.clone();
                let timeout_timer = timer.clone();
                *timer.borrow_mut() = Some(glib::timeout_add_local_once(
                    std::time::Duration::from_secs(5),
                    move || {
                        timeout_timer.borrow_mut().take();
                        timeout.force_exit();
                    },
                ));
                let completed = process.clone();
                process.wait_async(gio::Cancellable::NONE, move |_| {
                    if let Some(id) = timer.borrow_mut().take() {
                        id.remove();
                    }
                    PLAYER.with_borrow_mut(|current| {
                        if current.as_ref() == Some(&completed) {
                            current.take();
                            OWN_PLAYING.set(false);
                        }
                    });
                });
            },
        ));
    });
}

/// One theme sound (Mutter's `MetaSoundPlayer`: canberra, enabled only
/// with `org.gnome.desktop.sound event-sounds`), spawned with the same
/// guards as volume feedback. Best effort: silence on failure.
pub fn play_event(id: &str, description: &str) {
    let Some(settings) = SETTINGS.with(Clone::clone) else {
        return;
    };
    if !settings.boolean("event-sounds") {
        return;
    }
    let theme = format!("canberra.xdg-theme.name={}", settings.string("theme-name"));
    let launcher = gio::SubprocessLauncher::new(
        gio::SubprocessFlags::STDOUT_SILENCE | gio::SubprocessFlags::STDERR_SILENCE,
    );
    let parent = std::process::id() as libc::pid_t;
    launcher.set_child_setup(move || unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
            || libc::getppid() != parent
        {
            libc::_exit(127);
        }
        // The deadline survives exec and is independent of GTK's loop.
        libc::signal(libc::SIGALRM, libc::SIG_DFL);
        libc::alarm(5);
    });
    let _ = launcher.spawn(&[
        "canberra-gtk-play".as_ref(),
        "--id".as_ref(),
        id.as_ref(),
        "--description".as_ref(),
        description.as_ref(),
        "--property".as_ref(),
        theme.as_ref(),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn feedback_requires_enabled_changed_audible_idle_sink() {
        assert!(allowed(true, false, true, false, Some(false), false));
        assert!(!allowed(false, false, true, false, Some(false), false));
        assert!(!allowed(true, true, true, false, Some(false), false));
        assert!(!allowed(true, false, false, false, Some(false), false));
        assert!(!allowed(true, false, true, true, Some(false), false));
        assert!(allowed(true, false, true, false, Some(true), true));
        assert!(!allowed(true, false, true, false, None, true));
        assert!(!allowed(true, true, true, false, Some(true), true));
        assert!(!allowed(true, false, true, false, Some(true), false));
        assert!(!allowed(true, false, true, false, None, false));
    }
}
