//! GNOME 51 window map/destroy effects (windowManager.js, tag 51.0).
//!
//! Import the last shown frame while the surface is alive. Only a destroy
//! effect copies that retained frame into an offscreen texture; destruction
//! never reads protocol state. Steady-state windows incur no offscreen copy.
use crate::size_change::{RectF, Snapshot};
use smithay::backend::renderer::{
    element::surface::WaylandSurfaceRenderElement, gles::GlesRenderer,
};
use smithay::utils::{Logical, Rectangle};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;
use tuna_shell_control::motion::MotionPolicy;

type Rect = Rectangle<i32, Logical>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Normal,
    Dialog,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Map,
    Destroy,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub scale: (f64, f64),
    pub alpha: f32,
    pub pivot: (f64, f64),
}

pub fn duration(kind: Kind, policy: MotionPolicy) -> Duration {
    Duration::from_secs_f64(
        policy.adjust_ms(match kind {
            Kind::Normal => 150.0,
            Kind::Dialog => 100.0,
        }) / 1000.0,
    )
}
pub fn frame(kind: Kind, effect: Effect, progress: f64, policy: MotionPolicy) -> Frame {
    if !policy.allows_fades() {
        return Frame {
            scale: (1.0, 1.0),
            alpha: if effect == Effect::Map { 1.0 } else { 0.0 },
            pivot: (0.5, 0.5),
        };
    }
    let p = progress.clamp(0.0, 1.0);
    let ease = if kind == Kind::Normal && effect == Effect::Map {
        if p >= 1.0 {
            1.0
        } else if p <= 0.0 {
            0.0
        } else {
            1.0 - 2.0_f64.powf(-10.0 * p)
        }
    } else {
        1.0 - (1.0 - p).powi(2)
    };
    let motion = policy.allows_motion();
    // Clutter interpolates the actor's opacity as a byte and truncates it
    // (GNOME 51 draws 94/255 ten milliseconds into a map).
    let opacity = |a: f64| ((255.0 * a) as u8) as f32 / 255.0;
    match (kind, effect) {
        (Kind::Normal, Effect::Map) => Frame {
            scale: if motion {
                (0.01 + 0.99 * ease, 0.05 + 0.95 * ease)
            } else {
                (1.0, 1.0)
            },
            alpha: opacity(ease),
            pivot: (0.5, 1.0),
        },
        (Kind::Normal, Effect::Destroy) => Frame {
            scale: if motion {
                (1.0 - 0.2 * ease, 1.0 - 0.2 * ease)
            } else {
                (1.0, 1.0)
            },
            alpha: opacity(1.0 - ease),
            pivot: (0.5, 0.5),
        },
        (Kind::Dialog, Effect::Map) => Frame {
            scale: (1.0, if motion { ease } else { 1.0 }),
            alpha: if motion { opacity(ease) } else { 1.0 },
            pivot: (0.5, 0.5),
        },
        (Kind::Dialog, Effect::Destroy) => Frame {
            scale: (1.0, if motion { 1.0 - ease } else { 1.0 }),
            alpha: if motion { 1.0 } else { opacity(1.0 - ease) },
            pivot: (0.5, 0.5),
        },
    }
}
pub fn rect(geometry: Rect, frame: Frame) -> RectF {
    rect_f(geometry.to_f64(), frame)
}
fn rect_f(geometry: RectF, frame: Frame) -> RectF {
    let w = geometry.size.w;
    let h = geometry.size.h;
    Rectangle::new(
        (
            geometry.loc.x + w * (1.0 - frame.scale.0) * frame.pivot.0,
            geometry.loc.y + h * (1.0 - frame.scale.1) * frame.pivot.1,
        )
            .into(),
        (w * frame.scale.0, h * frame.scale.1).into(),
    )
}
/// Lifecycle snapshots do not carry workspace transforms. Suppress them during
/// both keyboard slides and gestures rather than leaking content across spaces.
pub fn effects_allowed(overview: bool, gesture: bool, slide: bool, locked: bool) -> bool {
    !overview && !gesture && !slide && !locked
}
fn interrupted_close(mapping: bool, resizing: bool) -> Option<&'static str> {
    if mapping {
        Some("interrupted-map")
    } else if resizing {
        Some("interrupted-size-change")
    } else {
        None
    }
}
// A dead actor retains the presentation actually rendered during restore,
// rather than jumping back to normal geometry before its destroy curve.
fn retained_geometry(normal: Rect, presentation: RectF, transformed: bool) -> RectF {
    if transformed {
        presentation
    } else {
        normal.to_f64()
    }
}
fn live_snapshot_destination(
    geometry: RectF,
    source: smithay::utils::Size<i32, Logical>,
    view: crate::runtime::View,
) -> Rectangle<i32, smithay::utils::Physical> {
    // Smithay RescaleRenderElement first rounds the unscaled surface at the
    // output scale, then rounds its scaled SIZE independently of its origin.
    // Rounding transformed endpoints instead adds a row at fractional origins.
    let size = (
        ((f64::from(source.w) * view.scale).round() * geometry.size.w / f64::from(source.w)).round()
            as i32,
        ((f64::from(source.h) * view.scale).round() * geometry.size.h / f64::from(source.h)).round()
            as i32,
    );
    Rectangle::new(view.physical(geometry.loc.x, geometry.loc.y), size.into())
}

