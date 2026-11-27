//! Mutter's window-move edge resistance and snapping, as pure geometry.
//!
//! Mutter 51.1 is the reference (`src/compositor/edge-resistance.c`,
//! `src/core/boxes.c`, `src/core/workspace.c`):
//! - Edge construction, cached once per drag by [`EdgeData::new`]:
//!   - screen edges: `meta_rectangle_find_onscreen_edges` (boxes.c:1589)
//!     over the display rect with the struts, via
//!     `get_disjoint_strut_rect_list_in_region` (boxes.c:1119),
//!     `get_rect_minus_overlap` (boxes.c:1033), `add_edges` (boxes.c:1361),
//!     `fix_up_edges` (boxes.c:1468), `split_edge` (boxes.c:1408),
//!     `edges_overlap` (boxes.c:1257) and `rectangle_and_edge_intersection`
//!     (boxes.c:1277), as `meta_workspace_ensure_work_areas_validated`
//!     STEP 5 builds them (workspace.c:1027);
//!   - monitor edges: `meta_rectangle_find_nonintersected_monitor_edges`
//!     (boxes.c:1675), cut by the struts with
//!     `meta_rectangle_remove_intersections_with_boxes_from_edges`
//!     (boxes.c:1521);
//!   - window edges: `compute_resistance_and_snapping_edges` steps 1-3
//!     (edge-resistance.c:891): every window's frame clipped to the
//!     display rect, its sides with the opposite side type, minus the
//!     parts covered by windows above it;
//!   - `cache_edges` (edge-resistance.c:727): every vertical edge in one
//!     array and every horizontal edge in another, stably sorted by
//!     `meta_rectangle_edge_cmp_ignore_type` (boxes.c:1195). Mutter keeps
//!     the same array twice (left/right, top/bottom); one copy suffices.
//! - The move itself, [`EdgeData::resist_move`]:
//!   `meta_window_drag_edge_resistance_for_move` (edge-resistance.c:1100),
//!   `apply_edge_resistance_to_each_side` (edge-resistance.c:473),
//!   `apply_edge_resistance` (edge-resistance.c:315),
//!   `apply_edge_snapping` (edge-resistance.c:431),
//!   `find_index_of_edge_near_position` (edge-resistance.c:71),
//!   `find_nearest_position` (edge-resistance.c:180),
//!   `points_on_same_side` (edge-resistance.c:174),
//!   `movement_towards_edge` (edge-resistance.c:298) and
//!   `meta_rectangle_edge_aligns` (boxes.c:1007).
//!
//! Not ported: the snapping of windows tiled side by side (Tuna only moves
//! floating windows) and `meta_window_drag_edge_resistance_for_resize`.
//!
//! Deviations from mutter:
//! - The display rect is the bounding box of the outputs; mutter's is
//!   `(0, 0, display width, display height)`, the same rect whenever the
//!   layout starts at the origin, as mutter's always does.
//! - A window wholly outside the display rect contributes no edges; mutter
//!   adds four empty edges at an uninitialised position for it.
//! - No outputs means no edges and every move passes unchanged; mutter
//!   indexes into the empty array.
//! - `find_index_of_edge_near_position` is a `partition_point`, which
//!   returns exactly what mutter's binary search plus linear walks return
//!   (the first edge at or after the position, else the length; the last
//!   at or before it, else -1). `find_nearest_position` keeps mutter's
//!   binary search verbatim, since where it stops decides which edges it
//!   compares (it can miss a nearer edge just past the stop).
//! - Edge lists are built in place rather than by `GList` prepends, so
//!   edges with the same coordinate *and* the same start may sit in another
//!   order than mutter's after the stable sort. Resistance visits all of
//!   them alike; snapping could only probe a different one of two
//!   coincident edges first.
//! - Arithmetic saturates or widens where mutter's `int` could overflow.

use std::cmp::Ordering;

use smithay::utils::{Logical, Point, Rectangle};

/// `PIXEL_DISTANCE_THRESHOLD_TOWARDS_WINDOW`.
const TOWARDS_WINDOW: u32 = 16;
/// `PIXEL_DISTANCE_THRESHOLD_AWAYFROM_WINDOW`.
const AWAYFROM_WINDOW: u32 = 0;
/// `PIXEL_DISTANCE_THRESHOLD_TOWARDS_MONITOR`.
const TOWARDS_MONITOR: u32 = 32;
/// `PIXEL_DISTANCE_THRESHOLD_AWAYFROM_MONITOR`.
const AWAYFROM_MONITOR: u32 = 0;
/// `PIXEL_DISTANCE_THRESHOLD_TOWARDS_SCREEN`.
const TOWARDS_SCREEN: u32 = 32;
/// `PIXEL_DISTANCE_THRESHOLD_AWAYFROM_SCREEN`.
const AWAYFROM_SCREEN: u32 = 0;
/// A pointer snap-move shorter than this keeps the old position unless
/// the snap target is this close too (`apply_edge_snapping`).
const ACCIDENTAL_SNAP_MOVE: u32 = 8;

/// `MetaEdgeResistanceFlags`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    /// `META_EDGE_RESISTANCE_SNAP`: snap to the nearest edge (Shift held)
    /// instead of resisting.
    pub snap: bool,
    /// `META_EDGE_RESISTANCE_WINDOWS`: other windows' edges resist pointer
    /// moves. Keyboard moves count them regardless, as in mutter (whose
    /// keyboard moves always set this flag).
    pub windows: bool,
    /// `META_EDGE_RESISTANCE_KEYBOARD_OP`: a keyboard move, which stops at
    /// the first edge it crosses.
    pub keyboard: bool,
}

