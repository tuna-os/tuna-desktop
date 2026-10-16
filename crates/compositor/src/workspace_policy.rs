//! Output affinity for GNOME's primary-only workspace policy.

/// Logical output bounds; calculations widen before adding coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputBounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
}

/// Choose the output with greatest overlap, preferring the primary on ties.
/// A window outside the layout uses the primary, including during hotplug.
pub fn on_secondary(window: (i32, i32, i32, i32), outputs: &[OutputBounds]) -> bool {
    let primary = outputs
        .iter()
        .position(|output| output.primary)
        .unwrap_or(0);
    let (x, y, width, height) = window;
    if width <= 0 || height <= 0 {
        return false;
    }
    let mut best = (0_i64, primary);
    for (index, output) in outputs.iter().enumerate() {
        let width = (i64::from(x) + i64::from(width))
            .min(i64::from(output.x) + i64::from(output.width.max(0)))
            - i64::from(x).max(i64::from(output.x));
        let height = (i64::from(y) + i64::from(height))
            .min(i64::from(output.y) + i64::from(output.height.max(0)))
            - i64::from(y).max(i64::from(output.y));
        let area = width.max(0) * height.max(0);
        if area > best.0 || (area == best.0 && index == primary) {
            best = (area, index);
        }
    }
    best.0 > 0 && best.1 != primary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outputs() -> [OutputBounds; 2] {
        [
            OutputBounds {
                x: -100,
                y: 0,
                width: 100,
                height: 100,
                primary: false,
            },
            OutputBounds {
                x: 0,
                y: 0,
                width: 100,
                height: 100,
                primary: true,
            },
        ]
    }

    #[test]
    fn greatest_overlap_selects_secondary_and_primary_wins_ties() {
        assert!(on_secondary((-90, 10, 40, 30), &outputs()));
        assert!(!on_secondary((-20, 10, 40, 30), &outputs()));
        assert!(!on_secondary((10, 10, 40, 30), &outputs()));
    }

    #[test]
    fn primary_change_moves_workspace_affinity_without_moving_windows() {
        let mut layout = outputs();
        assert!(on_secondary((-90, 10, 40, 30), &layout));
        layout[0].primary = true;
        layout[1].primary = false;
        assert!(!on_secondary((-90, 10, 40, 30), &layout));
        assert!(on_secondary((10, 10, 40, 30), &layout));
    }

    #[test]
    fn missing_offscreen_and_invalid_geometry_fall_back_to_primary() {
        assert!(!on_secondary((0, 0, 1, 1), &[]));
        assert!(!on_secondary((200, 200, 1, 1), &outputs()));
        assert!(!on_secondary((-20, 10, 0, 30), &outputs()));
        assert!(!on_secondary((-90, 10, 40, 30), &outputs()[..1]));
    }

    #[test]
    fn coordinate_additions_do_not_overflow_i32() {
        let layout = [
            OutputBounds {
                x: i32::MAX - 8,
                y: i32::MIN,
                width: 16,
                height: 16,
                primary: false,
            },
            OutputBounds {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
                primary: true,
            },
        ];
        assert!(on_secondary((i32::MAX - 4, i32::MIN, 16, 16), &layout));
    }
}
