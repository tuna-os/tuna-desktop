//! GNOME 51's panel menu close (`boxpointer.js` `BoxPointer.close`, tag
//! 51.0) for the GTK shell's panel popovers.
//!
//! GTK hides a popover at once: its `closed` signal, visibility, focus and
//! accessible state all change in the same main-loop turn, and an outside
//! click unmaps the xdg_popup before GTK even hears of it. So the shell
//! closes its menus logically at once and the compositor draws the close:
//! while a grabbed popup of the shell's top bar is shown its last frame is
//! retained (imported elements, no copy), and when the popup goes that
//! frame is copied once into a texture and eased out over
//! `POPUP_ANIMATION_TIME` with EASE_OUT_QUAD: opacity 1→0 and, unless the
//! close is fade-only, a rise of `-arrow-rise` (6 px) and a shrink to 96%
//! about the menu's top centre. The ghost is never an input target.
//!
//! `PopupMenu.close` animates FULL by default; `PopupMenuManager` closes
//! the old menu fade-only when another menu takes over, which here is a
//! close while another grabbed menu of the same bar is up. Reduced Motion
//! keeps only the fade, animations off (or a suspended frame: overview,
//! gesture, slide, lock) draws nothing, and the slow-down factor stretches
//! the duration. A menu that opens again drops its ghost, as GNOME's
//! `open` replaces the close transition.
use crate::size_change::{RectF, Snapshot};
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Rectangle, Size};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;
use tuna_shell_control::motion::MotionPolicy;

/// `boxpointer.js` POPUP_ANIMATION_TIME.
pub const MENU_MS: f64 = 150.0;
/// `-arrow-rise` of `.popup-menu-boxpointer` (`$base_padding`).
pub const RISE: f64 = 6.0;
/// `BoxPointer.close` shrinks the menu to 96%.
pub const SCALE: f64 = 0.96;
/// Retained copies are bounded like window destroy effects.
const MAX_GHOSTS: usize = 8;

pub fn duration(policy: MotionPolicy) -> Duration {
    Duration::from_secs_f64(policy.adjust_ms(MENU_MS) / 1000.0)
}

/// One frame of a closing menu.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub alpha: f64,
    /// Vertical translation, negative upward (arrow on top).
    pub dy: f64,
    pub scale: f64,
}

/// `BoxPointer.close` at linear `progress`: all properties ease out quad
/// from rest; `motion` is `useMotion && animate & SLIDE`.
pub fn frame(progress: f64, motion: bool) -> Frame {
    let p = progress.clamp(0.0, 1.0);
    let e = 1.0 - (1.0 - p).powi(2);
    Frame {
        alpha: 1.0 - e,
        dy: if motion { -RISE * e } else { 0.0 },
        scale: if motion { 1.0 - (1.0 - SCALE) * e } else { 1.0 },
    }
}

/// `rect` (the copied surface) scaled about `pivot` (the top centre of the
/// menu's window geometry) and then translated by `dy`.
pub fn transformed(rect: RectF, pivot: (f64, f64), f: Frame) -> RectF {
    Rectangle::new(
        (
            pivot.0 + (rect.loc.x - pivot.0) * f.scale,
            pivot.1 + (rect.loc.y - pivot.1) * f.scale + f.dy,
        )
            .into(),
        (rect.size.w * f.scale, rect.size.h * f.scale).into(),
    )
}

/// Whether the close of a menu is fade-only: another menu of the same bar
/// holds the grab (`PopupMenuManager` switching, `close({fadeOnly})`).
pub fn fade_only(sibling_grabbed: bool) -> bool {
    sibling_grabbed
}

struct Retained {
    parent: WlSurface,
    rect: Rectangle<i32, Logical>,
    pivot: (f64, f64),
    scale: f64,
    elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
}

pub struct Ghost {
    surface: WlSurface,
    parent: WlSurface,
    rect: RectF,
    pivot: (f64, f64),
    snapshot: Snapshot,
    motion: bool,
    fade_only: bool,
    start: Duration,
}

#[derive(Default)]
pub struct MenuClose {
    retained: HashMap<WlSurface, Retained>,
    ghosts: Vec<Ghost>,
    history: VecDeque<serde_json::Value>,
    now: Duration,
    policy: MotionPolicy,
}

impl MenuClose {
    pub fn running(&self) -> bool {
        !self.ghosts.is_empty()
    }

    fn progress(&self, ghost: &Ghost) -> f64 {
        let d = duration(self.policy).as_secs_f64();
        if d > 0.0 {
            self.now.saturating_sub(ghost.start).as_secs_f64() / d
        } else {
            1.0
        }
    }

    fn ghost_frame(&self, ghost: &Ghost) -> Frame {
        frame(self.progress(ghost), ghost.motion)
    }

    /// Closing menus of the layer `parent`, bottom to top.
    pub fn ghosts_of<'a>(&'a self, parent: &'a WlSurface) -> impl Iterator<Item = &'a Ghost> {
        self.ghosts.iter().filter(move |g| &g.parent == parent)
    }