/// `MetaEdgeResistanceData`: the edges a drag resists or snaps to,
/// computed once when the drag starts.
#[derive(Debug, Clone, Default)]
pub struct EdgeData {
    /// Left and right edges, sorted by x then y.
    vertical: Vec<Edge>,
    /// Top and bottom edges, sorted by y then x.
    horizontal: Vec<Edge>,
}

impl EdgeData {
    /// `compute_resistance_and_snapping_edges` + `cache_edges`.
    ///
    /// `outputs` are the logical output rectangles (monitors); the display
    /// rect is their bounding box. `struts` are the strut rectangles (the
    /// top panel strip of each output). `windows` are the frame rects of
    /// the windows whose edges count, bottom-to-top, without the dragged
    /// window; each one's edges are clipped to the display rect, and the
    /// parts covered by windows above it are dropped.
    pub fn new(
        outputs: &[Rectangle<i32, Logical>],
        struts: &[Rectangle<i32, Logical>],
        windows: &[Rectangle<i32, Logical>],
    ) -> Self {
        let monitors: Vec<Rect> = outputs.iter().map(|r| Rect::from(*r)).collect();
        let Some(display) = bounding_box(&monitors) else {
            return Self::default();
        };
        // `all_struts` reaches boxes.c through `g_slist_prepend`s.
        let struts: Vec<Rect> = struts.iter().rev().map(|r| Rect::from(*r)).collect();
        // Monitors likewise (workspace.c:1036).
        let monitors: Vec<Rect> = monitors.into_iter().rev().collect();

        let window_edges = window_edges(display, windows);
        let monitor_edges = find_nonintersected_monitor_edges(&monitors, &struts);
        let screen_edges = find_onscreen_edges(display, &struts);

        let mut data = Self::default();
        for edge in window_edges
            .into_iter()
            .chain(monitor_edges)
            .chain(screen_edges)
        {
            if edge.side.is_vertical() {
                data.vertical.push(edge);
            } else {
                data.horizontal.push(edge);
            }
        }
        data.vertical.sort_by(edge_cmp_ignore_type);
        data.horizontal.sort_by(edge_cmp_ignore_type);
        data
    }

    /// `meta_window_drag_edge_resistance_for_move`: `old` is the window's
    /// current frame rect and `proposed` its new top-left without
    /// resistance; returns the resisted or snapped top-left. Each side
    /// meets its edges on its own, then the window moves by the stricter
    /// of the left/right (top/bottom) changes, measured from the proposed
    /// position for pointer snapping and from the old one otherwise.
    pub fn resist_move(
        &self,
        old: Rectangle<i32, Logical>,
        proposed: Point<i32, Logical>,
        flags: Flags,
    ) -> Point<i32, Logical> {
        let old_outer = Rect::from(old);
        let proposed_outer = Rect {
            x: proposed.x,
            y: proposed.y,
            ..old_outer
        };
        let Some(new_outer) = self.apply_to_each_side(&old_outer, &proposed_outer, flags) else {
            return proposed;
        };
        let reference = if flags.snap && !flags.keyboard {
            proposed_outer
        } else {
            old_outer
        };
        let keyboard_snap = flags.snap && flags.keyboard;
        // edge-resistance.c:1148-1170; a keyboard snap with the far side
        // unmoved takes the near change, as does a smaller near change.
        let stricter = |near: i32, far: i32| {
            if keyboard_snap && near == 0 {
                far
            } else if (keyboard_snap && far == 0) || near.unsigned_abs() < far.unsigned_abs() {
                near
            } else {
                far
            }
        };
        let x_change = stricter(
            new_outer.left.saturating_sub(reference.x),
            new_outer.right.saturating_sub(reference.right()),
        );
        let y_change = stricter(
            new_outer.top.saturating_sub(reference.y),
            new_outer.bottom.saturating_sub(reference.bottom()),
        );
        Point::from((
            old_outer
                .x
                .saturating_add(x_change)
                .saturating_add(reference.x.saturating_sub(old_outer.x)),
            old_outer
                .y
                .saturating_add(y_change)
                .saturating_add(reference.y.saturating_sub(old_outer.y)),
        ))
    }

    /// `apply_edge_resistance_to_each_side` for a move of a window not
    /// tiled side by side: the resisted (or snapped) sides, or `None` when
    /// they are the proposed ones.
    fn apply_to_each_side(&self, old: &Rect, new: &Rect, flags: Flags) -> Option<Sides> {
        let sides = if flags.snap {
            Sides {
                left: apply_edge_snapping(old.x, new.x, new, &self.vertical, true, flags.keyboard),
                right: apply_edge_snapping(
                    old.right(),
                    new.right(),
                    new,
                    &self.vertical,
                    true,
                    flags.keyboard,
                ),
                top: apply_edge_snapping(
                    old.y,
                    new.y,
                    new,
                    &self.horizontal,
                    false,
                    flags.keyboard,
                ),
                bottom: apply_edge_snapping(
                    old.bottom(),
                    new.bottom(),
                    new,
                    &self.horizontal,
                    false,
                    flags.keyboard,
                ),
            }
        } else {
            let resist = |old_pos: i32, new_pos: i32, edges: &[Edge], xdir: bool| {
                apply_edge_resistance(old_pos, new_pos, old, new, edges, xdir, flags)
            };
            Sides {
                left: resist(old.x, new.x, &self.vertical, true),
                right: resist(old.right(), new.right(), &self.vertical, true),
                top: resist(old.y, new.y, &self.horizontal, false),
                bottom: resist(old.bottom(), new.bottom(), &self.horizontal, false),
            }
        };
        let unchanged = sides.left == new.x
            && sides.right == new.right()
            && sides.top == new.y
            && sides.bottom == new.bottom();
        (!unchanged).then_some(sides)
    }
}

