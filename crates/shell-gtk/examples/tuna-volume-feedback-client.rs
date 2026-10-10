//! Scoped audio proof driver: executes the production media-key and feedback
//! modules against a private real PipeWire graph. It does not qualify GTK key
//! delivery, physical devices, or notification/input-click sound consumers.
#![allow(dead_code)]
#[path = "../src/audio_state.rs"]
mod audio_state;
#[path = "../src/logic.rs"]
mod logic;
#[path = "../src/media_keys.rs"]
mod media_keys;
#[path = "../src/sound_feedback.rs"]
mod sound_feedback;

use gio::prelude::*;
use gtk4::{gio, glib};
use std::rc::Rc;

fn main() {
    let mode = std::env::args().nth(1).expect("mode required");
    assert!(
        ["up", "quiet", "muted", "playing", "event-disabled", "clamp"].contains(&mode.as_str())
    );
    assert_eq!(std::env::var("GSETTINGS_BACKEND").as_deref(), Ok("memory"));
    let settings = media_keys::sound_settings().expect("GNOME sound schema required");
    settings
        .set_boolean("event-sounds", mode != "event-disabled")
        .unwrap();
    settings
        .set_boolean("allow-volume-above-100-percent", mode == "clamp")
        .unwrap();
    let process =
        gio::Subprocess::newv(&["pw-dump".as_ref()], gio::SubprocessFlags::STDOUT_PIPE).unwrap();
    let loop_ = glib::MainLoop::new(None, false);
    let quit = loop_.clone();
    glib::timeout_add_local_once(std::time::Duration::from_secs(6), move || quit.quit());
    let timeout = process.clone();
    glib::timeout_add_local_once(std::time::Duration::from_secs(5), move || {
        timeout.force_exit()
    });
    process.communicate_utf8_async(None, gio::Cancellable::NONE, move |result| {
        let (out, _) = result.expect("pw-dump must finish");
        let mut state = audio_state::AudioState::default();
        state.feed(out.expect("pw-dump output").as_bytes()).unwrap();
        let snapshot = state.snapshot();
        assert!(snapshot.sink.is_some(), "private default sink required");
        assert_eq!(
            snapshot.sink_running,
            Some(mode == "playing"),
            "fixture must report actual expected playback state"
        );
        sound_feedback::set_sink_running(snapshot.sink_running);
        println!("backend={snapshot:?}");
        let keys = media_keys::MediaKeys::new(Rc::new(|percent, muted, microphone| {
            println!("osd percent={percent} muted={muted} microphone={microphone}");
        }));
        glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
            if mode == "clamp" {
                settings
                    .set_boolean("allow-volume-above-100-percent", false)
                    .unwrap();
                println!("amplification disabled live");
            } else {
                keys.change(
                    if mode == "muted" {
                        media_keys::VolumeKey::Down
                    } else {
                        media_keys::VolumeKey::Up
                    },
                    6.0,
                    false,
                    mode == "quiet",
                );
            }
        });
    });
    loop_.run();
}
