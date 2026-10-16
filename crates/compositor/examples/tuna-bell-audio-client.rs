//! Scoped audio proof: production native bell consumer against a private null
//! sink. Protocol delivery/visual presentation are separate qualifications.
#![allow(dead_code)]
#[path = "../src/bell.rs"]
mod bell;

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
    // The loop's deadline stays linked: the audio proof never schedules,
    // but the example must keep compiling against the full `Bell`
    // surface so a signature drift breaks loudly instead of rotting.
    let _deadline = bell.player_deadline();
}
