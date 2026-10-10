//! Scoped audio proof: production native bell consumer against a private null
//! sink. Protocol delivery/visual presentation are separate qualifications.
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
}
