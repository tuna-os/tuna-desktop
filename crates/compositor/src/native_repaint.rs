//! Compare complete nested/native scenes before submitting a frame or acquiring
//! a scanout buffer. The shared damage machinery does not require DRM.
//! Buffer-age history accounts for every intervening submitted frame; global
//! drawing changes and reset buffers conservatively require a complete paint.
//! Within one global drawing, the damage tracker takes each element's own
//! damage: what a client reported for a surface, not the whole surface.

use smithay::backend::renderer::{
    damage::OutputDamageTracker,
    element::{Element, Id, Kind},
    utils::{CommitCounter, DamageSet},
    Color32F,
};
use smithay::output::OutputNoMode;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Transform};

/// One drawn element as the damage tracker sees it: at the scale of the
/// pass that draws it, with the element's own damage history. It claims
/// no opaque region of its own: the tracker is given the passes bottom to
/// top, so its own opaque culling would hide elements under the ones
/// below them. What covers it is carried in `above` instead.
#[derive(Clone, Copy)]
pub(crate) struct Tracked<'a> {
    element: &'a dyn Element,
    scale: f64,
    /// Opaque frame-space area of the elements drawn over this one.
    above: &'a [Rectangle<i32, Physical>],
}

impl<'a> Tracked<'a> {
    pub(crate) fn new(element: &'a dyn Element, scale: f64) -> Self {
        Self {
            element,
            scale,
            above: &[],
        }
    }

    /// A pass's elements, given front to back, bottom to top: an element
    /// appearing or leaving on top shifts no other element's stacking
    /// position, so the tracker does not repaint everything beneath it.
    pub(crate) fn pass<E: Element>(elements: &'a [E], scale: f64) -> Vec<Self> {
        elements
            .iter()
            .rev()
            .map(|element| Self::new(element, scale))
            .collect()
    }

    /// [`pass`](Self::pass) whose elements damage only what the opaque
    /// elements in front of them in `cover` leave showing (#503), as
    /// drawing culls it: a graph redrawing under another window repaints
    /// nothing, like Mutter's unobscured-region clipping.
    pub(crate) fn covered_pass<E: Element>(
        elements: &'a [E],
        scale: f64,
        cover: &'a Cover,
    ) -> Vec<Self> {
        debug_assert_eq!(elements.len(), cover.ends.len());
        elements
            .iter()
            .zip(&cover.ends)
            .rev()
            .map(|(element, end)| Self {
                element,
                scale,
                above: &cover.rects[..*end],
            })
            .collect()
    }
}

/// The opaque area in front of each element of one pass, in frame
/// pixels, as [`crate::occlusion::Occlusion`] culls drawing beneath it:
/// only `Element::opaque_regions`, clipped to the element's geometry.
pub(crate) struct Cover {
    rects: Vec<Rectangle<i32, Physical>>,
    /// Per element, front to back: how many of `rects` lie in front of it.
    ends: Vec<usize>,
}

impl Cover {
    /// `elements` front to back, drawn at `scale`, every one of them
    /// drawn after (over) everything below it in the slice.
    pub(crate) fn new<E: Element>(elements: &[E], scale: f64) -> Self {
        let mut rects = Vec::new();
        let mut ends = Vec::with_capacity(elements.len());
        for element in elements {
            ends.push(rects.len());
            let geometry = element.geometry(scale.into());
            rects.extend(element.opaque_regions(scale.into()).iter().filter_map(|r| {
                Rectangle::new(r.loc + geometry.loc, r.size).intersection(geometry)
            }));
        }
        Self { rects, ends }
    }
}

