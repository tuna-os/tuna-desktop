//! Event sounds through libcanberra's player, as mutter's
//! `MetaSoundPlayer` plays them for GNOME Shell: serialized, bounded volume
//! feedback, and notification sounds. Playback state comes from the native
//! PipeWire event stream, so changing volume does not interrupt existing audio.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gio::prelude::*;
use gtk4::{gio, glib};
use tuna_shell_host::notifications::Sound;

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
                let Some(process) = spawn_player(
                    &settings,
                    &[
                        "--id",
                        "audio-volume-change",
                        "--description",
                        "Volume changed",
                    ],
                ) else {
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
    let _ = spawn_player(&settings, &["--id", id, "--description", description]);
}

/// One `canberra-gtk-play` run in `org.gnome.desktop.sound`'s
/// `theme-name`, killed with the shell and after five seconds at most.
fn spawn_player(settings: &gio::Settings, args: &[&str]) -> Option<gio::Subprocess> {
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
    let mut argv: Vec<&std::ffi::OsStr> = vec![std::ffi::OsStr::new("canberra-gtk-play")];
    argv.extend(args.iter().map(std::ffi::OsStr::new));
    argv.extend([
        std::ffi::OsStr::new("--property"),
        std::ffi::OsStr::new(&theme),
    ]);
    launcher.spawn(&argv).ok()
}

/// The player arguments for a notification's sound (messageTray.js 51
/// `Sound.play`): its theme name if it has one (`play_from_theme`),
/// else its file (`play_from_file`), described as "Notification sound".
fn notification_args(sound: &Sound) -> Option<[&str; 4]> {
    let (kind, what) = if !sound.name.is_empty() {
        ("--id", sound.name.as_str())
    } else if !sound.file.is_empty() {
        ("--file", sound.file.as_str())
    } else {
        return None;
    };
    Some([kind, what, "--description", "Notification sound"])
}

/// Play a notification's sound as its banner shows. Like
/// `MetaSoundPlayer`, `event-sounds` off plays nothing; sounds overlap
/// rather than replace one another or the volume feedback.
pub fn notification(sound: &Sound) {
    let Some(args) = notification_args(sound) else {
        return;
    };
    let Some(settings) = SETTINGS.with(Clone::clone) else {
        return;
    };
    if settings.boolean("event-sounds") {
        // The child reaps itself through GSubprocess's own child watch.
        let _ = spawn_player(&settings, &args);
    }
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

    #[test]
    fn notification_sounds_prefer_the_theme_name_like_gnome() {
        let sound = |name: &str, file: &str| Sound {
            name: name.into(),
            file: file.into(),
        };
        assert_eq!(
            notification_args(&sound("message-new-instant", "/tmp/a.oga")),
            Some([
                "--id",
                "message-new-instant",
                "--description",
                "Notification sound"
            ])
        );
        assert_eq!(
            notification_args(&sound("", "/tmp/a.oga")),
            Some([
                "--file",
                "/tmp/a.oga",
                "--description",
                "Notification sound"
            ])
        );
        assert_eq!(notification_args(&Sound::default()), None);
    }
}