/// The four sides of a resisted frame (`modified_rect`).
#[derive(Debug, Clone, Copy)]
struct Sides {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

/// `MtkRectangle`: edges are rectangles with a zero width or height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    /// `BOX_RIGHT`: one pixel past the right.
    fn right(&self) -> i32 {
        self.x.saturating_add(self.w)
    }

    /// `BOX_BOTTOM`: one pixel past the bottom.
    fn bottom(&self) -> i32 {
        self.y.saturating_add(self.h)
    }
}

impl From<Rectangle<i32, Logical>> for Rect {
    fn from(r: Rectangle<i32, Logical>) -> Self {
        Self {
            x: r.loc.x,
            y: r.loc.y,
            w: r.size.w,
            h: r.size.h,
        }
    }
}

/// `mtk_rectangle_intersect`: the common area, if it is not empty.
fn intersect(a: &Rect, b: &Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let w = a.right().min(b.right()).saturating_sub(x);
    let h = a.bottom().min(b.bottom()).saturating_sub(y);
    (w > 0 && h > 0).then_some(Rect { x, y, w, h })
}

/// `mtk_rectangle_vert_overlap`.
fn vert_overlap(a: &Rect, b: &Rect) -> bool {
    a.y < b.bottom() && b.y < a.bottom()
}

/// `mtk_rectangle_horiz_overlap`.
fn horiz_overlap(a: &Rect, b: &Rect) -> bool {
    a.x < b.right() && b.x < a.right()
}

fn bounding_box(rects: &[Rect]) -> Option<Rect> {
    let first = rects.first()?;
    let (mut left, mut top, mut right, mut bottom) =
        (first.x, first.y, first.right(), first.bottom());
    for r in &rects[1..] {
        left = left.min(r.x);
        top = top.min(r.y);
        right = right.max(r.right());
        bottom = bottom.max(r.bottom());
    }
    Some(Rect {
        x: left,
        y: top,
        w: right.saturating_sub(left),
        h: bottom.saturating_sub(top),
    })
}

/// `MetaSide`, with mutter's values (they order `meta_rectangle_edge_cmp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
    Left = 1,
    Right = 2,
    Top = 4,
    Bottom = 8,
}

impl Side {
    fn is_vertical(self) -> bool {
        matches!(self, Side::Left | Side::Right)
    }

    /// LEFT<->RIGHT, TOP<->BOTTOM (`add_edges` of an external rect).
    fn opposite(self) -> Self {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            Side::Top => Side::Bottom,
            Side::Bottom => Side::Top,
        }
    }
}

/// `MetaEdgeType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeKind {
    Window,
    Monitor,
    Screen,
}

/// `MetaEdge`: `side` is the side of the moved window it acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Edge {
    rect: Rect,
    side: Side,
    kind: EdgeKind,
}

impl Edge {
    /// The coordinate sorted on: x for a vertical edge, y otherwise.
    fn position(&self, xdir: bool) -> i32 {
        if xdir {
            self.rect.x
        } else {
            self.rect.y
        }
    }
}

/// `meta_rectangle_edge_aligns` (boxes.c:1007): whether the edge spans the
/// rect's extent across it, touching counting as aligned.
fn edge_aligns(rect: &Rect, edge: &Edge) -> bool {
    if edge.side.is_vertical() {
        rect.y <= edge.rect.bottom() && edge.rect.y <= rect.bottom()
    } else {
        rect.x <= edge.rect.right() && edge.rect.x <= rect.right()
    }
}

/// `meta_rectangle_edge_cmp_ignore_type` (boxes.c:1195).
fn edge_cmp_ignore_type(a: &Edge, b: &Edge) -> Ordering {
    if a.side.is_vertical() {
        (a.rect.x, a.rect.y).cmp(&(b.rect.x, b.rect.y))
    } else {
        (a.rect.y, a.rect.x).cmp(&(b.rect.y, b.rect.x))
    }
}

/// `meta_rectangle_edge_cmp` (boxes.c:1239).
fn edge_cmp(a: &Edge, b: &Edge) -> Ordering {
    a.side.cmp(&b.side).then_with(|| edge_cmp_ignore_type(a, b))
}

/// `edges_overlap` (boxes.c:1257): both vertical on the same x and
/// overlapping, or both horizontal likewise.
fn edges_overlap(a: &Rect, b: &Rect) -> bool {
    if a.w == 0 && b.w == 0 {
        vert_overlap(a, b) && a.x == b.x
    } else if a.h == 0 && b.h == 0 {
        horiz_overlap(a, b) && a.y == b.y
    } else {
        false
    }
}

