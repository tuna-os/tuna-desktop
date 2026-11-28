//! GNOME 51's per-monitor screen selectors (screenshot.js
//! `_rebuildMonitorBins`): Screen mode lays one toggle button over each
//! monitor, exactly one checked. The primary starts checked when the
//! monitors are (re)built and stays checked across openings until one is
//! picked; arrows move the check the way `_moveFocus` does,
//! `st_widget_navigate_focus` without wrap-around; Capture takes the
//! checked monitor alone.

use gtk4::glib;

use crate::screenshot_selection::{Direction, Rect};

/// One output as the compositor's `ScreenshotOutputs` answers it.
#[derive(Debug, Clone, PartialEq)]
pub struct ShotOutput {
    /// The connector (`wl_output` name), which GDK reports as the
    /// monitor's connector.
    pub name: String,
    /// Global logical rectangle.
    pub rect: Rect,
    pub scale: f64,
    pub primary: bool,
    /// The output's picture at its own scale.
    pub path: String,
}

/// Unpack a `ScreenshotOutputs` reply, `(a(siiiidbs))`.
pub fn parse_outputs(reply: &glib::Variant) -> Vec<ShotOutput> {
    let Some(list) = reply.try_child_value(0) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|o| o.get::<(String, i32, i32, i32, i32, f64, bool, String)>())
        .filter(|(_, _, _, w, h, _, _, _)| *w > 0 && *h > 0)
        .map(|(name, x, y, w, h, scale, primary, path)| ShotOutput {
            name,
            rect: Rect {
                x: f64::from(x),
                y: f64::from(y),
                w: f64::from(w),
                h: f64::from(h),
            },
            scale,
            primary,
            path,
        })
        .collect()
}

/// The monitors as the selectors were built for them: GNOME rebuilds
/// them (and checks the primary again) only on `monitors-changed`.
pub fn layout(outputs: &[ShotOutput]) -> Vec<(String, Rect)> {
    outputs.iter().map(|o| (o.name.clone(), o.rect)).collect()
}

/// The screen selector checked when the UI opens on `outputs`: the one
/// checked last time (`previous`, a name) while the monitors are those
/// it was built for (`built`), else the primary, as `_rebuildMonitorBins`
/// checks `Main.layoutManager.primaryIndex`. Without a primary, the
/// first (GNOME would check none and fail to capture).
pub fn selected_on_open(
    built: &[(String, Rect)],
    previous: Option<&str>,
    outputs: &[ShotOutput],
) -> Option<usize> {
    if outputs.is_empty() {
        return None;
    }
    if built == layout(outputs).as_slice() {
        if let Some(kept) = previous.and_then(|name| outputs.iter().position(|o| o.name == name)) {
            return Some(kept);
        }
    }
    Some(outputs.iter().position(|o| o.primary).unwrap_or(0))
}

/// `get_midpoint` in st-widget.c: integer midpoint of a box.
fn midpoint(r: &Rect) -> (i64, i64) {
    (
        ((r.x + r.x + r.w) / 2.0) as i64,
        ((r.y + r.y + r.h) / 2.0) as i64,
    )
}

/// Where an arrow moves the checked screen selector (screenshot.js
/// `_moveFocus` calling `navigate_focus(screen, direction, false)`):
/// among the other monitors wholly past `from`'s edge in `direction`
/// (0.1px slack, `filter_by_position`), the one whose midpoint is
/// nearest `from`'s (`sort_by_distance`); on a tie the later monitor, as
/// St walks the bins in reverse stacking order. `None` (the check stays)
/// when none lies that way: focus does not wrap around.
pub fn neighbour(rects: &[Rect], from: usize, direction: Direction) -> Option<usize> {
    let r = rects.get(from)?;
    let (fx, fy) = midpoint(r);
    rects
        .iter()
        .enumerate()
        .filter(|&(i, c)| {
            i != from
                && match direction {
                    Direction::Up => c.y + c.h <= r.y + 0.1,
                    Direction::Down => c.y >= r.y + r.h - 0.1,
                    Direction::Left => c.x + c.w <= r.x + 0.1,
                    Direction::Right => c.x >= r.x + r.w - 0.1,
                }
        })
        .min_by_key(|&(i, c)| {
            let (cx, cy) = midpoint(c);
            ((cx - fx).pow(2) + (cy - fy).pow(2), std::cmp::Reverse(i))
        })
        .map(|(i, _)| i)
}

