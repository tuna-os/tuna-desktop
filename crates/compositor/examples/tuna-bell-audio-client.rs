//! Scoped audio proof: production native bell consumer against a private null
//! sink. Protocol delivery/visual presentation are separate qualifications.
#[path = "../src/bell.rs"]
mod bell;

// The example crate root stands in for the compositor crate root, so
// `bell.rs`' `crate::idle_schedule::ACTIVE_INTERVAL` resolves here.
// Value mirrors `src/idle_schedule.rs` (read-only: that module is
// owned elsewhere and must not be changed for this example).
mod idle_schedule {
    pub const ACTIVE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);
}

fn main() {
    let mode = std::env::args().nth(1).expect("mode required");
    assert!(["audible", "silent", "event-disabled"].contains(&mode.as_str()));
    let mut bell = bell::Bell::default();
    bell.set_policy(tuna_shell_control::BellPolicy {
        audible: mode != "silent",
        event_sounds: mode != "event-disabled",
        ..Default::default()
    });
    bell.ring(None);
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(2) {
        bell.tick(std::time::Instant::now());
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(bell.rings, 1);
    assert!(bell.visual(std::time::Instant::now()).is_none());
    bell.clear_visual();
    // Scheduler introspection stays linked: the audio proof never
    // schedules, but the example must keep compiling against the full
    // `Bell` surface (and `ACTIVE_INTERVAL` through it) so a signature
    // drift breaks loudly instead of rotting silently.
    let _player = bell.player_pid();
    let _deadline = bell.player_deadline();
}