fn hidden_dead_actor(minimizing: bool, was_minimized: bool) -> bool {
    !minimizing && was_minimized
}
fn snapshot_allowed(count: usize, retained: u64, pixels: u64) -> bool {
    count < 32 && retained.saturating_add(pixels) <= 16 * 1024 * 1024
}
fn successors(stack: &[u64], visible: &HashSet<u64>) -> Vec<Option<u64>> {
    let mut result = vec![None; stack.len()];
    let mut next = None;
    for (rank, id) in stack.iter().enumerate().rev() {
        result[rank] = next;
        if visible.contains(id) {
            next = Some(*id);
        }
    }
    result
}
fn retire<T>(items: &mut Vec<T>, expired: impl Fn(&T) -> bool) -> Vec<T> {
    let mut removed = Vec::new();
    let mut active = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        if expired(&item) {
            removed.push(item);
        } else {
            active.push(item);
        }
    }
    *items = active;
    removed
}
struct Cached {
    geometry: Rect,
    presentation: RectF,
    scale: f64,
    kind: Kind,
    parent: Option<u64>,
    resizing: bool,
    restoring: bool,
    resize_snapshot: Option<
        smithay::backend::renderer::element::texture::TextureRenderElement<
            smithay::backend::renderer::gles::GlesTexture,
        >,
    >,
    brightness: f32,
    order: usize,
    elements: Vec<
        smithay::backend::renderer::element::utils::RescaleRenderElement<
            WaylandSurfaceRenderElement<GlesRenderer>,
        >,
    >,
}
#[derive(Clone, Copy)]
struct MinRun {
    start: Duration,
    icon: RectF,
    monitor_sized: bool,
}
struct Running {
    kind: Kind,
    start: Duration,
}
pub struct Ghost {
    pub id: u64,
    pub before: Option<u64>,
    order: usize,
    parent: Option<u64>,
    geometry: RectF,
    live_source_size: Option<smithay::utils::Size<i32, Logical>>,
    snapshot: Snapshot,
    pixels: u64,
    running: Running,
    minimizing: Option<MinRun>,
}
#[derive(Default)]
pub struct Lifecycle {
    known: HashSet<u64>,
    cache: HashMap<u64, Cached>,
    stack: Vec<u64>,
    opening: HashMap<u64, Running>,
    minimized: HashSet<u64>,
    restoring: HashMap<u64, MinRun>,
    minimizing: HashMap<u64, MinRun>,
    ghosts: Vec<Ghost>,
    history: VecDeque<serde_json::Value>,
    now: Duration,
    policy: MotionPolicy,
}
impl Lifecycle {
    fn suspend(&mut self, ids: HashSet<u64>) {
        self.opening.clear();
        self.restoring.clear();
        self.minimizing.clear();
        self.ghosts.clear();
        self.cache.clear();
        self.stack.clear();
        self.known = ids;
    }
    pub fn running(&self) -> bool {
        !self.opening.is_empty()
            || !self.ghosts.is_empty()
            || !self.restoring.is_empty()
            || !self.minimizing.is_empty()
    }

