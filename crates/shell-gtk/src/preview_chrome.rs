//! GNOME 51's window preview chrome in the overview (windowPreview.js):
//! each preview's app icon, 70% over its bottom edge, and on the hovered
//! one the window title in a caption pill below and a round close button
//! on its top-right corner. The compositor draws the previews and tells
//! the shell where they are (`OverviewPreviews`); this click-through
//! overlay draws the chrome, taking input only on the close button.
//! Under keyboard navigation (Tab from the search entry) it also holds
//! one focusable push button per preview, named by its window's title,
//! as GNOME's previews are; the focused one shows its caption and close
//! button as a hovered one does, and Enter activates its window.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use tuna_shell_control::PreviewInfo;

/// `ICON_SIZE`, `ICON_OVERLAP` and `ICON_TITLE_SPACING`.
const ICON_SIZE: i32 = 64;
const ICON_OVERLAP: f64 = 0.7;
const ICON_TITLE_SPACING: f64 = 6.0;
/// The close button (`.window-close`): 32px.
const CLOSE_SIZE: i32 = 32;
/// The hovered preview's caption and close button fade in
/// (`WINDOW_OVERLAY_FADE_TIME`, ease-out-quad).
const OVERLAY_FADE_MS: f64 = 200.0;
/// Leave room for the icon and close button's 4px blurred, 2px-offset
/// shadows (style.css), including their antialiased fringe.
const CHROME_BLEED: f64 = 12.0;
/// Fractional output scales are advertised in 120ths. An origin on
/// this logical grid is an integral physical pixel at every such scale,
/// so rebasing the GTK buffer cannot shift its subpixel rasterization.
const ORIGIN_QUANTUM: f64 = 120.0;

/// Bound the widgets in output coordinates, retaining fractional placement.
/// The old full-output surface clipped anything above/left of the output.
fn chrome_bounds(
    widgets: impl Iterator<Item = (f64, f64, i32, i32)>,
) -> Option<gtk::cairo::RectangleInt> {
    let (left, top, right, bottom) = widgets.fold(
        (f64::INFINITY, f64::INFINITY, 0.0_f64, 0.0_f64),
        |(left, top, right, bottom), (x, y, width, height)| {
            (
                left.min(x),
                top.min(y),
                right.max(x + f64::from(width)),
                bottom.max(y + f64::from(height)),
            )
        },
    );
    if !left.is_finite() {
        return None;
    }
    let origin = |start: f64| {
        (((start - CHROME_BLEED) / ORIGIN_QUANTUM).floor() * ORIGIN_QUANTUM).max(0.0) as i32
    };
    let x = origin(left);
    let y = origin(top);
    Some(gtk::cairo::RectangleInt::new(
        x,
        y,
        ((right + CHROME_BLEED).ceil() as i32 - x).max(1),
        ((bottom + CHROME_BLEED).ceil() as i32 - y).max(1),
    ))
}

/// Overlay opacity `elapsed_ms` into the fade under `policy`: a fade,
/// so fade-only keeps it; the slow-down factor stretches it.
pub fn overlay_opacity(elapsed_ms: f64, policy: tuna_shell_control::MotionPolicy) -> f64 {
    let duration = policy.adjust_ms(OVERLAY_FADE_MS);
    let t = if policy.allows_fades() && duration > 0.0 {
        (elapsed_ms / duration).clamp(0.0, 1.0)
    } else {
        1.0
    };
    1.0 - (1.0 - t) * (1.0 - t)
}