/// `rectangle_and_edge_intersection` (boxes.c:1277): the part of the edge
/// inside the rect and its handle type: 0 inside the rect, 1 on the
/// rect's side named by the edge's side, -1 on the opposite side.
fn rectangle_and_edge_intersection(rect: &Rect, edge: &Edge) -> Option<(Rect, i32)> {
    let x = rect.x.max(edge.rect.x);
    let y = rect.y.max(edge.rect.y);
    let w = rect.right().min(edge.rect.right()).saturating_sub(x);
    let h = rect.bottom().min(edge.rect.bottom()).saturating_sub(y);
    if w < 0 || h < 0 || (w == 0 && h == 0) {
        return None;
    }
    let handle = |at_start: bool, at_end: bool, start_is_side: bool| match (at_start, at_end) {
        (true, _) => {
            if start_is_side {
                1
            } else {
                -1
            }
        }
        (false, true) => {
            if start_is_side {
                -1
            } else {
                1
            }
        }
        (false, false) => 0,
    };
    let handle = match edge.side {
        Side::Left => handle(x == rect.x, x == rect.right(), true),
        Side::Right => handle(x == rect.x, x == rect.right(), false),
        Side::Top => handle(y == rect.y, y == rect.bottom(), true),
        Side::Bottom => handle(y == rect.y, y == rect.bottom(), false),
    };
    Some((Rect { x, y, w, h }, handle))
}

/// `add_edges` (boxes.c:1361): the rect's four sides as screen edges;
/// an external rect (a strut) gets the opposite side types.
fn add_edges(out: &mut Vec<Edge>, rect: &Rect, internal: bool) {
    let side = |s: Side| if internal { s } else { s.opposite() };
    let edges = [
        (Rect { w: 0, ..*rect }, side(Side::Left)),
        (
            Rect {
                x: rect.right(),
                w: 0,
                ..*rect
            },
            side(Side::Right),
        ),
        (Rect { h: 0, ..*rect }, side(Side::Top)),
        (
            Rect {
                y: rect.bottom(),
                h: 0,
                ..*rect
            },
            side(Side::Bottom),
        ),
    ];
    out.extend(edges.into_iter().map(|(rect, side)| Edge {
        rect,
        side,
        kind: EdgeKind::Screen,
    }));
}

/// `split_edge` (boxes.c:1408): what remains of `old` outside `remove`,
/// which overlaps it.
fn split_edge(out: &mut Vec<Edge>, old: &Edge, remove: &Rect) {
    if old.side.is_vertical() {
        if old.rect.y < remove.y {
            out.push(Edge {
                rect: Rect {
                    h: remove.y.saturating_sub(old.rect.y),
                    ..old.rect
                },
                ..*old
            });
        }
        if old.rect.bottom() > remove.bottom() {
            out.push(Edge {
                rect: Rect {
                    y: remove.bottom(),
                    h: old.rect.bottom().saturating_sub(remove.bottom()),
                    ..old.rect
                },
                ..*old
            });
        }
    } else {
        if old.rect.x < remove.x {
            out.push(Edge {
                rect: Rect {
                    w: remove.x.saturating_sub(old.rect.x),
                    ..old.rect
                },
                ..*old
            });
        }
        if old.rect.right() > remove.right() {
            out.push(Edge {
                rect: Rect {
                    x: remove.right(),
                    w: old.rect.right().saturating_sub(remove.right()),
                    ..old.rect
                },
                ..*old
            });
        }
    }
}

/// `fix_up_edges` (boxes.c:1468): splits `edge` where the strut `rect`
/// covers it (pushing the remains to `splits` and returning whether the
/// edge goes), and drops the strut's own edges lying on a screen edge.
fn fix_up_edges(
    rect: &Rect,
    edge: &Edge,
    strut_edges: &mut Vec<Edge>,
    splits: &mut Vec<Edge>,
) -> bool {
    let Some((overlap, handle)) = rectangle_and_edge_intersection(rect, edge) else {
        return false;
    };
    let mut needs_removal = false;
    if handle == 0 || handle == 1 {
        split_edge(splits, edge, &overlap);
        needs_removal = true;
    }
    if handle == -1 || handle == 1 {
        for cur in std::mem::take(strut_edges) {
            if edges_overlap(&cur.rect, &overlap) {
                split_edge(strut_edges, &cur, &overlap);
            } else {
                strut_edges.push(cur);
            }
        }
    }
    needs_removal
}

/// `meta_rectangle_remove_intersections_with_boxes_from_edges`
/// (boxes.c:1521): cuts the parts inside each rect out of the edges,
/// except where an edge only touches the rect from outside (handle 1:
/// window edges carry the side of the moved window they stop).
fn remove_intersections_with_boxes_from_edges(mut edges: Vec<Edge>, rects: &[Rect]) -> Vec<Edge> {
    const OPPOSING: i32 = 1;
    for rect in rects {
        let mut kept = Vec::with_capacity(edges.len());
        for edge in edges {
            match rectangle_and_edge_intersection(rect, &edge) {
                Some((overlap, handle)) if handle != OPPOSING => {
                    split_edge(&mut kept, &edge, &overlap);
                }
                _ => kept.push(edge),
            }
        }
        edges = kept;
    }
    edges
}