    pub fn opening_frame(&self, id: u64) -> Option<Frame> {
        let run = self.opening.get(&id)?;
        let d = duration(run.kind, self.policy).as_secs_f64();
        Some(frame(
            run.kind,
            Effect::Map,
            if d > 0.0 {
                self.now.saturating_sub(run.start).as_secs_f64() / d
            } else {
                1.0
            },
            self.policy,
        ))
    }
    pub fn is_minimizing(&self, id: u64) -> bool {
        self.minimizing.contains_key(&id)
    }
    pub fn minimizing_frame(&self, id: u64, normal: RectF) -> Option<(RectF, f32)> {
        let run = self.minimizing.get(&id)?;
        let d = crate::minimize::duration(self.policy).as_secs_f64();
        let progress = if d > 0.0 {
            self.now.saturating_sub(run.start).as_secs_f64() / d
        } else {
            1.0
        };
        let f = crate::minimize::frame(
            crate::minimize::Effect::Minimize,
            progress,
            self.policy,
            run.monitor_sized,
        );
        Some((
            crate::minimize::geometry(normal, run.icon, f.icon_weight),
            f.alpha,
        ))
    }
    pub fn restoring_frame(&self, id: u64, normal: RectF) -> Option<(RectF, f32)> {
        let run = self.restoring.get(&id)?;
        let d = crate::minimize::duration(self.policy).as_secs_f64();
        let progress = if d > 0.0 {
            self.now.saturating_sub(run.start).as_secs_f64() / d
        } else {
            1.0
        };
        let f = crate::minimize::frame(
            crate::minimize::Effect::Unminimize,
            progress,
            self.policy,
            run.monitor_sized,
        );
        Some((
            crate::minimize::geometry(normal, run.icon, f.icon_weight),
            f.alpha,
        ))
    }
    pub fn ghosts(&self) -> impl Iterator<Item = &Ghost> {
        self.ghosts.iter()
    }
    fn destroy_frame(&self, ghost: &Ghost) -> Frame {
        let d = duration(ghost.running.kind, self.policy).as_secs_f64();
        frame(
            ghost.running.kind,
            Effect::Destroy,
            if d > 0.0 {
                self.now.saturating_sub(ghost.running.start).as_secs_f64() / d
            } else {
                1.0
            },
            self.policy,
        )
    }
    pub fn element(
        &self,
        ghost: &Ghost,
        renderer: &GlesRenderer,
        view: crate::runtime::View,
    ) -> Option<
        smithay::backend::renderer::element::texture::TextureRenderElement<
            smithay::backend::renderer::gles::GlesTexture,
        >,
    > {
        let f = self.destroy_frame(ghost);
        let (geometry, alpha) = if let Some(run) = ghost.minimizing {
            let d = crate::minimize::duration(self.policy).as_secs_f64();
            let progress = if d > 0.0 {
                self.now.saturating_sub(run.start).as_secs_f64() / d
            } else {
                1.0
            };
            let f = crate::minimize::frame(
                crate::minimize::Effect::Minimize,
                progress,
                self.policy,
                run.monitor_sized,
            );
            (
                crate::minimize::geometry(ghost.geometry, run.icon, f.icon_weight),
                f.alpha,
            )
        } else {
            (rect_f(ghost.geometry, f), f.alpha)
        };
        if let Some(source) = ghost.live_source_size {
            return crate::size_change::snapshot_element_in_rect(
                renderer,
                &ghost.snapshot,
                alpha,
                live_snapshot_destination(geometry, source, view),
            );
        }
        crate::size_change::snapshot_element(
            renderer,
            &ghost.snapshot,
            &crate::size_change::Frame {
                live: geometry,
                live_scale: f.scale,
                live_visible: false,
                snapshot: geometry,
                snapshot_alpha: alpha,
            },
            view,
        )
    }
    fn record(&mut self, id: u64, effect: &str, kind: Kind, outcome: &str) {
        self.history.push_back(serde_json::json!({"window":id,"effect":effect,"kind":format!("{kind:?}"),"outcome":outcome,"at_ms":self.now.as_millis(),"duration_ms":if effect=="minimize" || effect=="unminimize" {crate::minimize::duration(self.policy).as_millis()} else {duration(kind,self.policy).as_millis()}}));
        while self.history.len() > 32 {
            self.history.pop_front();
        }
    }
    pub fn to_json(&self) -> serde_json::Value {
        let frame_json = |f: Frame| {
            serde_json::json!({
                "scale": f.scale, "alpha": f.alpha, "pivot": f.pivot,
            })
        };
        // Where the whole buffer (shadows included) is drawn this frame.
        let rf = |r: RectF| [r.loc.x, r.loc.y, r.size.w, r.size.h];
        let drawn = |id: &u64| self.cache.get(id).map(|c| rf(c.presentation));
        let opening: Vec<_> = self
            .opening
            .iter()
            .map(|(id, run)| {
                serde_json::json!({
                    "window": id, "kind": format!("{:?}",run.kind),
                    "elapsed_ms": self.now.saturating_sub(run.start).as_millis(),
                    "duration_ms": duration(run.kind,self.policy).as_millis(),
                    "frame": self.opening_frame(*id).map(frame_json),
                    "drawn": drawn(id),
                })
            })
            .collect();
        let destroying: Vec<_> = self
            .ghosts
            .iter()
            .filter(|g| g.minimizing.is_none())
            .map(|g| {
                serde_json::json!({
                    "window": g.id, "kind": format!("{:?}",g.running.kind),
                    "elapsed_ms": self.now.saturating_sub(g.running.start).as_millis(),
                    "duration_ms": duration(g.running.kind,self.policy).as_millis(),
                    "frame": frame_json(self.destroy_frame(g)),
                    "drawn": rf(rect_f(g.geometry, self.destroy_frame(g))),
                })
            })
            .collect();
        let mut minimizing:Vec<_>=self.ghosts.iter().filter_map(|g|g.minimizing.map(|r| {
            let progress=self.now.saturating_sub(r.start).as_secs_f64()/crate::minimize::duration(self.policy).as_secs_f64();
            let f=crate::minimize::frame(crate::minimize::Effect::Minimize,progress,self.policy,r.monitor_sized);
            let rect=crate::minimize::geometry(g.geometry,r.icon,f.icon_weight);
            serde_json::json!({"window":g.id,"icon":[r.icon.loc.x,r.icon.loc.y,r.icon.size.w,r.icon.size.h],"rect":[rect.loc.x,rect.loc.y,rect.size.w,rect.size.h],"alpha":f.alpha,"monitor_sized":r.monitor_sized,"duration_ms":crate::minimize::duration(self.policy).as_millis()})
        })).collect();
        minimizing.extend(self.minimizing.iter().filter_map(|(id, run)| {
            let normal = self.cache.get(id)?.geometry.to_f64();
            let (rect, alpha) = self.minimizing_frame(*id, normal)?;
            Some(serde_json::json!({"window":id,"icon":[run.icon.loc.x,run.icon.loc.y,run.icon.size.w,run.icon.size.h],"rect":[rect.loc.x,rect.loc.y,rect.size.w,rect.size.h],"alpha":alpha,"monitor_sized":run.monitor_sized,"duration_ms":crate::minimize::duration(self.policy).as_millis(),"live":true}))
        }));
        let restoring:Vec<_>=self.restoring.iter().map(|(id,r)|serde_json::json!({"window":id,"elapsed_ms":self.now.saturating_sub(r.start).as_millis(),"duration_ms":crate::minimize::duration(self.policy).as_millis(),"icon":[r.icon.loc.x,r.icon.loc.y,r.icon.size.w,r.icon.size.h],"monitor_sized":r.monitor_sized,"drawn":drawn(id)})).collect();
        serde_json::json!({
            "retained_destroy_bytes": self.ghosts.iter().map(|g|g.pixels*4).sum::<u64>(),
            "snapshot_budget_bytes": 64*1024*1024,
            "snapshot_effect_limit": 32,
            "opening": opening, "destroying": destroying, "minimizing":minimizing,"restoring":restoring,"history": self.history,
        })
    }
}

