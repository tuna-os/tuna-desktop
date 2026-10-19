//! GNOME 51's window preview chrome in the overview (windowPreview.js):
//! each preview's app icon, 70% over its bottom edge, and on the hovered
//! one the window title in a caption pill below and a round close button
//! on its top-right corner. The compositor draws the previews and tells
//! the shell where they are (`OverviewPreviews`); this click-through
//! overlay draws the chrome, taking input only on the close button.

use std::cell::{Cell, RefCell};
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

pub struct PreviewChrome {
    window: gtk::ApplicationWindow,
    fixed: gtk::Fixed,
    shown: RefCell<(Vec<PreviewInfo>, Option<u64>)>,
    close: Rc<dyn Fn(u64)>,
}

impl PreviewChrome {
    pub fn new(app: &gtk::Application, close: Rc<dyn Fn(u64)>) -> Rc<Self> {
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
        Rc::new(Self {
            window,
            fixed,
            shown: RefCell::new((Vec::new(), None)),
            close,
        })
    }

    /// Show `previews` (empty hides the overlay); `windows` gives each
    /// preview's title and icon by window id.
    pub fn update(
        self: &Rc<Self>,
        previews: &[PreviewInfo],
        hovered: Option<u64>,
        windows: &dyn Fn(u64) -> Option<PreviewWindow>,
    ) {
        let newly_hovered = {
            let shown = self.shown.borrow();
            if shown.0 == previews && shown.1 == hovered {
                return;
            }
            shown.1 != hovered
        };
        *self.shown.borrow_mut() = (previews.to_vec(), hovered);
        while let Some(child) = self.fixed.first_child() {
            self.fixed.remove(&child);
        }
        if previews.is_empty() {
            self.window.set_visible(false);
            return;
        }
        let mut input = None;
        for p in previews {
            let Some(info) = windows(p.window) else {
                continue;
            };
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
            if hovered != Some(p.window) {
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
            let close = gtk::Button::from_icon_name("preview-close-symbolic");
            close.add_css_class("window-close");
            close.update_property(&[gtk::accessible::Property::Label("Close")]);
            let (x, y) = (p.x + p.width - CLOSE_SIZE / 2, p.y - CLOSE_SIZE / 2);
            self.fixed.put(&close, f64::from(x), f64::from(y));
            {
                let (close_fn, id) = (self.close.clone(), p.window);
                close.connect_clicked(move |_| close_fn(id));
            }
            input = Some(gtk::cairo::RectangleInt::new(x, y, CLOSE_SIZE, CLOSE_SIZE));
            if newly_hovered {
                fade_in(vec![row.upcast(), close.upcast()]);
            }
        }
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
        // the close button.
        let region = match input {
            Some(rect) => gtk::cairo::Region::create_rectangle(&gtk::cairo::RectangleInt::new(
                rect.x() - bounds.x(),
                rect.y() - bounds.y(),
                rect.width(),
                rect.height(),
            )),
            None => gtk::cairo::Region::create(),
        };
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
    use super::{chrome_bounds, overlay_opacity};
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
}