/// `get_rect_minus_overlap` (boxes.c:1033), in mutter's list order.
fn rect_minus_overlap(rect: &Rect, overlap: &Rect) -> Vec<Rect> {
    let mut ret = Vec::with_capacity(4);
    if rect.x < overlap.x {
        ret.push(Rect {
            w: overlap.x.saturating_sub(rect.x),
            ..*rect
        });
    }
    if rect.right() > overlap.right() {
        ret.push(Rect {
            x: overlap.right(),
            w: rect.right().saturating_sub(overlap.right()),
            ..*rect
        });
    }
    if rect.y < overlap.y {
        ret.push(Rect {
            x: overlap.x,
            w: overlap.w,
            y: rect.y,
            h: overlap.y.saturating_sub(rect.y),
        });
    }
    if rect.bottom() > overlap.bottom() {
        ret.push(Rect {
            x: overlap.x,
            w: overlap.w,
            y: overlap.bottom(),
            h: rect.bottom().saturating_sub(overlap.bottom()),
        });
    }
    // Each piece was `g_list_prepend`ed.
    ret.reverse();
    ret
}

/// `get_disjoint_strut_rect_list_in_region` (boxes.c:1119): the struts
/// clipped to the region and split so that overlapping parts occur once.
/// Like mutter, a strut fully covered by an earlier one is dropped and
/// the strut after it is then not compared with that earlier one.
fn disjoint_struts_in_region(struts: &[Rect], region: &Rect) -> Vec<Rect> {
    // Prepended into the list, so reversed.
    let mut list: Vec<Rect> = struts
        .iter()
        .rev()
        .filter_map(|s| intersect(s, region))
        .collect();
    let mut t = 0;
    while t < list.len() {
        let mut c = t + 1;
        while c < list.len() {
            if let Some(overlap) = intersect(&list[t], &list[c]) {
                let mut cur_leftover = vec![overlap];
                cur_leftover.extend(rect_minus_overlap(&list[t], &overlap));
                let comp_leftover = rect_minus_overlap(&list[c], &overlap);
                // `compare` first: it sits after `tmp`.
                list.splice(c..=c, comp_leftover);
                let grown = cur_leftover.len() - 1;
                list.splice(t..=t, cur_leftover);
                // `compare` is now the head of its leftovers, or the
                // element after it when none are left.
                c += grown;
                if c >= list.len() {
                    break;
                }
            }
            c += 1;
        }
        t += 1;
    }
    list
}

/// `meta_rectangle_find_onscreen_edges` (boxes.c:1589): the display
/// rect's sides and the struts' inner sides, minus the struts.
fn find_onscreen_edges(basic: Rect, struts: &[Rect]) -> Vec<Edge> {
    let fixed_struts = disjoint_struts_in_region(struts, &basic);
    let mut ret = Vec::new();
    add_edges(&mut ret, &basic, true);
    for strut in &fixed_struts {
        let mut strut_edges = Vec::new();
        add_edges(&mut strut_edges, strut, false);
        let mut kept = Vec::with_capacity(ret.len());
        for edge in ret {
            let mut splits = Vec::new();
            if fix_up_edges(strut, &edge, &mut strut_edges, &mut splits) {
                kept.extend(splits);
            } else {
                kept.push(edge);
            }
        }
        strut_edges.extend(kept);
        ret = strut_edges;
    }
    ret.sort_by(edge_cmp);
    ret
}

/// `meta_rectangle_find_nonintersected_monitor_edges` (boxes.c:1675):
/// where two monitors meet, an edge for each side, minus the struts.
fn find_nonintersected_monitor_edges(monitors: &[Rect], struts: &[Rect]) -> Vec<Edge> {
    let mut ret = Vec::new();
    for cur in monitors {
        for compare in monitors {
            if vert_overlap(cur, compare) {
                let y = cur.y.max(compare.y);
                let h = cur
                    .bottom()
                    .saturating_sub(y)
                    .min(compare.bottom().saturating_sub(y));
                let side = if cur.x == compare.right() {
                    Some((cur.x, Side::Left))
                } else if cur.right() == compare.x {
                    Some((cur.right(), Side::Right))
                } else {
                    None
                };
                if let Some((x, side)) = side {
                    ret.push(Edge {
                        rect: Rect { x, y, w: 0, h },
                        side,
                        kind: EdgeKind::Monitor,
                    });
                }
            }
            if horiz_overlap(cur, compare) {
                let x = cur.x.max(compare.x);
                let w = cur
                    .right()
                    .saturating_sub(x)
                    .min(compare.right().saturating_sub(x));
                let side = if cur.y == compare.bottom() {
                    Some((cur.y, Side::Top))
                } else if cur.bottom() == compare.y {
                    Some((cur.bottom(), Side::Bottom))
                } else {
                    None
                };
                if let Some((y, side)) = side {
                    ret.push(Edge {
                        rect: Rect { x, y, w, h: 0 },
                        side,
                        kind: EdgeKind::Monitor,
                    });
                }
            }
        }
    }
    // `temp_rects` is prepended from `all_struts`, so reversed again.
    let struts: Vec<Rect> = struts.iter().rev().copied().collect();
    let mut ret = remove_intersections_with_boxes_from_edges(ret, &struts);
    ret.sort_by(edge_cmp);
    ret
}

