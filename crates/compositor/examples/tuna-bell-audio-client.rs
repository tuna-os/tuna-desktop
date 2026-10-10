//! Scoped audio proof: production native bell consumer against a private null
//! sink. Protocol delivery/visual presentation are separate qualifications.
#[path = "../src/bell.rs"]
mod bell;
// Bell deadlines use the same scheduler constant as production. The
// remaining scheduler methods are unused by this standalone audio fixture.
#[allow(dead_code)]
#[path = "../src/idle_schedule.rs"]
mod idle_schedule;

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
    if mode != "audible" {
        assert!(bell.player_pid().is_none());
        assert!(bell.player_deadline().is_none());
    }
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(2) {
        bell.tick(std::time::Instant::now());
        assert_eq!(bell.player_pid().is_some(), bell.player_deadline().is_some());
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(bell.rings, 1);
    assert!(bell.visual(std::time::Instant::now()).is_none());
    bell.clear_visual();
}
