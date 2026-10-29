//! Animation time for shell animations that nested proofs sample at exact
//! delays: the shell's side of the compositor's proof clock
//! (`crates/compositor/src/animation_clock.rs`), driven by the same file.
//!
//! Sessions animate on the monotonic clock. An instrumented session
//! (`TUNA_COMPOSITOR_STATE` set to an absolute path) may name an absolute
//! file in `TUNA_ANIMATION_CLOCK`, with the compositor's rules:
//!
//! - while the file is absent, animation time is real time;
//! - when it first holds a whole number of milliseconds `N`, time freezes
//!   at the moment it was read plus `N`;
//! - rewriting it with another `M` moves time to that moment plus `M`;
//! - removing it resumes real time from where the clock stood.
//!
//! The file is read only when an animation asks for the time, so an idle
//! shell never touches it.
use std::cell::RefCell;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CLOCK_ENV: &str = "TUNA_ANIMATION_CLOCK";
const STATE_ENV: &str = "TUNA_COMPOSITOR_STATE";
/// A clock body is a few digits; anything longer is not a time.
const MAX_BYTES: u64 = 64;

#[derive(Debug)]
enum Mode {
    /// `offset` plus real time since `origin`.
    Real { origin: Instant, offset: Duration },
    /// Frozen at `now`; file values count from `base`.
    Manual { base: Duration, now: Duration },
}

#[derive(Debug)]
pub struct AnimationClock {
    mode: Mode,
    path: Option<PathBuf>,
    /// The file body last applied, so each write applies once.
    last: String,
}

impl AnimationClock {
    fn with_file(path: Option<PathBuf>) -> Self {
        Self {
            mode: Mode::Real {
                origin: Instant::now(),
                offset: Duration::ZERO,
            },
            path,
            last: String::new(),
        }
    }

    /// Real time, drivable through the clock file only in an
    /// instrumented session and only through an absolute path.
    fn from_env() -> Self {
        let instrumented =
            std::env::var_os(STATE_ENV).is_some_and(|state| Path::new(&state).is_absolute());
        Self::with_file(
            std::env::var_os(CLOCK_ENV)
                .map(PathBuf::from)
                .filter(|path| instrumented && path.is_absolute()),
        )
    }

    fn is_manual(&self) -> bool {
        matches!(self.mode, Mode::Manual { .. })
    }

    fn now(&self) -> Duration {
        match self.mode {
            Mode::Real { origin, offset } => offset + origin.elapsed(),
            Mode::Manual { now, .. } => now,
        }
    }

    fn set(&mut self, to: Duration) {
        let base = match self.mode {
            Mode::Manual { base, .. } => base,
            Mode::Real { .. } => self.now(),
        };
        self.mode = Mode::Manual {
            base,
            now: base + to,
        };
    }

    fn resume(&mut self) {
        if let Mode::Manual { now, .. } = self.mode {
            self.mode = Mode::Real {
                origin: Instant::now(),
                offset: now,
            };
        }
    }

    /// Follow the clock file once per change.
    fn poll(&mut self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let mut body = String::new();
        let read = std::fs::File::open(path)
            .and_then(|file| file.take(MAX_BYTES).read_to_string(&mut body));
        match read {
            Ok(_) => {
                if body == self.last {
                    return;
                }
                if let Ok(ms) = body.trim().parse::<u64>() {
                    self.set(Duration::from_millis(ms));
                }
                self.last = body;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.last.clear();
                self.resume();
            }
            Err(_) => {}
        }
    }
}

thread_local! {
    static CLOCK: RefCell<AnimationClock> = RefCell::new(AnimationClock::from_env());
}

/// Animation time, after following the clock file. Kept as a `Duration`
/// (whole nanoseconds): differences of it are exact, so a proof that
/// moves the frozen clock by 300 ms ends a 300 ms ease exactly, however
/// long the session has run.
pub fn now() -> Duration {
    CLOCK.with(|clock| {
        let mut clock = clock.borrow_mut();
        clock.poll();
        clock.now()
    })
}

/// Whether a proof holds animation time (as of the last [`now`]).
pub fn manual() -> bool {
    CLOCK.with(|clock| clock.borrow().is_manual())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_freezes_steps_and_releases_the_clock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clock");
        let mut clock = AnimationClock::with_file(Some(path.clone()));
        clock.poll();
        assert!(!clock.is_manual(), "no file: real time");
        std::fs::write(&path, "0\n").unwrap();
        clock.poll();
        assert!(clock.is_manual());
        let base = clock.now();
        std::thread::sleep(Duration::from_millis(5));
        clock.poll();
        assert_eq!(clock.now(), base, "frozen while the file is unchanged");
        std::fs::write(&path, "150").unwrap();
        clock.poll();
        assert_eq!(clock.now() - base, Duration::from_millis(150));
        std::fs::write(&path, "not a time").unwrap();
        clock.poll();
        assert_eq!(clock.now() - base, Duration::from_millis(150), "garbage");
        std::fs::write(&path, "300").unwrap();
        clock.poll();
        assert_eq!(clock.now() - base, Duration::from_millis(300));
        std::fs::remove_file(&path).unwrap();
        clock.poll();
        assert!(!clock.is_manual(), "removing the file resumes real time");
        assert!(
            clock.now() >= base + Duration::from_millis(300),
            "never backwards"
        );
    }

    #[test]
    fn without_a_file_time_is_real() {
        let mut clock = AnimationClock::with_file(None);
        let before = clock.now();
        std::thread::sleep(Duration::from_millis(2));
        clock.poll();
        assert!(!clock.is_manual());
        assert!(clock.now() > before);
    }
}