/// `compute_resistance_and_snapping_edges` steps 1-3 (edge-resistance.c:891):
/// each window's sides, clipped to the display rect, typed by the side of
/// the moved window they stop (a window's left side stops a right side),
/// minus the parts covered by the windows above it.
fn window_edges(display: Rect, windows: &[Rectangle<i32, Logical>]) -> Vec<Edge> {
    let windows: Vec<Rect> = windows.iter().map(|r| Rect::from(*r)).collect();
    let mut edges = Vec::new();
    for (i, window) in windows.iter().enumerate() {
        let Some(reduced) = intersect(window, &display) else {
            continue;
        };
        let new_edges = [
            (Rect { h: 0, ..reduced }, Side::Bottom),
            (Rect { w: 0, ..reduced }, Side::Right),
            (
                Rect {
                    y: reduced.bottom(),
                    h: 0,
                    ..reduced
                },
                Side::Top,
            ),
            (
                Rect {
                    x: reduced.right(),
                    w: 0,
                    ..reduced
                },
                Side::Left,
            ),
        ]
        .into_iter()
        .map(|(rect, side)| Edge {
            rect,
            side,
            kind: EdgeKind::Window,
        })
        .collect();
        edges.extend(remove_intersections_with_boxes_from_edges(
            new_edges,
            &windows[i + 1..],
        ));
    }
    edges.sort_by(edge_cmp);
    edges
}

/// `find_index_of_edge_near_position` (edge-resistance.c:71): the first
/// edge at or after `position` (the length if none) when `want_interval_min`,
/// else the last edge at or before it (-1 if none).
fn find_index_of_edge_near_position(
    edges: &[Edge],
    position: i32,
    want_interval_min: bool,
    xdir: bool,
) -> isize {
    if want_interval_min {
        edges.partition_point(|e| e.position(xdir) < position) as isize
    } else {
        edges.partition_point(|e| e.position(xdir) <= position) as isize - 1
    }
}

/// `points_on_same_side` (edge-resistance.c:174).
fn points_on_same_side(reference: i32, a: i32, b: i32) -> bool {
    (i64::from(a) - i64::from(reference)).signum() * (i64::from(b) - i64::from(reference)).signum()
        > 0
}

/// `find_nearest_position` (edge-resistance.c:180): mutter's binary search
/// for `position`, then the nearest of the edge where it stopped and the
/// first aligned edge on either side of it; `old_position` if none.
/// `only_forward` skips edges on the old position's side.
fn find_nearest_position(
    edges: &[Edge],
    position: i32,
    old_position: i32,
    new_rect: &Rect,
    xdir: bool,
    only_forward: bool,
) -> i32 {
    if edges.is_empty() {
        return old_position;
    }
    let mut mid: isize = 0;
    let mut low: isize = 0;
    let mut high: isize = edges.len() as isize - 1;
    while low < high {
        mid = low + (high - low) / 2;
        let compare = edges[mid as usize].position(xdir);
        if compare == position {
            break;
        }
        if compare > position {
            high = mid - 1;
        } else {
            low = mid + 1;
        }
    }

    let mut best = old_position;
    let mut best_dist = i32::MAX.unsigned_abs();
    let mut consider = |compare: i32| {
        let dist = compare.abs_diff(position);
        if dist < best_dist {
            best = compare;
            best_dist = dist;
        }
    };
    let forward_ok =
        |compare: i32| !only_forward || !points_on_same_side(position, compare, old_position);
    let overlaps = |edge: &Edge| {
        if xdir {
            vert_overlap(&edge.rect, new_rect)
        } else {
            horiz_overlap(&edge.rect, new_rect)
        }
    };

    let mid = mid as usize;
    let edge = &edges[mid];
    let compare = edge.position(xdir);
    if edge_aligns(new_rect, edge) && forward_ok(compare) {
        consider(compare);
    }
    if let Some(compare) = edges[mid + 1..]
        .iter()
        .find(|e| overlaps(e) && forward_ok(e.position(xdir)))
        .map(|e| e.position(xdir))
    {
        consider(compare);
    }
    if let Some(compare) = edges[..mid]
        .iter()
        .rev()
        .find(|e| overlaps(e) && forward_ok(e.position(xdir)))
        .map(|e| e.position(xdir))
    {
        consider(compare);
    }
    best
}

/// `movement_towards_edge` (edge-resistance.c:298).
fn movement_towards_edge(side: Side, increment: isize) -> bool {
    match side {
        Side::Left | Side::Top => increment < 0,
        Side::Right | Side::Bottom => increment > 0,
    }
}