pub fn step_frame(
    manager: &mut crate::windows::WindowManager,
    renderer: &mut GlesRenderer,
    state: &crate::State,
    now: Duration,
    policy: MotionPolicy,
    allowed: bool,
    destination: crate::minimize::TargetContext,
) {
    use smithay::backend::renderer::element::{
        surface::render_elements_from_surface_tree, Kind as RenderKind,
    };
    use smithay::wayland::seat::WaylandFocus;
    let ids: HashSet<u64> = manager.model().windows().map(|w| w.id).collect();
    let buffered: HashSet<u64> = ids
        .iter()
        .copied()
        .filter(|id| {
            manager
                .surface_of(*id)
                .as_ref()
                .and_then(crate::windows::committed_size)
                .is_some_and(|size| size.w > 0 && size.h > 0)
        })
        .collect();
    let minimized: HashSet<u64> = ids
        .iter()
        .copied()
        .filter(|id| manager.is_minimized(*id))
        .collect();
    let targets: HashMap<u64, (RectF, bool)> = ids
        .iter()
        .filter_map(|id| {
            let geometry = manager.geometry(*id)?;
            let monitor = state.monitor_geometry(geometry);
            let icon = crate::minimize::target(
                manager.client_icon_geometry(*id),
                manager.icon_geometry(*id),
                destination.dash_visible,
                monitor,
                destination.right_to_left,
            );
            Some((
                *id,
                (
                    icon,
                    crate::minimize::monitor_sized(
                        manager.is_fullscreen(*id),
                        geometry,
                        state.desktop_size(),
                    ),
                ),
            ))
        })
        .collect();
    let newly_minimized: Vec<_> = minimized
        .difference(&manager.window_lifecycle().minimized)
        .copied()
        .collect();
    let hidden = manager.hidden_by_minimize();
    // Mutter kills prior window effects before minimizing and continues to
    // sample the live surface. Only a surface that dies needs a retained copy.
    for id in newly_minimized {
        if !manager.window_lifecycle().cache.contains_key(&id) {
            continue;
        }
        manager
            .size_changes_mut()
            .end(id, crate::size_change::Outcome::Cancelled, now);
        let life = manager.window_lifecycle_mut();
        life.opening.remove(&id);
        life.restoring.remove(&id);
        if let Some((icon, monitor_sized)) = targets.get(&id) {
            if let Some(cached) = life.cache.get(&id) {
                let kind = cached.kind;
                life.record(id, "minimize", kind, "started-live");
            }
            life.minimizing.insert(
                id,
                MinRun {
                    start: now,
                    icon: *icon,
                    monitor_sized: *monitor_sized,
                },
            );
        }
    }
    let restored_ids: Vec<_> = manager
        .window_lifecycle()
        .minimized
        .difference(&minimized)
        .copied()
        .filter(|id| ids.contains(id))
        .collect();
    for id in restored_ids {
        manager
            .size_changes_mut()
            .end(id, crate::size_change::Outcome::Cancelled, now);
    }
    let entries = manager.render_entries();
    // Register actor clocks before caching. Minimize cancels prior effects;
    // its cache contains untransformed imports for safe promotion after death.
    // Hidden transients still run their map clock; becoming visible later
    // must not restart an already completed actor effect.
    let map_candidates: Vec<_> = buffered
        .iter()
        .filter_map(|id| manager.lifecycle_kind(*id).map(|kind| (*id, kind)))
        .collect();
    {
        let life = manager.window_lifecycle_mut();
        life.now = now;
        life.policy = policy;
        if !allowed || !policy.allows_fades() {
            life.suspend(ids);
            life.minimized = minimized;
            return;
        }
        let restored: Vec<_> = life
            .minimized
            .difference(&minimized)
            .copied()
            .filter(|id| ids.contains(id))
            .collect();
        for id in restored {
            life.ghosts.retain(|g| g.id != id);
            life.minimizing.remove(&id);
            life.opening.remove(&id);
            if hidden.contains(&id) {
                continue;
            }
            if let Some((icon, monitor_sized)) = targets.get(&id) {
                let kind = map_candidates.iter().find(|e| e.0 == id).map(|e| e.1);
                if !crate::minimize::restore_animates(policy, *monitor_sized) {
                    if let Some(kind) = kind {
                        life.record(id, "unminimize", kind, "instant");
                    }
                    continue;
                }
                life.restoring.insert(
                    id,
                    MinRun {
                        start: now,
                        icon: *icon,
                        monitor_sized: *monitor_sized,
                    },
                );
                if let Some(kind) = kind {
                    life.record(id, "unminimize", kind, "started");
                }
            }
        }
        for (id, kind) in map_candidates {
            if life.known.insert(id) {
                life.opening.insert(id, Running { kind, start: now });
                life.record(id, "map", kind, "started");
            }
        }
    }
    let mut shown = Vec::new();
    if allowed && policy.allows_fades() {
        for (index, (id, window, geometry)) in entries.iter().enumerate() {
            let Some(kind) = manager.lifecycle_kind(*id) else {
                continue;
            };
            let Some(surface) = window.wl_surface() else {
                continue;
            };
            let Some(size) = crate::windows::buffer_size(surface.as_ref()) else {
                continue;
            };
            if size.w <= 0 || size.h <= 0 {
                continue;
            }
            let scale = state.scale_for(*geometry).fractional_scale();
            let geo = crate::popup::window_geometry_loc(&surface);
            let buffer_geometry =
                Rect::new(crate::popup::surface_origin(&surface, geometry.loc), size);
            let restoring = manager
                .window_lifecycle()
                .restoring_frame(*id, buffer_geometry.to_f64());
            let mapped = manager.window_lifecycle().opening_frame(*id);
            let (
                mut capture_geometry,
                mut presentation,
                mut fade,
                mut live_scale,
                mut live_visible,
            ) = (
                buffer_geometry,
                buffer_geometry.to_f64(),
                1.0,
                (1.0, 1.0),
                true,
            );
            if let Some((r, alpha)) = restoring {
                presentation = r;
                fade = alpha;
            } else if let Some(f) = mapped {
                // GNOME scales the whole actor, client shadows included, so
                // the pivot sits on the buffer rect (windowManager.js:1514).
                presentation = rect(buffer_geometry, f);
                fade = f.alpha;
            }
            let resizing = manager.size_changes().frame(*id);
            let mut resize_snapshot = None;
            let mut origin = (0, 0).into();
            if let Some((f, snapshot)) = resizing {
                live_scale = f.live_scale;
                live_visible = f.live_visible;
                fade = 1.0;
                let live = Rectangle::new(
                    (
                        f.live.loc.x - f64::from(geo.x) * live_scale.0,
                        f.live.loc.y - f64::from(geo.y) * live_scale.1,
                    )
                        .into(),
                    (
                        f64::from(size.w) * live_scale.0,
                        f64::from(size.h) * live_scale.1,
                    )
                        .into(),
                );
                let bounds = if live_visible {
                    crate::minimize::bounds(live, f.snapshot)
                } else {
                    f.snapshot
                };
                capture_geometry = bounds.to_i32_up();
                presentation = capture_geometry.to_f64();
                let view = crate::runtime::View {
                    offset: (capture_geometry.loc.x, capture_geometry.loc.y),
                    scale,
                };
                origin = view.physical(live.loc.x, live.loc.y);
                resize_snapshot =
                    crate::size_change::snapshot_element(renderer, snapshot, &f, view);
            }
            let elements: Vec<_> = if live_visible {
                render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<GlesRenderer>>(
                    renderer,
                    &surface,
                    origin,
                    scale,
                    fade,
                    RenderKind::Unspecified,
                )
                .into_iter()
                .map(|e| {
                    smithay::backend::renderer::element::utils::RescaleRenderElement::from_element(
                        e, origin, live_scale,
                    )
                })
                .collect()
            } else {
                Vec::new()
            };
            if elements.is_empty() && resize_snapshot.is_none() {
                continue;
            }
            shown.push((
                *id,
                Cached {
                    geometry: capture_geometry,
                    presentation,
                    scale,
                    kind,
                    parent: manager.transient_parent(*id),
                    resizing: resizing.is_some(),
                    restoring: restoring.is_some(),
                    resize_snapshot,
                    brightness: manager.modal_dim().multiplier(*id),
                    order: index,
                    elements,
                },
            ));
        }
    }
    let life = manager.window_lifecycle_mut();
    life.now = now;
    life.policy = policy;
    if !allowed || !policy.allows_fades() {
        life.suspend(ids);
        life.minimized = minimized;
        return;
    }
    // Release expired textures before admitting new snapshots to the budget.
    let finished = retire(&mut life.ghosts, |g| {
        now.saturating_sub(g.running.start)
            >= if g.minimizing.is_some() {
                crate::minimize::duration(policy)
            } else {
                duration(g.running.kind, policy)
            }
            || (g.minimizing.is_none() && g.parent.is_some_and(|p| !ids.contains(&p)))
    });
    for g in finished {
        life.record(
            g.id,
            if g.minimizing.is_some() {
                "minimize"
            } else {
                "destroy"
            },
            g.running.kind,
            "completed",
        );
    }
    life.restoring.retain(|id, r| {
        ids.contains(id)
            && !minimized.contains(id)
            && now.saturating_sub(r.start) < crate::minimize::duration(policy)
    });
    let visible_ids = entries.iter().map(|e| e.0).collect();
    let next = successors(&life.stack, &visible_ids);
    let gone: Vec<_> = life
        .cache
        .keys()
        .copied()
        .filter(|id| !ids.contains(id))
        .collect();
    for id in gone {
        let old = life.cache.remove(&id).expect("cached id");
        let min_run = life.minimizing.remove(&id);
        let minimizing = min_run.is_some();
        if hidden_dead_actor(minimizing, life.minimized.contains(&id)) {
            life.record(id, "destroy", old.kind, "minimized-hidden");
            continue;
        }
        if min_run
            .is_some_and(|run| now.saturating_sub(run.start) >= crate::minimize::duration(policy))
        {
            life.record(id, "minimize", old.kind, "completed");
            continue;
        }
        let effect = if minimizing { "minimize" } else { "destroy" };
        let mapping = life.opening.remove(&id).is_some();
        // Raw cached elements exclude active map/resize transforms. A safe
        // immediate close is preferable to an opaque full-size frozen jump.
        if let Some(reason) = interrupted_close(mapping && !minimizing, old.resizing && !minimizing)
        {
            life.record(id, effect, old.kind, reason);
            continue;
        }
        if !minimizing
            && old
                .parent
                .is_some_and(|parent| !ids.contains(&parent) || hidden.contains(&parent))
        {
            life.record(id, effect, old.kind, "parent-hidden-or-gone");
            continue;
        }
        let pixels = ((f64::from(old.geometry.size.w) * old.scale).round() as u64)
            .saturating_mul((f64::from(old.geometry.size.h) * old.scale).round() as u64);
        // Bound retained GPU copies to 64 MiB and 32 simultaneous effects.
        if !snapshot_allowed(
            life.ghosts.len(),
            life.ghosts.iter().map(|g| g.pixels).sum(),
            pixels,
        ) {
            life.record(id, effect, old.kind, "snapshot-budget");
            continue;
        }
        if let Some(texture) = copy_frame(renderer, &old) {
            life.record(
                id,
                effect,
                old.kind,
                if old.restoring {
                    "started-from-restore"
                } else {
                    "started"
                },
            );
            life.ghosts.push(Ghost {
                id,
                before: next.get(old.order).copied().flatten(),
                order: old.order,
                parent: old.parent,
                geometry: retained_geometry(
                    old.geometry,
                    old.presentation,
                    minimizing || old.restoring,
                ),
                pixels,
                live_source_size: (minimizing || old.restoring).then_some(old.geometry.size),
                snapshot: Snapshot {
                    texture,
                    id: smithay::backend::renderer::element::Id::new(),
                },
                running: Running {
                    kind: old.kind,
                    start: min_run.map_or(now, |run| run.start),
                },
                minimizing: min_run,
            });
        } else {
            life.record(id, effect, old.kind, "snapshot-failed");
        }
    }
    let completed_min: Vec<_> = life
        .minimizing
        .iter()
        .filter(|(_, run)| now.saturating_sub(run.start) >= crate::minimize::duration(policy))
        .map(|(id, _)| *id)
        .collect();
    for id in completed_min {
        life.minimizing.remove(&id);
        if let Some(cached) = life.cache.get(&id) {
            let kind = cached.kind;
            life.record(id, "minimize", kind, "completed");
        }
    }
    life.ghosts.sort_by_key(|g| g.order);
    let completed: Vec<_> = life
        .opening
        .iter()
        .filter(|(id, r)| {
            !ids.contains(id) || now.saturating_sub(r.start) >= duration(r.kind, policy)
        })
        .map(|(id, r)| (*id, r.kind))
        .collect();
    for (id, kind) in completed {
        life.opening.remove(&id);
        life.record(id, "map", kind, "completed");
    }
    let visible: HashSet<_> = shown.iter().map(|e| e.0).collect();
    life.cache.retain(|id, _| visible.contains(id));
    for (id, cached) in shown {
        life.cache.insert(id, cached);
    }
    life.minimized = minimized;
    life.stack = entries.iter().map(|e| e.0).collect();
    // A first commit on a hidden workspace is a map too: showing that
    // workspace later must not replay a fresh-window effect.
    life.known.extend(buffered);
    life.known.retain(|id| ids.contains(id));
}
fn copy_frame(
    renderer: &mut GlesRenderer,
    cached: &Cached,
) -> Option<smithay::backend::renderer::gles::GlesTexture> {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::{
        utils::draw_render_elements, Bind, Color32F, Frame as _, Offscreen, Renderer,
    };
    use smithay::utils::{Physical, Size, Transform};
    let size: Size<i32, Physical> = (
        (f64::from(cached.geometry.size.w) * cached.scale).round() as i32,
        (f64::from(cached.geometry.size.h) * cached.scale).round() as i32,
    )
        .into();
    let dim_program = if cached.brightness < 1.0 {
        Some(crate::modal_dim::program(renderer)?)
    } else {
        None
    };
    let mut texture: smithay::backend::renderer::gles::GlesTexture = renderer
        .create_buffer(Fourcc::Abgr8888, (size.w, size.h).into())
        .ok()?;
    {
        let mut target = renderer.bind(&mut texture).ok()?;
        let mut frame = renderer.render(&mut target, size, Transform::Normal).ok()?;
        let all = Rectangle::from_size(size);
        frame
            .clear(Color32F::new(0.0, 0.0, 0.0, 0.0), &[all])
            .ok()?;
        if let Some(program) = dim_program {
            frame.override_default_tex_program(
                program,
                vec![smithay::backend::renderer::gles::Uniform::new(
                    "brightness",
                    cached.brightness,
                )],
            );
        }
        draw_render_elements(&mut frame, cached.scale, &cached.elements, &[all]).ok()?;
        if let Some(snapshot) = &cached.resize_snapshot {
            draw_render_elements::<GlesRenderer, _, _>(
                &mut frame,
                1.0,
                std::slice::from_ref(snapshot),
                &[all],
            )
            .ok()?;
        }
        frame.clear_tex_program_override();
        let _ = frame.finish().ok()?;
    }
    Some(texture)
}
#[cfg(test)]
mod tests {
    use super::*;
    use tuna_shell_control::motion::MotionLevel;
    fn policy(level: MotionLevel) -> MotionPolicy {
        MotionPolicy::new(level, 1.0)
    }
    #[test]
    fn retained_restore_uses_live_size_rounding_not_endpoint_rounding() {
        let weight = 2f64.powf(-2.5);
        let geometry = Rectangle::new(
            (390.0 * (1.0 - weight), 241.0 * (1.0 - weight)).into(),
            (500.0 * (1.0 - weight), 350.0 * (1.0 - weight)).into(),
        );
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let view = crate::runtime::View {
                offset: (0, 0),
                scale,
            };
            let dest = live_snapshot_destination(geometry, (500, 350).into(), view);
            assert_eq!(dest.loc, view.physical(geometry.loc.x, geometry.loc.y));
            assert_eq!(
                dest.size.h,
                ((350.0 * scale).round() * (1.0 - weight)).round() as i32
            );
            if scale == 1.0 {
                assert_eq!(dest.size.h, 288);
                let end = view.physical(
                    geometry.loc.x + geometry.size.w,
                    geometry.loc.y + geometry.size.h,
                );
                assert_eq!(
                    end.y - dest.loc.y,
                    289,
                    "failed native capture added exactly one row"
                );
            }
        }
    }

    #[test]
    fn destroyed_restore_keeps_last_rendered_geometry_without_changing_normal_close() {
        let normal = Rect::new((100, 200).into(), (500, 350).into());
        let shown = RectF::new((75.25, 160.5).into(), (420.75, 300.25).into());
        assert_eq!(retained_geometry(normal, shown, true), shown);
        assert_eq!(retained_geometry(normal, shown, false), normal.to_f64());
        let start = frame(Kind::Normal, Effect::Destroy, 0.0, MotionPolicy::default());
        assert_eq!(
            rect_f(shown, start),
            shown,
            "retained fractional transform must not snap to integer geometry"
        );
    }
    #[test]
    fn hidden_actor_death_after_minimize_deadline_does_not_restart_destroy() {
        assert!(hidden_dead_actor(false, true));
        assert!(!hidden_dead_actor(true, true));
        assert!(!hidden_dead_actor(false, false));
    }
    #[test]
    fn live_minimize_frame_uses_latest_buffer_geometry_and_owns_no_snapshot() {
        let mut life = Lifecycle {
            policy: MotionPolicy::default(),
            now: Duration::from_millis(100),
            ..Default::default()
        };
        let icon = Rect::new((900, 700).into(), (64, 64).into()).to_f64();
        life.minimizing.insert(
            7,
            MinRun {
                start: Duration::ZERO,
                icon,
                monitor_sized: false,
            },
        );
        let normal = Rect::new((100, 200).into(), (500, 350).into()).to_f64();
        let (geometry, alpha) = life.minimizing_frame(7, normal).unwrap();
        assert_eq!(alpha, 45.0 / 255.0);
        assert_eq!(
            geometry,
            crate::minimize::geometry(normal, icon, 1.0 - 2f64.powf(-2.5))
        );
        assert!(life.ghosts.is_empty());
        assert!(life.running());
        life.suspend(HashSet::from([7]));
        assert!(!life.is_minimizing(7));
    }
    #[test]
    fn keyboard_workspace_slide_suppresses_lifecycle_and_resume_does_not_replay_map() {
        assert!(effects_allowed(false, false, false, false));
        assert!(!effects_allowed(false, false, true, false));
        let mut life = Lifecycle::default();
        life.opening.insert(
            7,
            Running {
                kind: Kind::Normal,
                start: Duration::ZERO,
            },
        );
        life.stack.push(7);
        life.suspend(HashSet::from([7]));
        assert!(!life.running());
        assert!(life.stack.is_empty());
        assert!(!life.known.insert(7));
    }
    #[test]
    fn interrupted_map_and_size_change_close_skip_raw_snapshot() {
        assert_eq!(interrupted_close(true, false), Some("interrupted-map"));
        assert_eq!(
            interrupted_close(false, true),
            Some("interrupted-size-change")
        );
        assert_eq!(interrupted_close(false, false), None);
    }
    #[test]
    fn expired_ghosts_release_budget_before_new_close() {
        let mut retained = vec![Duration::ZERO; 32];
        let removed = retire(&mut retained, |start| {
            Duration::from_millis(150).saturating_sub(*start) >= Duration::from_millis(150)
        });
        assert_eq!(removed.len(), 32);
        assert!(snapshot_allowed(retained.len(), 0, 1024));
        assert!(!snapshot_allowed(32, 0, 1));
        assert!(!snapshot_allowed(0, u64::MAX, 1));
    }
    #[test]
    fn shared_stack_successors_preserve_order_without_hidden_windows() {
        assert_eq!(
            successors(&[1, 2, 3, 4], &HashSet::from([1, 4])),
            vec![Some(4), Some(4), Some(4), None]
        );
    }
    #[test]
    fn gnome51_normal_map_pivot_and_exponential_curve() {
        let p = policy(MotionLevel::Full);
        let start = frame(Kind::Normal, Effect::Map, 0.0, p);
        assert_eq!(start.scale, (0.01, 0.05));
        assert_eq!(start.alpha, 0.0);
        assert_eq!(start.pivot, (0.5, 1.0));
        let mid = frame(Kind::Normal, Effect::Map, 0.5, p);
        assert_eq!(mid.alpha, 247.0 / 255.0);
        // GNOME 51 paired capture (tests/gnome-reference/motion-reference.json):
        // ten milliseconds in, Clutter's byte opacity is 94.
        assert_eq!(
            frame(Kind::Normal, Effect::Map, 10.0 / 150.0, p).alpha,
            94.0 / 255.0
        );
        assert_eq!(frame(Kind::Normal, Effect::Map, 1.0, p).scale, (1.0, 1.0));
        assert_eq!(duration(Kind::Normal, p), Duration::from_millis(150));
    }
    #[test]
    fn destroy_and_attached_dialog_follow_quad() {
        let p = policy(MotionLevel::Full);
        let mid = frame(Kind::Normal, Effect::Destroy, 0.5, p);
        assert_eq!(mid.scale, (0.85, 0.85));
        assert_eq!(mid.alpha, 63.0 / 255.0);
        let d = frame(Kind::Dialog, Effect::Destroy, 0.5, p);
        assert_eq!(d.scale, (1.0, 0.25));
        assert_eq!(d.alpha, 1.0);
        assert_eq!(duration(Kind::Dialog, p), Duration::from_millis(100));
    }
    #[test]
    fn reduced_motion_and_slowdown() {
        let p = policy(MotionLevel::FadeOnly);
        let f = frame(Kind::Normal, Effect::Destroy, 0.5, p);
        assert_eq!(f.scale, (1.0, 1.0));
        assert_eq!(f.alpha, 63.0 / 255.0);
        let d = frame(Kind::Dialog, Effect::Map, 0.0, p);
        assert_eq!(d.scale, (1.0, 1.0));
        assert_eq!(d.alpha, 1.0);
        assert_eq!(
            duration(Kind::Normal, MotionPolicy::new(MotionLevel::Full, 2.0)),
            Duration::from_millis(300)
        );
    }
    #[test]
    fn animations_off_land_without_scale_or_fade() {
        let p = policy(MotionLevel::Off);
        let opened = frame(Kind::Normal, Effect::Map, 0.0, p);
        assert_eq!(opened.scale, (1.0, 1.0));
        assert_eq!(opened.alpha, 1.0);
        assert_eq!(frame(Kind::Dialog, Effect::Destroy, 0.0, p).alpha, 0.0);
        assert_eq!(duration(Kind::Normal, p), Duration::ZERO);
    }
    #[test]
    fn pivot_keeps_bottom_of_map_and_centre_of_destroy_fixed() {
        let r = Rect::new((100, 200).into(), (400, 300).into());
        let p = policy(MotionLevel::Full);
        let mapped = rect(r, frame(Kind::Normal, Effect::Map, 0.0, p));
        assert_eq!(mapped.loc.y + mapped.size.h, 500.0);
        assert_eq!(mapped.loc.x + mapped.size.w / 2.0, 300.0);
        let closed = rect(r, frame(Kind::Normal, Effect::Destroy, 1.0, p));
        assert_eq!(closed.loc.y + closed.size.h / 2.0, 350.0);
    }
    #[test]
    fn map_pivot_is_the_buffer_bottom_like_gnome() {
        // GNOME 51 scales the window actor, shadows included, about the
        // bottom centre of its buffer: 10 ms into the paired capture the
        // 528x379 buffer at (376, 229) is drawn at (540.6, 455.8) 198.7x152.2.
        let buffer = Rect::new((376, 229).into(), (528, 379).into());
        let drawn = rect(
            buffer,
            frame(
                Kind::Normal,
                Effect::Map,
                10.0 / 150.0,
                policy(MotionLevel::Full),
            ),
        );
        for (got, want) in [
            (drawn.loc.x, 540.6),
            (drawn.loc.y, 455.8),
            (drawn.size.w, 198.7),
            (drawn.size.h, 152.2),
        ] {
            assert!((got - want).abs() < 0.5, "{drawn:?}");
        }
    }
}
