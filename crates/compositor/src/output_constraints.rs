//! Preserve fully visible, fitting floating windows across logical workarea changes.
//! Mutter 51 constraints.c retains require_fully_onscreen for non-user changes;
//! intentional offscreen placement and client minimum-size conflicts are separate.

pub(crate) type Rect = (i32, i32, i32, i32);

fn contains(area: Rect, window: Rect) -> bool {
    let (ax, ay, aw, ah) = area;
    let (x, y, w, h) = window;
    aw > 0
        && ah > 0
        && w > 0
        && h > 0
        && x >= ax
        && y >= ay
        && i64::from(x) + i64::from(w) <= i64::from(ax) + i64::from(aw)
        && i64::from(y) + i64::from(h) <= i64::from(ay) + i64::from(ah)
}

/// Move only when the old rectangle was wholly visible on the same named
/// output. Never resize a client, migrate a removed output, or undo a user move.
pub(crate) fn reposition(window: Rect, old: Rect, new: Rect) -> Option<Rect> {
    if old == new || !contains(old, window) || window.2 > new.2 || window.3 > new.3 {
        return None;
    }
    let max_x = i64::from(new.0) + i64::from(new.2) - i64::from(window.2);
    let max_y = i64::from(new.1) + i64::from(new.3) - i64::from(window.3);
    if max_x < i64::from(new.0) || max_y < i64::from(new.1) {
        return None;
    }
    let x = i64::from(window.0).clamp(i64::from(new.0), max_x);
    let y = i64::from(window.1).clamp(i64::from(new.1), max_y);
    let result = (
        i32::try_from(x).ok()?,
        i32::try_from(y).ok()?,
        window.2,
        window.3,
    );
    (result != window).then_some(result)
}

/// Output identity is stable across scale changes; output removal has its own
/// migration path and must not accidentally adopt another output's workarea.
pub(crate) fn reposition_on_output(
    window: Rect,
    old: &[(String, Rect)],
    new: &[(String, Rect)],
) -> Option<Rect> {
    old.iter().find_map(|(name, previous)| {
        let (_, current) = new.iter().find(|(candidate, _)| candidate == name)?;
        reposition(window, *previous, *current)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_output_change_does_not_move_secondary_or_removed_output_windows() {
        let old = vec![
            ("primary".to_owned(), (0, 32, 1280, 768)),
            ("secondary".to_owned(), (1280, 32, 1280, 768)),
        ];
        let new = vec![
            ("primary".to_owned(), (0, 32, 1024, 608)),
            ("secondary".to_owned(), (1280, 32, 1280, 768)),
        ];
        assert_eq!(
            reposition_on_output((420, 306, 640, 420), &old, &new),
            Some((384, 220, 640, 420))
        );
        assert_eq!(
            reposition_on_output((1600, 306, 640, 420), &old, &new),
            None
        );
        assert_eq!(
            reposition_on_output((1600, 306, 640, 420), &old, &new[..1]),
            None
        );
    }
    #[test]
    fn fractional_scale_keeps_previously_visible_gamma_inside_workarea() {
        assert_eq!(
            reposition((420, 306, 640, 420), (0, 32, 1280, 768), (0, 32, 1024, 608)),
            Some((384, 220, 640, 420))
        );
    }
    #[test]
    fn independent_windows_and_unchanged_outputs_keep_their_geometry() {
        assert_eq!(
            reposition((100, 100, 640, 420), (0, 32, 1280, 768), (0, 32, 1024, 608)),
            None
        );
        assert_eq!(
            reposition(
                (1400, 100, 640, 420),
                (1280, 32, 1280, 768),
                (1280, 32, 1280, 768)
            ),
            None
        );
    }
    #[test]
    fn intentional_offscreen_and_nonfitting_clients_are_not_forced_or_resized() {
        assert_eq!(
            reposition((700, 500, 640, 420), (0, 32, 1280, 768), (0, 32, 1024, 608)),
            None
        );
        assert_eq!(
            reposition(
                (100, 100, 1100, 700),
                (0, 32, 1280, 768),
                (0, 32, 1024, 608)
            ),
            None
        );
    }
    #[test]
    fn moved_negative_origin_output_and_invalid_workareas_are_bounded() {
        assert_eq!(
            reposition(
                (-900, 100, 640, 420),
                (-1280, 32, 1280, 768),
                (-1024, 32, 1024, 608)
            ),
            None
        );
        assert_eq!(
            reposition((100, 100, 640, 420), (0, 32, 1280, 768), (0, 32, 0, 0)),
            None
        );
        assert_eq!(
            reposition(
                (100, 100, 640, 420),
                (0, 32, 1280, 768),
                (2000, 32, 1024, 608)
            ),
            Some((2000, 100, 640, 420))
        );
    }
}