/// `apply_edge_resistance` (edge-resistance.c:315): one side moving from
/// `old_pos` to `new_pos` over the sorted `edges`. A keyboard move stops
/// at the first aligned edge strictly between the two; a pointer move
/// stops at the first aligned edge it has not yet passed by the edge
/// type's threshold (window edges only with `flags.windows`).
fn apply_edge_resistance(
    old_pos: i32,
    new_pos: i32,
    old_rect: &Rect,
    new_rect: &Rect,
    edges: &[Edge],
    xdir: bool,
    flags: Flags,
) -> i32 {
    if old_pos == new_pos || edges.is_empty() {
        return new_pos;
    }
    let increasing = new_pos > old_pos;
    let increment: isize = if increasing { 1 } else { -1 };

    let last_edge = edges.len() as isize - 1;
    // Either can fall outside the array when the window is partly
    // off-screen.
    let begin =
        find_index_of_edge_near_position(edges, old_pos, increasing, xdir).clamp(0, last_edge);
    let end =
        find_index_of_edge_near_position(edges, new_pos, !increasing, xdir).clamp(0, last_edge);

    let mut i = begin;
    while (increasing && i <= end) || (!increasing && i >= end) {
        let edge = &edges[i as usize];
        let compare = edge.position(xdir);
        if edge_aligns(new_rect, edge) || edge_aligns(old_rect, edge) {
            if flags.keyboard {
                if (old_pos < compare && compare < new_pos)
                    || (old_pos > compare && compare > new_pos)
                {
                    return compare;
                }
            } else {
                let towards = movement_towards_edge(edge.side, increment);
                let threshold = match edge.kind {
                    EdgeKind::Window if !flags.windows => 0,
                    EdgeKind::Window if towards => TOWARDS_WINDOW,
                    EdgeKind::Window => AWAYFROM_WINDOW,
                    EdgeKind::Monitor if towards => TOWARDS_MONITOR,
                    EdgeKind::Monitor => AWAYFROM_MONITOR,
                    EdgeKind::Screen if towards => TOWARDS_SCREEN,
                    EdgeKind::Screen => AWAYFROM_SCREEN,
                };
                if compare.abs_diff(new_pos) < threshold {
                    return compare;
                }
            }
        }
        i += increment;
    }
    new_pos
}