/// The global logical area a recording from the UI takes (screenshot.js
/// `_startScreencast`): the area `selection`, drawn on the primary
/// monitor's overlay, or else the checked monitor.
pub fn recording_area(
    selection: Option<Rect>,
    primary: Rect,
    selected: Rect,
) -> (i32, i32, i32, i32) {
    let area = match selection {
        Some(sel) => Rect {
            x: primary.x + sel.x,
            y: primary.y + sel.y,
            ..sel
        },
        None => selected,
    };
    (
        area.x.round() as i32,
        area.y.round() as i32,
        area.w.round() as i32,
        area.h.round() as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk4::prelude::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    fn output(name: &str, r: Rect, primary: bool) -> ShotOutput {
        ShotOutput {
            name: name.into(),
            rect: r,
            scale: 1.0,
            primary,
            path: format!("/run/{name}.png"),
        }
    }

    #[test]
    fn the_compositors_outputs_parse_with_scale_and_primary() {
        let reply = (vec![
            (
                "Virtual-1".to_owned(),
                0i32,
                0i32,
                1024i32,
                768i32,
                1.0f64,
                true,
                "/run/a.png".to_owned(),
            ),
            (
                "Virtual-2".to_owned(),
                1024,
                0,
                800,
                600,
                2.0,
                false,
                "/run/b.png".to_owned(),
            ),
            // A zero-sized output has nothing to select.
            (
                "Virtual-3".to_owned(),
                0,
                0,
                0,
                600,
                1.0,
                false,
                "/run/c.png".to_owned(),
            ),
        ],)
            .to_variant();
        assert_eq!(reply.type_().as_str(), "(a(siiiidbs))");
        let parsed = parse_outputs(&reply);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].name, "Virtual-2");
        assert_eq!(parsed[1].rect, rect(1024.0, 0.0, 800.0, 600.0));
        assert_eq!(parsed[1].scale, 2.0);
        assert!(parsed[0].primary && !parsed[1].primary);
        assert!(parse_outputs(&().to_variant()).is_empty());
    }

    #[test]
    fn the_primary_starts_checked_and_the_check_survives_until_monitors_change() {
        let a = output("A", rect(0.0, 0.0, 1024.0, 768.0), false);
        let b = output("B", rect(1024.0, 0.0, 800.0, 600.0), true);
        let outputs = vec![a.clone(), b.clone()];
        // First opening: the primary, wherever it is in the list (not
        // the pointer's monitor nor the first).
        assert_eq!(selected_on_open(&[], None, &outputs), Some(1));
        // Reopened on the same monitors: the last check stays.
        let built = layout(&outputs);
        assert_eq!(selected_on_open(&built, Some("A"), &outputs), Some(0));
        // Monitors changed (B moved): rebuilt, so the primary again.
        let moved = vec![a.clone(), output("B", rect(0.0, 768.0, 800.0, 600.0), true)];
        assert_eq!(selected_on_open(&built, Some("A"), &moved), Some(1));
        // The checked monitor went away: the primary.
        assert_eq!(selected_on_open(&built, Some("C"), &outputs), Some(1));
        // No primary: the first.
        let none = vec![output("A", a.rect, false), output("B", b.rect, false)];
        assert_eq!(selected_on_open(&[], None, &none), Some(0));
        assert_eq!(selected_on_open(&[], None, &[]), None);
    }

    #[test]
    fn arrows_move_to_the_nearest_monitor_that_way_without_wrapping() {
        use Direction::*;
        // Two side by side, B smaller and top-aligned (mixed scales).
        let pair = [
            rect(0.0, 0.0, 1024.0, 768.0),
            rect(1024.0, 0.0, 800.0, 600.0),
        ];
        assert_eq!(neighbour(&pair, 0, Right), Some(1));
        assert_eq!(neighbour(&pair, 1, Left), Some(0));
        // No wrap-around, and nothing above or below.
        assert_eq!(neighbour(&pair, 1, Right), None);
        assert_eq!(neighbour(&pair, 0, Left), None);
        assert_eq!(neighbour(&pair, 0, Up), None);
        assert_eq!(neighbour(&pair, 0, Down), None);
        assert_eq!(neighbour(&pair, 5, Right), None);
        // An L: C below A. Right from C reaches B (wholly right of C).
        let ell = [
            rect(0.0, 0.0, 1000.0, 800.0),
            rect(1000.0, 0.0, 1000.0, 800.0),
            rect(0.0, 800.0, 1000.0, 800.0),
        ];
        assert_eq!(neighbour(&ell, 0, Down), Some(2));
        assert_eq!(neighbour(&ell, 2, Up), Some(0));
        assert_eq!(neighbour(&ell, 2, Right), Some(1));
        // St filters by direction alone, not overlap: Down from B
        // reaches C, diagonally below it; nothing is above B.
        assert_eq!(neighbour(&ell, 1, Down), Some(2));
        assert_eq!(neighbour(&ell, 1, Up), None);
        // Overlapping monitors (a partial mirror) are in no direction.
        let overlap = [
            rect(0.0, 0.0, 1000.0, 800.0),
            rect(500.0, 0.0, 1000.0, 800.0),
        ];
        assert_eq!(neighbour(&overlap, 0, Right), None);
        // Monitors sharing an edge within St's 0.1px slack still count.
        let slack = [
            rect(0.0, 0.0, 1000.0, 800.0),
            rect(999.95, 0.0, 1000.0, 800.0),
        ];
        assert_eq!(neighbour(&slack, 0, Right), Some(1));
    }

    #[test]
    fn the_nearest_midpoint_wins_and_ties_go_to_the_later_monitor() {
        use Direction::*;
        // Two stacked on the right of A: the one level with A's middle
        // is nearer than the one below it.
        let near = [
            rect(0.0, 0.0, 1000.0, 1000.0),
            rect(1000.0, 800.0, 1000.0, 1000.0),
            rect(1000.0, 0.0, 1000.0, 1000.0),
        ];
        assert_eq!(neighbour(&near, 0, Right), Some(2));
        // Equidistant above and below the middle: the later one, as
        // `filter_by_position` reverses the bins before a stable sort.
        let tie = [
            rect(0.0, 500.0, 1000.0, 1000.0),
            rect(1000.0, 0.0, 1000.0, 1000.0),
            rect(1000.0, 1000.0, 1000.0, 1000.0),
        ];
        assert_eq!(neighbour(&tie, 0, Right), Some(2));
    }

    #[test]
    fn recordings_take_the_checked_monitor_or_the_area_in_global_pixels() {
        let primary = rect(1280.0, 0.0, 1280.0, 800.0);
        let other = rect(0.0, 0.0, 1280.0, 800.0);
        // Screen mode: the checked monitor, not the primary.
        assert_eq!(recording_area(None, primary, other), (0, 0, 1280, 800));
        assert_eq!(recording_area(None, primary, primary), (1280, 0, 1280, 800));
        // Selection mode: the area, drawn on the primary's overlay.
        let sel = rect(480.0, 300.0, 320.0, 200.0);
        assert_eq!(
            recording_area(Some(sel), primary, other),
            (1760, 300, 320, 200)
        );
    }
}
