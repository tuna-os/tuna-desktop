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

/// Mutter 51's `constrain_fully_onscreen` for a window an output change
/// moved from `before` to `moved`: one wholly inside the old workarea `old`
/// that fits the new workarea `new` ends wholly inside `new`. `None` when it
/// was not wholly visible, does not fit, or already sits inside. Never
/// resizes a client.
pub(crate) fn keep_fully_visible(before: Rect, old: Rect, moved: Rect, new: Rect) -> Option<Rect> {
    if !contains(old, before) || moved.2 > new.2 || moved.3 > new.3 {
        return None;
    }
    let max_x = i64::from(new.0) + i64::from(new.2) - i64::from(moved.2);
    let max_y = i64::from(new.1) + i64::from(new.3) - i64::from(moved.3);
    if max_x < i64::from(new.0) || max_y < i64::from(new.1) {
        return None;
    }
    let x = i64::from(moved.0).clamp(i64::from(new.0), max_x);
    let y = i64::from(moved.1).clamp(i64::from(new.1), max_y);
    let result = (
        i32::try_from(x).ok()?,
        i32::try_from(y).ok()?,
        moved.2,
        moved.3,
    );
    (result != moved).then_some(result)
}

/// Move only when the old rectangle was wholly visible on the same named
/// output. Never resize a client, migrate a removed output, or undo a user move.
pub(crate) fn reposition(window: Rect, old: Rect, new: Rect) -> Option<Rect> {
    if old == new {
        return None;
    }
    keep_fully_visible(window, old, window, new)
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
    #[test]
    fn rehomed_window_stays_below_the_panel_only_when_it_was_wholly_visible() {
        // Re-homed from (0,40) on a 1024x768 output to 768x576 scaled: the
        // relative place lands at y=22, under the panel strip, so the
        // workarea constraint takes it back to the strip's edge.
        assert_eq!(
            keep_fully_visible(
                (0, 40, 480, 320),
                (0, 32, 1024, 736),
                (0, 22, 480, 320),
                (0, 32, 768, 544)
            ),
            Some((0, 32, 480, 320))
        );
        // Already wholly inside: nothing to do.
        assert_eq!(
            keep_fully_visible(
                (1296, 240, 480, 320),
                (1024, 32, 1024, 736),
                (2192, 137, 480, 320),
                (2048, 32, 768, 544)
            ),
            None
        );
        // Partly under the panel before the change: left where it landed.
        assert_eq!(
            keep_fully_visible(
                (0, 10, 480, 320),
                (0, 32, 1024, 736),
                (0, 5, 480, 320),
                (0, 32, 768, 544)
            ),
            None
        );
        // Larger than the new workarea: never resized or forced.
        assert_eq!(
            keep_fully_visible(
                (0, 40, 900, 320),
                (0, 32, 1024, 736),
                (-66, 22, 900, 320),
                (0, 32, 768, 544)
            ),
            None
        );
    }
}