/// `apply_edge_snapping` (edge-resistance.c:431): the nearest aligned
/// edge (keyboard moves only forward); a pointer move shorter than 8px
/// keeps the old position unless that edge is within 8px of it too.
fn apply_edge_snapping(
    old_pos: i32,
    new_pos: i32,
    new_rect: &Rect,
    edges: &[Edge],
    xdir: bool,
    keyboard_op: bool,
) -> i32 {
    if old_pos == new_pos || edges.is_empty() {
        return new_pos;
    }
    let snap_to = find_nearest_position(edges, new_pos, old_pos, new_rect, xdir, keyboard_op);
    if !keyboard_op
        && snap_to.abs_diff(old_pos) >= ACCIDENTAL_SNAP_MOVE
        && new_pos.abs_diff(old_pos) < ACCIDENTAL_SNAP_MOVE
    {
        old_pos
    } else {
        snap_to
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn pt(x: i32, y: i32) -> Point<i32, Logical> {
        (x, y).into()
    }

    const POINTER: Flags = Flags {
        snap: false,
        windows: false,
        keyboard: false,
    };
    const POINTER_WINDOWS: Flags = Flags {
        windows: true,
        ..POINTER
    };
    const SNAP: Flags = Flags {
        snap: true,
        ..POINTER
    };
    const KEYBOARD: Flags = Flags {
        keyboard: true,
        windows: true,
        snap: false,
    };

    /// One 1280x800 output with a 32px top bar.
    fn single(windows: &[Rectangle<i32, Logical>]) -> EdgeData {
        EdgeData::new(&[rect(0, 0, 1280, 800)], &[rect(0, 0, 1280, 32)], windows)
    }

    #[test]
    fn screen_edge_holds_a_pointer_move_for_32px_towards_it_only() {
        let data = single(&[]);
        let win = rect(100, 100, 400, 300);
        assert_eq!(data.resist_move(win, pt(-20, 100), POINTER), pt(0, 100));
        assert_eq!(data.resist_move(win, pt(-31, 100), POINTER), pt(0, 100));
        assert_eq!(data.resist_move(win, pt(-40, 100), POINTER), pt(-40, 100));
        // The right side too.
        assert_eq!(data.resist_move(win, pt(900, 100), POINTER), pt(880, 100));
        // Leaving the edge is free.
        let at_edge = rect(0, 100, 400, 300);
        assert_eq!(data.resist_move(at_edge, pt(20, 100), POINTER), pt(20, 100));
        assert_eq!(data.resist_move(at_edge, pt(-20, 100), POINTER), pt(0, 100));
    }

    #[test]
    fn the_top_resists_at_the_struts_bottom_not_the_screen_top() {
        let data = single(&[]);
        let win = rect(100, 100, 400, 300);
        assert_eq!(data.resist_move(win, pt(100, 12), POINTER), pt(100, 32));
        // Past it there is no screen top left under the strut.
        assert_eq!(data.resist_move(win, pt(100, -10), POINTER), pt(100, -10));
        // The bottom screen edge.
        assert_eq!(data.resist_move(win, pt(100, 520), POINTER), pt(100, 500));
    }

    #[test]
    fn monitor_edges_resist_32px_between_side_by_side_outputs() {
        let outputs = [rect(0, 0, 1280, 800), rect(1280, 0, 1280, 800)];
        let struts = [rect(0, 0, 1280, 32), rect(1280, 0, 1280, 32)];
        let data = EdgeData::new(&outputs, &struts, &[]);
        let win = rect(700, 100, 400, 300);
        // Right side 20px past x=1280.
        assert_eq!(data.resist_move(win, pt(900, 100), POINTER), pt(880, 100));
        assert_eq!(data.resist_move(win, pt(920, 100), POINTER), pt(920, 100));
        // Left side into the left output.
        let right = rect(1400, 100, 400, 300);
        assert_eq!(
            data.resist_move(right, pt(1260, 100), POINTER),
            pt(1280, 100)
        );
        assert_eq!(
            data.resist_move(right, pt(1240, 100), POINTER),
            pt(1240, 100)
        );
        // Struts cut the monitor edge: nothing to meet beside the bars.
        let bar = rect(700, 0, 400, 20);
        assert_eq!(data.resist_move(bar, pt(900, 0), POINTER), pt(900, 0));
    }

    #[test]
    fn window_edges_resist_16px_only_with_the_windows_flag() {
        let data = single(&[rect(600, 100, 300, 300)]);
        let win = rect(1000, 150, 200, 200);
        assert_eq!(
            data.resist_move(win, pt(890, 150), POINTER_WINDOWS),
            pt(900, 150)
        );
        assert_eq!(data.resist_move(win, pt(890, 150), POINTER), pt(890, 150));
        assert_eq!(
            data.resist_move(win, pt(880, 150), POINTER_WINDOWS),
            pt(880, 150)
        );
        // A window beside, not across, the moved one's extent: no edge.
        let below = rect(1000, 600, 200, 100);
        assert_eq!(
            data.resist_move(below, pt(890, 600), POINTER_WINDOWS),
            pt(890, 600)
        );
    }

    #[test]
    fn snapping_jumps_to_the_nearest_edge_but_ignores_small_moves() {
        let data = single(&[]);
        let win = rect(100, 100, 400, 300);
        assert_eq!(data.resist_move(win, pt(60, 100), SNAP), pt(0, 100));
        assert_eq!(data.resist_move(win, pt(100, 50), SNAP), pt(100, 32));
        // Under 8px towards an edge more than 8px away: stays.
        assert_eq!(data.resist_move(win, pt(95, 100), SNAP), pt(100, 100));
        // Without snapping, that move is free.
        assert_eq!(data.resist_move(win, pt(95, 100), POINTER), pt(95, 100));
    }

    #[test]
    fn keyboard_moves_stop_at_the_first_crossed_edge() {
        let data = single(&[rect(600, 100, 300, 300)]);
        let win = rect(1000, 150, 200, 200);
        assert_eq!(data.resist_move(win, pt(850, 150), KEYBOARD), pt(900, 150));
        // A pointer move that far passes the window edge.
        assert_eq!(
            data.resist_move(win, pt(850, 150), POINTER_WINDOWS),
            pt(850, 150)
        );
        // The screen edge, however far past.
        let near = rect(50, 150, 200, 200);
        assert_eq!(data.resist_move(near, pt(-50, 150), KEYBOARD), pt(0, 150));
        assert_eq!(data.resist_move(near, pt(-50, 150), POINTER), pt(-50, 150));
        // Landing exactly on an edge is not crossing it.
        assert_eq!(data.resist_move(near, pt(0, 150), KEYBOARD), pt(0, 150));
    }

    #[test]
    fn edges_covered_by_a_window_above_are_removed() {
        let lower = rect(600, 100, 300, 300);
        let upper = rect(500, 50, 500, 500);
        // Its right side (x=1250, moving to 1090) meets no edge.
        let win = rect(1050, 150, 200, 200);
        // `upper` hides `lower`'s right side at x=900; its own at x=1000
        // is passed by 110px.
        let covered = single(&[lower, upper]);
        assert_eq!(
            covered.resist_move(win, pt(890, 150), POINTER_WINDOWS),
            pt(890, 150)
        );
        // Stacked the other way round it shows.
        let shown = single(&[upper, lower]);
        assert_eq!(
            shown.resist_move(win, pt(890, 150), POINTER_WINDOWS),
            pt(900, 150)
        );
    }

    #[test]
    fn index_near_position_matches_mutters_table() {
        let edges: Vec<Edge> = [3, 27, 316, 316, 316, 505, 522, 800, 1213]
            .into_iter()
            .map(|x| Edge {
                rect: Rect {
                    x,
                    y: 0,
                    w: 0,
                    h: 10,
                },
                side: Side::Left,
                kind: EdgeKind::Screen,
            })
            .collect();
        assert_eq!(find_index_of_edge_near_position(&edges, 500, true, true), 5);
        assert_eq!(
            find_index_of_edge_near_position(&edges, 805, false, true),
            7
        );
        assert_eq!(find_index_of_edge_near_position(&edges, 316, true, true), 2);
        assert_eq!(
            find_index_of_edge_near_position(&edges, 316, false, true),
            4
        );
        assert_eq!(find_index_of_edge_near_position(&edges, 2, false, true), -1);
        assert_eq!(
            find_index_of_edge_near_position(&edges, 2000, true, true),
            9
        );
    }

    #[test]
    fn nearest_position_keeps_mutters_binary_search_stop() {
        let edges: Vec<Edge> = [0, 10, 20, 30, 40, 50, 60]
            .into_iter()
            .map(|x| Edge {
                rect: Rect {
                    x,
                    y: 0,
                    w: 0,
                    h: 100,
                },
                side: Side::Left,
                kind: EdgeKind::Screen,
            })
            .collect();
        let probe = Rect {
            x: 28,
            y: 0,
            w: 10,
            h: 10,
        };
        // The search stops at 10, so 30 (2px away) is never compared.
        assert_eq!(
            find_nearest_position(&edges, 28, 100, &probe, true, false),
            20
        );
        // It stops at 50 here, so 30 (3px away) loses to 40.
        assert_eq!(
            find_nearest_position(&edges, 33, 100, &probe, true, false),
            40
        );
        // Stopping beside the nearest edge finds it.
        assert_eq!(
            find_nearest_position(&edges, 21, 100, &probe, true, false),
            20
        );
    }
}