/// Fade `widgets` in together, GNOME's hover overlay.
///
/// The tick callback lives on the first widget and holds every widget
/// weakly. [`PreviewChrome::update`] rebuilds the chrome on each preview
/// change, often before a fade ends, and a removed widget never ticks
/// again: a strong capture made the first widget own a callback that
/// owned it, leaking the caption row and close button of every
/// interrupted fade (#658).
fn fade_in(widgets: Vec<gtk::Widget>) {
    let Some(first) = widgets.first().cloned() else {
        return;
    };
    if !crate::motion::current().allows_fades() {
        return;
    }
    for w in &widgets {
        w.set_opacity(0.0);
    }
    let widgets: Vec<_> = widgets.iter().map(ObjectExt::downgrade).collect();
    let started = Cell::new(None);
    first.add_tick_callback(move |_, clock| {
        let start = started.get().unwrap_or_else(|| {
            started.set(Some(clock.frame_time()));
            clock.frame_time()
        });
        let elapsed_ms = (clock.frame_time() - start) as f64 / 1000.0;
        let opacity = overlay_opacity(elapsed_ms, crate::motion::current());
        for w in widgets.iter().filter_map(glib::WeakRef::upgrade) {
            w.set_opacity(opacity);
        }
        if opacity >= 1.0 {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

/// What one preview shows: its window's title and app icon.
pub struct PreviewWindow {
    pub title: String,
    pub icon: gio::Icon,
}

/// A key's move through the window picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
    /// Tab: the next preview in the picker's order.
    Next,
    /// Shift+Tab: the previous one.
    Previous,
}

impl Direction {
    fn of(key: gtk::gdk::Key) -> Option<Self> {
        use gtk::gdk::Key;
        Some(match key {
            Key::Left | Key::KP_Left => Self::Left,
            Key::Right | Key::KP_Right => Self::Right,
            Key::Up | Key::KP_Up => Self::Up,
            Key::Down | Key::KP_Down => Self::Down,
            Key::Tab | Key::KP_Tab => Self::Next,
            Key::ISO_Left_Tab => Self::Previous,
            _ => return None,
        })
    }
}

/// Where a key takes the window picker's focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewStep {
    /// Focus the preview at this index.
    Preview(usize),
    /// Back to the search entry: up from the top row, or back from the
    /// first preview (GNOME keeps the entry in the overview's Tab path).
    Search,
    /// Nothing that way: the focus stays.
    Stay,
}

/// St's directional focus between the previews (`x, y, width, height`):
/// the nearest preview whose center lies that way, the distance across
/// the move counting double so a row's neighbour wins over a diagonal.
pub fn preview_step(
    rects: &[(i32, i32, i32, i32)],
    from: usize,
    direction: Direction,
) -> PreviewStep {
    let center = |(x, y, w, h): (i32, i32, i32, i32)| (x + w / 2, y + h / 2);
    let Some(&here) = rects.get(from) else {
        return PreviewStep::Stay;
    };
    let (cx, cy) = center(here);
    let edge = |direction| match direction {
        Direction::Up | Direction::Previous => PreviewStep::Search,
        _ => PreviewStep::Stay,
    };
    match direction {
        Direction::Next => {
            if from + 1 < rects.len() {
                PreviewStep::Preview(from + 1)
            } else {
                edge(direction)
            }
        }
        Direction::Previous => {
            if from > 0 {
                PreviewStep::Preview(from - 1)
            } else {
                edge(direction)
            }
        }
        _ => rects
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != from)
            .filter_map(|(index, rect)| {
                let (x, y) = center(*rect);
                let (along, across) = match direction {
                    Direction::Left => (cx - x, y - cy),
                    Direction::Right => (x - cx, y - cy),
                    Direction::Up => (cy - y, x - cx),
                    _ => (y - cy, x - cx),
                };
                (along > 0).then_some((along + 2 * across.abs(), index))
            })
            .min()
            .map_or_else(|| edge(direction), |(_, index)| PreviewStep::Preview(index)),
    }
}

/// Hands the keyboard back to the search entry, with any typed text.
pub type OnLeave = Rc<dyn Fn(Option<char>)>;

pub struct PreviewChrome {
    window: gtk::ApplicationWindow,
    fixed: gtk::Fixed,
    shown: RefCell<(Vec<PreviewInfo>, Option<u64>)>,
    /// Each shown preview's title and icon, looked up at the last update.
    windows: RefCell<HashMap<u64, PreviewWindow>>,
    /// Previews whose caption and close button the last render showed.
    overlaid: RefCell<Vec<u64>>,
    /// Keyboard navigation of the window picker (windowPreview.js: each
    /// preview is a focusable push button named by its window's title):
    /// one focus target per preview while it runs, kept across renders
    /// so the focus survives preview updates.
    keynav: RefCell<Option<Vec<(u64, gtk::Button)>>>,
    /// The preview the keyboard focus is on; it shows its caption and
    /// close button as a hovered one does.
    key_focus: Cell<Option<u64>>,
    close: Rc<dyn Fn(u64)>,
    activate: Rc<dyn Fn(u64)>,
    /// Hands the keyboard back to the search entry, with any typed text.
    on_leave: RefCell<Option<OnLeave>>,
}

impl PreviewChrome {
    pub fn new(
        app: &gtk::Application,
        close: Rc<dyn Fn(u64)>,
        activate: Rc<dyn Fn(u64)>,
    ) -> Rc<Self> {
        let window = gtk::ApplicationWindow::new(app);
        window.add_css_class("tuna-overview-chrome");
        window.init_layer_shell();
        window.set_layer(Layer::Top);
        window.set_namespace(Some(tuna_shell_control::PREVIEW_CHROME_NAMESPACE));
        for edge in [Edge::Top, Edge::Left] {
            window.set_anchor(edge, true);
        }
        // Output coordinates, as the compositor sends them: ignore the
        // bar's exclusive zone.
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_title(Some("Window Previews"));
        let fixed = gtk::Fixed::new();
        window.set_child(Some(&fixed));
        // Click-through from the first frame: no input until a close
        // button needs it.
        window.connect_realize(|w| {
            if let Some(surface) = w.surface() {
                surface.set_input_region(Some(&gtk::cairo::Region::create()));
            }
        });
        let chrome = Rc::new(Self {
            window,
            fixed,
            shown: RefCell::new((Vec::new(), None)),
            windows: RefCell::default(),
            overlaid: RefCell::default(),
            keynav: RefCell::new(None),
            key_focus: Cell::new(None),
            close,
            activate,
            on_leave: RefCell::new(None),
        });
        // Arrows and Tab move between the previews; typing goes back to
        // the search entry with the typed text, as GNOME's entry
        // intercepts it anywhere in the overview.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&chrome);
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(chrome) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if chrome.keynav.borrow().is_none() {
                return glib::Propagation::Proceed;
            }
            if let Some(c) = crate::overview::typed_text(key, state) {
                chrome.leave(Some(c));
                return glib::Propagation::Stop;
            }
            let Some(direction) = Direction::of(key) else {
                // Enter and Space activate the focused preview.
                return glib::Propagation::Proceed;
            };
            let ids = chrome.focusable();
            let rects: Vec<_> = {
                let shown = chrome.shown.borrow();
                ids.iter()
                    .filter_map(|id| shown.0.iter().find(|p| p.window == *id))
                    .map(|p| (p.x, p.y, p.width, p.height))
                    .collect()
            };
            let from = chrome
                .key_focus
                .get()
                .and_then(|id| ids.iter().position(|shown| *shown == id))
                .unwrap_or(0);
            match preview_step(&rects, from, direction) {
                PreviewStep::Preview(index) => chrome.focus(ids[index]),
                PreviewStep::Search => chrome.leave(None),
                PreviewStep::Stay => {}
            }
            glib::Propagation::Stop
        });
        chrome.window.add_controller(keys);
        chrome
    }

    /// Who takes the keyboard back when the navigation leaves the picker.
    pub fn set_on_leave(&self, on_leave: OnLeave) {
        *self.on_leave.borrow_mut() = Some(on_leave);
    }

    /// Show `previews` (empty hides the overlay); `windows` gives each
    /// preview's title and icon by window id.
    pub fn update(
        self: &Rc<Self>,
        previews: &[PreviewInfo],
        hovered: Option<u64>,
        windows: &dyn Fn(u64) -> Option<PreviewWindow>,
    ) {
        {
            let shown = self.shown.borrow();
            if shown.0 == previews && shown.1 == hovered {
                return;
            }
        }
        *self.shown.borrow_mut() = (previews.to_vec(), hovered);
        *self.windows.borrow_mut() = previews
            .iter()
            .filter_map(|p| Some((p.window, windows(p.window)?)))
            .collect();
        self.render();
    }

    /// The previews keyboard navigation can focus, in the picker's order.
    fn focusable(&self) -> Vec<u64> {
        let windows = self.windows.borrow();
        self.shown
            .borrow()
            .0
            .iter()
            .map(|p| p.window)
            .filter(|id| windows.contains_key(id))
            .collect()
    }

    /// GNOME's Tab from the search entry into the window picker: the
    /// chrome takes the keyboard and the first preview (the last going
    /// `backward`) the focus. False with no previews.
    pub fn enter_keynav(self: &Rc<Self>, backward: bool) -> bool {
        let ids = self.focusable();
        let Some(&id) = (if backward { ids.last() } else { ids.first() }) else {
            return false;
        };
        if self.keynav.borrow().is_none() {
            *self.keynav.borrow_mut() = Some(Vec::new());
        }
        self.key_focus.set(Some(id));
        self.render();
        self.window.set_keyboard_mode(KeyboardMode::Exclusive);
        self.window.set_focus_visible(true);
        self.focus(id);
        true
    }

    /// End keyboard navigation: the focus targets go and the chrome stops
    /// asking for the keyboard, which the compositor parks on search.
    pub fn leave_keynav(self: &Rc<Self>) {
        let Some(targets) = self.keynav.borrow_mut().take() else {
            return;
        };
        for (_, target) in targets {
            self.fixed.remove(&target);
        }
        self.key_focus.set(None);
        self.window.set_keyboard_mode(KeyboardMode::None);
        self.render();
    }

    fn leave(&self, typed: Option<char>) {
        let on_leave = self.on_leave.borrow().clone();
        if let Some(on_leave) = on_leave {
            on_leave(typed);
        }
    }

    fn focus(&self, id: u64) {
        let target = self.keynav.borrow().as_ref().and_then(|targets| {
            targets
                .iter()
                .find(|(target, _)| *target == id)
                .map(|(_, button)| button.clone())
        });
        if let Some(target) = target {
            target.grab_focus();
        }
    }

    /// One preview's focus target: a transparent push button over it,
    /// named by its window's title; Enter or Space activates the window.
    fn focus_target(self: &Rc<Self>, id: u64) -> gtk::Button {
        let target = gtk::Button::new();
        target.add_css_class("window-preview-focus");
        let activate = self.activate.clone();
        target.connect_clicked(move |_| activate(id));
        let weak = Rc::downgrade(self);
        target.connect_has_focus_notify(move |target| {
            let Some(chrome) = weak.upgrade() else {
                return;
            };
            if !target.has_focus() || chrome.key_focus.replace(Some(id)) == Some(id) {
                return;
            }
            // The caption and close button follow the focus, as on hover;
            // render after the focus change finishes.
            glib::idle_add_local_once(move || chrome.render());
        });
        target
    }

    /// Draw the chrome for the shown previews.
    fn render(self: &Rc<Self>) {
        let (previews, hovered) = self.shown.borrow().clone();
        let mut keynav = self.keynav.borrow_mut();
        // Rebuild everything but the focus targets.
        let mut child = self.fixed.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            let kept = keynav.as_ref().is_some_and(|targets| {
                targets
                    .iter()
                    .any(|(_, t)| t.upcast_ref::<gtk::Widget>() == &widget)
            });
            if !kept {
                self.fixed.remove(&widget);
            }
        }
        let windows = self.windows.borrow();
        if let Some(targets) = keynav.as_mut() {
            targets.retain(|(id, target)| {
                let keep = windows.contains_key(id) && previews.iter().any(|p| p.window == *id);
                if !keep {
                    self.fixed.remove(target);
                }
                keep
            });
        }
        if previews.is_empty() {
            if keynav.take().is_some() {
                self.key_focus.set(None);
                self.window.set_keyboard_mode(KeyboardMode::None);
            }
            self.overlaid.borrow_mut().clear();
            self.window.set_visible(false);
            return;
        }
        let focused = self.key_focus.get().filter(|_| keynav.is_some());
        let previous = self.overlaid.replace(Vec::new());
        let mut inputs = Vec::new();
        for p in &previews {
            let Some(info) = windows.get(&p.window) else {
                continue;
            };
            if let Some(targets) = keynav.as_mut() {
                let existing = targets
                    .iter()
                    .find(|(id, _)| *id == p.window)
                    .map(|(_, target)| target.clone());
                let target = match existing {
                    Some(target) => {
                        self.fixed.move_(&target, f64::from(p.x), f64::from(p.y));
                        target
                    }
                    None => {
                        let target = self.focus_target(p.window);
                        self.fixed.put(&target, f64::from(p.x), f64::from(p.y));
                        targets.push((p.window, target.clone()));
                        target
                    }
                };
                target.set_size_request(p.width, p.height);
                target.update_property(&[gtk::accessible::Property::Label(&info.title)]);
            }
            let bottom = f64::from(p.y + p.height);
            let center_x = f64::from(p.x) + f64::from(p.width) / 2.0;
            let icon = gtk::Image::from_gicon(&info.icon);
            icon.set_pixel_size(ICON_SIZE);
            icon.add_css_class("window-icon");
            self.fixed.put(
                &icon,
                center_x - f64::from(ICON_SIZE) / 2.0,
                bottom - f64::from(ICON_SIZE) * ICON_OVERLAP,
            );
            if hovered != Some(p.window) && focused != Some(p.window) {
                continue;
            }
            // Centered under the preview, never wider than it.
            let caption = gtk::Label::new(Some(&info.title));
            caption.add_css_class("window-caption");
            caption.set_ellipsize(gtk::pango::EllipsizeMode::End);
            caption.set_max_width_chars(((p.width - 24) / 8).max(1));
            caption.set_halign(gtk::Align::Center);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.set_size_request(p.width, -1);
            row.append(&caption);
            caption.set_hexpand(true);
            self.fixed.put(
                &row,
                f64::from(p.x),
                bottom + f64::from(ICON_SIZE) * (1.0 - ICON_OVERLAP) + ICON_TITLE_SPACING,
            );
            // St.Button's default: the pointer's, not in the Tab path.
            let close = gtk::Button::from_icon_name("preview-close-symbolic");
            close.add_css_class("window-close");
            close.set_focusable(false);
            close.update_property(&[gtk::accessible::Property::Label("Close")]);
            let (x, y) = (p.x + p.width - CLOSE_SIZE / 2, p.y - CLOSE_SIZE / 2);
            self.fixed.put(&close, f64::from(x), f64::from(y));
            {
                let (close_fn, id) = (self.close.clone(), p.window);
                close.connect_clicked(move |_| close_fn(id));
            }
            inputs.push(gtk::cairo::RectangleInt::new(x, y, CLOSE_SIZE, CLOSE_SIZE));
            if !previous.contains(&p.window) {
                fade_in(vec![row.upcast(), close.upcast()]);
            }
            self.overlaid.borrow_mut().push(p.window);
        }
        drop((windows, keynav));
        // A transparent full-output buffer still shades every pixel on
        // transition frames. Keep the same widgets and output positions,
        // but allocate only their union, including CSS shadows.
        // child_position() reads the allocation, which does not exist
        // yet for these new children. The stored layout transform is
        // already the translation supplied to put(); None is identity.
        let position = |child: &gtk::Widget| {
            let (x, y) = self
                .fixed
                .child_transform(child)
                .map(|transform| transform.to_translate())
                .unwrap_or((0.0, 0.0));
            (f64::from(x), f64::from(y))
        };
        let children = std::iter::successors(self.fixed.first_child(), |w| w.next_sibling());
        let bounds = chrome_bounds(children.map(|child| {
            let (x, y) = position(&child);
            let (_, width, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
            let (_, height, _, _) = child.measure(gtk::Orientation::Vertical, width);
            (x, y, width, height)
        }));
        let Some(bounds) = bounds else {
            self.window.set_visible(false);
            return;
        };
        let mut child = self.fixed.first_child();
        while let Some(widget) = child {
            let (x, y) = position(&widget);
            self.fixed.move_(
                &widget,
                x - f64::from(bounds.x()),
                y - f64::from(bounds.y()),
            );
            child = widget.next_sibling();
        }
        self.fixed.set_size_request(bounds.width(), bounds.height());
        // As with the switcher's thumbnail layer, explicitly request a
        // fresh natural size so a previously larger surface can shrink.
        self.window.set_default_size(1, 1);
        self.window.set_margin(Edge::Left, bounds.x());
        self.window.set_margin(Edge::Top, bounds.y());
        self.window.set_visible(true);
        // Clicks fall through to the compositor's previews except on
        // the close buttons.
        let region = gtk::cairo::Region::create();
        for rect in inputs {
            let _ = region.union_rectangle(&gtk::cairo::RectangleInt::new(
                rect.x() - bounds.x(),
                rect.y() - bounds.y(),
                rect.width(),
                rect.height(),
            ));
        }
        if let Some(surface) = self.window.surface() {
            surface.set_input_region(Some(&region));
        }
    }

    /// Forget what the overlay drew, so the next [`PreviewChrome::update`]
    /// looks every window's title and icon up again (the app list
    /// changed). The hover stays, so nothing fades in again.
    pub fn forget_windows(&self) {
        self.shown.borrow_mut().0.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{chrome_bounds, overlay_opacity, preview_step, Direction, PreviewStep};
    use tuna_shell_control::{MotionLevel, MotionPolicy};

    #[test]
    fn bounds_include_shadow_bleed_without_rounding_widget_positions() {
        let bounds =
            chrome_bounds([(200.5, 400.2, 64, 64), (100.0, 500.2, 300, 32)].into_iter()).unwrap();
        assert_eq!((bounds.x(), bounds.y()), (0, 360));
        assert_eq!((bounds.width(), bounds.height()), (412, 185));
    }

    #[test]
    fn bounds_keep_output_edge_clipping_and_empty_chrome_has_no_surface() {
        let bounds = chrome_bounds([(-8.5, -16.0, 32, 32)].into_iter()).unwrap();
        assert_eq!((bounds.x(), bounds.y()), (0, 0));
        assert_eq!((bounds.width(), bounds.height()), (36, 28));
        assert!(chrome_bounds(std::iter::empty()).is_none());
    }

    #[test]
    fn bounds_origins_preserve_the_physical_grid_for_fractional_protocol_scales() {
        let bounds = chrome_bounds([(521.5, 297.2, 383, 64)].into_iter()).unwrap();
        for scale_120 in 60..=480 {
            assert_eq!(bounds.x() * scale_120 % 120, 0);
            assert_eq!(bounds.y() * scale_120 % 120, 0);
        }
    }

    #[test]
    fn the_hover_overlay_fades_in_over_200_ms_ease_out_quad() {
        let full = MotionPolicy::default();
        assert_eq!(overlay_opacity(0.0, full), 0.0);
        assert_eq!(overlay_opacity(100.0, full), 0.75);
        assert_eq!(overlay_opacity(200.0, full), 1.0);
        let fade_only = MotionPolicy::new(MotionLevel::FadeOnly, 1.0);
        assert_eq!(overlay_opacity(100.0, fade_only), 0.75);
        assert_eq!(
            overlay_opacity(200.0, MotionPolicy::new(MotionLevel::Full, 2.0)),
            0.75
        );
        assert_eq!(
            overlay_opacity(0.0, MotionPolicy::new(MotionLevel::Off, 1.0)),
            1.0
        );
    }

    #[test]
    fn arrows_move_to_the_nearest_preview_that_way_and_up_leaves_for_search() {
        // Two previews side by side over a wide third.
        let rects = [
            (100, 100, 300, 200),
            (500, 100, 300, 200),
            (200, 400, 600, 250),
        ];
        assert_eq!(
            preview_step(&rects, 0, Direction::Right),
            PreviewStep::Preview(1)
        );
        assert_eq!(
            preview_step(&rects, 1, Direction::Left),
            PreviewStep::Preview(0)
        );
        assert_eq!(
            preview_step(&rects, 0, Direction::Down),
            PreviewStep::Preview(2)
        );
        // The wide preview's center (500, 525) is nearer the right one's.
        assert_eq!(
            preview_step(&rects, 2, Direction::Up),
            PreviewStep::Preview(1)
        );
        assert_eq!(preview_step(&rects, 2, Direction::Down), PreviewStep::Stay);
        assert_eq!(preview_step(&rects, 1, Direction::Right), PreviewStep::Stay);
        assert_eq!(preview_step(&rects, 0, Direction::Up), PreviewStep::Search);
    }

    #[test]
    fn tab_follows_the_picker_order_and_back_from_the_first_leaves_for_search() {
        let rects = [(500, 100, 300, 200), (100, 100, 300, 200)];
        assert_eq!(
            preview_step(&rects, 0, Direction::Next),
            PreviewStep::Preview(1)
        );
        assert_eq!(preview_step(&rects, 1, Direction::Next), PreviewStep::Stay);
        assert_eq!(
            preview_step(&rects, 1, Direction::Previous),
            PreviewStep::Preview(0)
        );
        assert_eq!(
            preview_step(&rects, 0, Direction::Previous),
            PreviewStep::Search
        );
    }
}