impl Element for Tracked<'_> {
    fn id(&self) -> &Id {
        self.element.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.element.current_commit()
    }
    fn location(&self, _scale: Scale<f64>) -> Point<i32, Physical> {
        self.element.location(self.scale.into())
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.element.src()
    }
    fn transform(&self) -> Transform {
        self.element.transform()
    }
    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.element.geometry(self.scale.into())
    }
    fn damage_since(
        &self,
        _scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        let damage = self.element.damage_since(self.scale.into(), commit);
        if self.above.is_empty() || damage.is_empty() {
            return damage;
        }
        // Damage is relative to the element, as the tracker places it.
        let origin = self.element.geometry(self.scale.into()).loc;
        Rectangle::subtract_rects_many_in_place(
            damage
                .iter()
                .map(|r| Rectangle::new(r.loc + origin, r.size))
                .collect(),
            self.above.iter().copied(),
        )
        .into_iter()
        .map(|r| Rectangle::new(r.loc - origin, r.size))
        .collect()
    }
    fn alpha(&self) -> f32 {
        self.element.alpha()
    }
    fn kind(&self) -> Kind {
        self.element.kind()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ElementSignature {
    id: Id,
    commit: CommitCounter,
    geometry: Rectangle<i32, Physical>,
    source: Rectangle<f64, Buffer>,
    transform: Transform,
    alpha: f32,
    opaque: Vec<Rectangle<i32, Physical>>,
}

impl ElementSignature {
    pub(crate) fn capture(tracked: &Tracked<'_>) -> Self {
        let scale = tracked.scale.into();
        let element = tracked.element;
        Self {
            id: element.id().clone(),
            commit: element.current_commit(),
            geometry: element.geometry(scale),
            source: element.src(),
            transform: element.transform(),
            alpha: element.alpha(),
            opaque: element.opaque_regions(scale).iter().copied().collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FrameSignature {
    pub effects: Vec<u32>,
    pub size: (i32, i32),
    pub location: (i32, i32),
    pub scale: f64,
    /// DisplayConfig transform (#345): rotation invalidates damage history.
    pub transform: u32,
    pub locked: bool,
    pub background: Color32F,
    pub blank_alpha: f32,
    pub accent: [f32; 3],
    pub decor: Vec<(Color32F, Vec<Rectangle<i32, Physical>>)>,
    pub above: usize,
    // Separate passes preserve both ordering and which scale each pass uses.
    pub groups: Vec<Vec<ElementSignature>>,
}

impl FrameSignature {
    /// The signature of `passes`, as [`damage_region`] tracks them.
    pub(crate) fn groups(passes: &[Vec<Tracked<'_>>]) -> Vec<Vec<ElementSignature>> {
        passes
            .iter()
            .map(|pass| pass.iter().map(ElementSignature::capture).collect())
            .collect()
    }

    /// Inputs outside the element damage history must repaint the complete
    /// acquired buffer. Elements that come, go, move or restack are the
    /// tracker's; an element whose opaque region changed is not (drawing
    /// culls beneath opaque regions).
    pub(crate) fn same_global_drawing(&self, old: &Self) -> bool {
        self.size == old.size
            && self.location == old.location
            && self.scale == old.scale
            && self.transform == old.transform
            && self.locked == old.locked
            && self.background == old.background
            && self.blank_alpha == old.blank_alpha
            && self.accent == old.accent
            && self.decor == old.decor
            && self.effects == old.effects
            && self.above == old.above
            && self.groups.len() == old.groups.len()
            && self.groups.iter().zip(&old.groups).all(|(new, old)| {
                new.iter().all(|new| {
                    old.iter()
                        .find(|old| old.id == new.id)
                        .is_none_or(|old| old.opaque == new.opaque)
                })
            })
    }
}

/// More separate rectangles than this cost more in per-rect setup than
/// they save in fill; past it the damage collapses to its bounds.
pub(crate) const MAX_DAMAGE_RECTS: usize = 16;

/// The one pixel a frame without visible damage repaints (#503).
fn timing_rect() -> Rectangle<i32, Physical> {
    Rectangle::new((0, 0).into(), (1, 1).into())
}

/// The buffer-age damage as a set of rectangles (#503): a clock tick in
/// the bar and a progress bar in a window repaint two small areas, not
/// the bounding box spanning the output. A surface's commit damages what
/// its client reported, not the whole surface: a ticking graph in a
/// window repaints the graph.
pub(crate) fn damage_region(
    tracker: &mut OutputDamageTracker,
    age: usize,
    passes: &[Vec<Tracked<'_>>],
) -> Result<Vec<Rectangle<i32, Physical>>, OutputNoMode> {
    // Every frame damages the timing pixel, on top of everything: a frame
    // whose element damage is all covered is still a submitted frame, so
    // the tracker must record it and the commits it covered. With nothing
    // damaged it records neither, and a window committing on under
    // another soon outruns its surface damage history and damages whole.
    let timing = timing_pixel();
    let timing = Tracked::new(&timing, 1.0);
    let elements: Vec<_> = passes
        .iter()
        .flatten()
        .chain(std::iter::once(&timing))
        .collect();
    let (damage, _) = tracker.damage_output(age, &elements)?;
    let mut rects = coalesce(damage.map(|rects| rects.to_vec()).unwrap_or_default());
    // The pixel never changes the picture: other damage paints alone. A
    // timing-only request still needs an actual page flip; repainting the
    // pixel submits that real frame rather than manufacturing
    // presentation feedback.
    let pixel = timing_rect();
    if rects.len() > 1 {
        rects.retain(|rect| *rect != pixel);
    }
    Ok(if rects.is_empty() { vec![pixel] } else { rects })
}

/// The timing pixel's element: a new commit each frame, never drawn.
fn timing_pixel() -> smithay::backend::renderer::element::solid::SolidColorRenderElement {
    use std::cell::Cell;
    thread_local! {
        static TIMING: (Id, Cell<usize>) = (Id::new(), Cell::new(0));
    }
    TIMING.with(|(id, frame)| {
        frame.set(frame.get().wrapping_add(1));
        smithay::backend::renderer::element::solid::SolidColorRenderElement::new(
            id.clone(),
            timing_rect(),
            frame.get(),
            Color32F::TRANSPARENT,
            Kind::Unspecified,
        )
    })
}

/// Drop empty rectangles and merge overlapping ones, so every pixel is
/// painted once; too many left collapse to their bounding box.
pub(crate) fn coalesce(rects: Vec<Rectangle<i32, Physical>>) -> Vec<Rectangle<i32, Physical>> {
    let mut out: Vec<Rectangle<i32, Physical>> = Vec::with_capacity(rects.len());
    for mut rect in rects.into_iter().filter(|r| !r.is_empty()) {
        // Merging can make a rectangle overlap ones already kept.
        while let Some(at) = out.iter().position(|o| o.overlaps(rect)) {
            rect = rect.merge(out.swap_remove(at));
        }
        out.push(rect);
    }
    if out.len() > MAX_DAMAGE_RECTS {
        out = out
            .into_iter()
            .reduce(Rectangle::merge)
            .into_iter()
            .collect();
    }
    out
}

/// The plane damage (KMS `FB_DAMAGE_CLIPS`) submitted with a repainted
/// buffer: exactly the region repainted into it. That is the buffer-age
/// damage since this buffer was last scanned out, so it covers both the
/// change from the previous frame (drivers copying into one shadow
/// scanout) and everything stale in this buffer (drivers uploading each
/// buffer separately, such as virtio-gpu). Without it the kernel treats
/// the whole plane as damaged and copies or uploads every pixel.
pub(crate) fn scanout_damage(
    repainted: &[Rectangle<i32, Physical>],
) -> Vec<Rectangle<i32, Physical>> {
    repainted.to_vec()
}

/// KMS damage clips in scanout-buffer pixels (#345): `repainted`
/// damage from [`damage_region`] is in pre-transform output space,
/// which matches the buffer only when unrotated. Under rotation each
/// rect maps into the buffer through the output transform (Smithay
/// `Transform::transform_rect_in`, the logical-to-buffer direction
/// also used for `RecordArea` capture); unrotated outputs pass
/// through exactly as [`scanout_damage`] does.
pub(crate) fn scanout_damage_rotated(
    repainted: &[Rectangle<i32, Physical>],
    transform: Transform,
    logical: (i32, i32),
) -> Vec<Rectangle<i32, Physical>> {
    if transform == Transform::Normal {
        return scanout_damage(repainted);
    }
    let area = smithay::utils::Size::<i32, smithay::utils::Physical>::from(logical);
    repainted
        .iter()
        .map(|rect| transform.transform_rect_in(*rect, &area))
        .collect()
}

/// Timing requests need an actual submitted frame even without pixel damage.
pub(crate) fn needs_repaint(
    previous: Option<&FrameSignature>,
    next: &FrameSignature,
    timing: bool,
) -> bool {
    timing || previous != Some(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::renderer::element::solid::SolidColorRenderElement;

    fn scene() -> FrameSignature {
        FrameSignature {
            effects: Vec::new(),
            size: (1280, 800),
            location: (0, 0),
            scale: 1.0,
            transform: 0,
            locked: false,
            background: Color32F::BLACK,
            blank_alpha: 0.0,
            accent: [0.0; 3],
            decor: vec![],
            above: 0,
            groups: vec![],
        }
    }

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn solid(
        id: &Id,
        geometry: Rectangle<i32, Physical>,
        commit: usize,
    ) -> SolidColorRenderElement {
        SolidColorRenderElement::new(
            id.clone(),
            geometry,
            commit,
            Color32F::new(1.0, 0.0, 0.0, 1.0),
            Kind::Unspecified,
        )
    }

    fn sig(element: &impl Element) -> ElementSignature {
        ElementSignature::capture(&Tracked::new(element, 1.0))
    }

    /// One pass of `elements`, given front to back.
    fn damage<E: Element>(
        tracker: &mut OutputDamageTracker,
        age: usize,
        elements: &[E],
    ) -> Vec<Rectangle<i32, Physical>> {
        damage_region(tracker, age, &[Tracked::pass(elements, 1.0)]).unwrap()
    }

    fn contains(rects: &[Rectangle<i32, Physical>], x: i32, y: i32) -> bool {
        rects.iter().any(|r| r.contains((x, y)))
    }

    fn area(rects: &[Rectangle<i32, Physical>]) -> i32 {
        rects.iter().map(|r| r.size.w * r.size.h).sum()
    }

    #[test]
    fn parent_brightness_change_invalidates_native_repaint_and_damage_layout() {
        let before = scene();
        let mut after = before.clone();
        after.effects = vec![0.7f32.to_bits()];
        assert!(needs_repaint(Some(&before), &after, false));
        assert!(!after.same_global_drawing(&before));
    }
    #[test]
    fn rotation_change_repaints_and_resets_damage() {
        let before = scene();
        let mut after = before.clone();
        after.transform = 1;
        assert!(needs_repaint(Some(&before), &after, false));
        assert!(!after.same_global_drawing(&before));
    }
    #[test]
    fn unchanged_scene_sleeps_but_first_frame_and_timing_requests_render() {
        let next = scene();
        assert!(needs_repaint(None, &next, false));
        assert!(!needs_repaint(Some(&next), &next, false));
        assert!(needs_repaint(Some(&next), &next, true));
    }

    #[test]
    fn lock_fade_output_and_decor_changes_render_immediately() {
        let before = scene();
        let mut variants = vec![before.clone(); 8];
        variants[0].locked = true;
        variants[1].blank_alpha = 1.0;
        variants[2].size = (800, 600);
        variants[3].scale = 1.25;
        variants[4].location = (1280, 0);
        variants[5].background = Color32F::new(1.0, 1.0, 1.0, 1.0);
        variants[6].accent = [1.0; 3];
        variants[7].decor = vec![(
            Color32F::new(1.0, 1.0, 1.0, 1.0),
            vec![Rectangle::from_size((40, 30).into())],
        )];
        for next in variants {
            assert!(needs_repaint(Some(&before), &next, false));
        }
    }

    #[test]
    fn cursor_motion_keeps_buffer_age_history_and_bounds_damage() {
        let id = Id::new();
        let cursor = |x| {
            [SolidColorRenderElement::new(
                id.clone(),
                Rectangle::new((x, 40).into(), (24, 24).into()),
                CommitCounter::default(),
                Color32F::BLACK,
                Kind::Cursor,
            )]
        };
        let (at_20, at_25) = (cursor(20), cursor(25));
        let mut before = scene();
        before.groups = vec![vec![sig(&at_20[0])]];
        let mut after = before.clone();
        after.groups[0][0] = sig(&at_25[0]);
        assert!(needs_repaint(Some(&before), &after, false));
        assert!(after.same_global_drawing(&before));
        let mut tracker = OutputDamageTracker::new(
            smithay::utils::Size::<i32, smithay::utils::Physical>::from((1280, 800)),
            1.0,
            Transform::Normal,
        );
        damage(&mut tracker, 0, &at_20);
        let damage = damage(&mut tracker, 1, &at_25);
        assert!(!damage.is_empty());
        let expected = Rectangle::new((20, 40).into(), (29, 24).into());
        assert!(damage
            .iter()
            .all(|rect| rect.intersection(expected) == Some(*rect)));
        // No cursor element on a second output means no changed signature.
        let other_output = scene();
        assert!(!needs_repaint(Some(&other_output), &other_output, false));
    }

    #[test]
    fn buffer_commits_and_geometry_are_part_of_the_image() {
        let id = Id::new();
        let geo = rect(0, 0, 40, 30);
        let mut before = scene();
        before.groups = vec![vec![sig(&solid(&id, geo, 0))]];
        for changed in [
            sig(&solid(&id, geo, 1)),
            sig(&solid(&id, rect(0, 0, 41, 30), 0)),
        ] {
            let mut next = before.clone();
            next.groups[0][0] = changed;
            assert!(needs_repaint(Some(&before), &next, false));
        }
        let mut removed = before.clone();
        removed.groups.clear();
        assert!(needs_repaint(Some(&before), &removed, false));
    }

    #[test]
    fn asynchronously_ready_background_repaints_an_idle_scene() {
        // The native path polls wallpaper/card workers before capturing this
        // signature. An absent card becoming ready adds its new buffer ID to
        // the background pass even without an input event or client commit.
        let mut pending = scene();
        pending.groups = vec![vec![]];
        assert!(!needs_repaint(Some(&pending), &pending, false));
        let card = solid(&Id::new(), rect(0, 0, 40, 30), 0);
        let mut ready = pending.clone();
        ready.groups[0].push(sig(&card));
        assert!(needs_repaint(Some(&pending), &ready, false));
        // The tracker repaints the new card's area; nothing global changed.
        assert!(ready.same_global_drawing(&pending));
        assert!(!needs_repaint(Some(&ready), &ready, false));
        let mut tracker = OutputDamageTracker::new((1280, 800), 1.0, Transform::Normal);
        damage::<SolidColorRenderElement>(&mut tracker, 0, &[]);
        assert_eq!(damage(&mut tracker, 1, &[card]), vec![rect(0, 0, 40, 30)]);
    }

    #[test]
    fn overlapping_elements_and_render_passes_preserve_order() {
        let element = || sig(&solid(&Id::new(), rect(0, 0, 40, 30), 0));
        let mut before = scene();
        before.groups = vec![vec![element(), element()], vec![]];
        let mut reordered = before.clone();
        reordered.groups[0].reverse();
        assert!(needs_repaint(Some(&before), &reordered, false));
        let mut other_pass = before.clone();
        let moved = other_pass.groups[0].pop().unwrap();
        other_pass.groups[1].push(moved);
        assert!(needs_repaint(Some(&before), &other_pass, false));
        let mut partition = before.clone();
        partition.above = 1;
        assert!(needs_repaint(Some(&before), &partition, false));
    }

    #[test]
    fn same_buffer_with_changed_crop_transform_or_opacity_repaints() {
        let mut before = scene();
        before.groups = vec![vec![sig(&solid(&Id::new(), rect(0, 0, 40, 30), 0))]];
        let mut variants = vec![before.clone(); 3];
        variants[0].groups[0][0].source.loc.x += 1.0;
        variants[1].groups[0][0].transform = Transform::Flipped180;
        variants[2].groups[0][0].alpha = 0.5;
        for next in variants {
            assert!(needs_repaint(Some(&before), &next, false));
        }
    }

    fn moving(id: &Id, x: i32, commit: usize) -> [SolidColorRenderElement; 1] {
        [solid(id, rect(x, 0, 8, 8), commit)]
    }

    #[test]
    fn recycled_buffer_repaints_damage_from_intervening_frames() {
        let id = Id::new();
        let mut tracker = OutputDamageTracker::new((32, 32), 1.0, Transform::Normal);
        let full = vec![rect(0, 0, 32, 32)];
        assert_eq!(damage(&mut tracker, 0, &moving(&id, 0, 0)), full);
        damage(&mut tracker, 1, &moving(&id, 8, 1));
        let aged = damage(&mut tracker, 2, &moving(&id, 16, 2));
        assert!(contains(&aged, 1, 1));
        assert!(contains(&aged, 23, 7));
        assert!(aged.iter().all(|r| r.size.w < 32 && r.size.h < 32));
        assert_eq!(damage(&mut tracker, 100, &moving(&id, 16, 2)), full);
    }

    #[test]
    fn timing_only_frame_and_removed_elements_keep_correct_damage() {
        let id = Id::new();
        let mut tracker = OutputDamageTracker::new((32, 32), 1.0, Transform::Normal);
        damage(&mut tracker, 0, &moving(&id, 0, 0));
        assert_eq!(
            damage(&mut tracker, 1, &moving(&id, 0, 0)),
            vec![rect(0, 0, 1, 1)]
        );
        assert!(contains(
            &damage::<SolidColorRenderElement>(&mut tracker, 1, &[]),
            7,
            7
        ));
    }

    #[test]
    fn rotated_scanout_damage_maps_into_buffer_pixels() {
        // A 90° 4x2 panel tracks damage in its 2x4 logical frame; the
        // KMS clips must name 4x2 buffer pixels instead.
        let r = |x, y, w, h| Rectangle::<i32, Physical>::new((x, y).into(), (w, h).into());
        let full = vec![r(0, 0, 2, 4)];
        assert_eq!(
            scanout_damage_rotated(&full, Transform::_90, (2, 4)),
            vec![r(0, 0, 4, 2)]
        );
        // A logical corner rect rotates with the content it covers.
        let corner = vec![r(0, 0, 1, 1)];
        assert_eq!(
            scanout_damage_rotated(&corner, Transform::_90, (2, 4)),
            vec![r(3, 0, 1, 1)]
        );
        // 180° and the flip map to their own buffer corners.
        assert_eq!(
            scanout_damage_rotated(&corner, Transform::_180, (4, 2)),
            vec![r(3, 1, 1, 1)]
        );
        assert_eq!(
            scanout_damage_rotated(&corner, Transform::Flipped, (4, 2)),
            vec![r(3, 0, 1, 1)]
        );
        // Unrotated outputs pass damage through untouched, and empty
        // damage stays empty under any transform.
        assert_eq!(
            scanout_damage_rotated(&corner, Transform::Normal, (4, 2)),
            corner
        );
        assert!(scanout_damage_rotated(&[], Transform::_90, (2, 4)).is_empty());
    }

    #[test]
    fn scanout_damage_is_the_repainted_buffer_age_region() {
        let id = Id::new();
        let mut tracker = OutputDamageTracker::new((32, 32), 1.0, Transform::Normal);
        let full = damage(&mut tracker, 0, &moving(&id, 0, 0));
        assert_eq!(scanout_damage(&full), vec![rect(0, 0, 32, 32)]);
        damage(&mut tracker, 1, &moving(&id, 8, 1));
        // Two frames back: covers both moves, not the whole plane.
        let aged = damage(&mut tracker, 2, &moving(&id, 16, 2));
        let clips = scanout_damage(&aged);
        assert_eq!(clips, aged);
        assert!(contains(&clips, 1, 1) && contains(&clips, 23, 7));
        assert!(clips.iter().all(|r| r.size.w < 32));
    }

    /// Two small elements far apart: a clock in the bar and a progress
    /// bar near the bottom.
    fn scattered(clock: &Id, bar: &Id, commit: usize) -> [SolidColorRenderElement; 2] {
        [
            solid(clock, rect(600, 0, 80, 32), commit),
            solid(bar, rect(200, 700, 300, 8), commit),
        ]
    }

    #[test]
    fn scattered_damage_stays_a_set_of_small_rects() {
        let (clock, bar) = (Id::new(), Id::new());
        let mut tracker = OutputDamageTracker::new((1280, 800), 1.0, Transform::Normal);
        damage(&mut tracker, 0, &scattered(&clock, &bar, 0));
        let rects = damage(&mut tracker, 1, &scattered(&clock, &bar, 1));
        assert_eq!(rects.len(), 2, "{rects:?}");
        assert_eq!(
            area(&rects),
            80 * 32 + 300 * 8,
            "not the bounding box spanning the output"
        );
    }

    /// A client surface whose commits report damage of their own.
    struct Surface {
        id: Id,
        commit: CommitCounter,
        geometry: Rectangle<i32, Physical>,
        damage: Rectangle<i32, Physical>,
        /// Whether the client declared the whole surface opaque.
        opaque: bool,
    }

    impl Element for Surface {
        fn id(&self) -> &Id {
            &self.id
        }
        fn current_commit(&self) -> CommitCounter {
            self.commit
        }
        fn src(&self) -> Rectangle<f64, Buffer> {
            let size = self.geometry.size;
            Rectangle::from_size((f64::from(size.w), f64::from(size.h)).into())
        }
        fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
            Rectangle::new(
                self.geometry.loc,
                self.geometry.size.to_f64().upscale(scale).to_i32_round(),
            )
        }
        fn damage_since(
            &self,
            scale: Scale<f64>,
            commit: Option<CommitCounter>,
        ) -> DamageSet<i32, Physical> {
            match self.commit.distance(commit) {
                Some(0) => DamageSet::default(),
                Some(1) => DamageSet::from_slice(&[Rectangle::new(
                    self.damage.loc.to_f64().upscale(scale).to_i32_round(),
                    self.damage.size.to_f64().upscale(scale).to_i32_round(),
                )]),
                _ => DamageSet::from_slice(&[Rectangle::from_size(self.geometry(scale).size)]),
            }
        }
        fn opaque_regions(
            &self,
            scale: Scale<f64>,
        ) -> smithay::backend::renderer::utils::OpaqueRegions<i32, Physical> {
            if self.opaque {
                [Rectangle::from_size(self.geometry(scale).size)]
                    .into_iter()
                    .collect()
            } else {
                Default::default()
            }
        }
    }

    /// One window pass, front to back, culled by its own opaque windows.
    fn covered(
        tracker: &mut OutputDamageTracker,
        age: usize,
        windows: &[Surface],
    ) -> Vec<Rectangle<i32, Physical>> {
        let cover = Cover::new(windows, 1.0);
        damage_region(tracker, age, &[Tracked::covered_pass(windows, 1.0, &cover)]).unwrap()
    }

    fn window(
        geometry: Rectangle<i32, Physical>,
        damage: Rectangle<i32, Physical>,
        opaque: bool,
    ) -> Surface {
        Surface {
            id: Id::new(),
            commit: CommitCounter::default(),
            geometry,
            damage,
            opaque,
        }
    }

    #[test]
    fn damage_under_an_opaque_window_repaints_nothing() {
        // System Monitor's graph redraws under Files (the perf lane's
        // final settle): nothing on screen changes, so only the 1x1
        // timing repaint is left, not the graph and Files over it.
        let files = window(rect(195, 141, 890, 549), rect(0, 0, 1, 1), true);
        let monitor = window(rect(240, 56, 800, 719), rect(20, 200, 760, 150), false);
        let mut windows = [files, monitor];
        let mut tracker = OutputDamageTracker::new((1280, 800), 1.0, Transform::Normal);
        covered(&mut tracker, 0, &windows);
        windows[1].commit.increment();
        assert_eq!(covered(&mut tracker, 1, &windows), vec![rect(0, 0, 1, 1)]);
        // Damage reaching past Files repaints only the part that shows.
        windows[1].damage = rect(20, 600, 760, 100);
        windows[1].commit.increment();
        assert_eq!(
            covered(&mut tracker, 1, &windows),
            vec![rect(260, 690, 760, 66)]
        );
        // A buffer two frames old also repaints the previous frame's.
        windows[1].damage = rect(20, 200, 760, 150);
        windows[1].commit.increment();
        assert_eq!(
            covered(&mut tracker, 2, &windows),
            vec![rect(260, 690, 760, 66)]
        );
    }

    #[test]
    fn translucent_windows_hide_no_damage() {
        // A shadow, a fading window or a translucent terminal: whatever
        // redraws beneath it shows through.
        let glass = window(rect(100, 100, 400, 300), rect(0, 0, 1, 1), false);
        let below = window(rect(150, 150, 200, 200), rect(10, 10, 50, 50), false);
        let mut windows = [glass, below];
        let mut tracker = OutputDamageTracker::new((1280, 800), 1.0, Transform::Normal);
        covered(&mut tracker, 0, &windows);
        windows[1].commit.increment();
        assert_eq!(
            covered(&mut tracker, 1, &windows),
            vec![rect(160, 160, 50, 50)]
        );
    }

    #[test]
    fn covered_commits_never_outrun_the_surface_damage_history() {
        // Each covered frame still records the commit it hid: ten graph
        // updates under Files never fall back to damaging System Monitor
        // whole (this test surface damages whole past one commit, as a
        // surface past its damage history does).
        let files = window(rect(195, 141, 890, 549), rect(0, 0, 1, 1), true);
        let monitor = window(rect(240, 56, 800, 719), rect(20, 200, 760, 150), false);
        let mut windows = [files, monitor];
        let mut tracker = OutputDamageTracker::new((1280, 800), 1.0, Transform::Normal);
        covered(&mut tracker, 0, &windows);
        // Two buffers cycling: the first aged frame has no history yet.
        windows[1].commit.increment();
        covered(&mut tracker, 2, &windows);
        for _ in 0..10 {
            windows[1].commit.increment();
            assert_eq!(covered(&mut tracker, 2, &windows), vec![rect(0, 0, 1, 1)]);
        }
    }

    #[test]
    fn uncovering_repaints_what_changed_beneath() {
        // Damage hidden while covered is never lost: when the window on
        // top moves away, what is beneath repaints as it is now.
        let files = window(rect(100, 100, 400, 300), rect(0, 0, 1, 1), true);
        let monitor = window(rect(100, 100, 400, 300), rect(0, 0, 400, 300), false);
        let mut windows = [files, monitor];
        let mut tracker = OutputDamageTracker::new((1280, 800), 1.0, Transform::Normal);
        covered(&mut tracker, 0, &windows);
        windows[1].commit.increment();
        assert_eq!(covered(&mut tracker, 1, &windows), vec![rect(0, 0, 1, 1)]);
        windows[0].geometry.loc.x = 600;
        let shown = covered(&mut tracker, 1, &windows);
        assert!(
            contains(&shown, 100, 100) && contains(&shown, 499, 399),
            "{shown:?}"
        );
    }

    #[test]
    fn covered_damage_stays_relative_to_its_scaled_element() {
        // At scale 2 the cover and the damage are both in frame pixels.
        let top = window(rect(0, 0, 100, 100), rect(0, 0, 1, 1), true);
        let below = window(rect(50, 0, 100, 100), rect(0, 0, 100, 10), false);
        let mut windows = [top, below];
        assert_eq!(
            Cover::new(&windows[..1], 2.0).rects,
            vec![rect(0, 0, 200, 200)]
        );
        let scaled = |tracker: &mut OutputDamageTracker, age, windows: &[Surface]| {
            let cover = Cover::new(windows, 2.0);
            damage_region(tracker, age, &[Tracked::covered_pass(windows, 2.0, &cover)]).unwrap()
        };
        let mut tracker = OutputDamageTracker::new((2560, 1600), 1.0, Transform::Normal);
        scaled(&mut tracker, 0, &windows);
        windows[1].commit.increment();
        // `below` starts at x 50 and spans 200 frame pixels; its damage
        // is 200x20 there, of which x 200..250 shows.
        assert_eq!(
            scaled(&mut tracker, 1, &windows),
            vec![rect(200, 0, 50, 20)]
        );
    }

    #[test]
    fn a_surface_commit_repaints_what_its_client_damaged() {
        // A window of 600x400 whose client redraws a 100x50 graph.
        let mut window = Surface {
            id: Id::new(),
            commit: CommitCounter::default(),
            geometry: rect(100, 100, 600, 400),
            damage: rect(20, 30, 100, 50),
            opaque: false,
        };
        let mut tracker = OutputDamageTracker::new((1280, 800), 1.0, Transform::Normal);
        damage(&mut tracker, 0, std::slice::from_ref(&window));
        window.commit.increment();
        let rects = damage(&mut tracker, 1, std::slice::from_ref(&window));
        assert_eq!(rects, vec![rect(120, 130, 100, 50)]);
        // Two buffers back, both commits' damage.
        window.damage = rect(300, 0, 10, 10);
        window.commit.increment();
        let aged = damage(&mut tracker, 2, std::slice::from_ref(&window));
        assert!(contains(&aged, 120, 130) && contains(&aged, 405, 105));
        assert_eq!(area(&aged), 100 * 50 + 10 * 10);
        // Drawn at its pass's scale, the damage scales with it.
        let mut scaled = OutputDamageTracker::new((2560, 1600), 1.0, Transform::Normal);
        damage_region(&mut scaled, 0, &[vec![Tracked::new(&window, 2.0)]]).unwrap();
        window.commit.increment();
        let rects = damage_region(&mut scaled, 1, &[vec![Tracked::new(&window, 2.0)]]).unwrap();
        assert_eq!(rects, vec![rect(700, 100, 20, 20)]);
    }

    #[test]
    fn elements_coming_and_going_on_top_repaint_only_their_area() {
        let (window, banner) = (Id::new(), Id::new());
        let mut tracker = OutputDamageTracker::new((1280, 800), 1.0, Transform::Normal);
        let below = solid(&window, rect(100, 100, 600, 400), 0);
        let above = solid(&banner, rect(400, 40, 300, 80), 0);
        damage(&mut tracker, 0, std::slice::from_ref(&below));
        // Passes are given front to back: the banner over the window.
        let shown = damage(&mut tracker, 1, &[above.clone(), below.clone()]);
        assert_eq!(shown, vec![rect(400, 40, 300, 80)]);
        let hidden = damage(&mut tracker, 1, std::slice::from_ref(&below));
        assert_eq!(hidden, vec![rect(400, 40, 300, 80)]);
    }

    #[test]
    fn restacking_repaints_the_restacked_elements() {
        let (a, b) = (Id::new(), Id::new());
        let mut tracker = OutputDamageTracker::new((1280, 800), 1.0, Transform::Normal);
        let first = solid(&a, rect(0, 0, 100, 100), 0);
        let second = solid(&b, rect(50, 50, 100, 100), 0);
        damage(&mut tracker, 0, &[first.clone(), second.clone()]);
        let rects = damage(&mut tracker, 1, &[second, first]);
        assert!(contains(&rects, 10, 10) && contains(&rects, 140, 140));
    }

    #[test]
    fn coalesce_merges_overlaps_and_caps_the_rect_count() {
        let r = rect;
        // Overlapping rects merge (each pixel painted once); a merge that
        // creates a new overlap merges again; empties drop.
        let merged = coalesce(vec![
            r(0, 0, 10, 10),
            r(20, 0, 10, 10),
            r(5, 0, 20, 5),
            r(50, 50, 0, 4),
        ]);
        assert_eq!(merged, vec![r(0, 0, 30, 10)]);
        let disjoint = coalesce(vec![r(0, 0, 4, 4), r(100, 100, 4, 4)]);
        assert_eq!(disjoint.len(), 2);
        let many: Vec<_> = (0..=MAX_DAMAGE_RECTS as i32)
            .map(|i| r(i * 10, 0, 4, 4))
            .collect();
        assert_eq!(
            coalesce(many),
            vec![r(0, 0, MAX_DAMAGE_RECTS as i32 * 10 + 4, 4)]
        );
    }

    #[test]
    fn global_drawing_pass_order_and_opacity_changes_reset_buffer_history() {
        let id = Id::new();
        let capture = |x, commit| {
            let mut next = scene();
            next.size = (32, 32);
            next.groups = vec![moving(&id, x, commit).iter().map(sig).collect()];
            next
        };
        let before = capture(0, 0);
        let moved = capture(8, 1);
        assert!(needs_repaint(Some(&before), &moved, false));
        assert!(
            before.same_global_drawing(&moved),
            "element movement must not reset global buffer history"
        );
        let mut variants = vec![before.clone(); 4];
        variants[0].locked = true;
        variants[1].scale = 2.0;
        variants[2].groups[0][0].opaque.clear();
        variants[2].groups[0][0].opaque.push(rect(0, 0, 1, 1));
        variants[3].effects = vec![0.5f32.to_bits()];
        for (cause, next) in ["lock", "scale", "opacity", "effects"]
            .into_iter()
            .zip(variants)
        {
            assert!(
                !next.same_global_drawing(&before),
                "{cause} must reset buffer history"
            );
        }
        // A replaced element is the tracker's: old and new areas repaint.
        let mut replaced = before.clone();
        replaced.groups[0][0].id = Id::new();
        assert!(replaced.same_global_drawing(&before));
    }
}