    /// The ghost as a texture element for one output, drawn at scale 1.
    pub fn element(
        &self,
        ghost: &Ghost,
        renderer: &GlesRenderer,
        view: crate::runtime::View,
    ) -> Option<smithay::backend::renderer::element::texture::TextureRenderElement<GlesTexture>>
    {
        let f = self.ghost_frame(ghost);
        let r = transformed(ghost.rect, ghost.pivot, f);
        crate::size_change::snapshot_element(
            renderer,
            &ghost.snapshot,
            &crate::size_change::Frame {
                live: r,
                live_scale: (f.scale, f.scale),
                live_visible: false,
                snapshot: r,
                snapshot_alpha: f.alpha as f32,
            },
            view,
        )
    }

    fn record(&mut self, outcome: &str, fade_only: bool) {
        self.history.push_back(serde_json::json!({
            "outcome": outcome,
            "fade_only": fade_only,
            "motion": self.policy.allows_motion() && !fade_only,
            "at_ms": self.now.as_millis(),
            "duration_ms": duration(self.policy).as_millis(),
        }));
        while self.history.len() > 16 {
            self.history.pop_front();
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        let ghosts: Vec<_> = self
            .ghosts
            .iter()
            .map(|g| {
                let f = self.ghost_frame(g);
                let r = transformed(g.rect, g.pivot, f);
                serde_json::json!({
                    "elapsed_ms": self.now.saturating_sub(g.start).as_millis(),
                    "duration_ms": duration(self.policy).as_millis(),
                    "fade_only": g.fade_only,
                    "motion": g.motion,
                    "frame": {"alpha": f.alpha, "dy": f.dy, "scale": f.scale},
                    "rect": [r.loc.x, r.loc.y, r.size.w, r.size.h],
                })
            })
            .collect();
        serde_json::json!({"closing": ghosts, "retained": self.retained.len(), "history": self.history})
    }

    /// Retain this frame's panel menus and start a ghost for each one that
    /// went away since the last frame.
    pub fn step(
        &mut self,
        renderer: &mut GlesRenderer,
        state: &crate::State,
        now: Duration,
        policy: MotionPolicy,
        allowed: bool,
    ) {
        use smithay::backend::renderer::element::{
            surface::render_elements_from_surface_tree, Kind,
        };
        self.now = now;
        self.policy = policy;
        if !allowed || !policy.allows_fades() {
            self.retained.clear();
            self.ghosts.clear();
            return;
        }
        let mut previous = std::mem::take(&mut self.retained);
        let mut present = HashSet::new();
        let mut grabbed_parents = HashSet::new();
        for (parent, (x, y), _) in crate::layer::layer_layout(state) {
            if !state.is_gtk_panel(&parent) {
                continue;
            }
            for (popup, offset) in smithay::desktop::PopupManager::popups_for_surface(&parent) {
                let smithay::desktop::PopupKind::Xdg(xdg) = &popup else {
                    continue;
                };
                // Menus hang directly off the bar; nested popovers close
                // with their menu.
                if xdg.get_parent_surface().as_ref() != Some(&parent) {
                    continue;
                }
                let surface = popup.wl_surface().clone();
                present.insert(surface.clone());
                let grabbed = state.popup_grabbed(&surface);
                if grabbed {
                    grabbed_parents.insert(parent.clone());
                }
                // A menu is a grabbed popup (tooltips never grab); it stays
                // one after an outside click releases the grab.
                if !grabbed && !previous.contains_key(&surface) {
                    continue;
                }
                let buffer =
                    smithay::backend::renderer::utils::with_renderer_surface_state(&surface, |s| {
                        s.surface_size()
                    })
                    .flatten();
                let geometry = popup.geometry();
                let origin = Point::<i32, Logical>::from((x, y)) + offset - geometry.loc;
                let elements = match buffer.filter(|size| size.w > 0 && size.h > 0) {
                    Some(size) => {
                        let rect = Rectangle::new(origin, size);
                        let scale = state.scale_for(rect).fractional_scale();
                        let elements = render_elements_from_surface_tree::<
                            _,
                            WaylandSurfaceRenderElement<GlesRenderer>,
                        >(
                            renderer,
                            &surface,
                            Point::<i32, Physical>::from((0, 0)),
                            scale,
                            1.0,
                            Kind::Unspecified,
                        );
                        (!elements.is_empty()).then_some((rect, scale, elements))
                    }
                    None => None,
                };
                match elements {
                    Some((rect, scale, elements)) => {
                        let pivot = (
                            f64::from(origin.x + geometry.loc.x)
                                + f64::from(if geometry.size.w > 0 {
                                    geometry.size.w
                                } else {
                                    rect.size.w
                                }) / 2.0,
                            f64::from(origin.y + geometry.loc.y),
                        );
                        self.retained.insert(
                            surface,
                            Retained {
                                parent: parent.clone(),
                                rect,
                                pivot,
                                scale,
                                elements,
                            },
                        );
                    }
                    // Nothing new to show (a null attach before the role
                    // goes): keep the last shown frame.
                    None => {
                        if let Some(old) = previous.remove(&surface) {
                            self.retained.insert(surface, old);
                        }
                    }
                }
            }
        }
        // Opening again replaces the close (BoxPointer.open eases from 0).
        self.ghosts.retain(|g| !present.contains(&g.surface));
        for (surface, old) in previous {
            if present.contains(&surface) {
                continue;
            }
            let fade_only = fade_only(grabbed_parents.contains(&old.parent));
            if self.ghosts.len() >= MAX_GHOSTS {
                self.record("snapshot-budget", fade_only);
                continue;
            }
            let Some(texture) = copy(renderer, &old) else {
                self.record("snapshot-failed", fade_only);
                continue;
            };
            self.record("started", fade_only);
            self.ghosts.push(Ghost {
                surface,
                parent: old.parent,
                rect: old.rect.to_f64(),
                pivot: old.pivot,
                snapshot: Snapshot {
                    texture,
                    id: smithay::backend::renderer::element::Id::new(),
                },
                motion: policy.allows_motion() && !fade_only,
                fade_only,
                start: now,
            });
        }
        let d = duration(policy);
        self.ghosts
            .retain(|g| now.saturating_sub(g.start) < d && !d.is_zero());
    }
}

fn copy(renderer: &mut GlesRenderer, retained: &Retained) -> Option<GlesTexture> {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::renderer::{
        utils::draw_render_elements, Bind, Color32F, Frame as _, Offscreen, Renderer,
    };
    use smithay::utils::Transform;
    let size: Size<i32, Physical> = (
        (f64::from(retained.rect.size.w) * retained.scale).round() as i32,
        (f64::from(retained.rect.size.h) * retained.scale).round() as i32,
    )
        .into();
    let mut texture: GlesTexture = renderer
        .create_buffer(Fourcc::Abgr8888, (size.w, size.h).into())
        .ok()?;
    {
        let mut target = renderer.bind(&mut texture).ok()?;
        let mut frame = renderer.render(&mut target, size, Transform::Normal).ok()?;
        let all = Rectangle::from_size(size);
        frame
            .clear(Color32F::new(0.0, 0.0, 0.0, 0.0), &[all])
            .ok()?;
        draw_render_elements(&mut frame, retained.scale, &retained.elements, &[all]).ok()?;
        let _ = frame.finish().ok()?;
    }
    Some(texture)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn closes_with_gnome_51_boxpointer_curve() {
        let policy = MotionPolicy::from_gnome(true, false, 1.0);
        assert_eq!(duration(policy), Duration::from_millis(150));
        assert_eq!(
            frame(0.0, true),
            Frame {
                alpha: 1.0,
                dy: 0.0,
                scale: 1.0
            }
        );
        // Ease-out quad: three quarters of the way at half time.
        let mid = frame(0.5, true);
        assert!(near(mid.alpha, 0.25) && near(mid.dy, -4.5) && near(mid.scale, 0.97));
        let end = frame(1.0, true);
        assert!(near(end.alpha, 0.0) && near(end.dy, -RISE) && near(end.scale, SCALE));
        assert_eq!(frame(2.0, true), end);
    }

    #[test]
    fn fade_only_and_reduced_motion_drop_the_rise() {
        // PopupMenuManager switching closes the old menu fade-only.
        assert!(fade_only(true));
        assert!(!fade_only(false));
        let faded = frame(0.5, false);
        assert!(near(faded.alpha, 0.25));
        assert_eq!((faded.dy, faded.scale), (0.0, 1.0));
        let reduced = MotionPolicy::from_gnome(true, true, 1.0);
        assert!(reduced.allows_fades() && !reduced.allows_motion());
        let off = MotionPolicy::from_gnome(false, false, 1.0);
        assert!(!off.allows_fades());
        // GNOME's slow-down factor stretches the close.
        let slow = MotionPolicy::from_gnome(true, false, 2.0);
        assert_eq!(duration(slow), Duration::from_millis(300));
    }

    #[test]
    fn shrinks_about_the_top_centre_and_rises() {
        let rect: RectF = Rectangle::new((100.0, 30.0).into(), (200.0, 100.0).into());
        let pivot = (200.0, 30.0);
        assert_eq!(transformed(rect, pivot, frame(0.0, true)), rect);
        let end = transformed(rect, pivot, frame(1.0, true));
        assert!(near(end.loc.x, 104.0) && near(end.size.w, 192.0));
        assert!(near(end.loc.y, 24.0) && near(end.size.h, 96.0));
        // The centre stays put horizontally.
        assert!(near(end.loc.x + end.size.w / 2.0, 200.0));
    }
}
