//! GNOME 51 Flashspot: white immediately, fading out for 500 ms with out-quad.
//! Feedback is separate from capture rendering, so it cannot enter the saved PNG.

use std::time::Duration;

pub const MAX_FLASHES: usize = 32;

struct Entry {
    area: [i32; 4],
    started: Duration,
    duration: Duration,
}

#[derive(Default)]
pub struct Flashes {
    entries: Vec<Entry>,
}

impl Flashes {
    pub fn trigger(&mut self, area: [i32; 4], now: Duration, duration: Duration) {
        self.expire(now);
        if area[2] <= 0 || area[3] <= 0 || duration.is_zero() {
            return;
        }
        // Keep transient feedback bounded even under repeated admitted requests.
        if self.entries.len() == MAX_FLASHES {
            self.entries.remove(0);
        }
        self.entries.push(Entry {
            area,
            started: now,
            duration,
        });
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn expire(&mut self, now: Duration) {
        self.entries
            .retain(|e| now.saturating_sub(e.started) < e.duration);
    }

    pub fn samples(&self, now: Duration) -> Vec<([i32; 4], f32)> {
        self.entries
            .iter()
            .filter_map(|e| {
                let elapsed = now.saturating_sub(e.started);
                if elapsed >= e.duration {
                    return None;
                }
                let remaining = 1.0 - elapsed.as_secs_f64() / e.duration.as_secs_f64();
                Some((e.area, (remaining * remaining) as f32))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_then_quarter_opacity_at_half_time_and_absent_at_end() {
        let mut flash = Flashes::default();
        flash.trigger([10, 20, 30, 40], Duration::ZERO, Duration::from_millis(500));
        assert_eq!(flash.samples(Duration::ZERO), vec![([10, 20, 30, 40], 1.0)]);
        assert_eq!(
            flash.samples(Duration::from_millis(250)),
            vec![([10, 20, 30, 40], 0.25)]
        );
        assert!(flash.samples(Duration::from_millis(500)).is_empty());
        flash.expire(Duration::from_millis(500));
        assert!(flash.entries.is_empty());
    }

    #[test]
    fn disabled_motion_and_invalid_areas_create_no_feedback() {
        let mut flash = Flashes::default();
        flash.trigger([0, 0, 10, 10], Duration::ZERO, Duration::ZERO);
        flash.trigger([0, 0, 0, 10], Duration::ZERO, Duration::from_millis(500));
        assert!(flash.samples(Duration::ZERO).is_empty());
    }

    #[test]
    fn concurrent_feedback_is_bounded_and_lock_clear_is_immediate() {
        let mut flash = Flashes::default();
        for i in 0..100 {
            flash.trigger([i, 0, 1, 1], Duration::ZERO, Duration::from_millis(500));
        }
        assert_eq!(flash.samples(Duration::ZERO).len(), MAX_FLASHES);
        flash.clear();
        assert!(flash.samples(Duration::ZERO).is_empty());
    }
}
