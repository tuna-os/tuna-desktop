//! GNOME 51's on-screen display (`osdWindow.js`): the pill near the bottom
//! of the screen that shows a volume or brightness level, or a label such
//! as a keyboard layout. Shown through `org.gnome.Shell.ShowOSD`
//! (gnome-settings-daemon's media keys) and hidden 1.5 s after the last
//! call, as GNOME does.

use std::cell::{Cell, RefCell};
use std::f64::consts::TAU;
use std::rc::Rc;
use std::time::Duration;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

/// GNOME's HIDE_TIMEOUT.
const HIDE_AFTER: Duration = Duration::from_millis(1500);
/// `.osd-window { margin-bottom: 4em }` at the 14.666px shell font.
const BOTTOM_MARGIN: i32 = 59;
/// `.osd-window .level`: 160px wide, a 6px bar.
const LEVEL_WIDTH: i32 = 160;
const LEVEL_HEIGHT: f64 = 6.0;
/// `-barlevel-overdrive-separator-width`.
const SEPARATOR_WIDTH: f64 = 3.0;

/// One ShowOSD request, as GNOME's shellDBus.js unpacks it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OsdRequest {
    pub icon: Option<String>,
    pub label: Option<String>,
    pub level: Option<f64>,
    pub max_level: Option<f64>,
}

/// Identity of one accepted OSD display; retained tokens cannot alias a later
/// display or a different OSD instance, even after timers expire.
#[derive(Clone)]
pub struct OsdToken(Rc<()>);

pub struct OsdUi {
    window: gtk::Window,
    icon: gtk::Image,
    label: gtk::Label,
    level: gtk::DrawingArea,
    value: Rc<Cell<(f64, f64)>>,
    hide: RefCell<Option<glib::SourceId>>,
    current: RefCell<Option<OsdToken>>,
}

impl OsdUi {
    pub fn new(app: &gtk::Application) -> Rc<Self> {
        Self::build(app, true)
    }

    // The owned X11 GTK fixture exercises the real widget/timer code without
    // claiming a Wayland layer-surface proof. Production always requests it.
    fn build(app: &gtk::Application, layer_shell: bool) -> Rc<Self> {
        let window = gtk::Window::new();
        window.set_application(Some(app));
        window.add_css_class("tuna-osd");
        window.set_title(Some("On-Screen Display"));
        if layer_shell {
            window.init_layer_shell();
            window.set_layer(Layer::Overlay);
            window.set_namespace(Some(tuna_shell_control::OSD_NAMESPACE));
            window.set_anchor(Edge::Bottom, true);
            window.set_margin(Edge::Bottom, BOTTOM_MARGIN);
            window.set_exclusive_zone(-1);
            window.set_keyboard_mode(KeyboardMode::None);
        }
        // Click-through, like GNOME's OSD (not reactive).
        window.connect_realize(|w| {
            if let Some(surface) = w.surface() {
                surface.set_input_region(Some(&gtk::cairo::Region::create()));
            }
        });

        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        pill.add_css_class("osd-window");
        let icon = gtk::Image::new();
        icon.set_pixel_size(32);
        icon.set_valign(gtk::Align::Center);
        pill.append(&icon);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
        column.set_valign(gtk::Align::Center);
        let label = gtk::Label::new(None);
        label.add_css_class("osd-label");
        column.append(&label);
        let level = gtk::DrawingArea::new();
        level.add_css_class("level");
        level.set_content_width(LEVEL_WIDTH);
        level.set_content_height(LEVEL_HEIGHT as i32);
        let value = Rc::new(Cell::new((0.0, 1.0)));
        {
            let value = value.clone();
            level.set_draw_func(move |_, cr, w, h| {
                let (v, max) = value.get();
                draw_bar_level(cr, f64::from(w), f64::from(h), v, max);
            });
        }
        column.append(&level);
        pill.append(&column);
        window.set_child(Some(&pill));
        Rc::new(Self {
            window,
            icon,
            label,
            level,
            value,
            hide: RefCell::new(None),
            current: RefCell::new(None),
        })
    }

    /// Show `request` on every monitor's OSD (Tuna Desktop draws one), as
    /// GNOME's `showAll`. Without an icon nothing shows, as in GNOME.
    pub fn show(self: &Rc<Self>, request: &OsdRequest) {
        let _ = self.show_owned(request);
    }

    /// Show with cancellable ownership. A later accepted request, including a
    /// normal ShowOSD call, replaces this identity and invalidates its token.
    pub fn show_owned(self: &Rc<Self>, request: &OsdRequest) -> Option<OsdToken> {
        let icon = request
            .icon
            .as_deref()
            .and_then(|s| gio::Icon::for_string(s).ok())?;
        let token = OsdToken(Rc::new(()));
        *self.current.borrow_mut() = Some(token.clone());
        self.icon.set_from_gicon(&icon);
        self.label.set_visible(request.label.is_some());
        self.label.set_label(request.label.as_deref().unwrap_or(""));
        self.level.set_visible(request.level.is_some());
        let max = request.max_level.filter(|m| *m > 0.0).unwrap_or(1.0);
        self.value.set((request.level.unwrap_or(0.0), max));
        // The first visible row carries no bottom margin (St's
        // :first-child on `.level`).
        if request.label.is_some() {
            self.level.add_css_class("after-label");
        } else {
            self.level.remove_css_class("after-label");
        }
        self.level.queue_draw();
        // Shrink to the new content: a layer surface keeps its last size
        // unless asked for its natural one.
        self.window.set_default_size(1, 1);
        self.window.present();
        if let Some(id) = self.hide.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let timer_token = token.clone();
        let id = glib::timeout_add_local_once(HIDE_AFTER, move || {
            if let Some(ui) = weak.upgrade().filter(|ui| ui.matches(&timer_token)) {
                ui.hide.borrow_mut().take();
                ui.current.borrow_mut().take();
                ui.window.set_visible(false);
            }
        });
        *self.hide.borrow_mut() = Some(id);
        Some(token)
    }

    fn matches(&self, token: &OsdToken) -> bool {
        self.current
            .borrow()
            .as_ref()
            .is_some_and(|current| Rc::ptr_eq(&current.0, &token.0))
    }

    /// Hide only this display and cancel its timer; stale/repeated cancellation
    /// leaves a newer volume, brightness or countdown display untouched.
    pub fn cancel_owned(&self, token: &OsdToken) -> bool {
        if !self.matches(token) {
            return false;
        }
        self.current.borrow_mut().take();
        if let Some(id) = self.hide.borrow_mut().take() {
            id.remove();
        }
        self.window.set_visible(false);
        true
    }

    /// The production countdown hooks share one owned token. The stop hook
    /// consumes it, so repeated StopCountdown events are harmless.
    pub fn countdown_hooks(
        self: &Rc<Self>,
    ) -> (
        crate::wellbeing::CountdownHook,
        crate::wellbeing::CountdownHideHook,
    ) {
        let owned = Rc::new(RefCell::new(None));
        let show_ui = self.clone();
        let show_owned = owned.clone();
        let show = Rc::new(move |label: String, progress: f64| {
            eprintln!("tuna-shell-gtk: wellbeing: countdown {label}");
            *show_owned.borrow_mut() = show_ui.show_owned(&OsdRequest {
                icon: Some("alarm-symbolic".into()),
                label: Some(label),
                level: Some(progress.clamp(0.0, 1.0)),
                max_level: Some(1.0),
            });
        });
        let hide_ui = self.clone();
        let hide = Rc::new(move || {
            if let Some(token) = owned.borrow_mut().take() {
                hide_ui.cancel_owned(&token);
            }
        });
        (show, hide)
    }
}

/// GNOME's BarLevel (`barLevel.js` vfunc_repaint, left to right): a 10%
/// white track, the level in white with round ends, and past 100% (a
/// `max` above 1) the overdrive in red behind a 3px separator.
fn draw_bar_level(cr: &gtk::cairo::Context, width: f64, height: f64, value: f64, max: f64) {
    let radius = width.min(LEVEL_HEIGHT) / 2.0;
    let top = (height - LEVEL_HEIGHT) / 2.0;
    let bottom = (height + LEVEL_HEIGHT) / 2.0;
    let overdrive_start = 1.0;
    let progress = if max > 0.0 { value / max } else { 0.0 };
    let end_x = radius + (width - 2.0 * radius) * progress;
    let separator_x = radius + (width - 2.0 * radius) * (overdrive_start / max);
    let overdrive = (overdrive_start - max).abs() > f64::EPSILON;
    let separator_w = if overdrive { SEPARATOR_WIDTH } else { 0.0 };
    let (arc_start, arc_end) = (radius, width - radius);
    let track = (1.0, 1.0, 1.0, 0.1);
    let active = (1.0, 1.0, 1.0, 1.0);
    let red = (
        0xc0 as f64 / 255.0,
        0x1c as f64 / 255.0,
        0x28 as f64 / 255.0,
        1.0,
    );
    let set = |c: (f64, f64, f64, f64)| cr.set_source_rgba(c.0, c.1, c.2, c.3);

    // Track, from the level's end to the right.
    cr.arc(arc_end, height / 2.0, radius, TAU * 0.75, TAU * 0.25);
    cr.line_to(end_x, bottom);
    cr.line_to(end_x, top);
    cr.line_to(arc_end, top);
    set(track);
    let _ = cr.fill();

    // The level up to the separator.
    let x = end_x.min(separator_x - separator_w / 2.0);
    cr.arc(arc_start, height / 2.0, radius, TAU * 0.25, TAU * 0.75);
    cr.line_to(x, top);
    cr.line_to(x, bottom);
    cr.line_to(arc_start, bottom);
    if value > 0.0 {
        set(active);
    }
    let _ = cr.fill();

    // Overdrive.
    let x = end_x.min(separator_x) + separator_w / 2.0;
    if value > overdrive_start {
        cr.move_to(x, top);
        cr.line_to(end_x, top);
        cr.line_to(end_x, bottom);
        cr.line_to(x, bottom);
        cr.line_to(x, top);
        set(red);
        let _ = cr.fill();
    }

    // The level's round end.
    if value > 0.0 {
        set(if value <= overdrive_start {
            active
        } else {
            red
        });
        cr.arc(end_x, height / 2.0, radius, TAU * 0.75, TAU * 0.25);
        cr.line_to(end_x.floor(), bottom);
        cr.line_to(end_x.floor(), top);
        cr.line_to(end_x, top);
        let _ = cr.fill();
    }

    // The separator.
    if overdrive {
        cr.rectangle(
            separator_x - separator_w / 2.0,
            top,
            separator_w,
            LEVEL_HEIGHT,
        );
        set(if value <= overdrive_start {
            active
        } else {
            track
        });
        let _ = cr.fill();
    }
}

#[cfg(test)]
mod gtk_countdown_tests {
    use super::*;
    use crate::wellbeing::{self, BreakType, BreakUiEvent, EffectState, ShellHooks};

    #[test]
    #[ignore = "requires owned Xvfb/private D-Bus and explicit countdown OSD admission"]
    fn actual_gtk_stop_countdown_preserves_newer_osd_and_timers() {
        assert_eq!(
            std::env::var("TUNA_REQUIRE_COUNTDOWN_OSD").as_deref(),
            Ok("1")
        );
        assert!(std::env::var("DBUS_SESSION_BUS_ADDRESS").is_ok());
        assert!(std::env::var_os("WAYLAND_DISPLAY").is_none());
        assert!(std::env::var_os("WAYLAND_SOCKET").is_none());
        gtk::init().expect("actual GTK display required");
        let app = gtk::Application::builder()
            .application_id("org.tuna.CountdownOsdProof")
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        // Real GTK widgets/timers and production countdown hooks; X11 does not
        // implement Wayland layer-shell, so this is not a layer/DRM proof.
        let ui = OsdUi::build(&app, false);
        let (show_countdown, hide_countdown) = ui.countdown_hooks();
        let hooks = ShellHooks {
            show_countdown,
            hide_countdown,
            post_notice: Rc::new(|_, _| {}),
            lock: Rc::new(|| {}),
            post_action_notice: Rc::new(|_, _, _| None),
            dismiss_notice: Rc::new(|_| false),
            on_notice_action: Rc::new(|_, _, _| {}),
            drop_notice_actions: Rc::new(|_| {}),
            break_action: Rc::new(|_| {}),
            effect_hook: Rc::new(|_, _| false),
        };
        let mut effect = EffectState::default();
        let mut countdown = None;
        let mut resident = None;
        let mut dispatch = |event| {
            wellbeing::dispatch_break_ui(
                &hooks,
                &mut effect,
                &mut countdown,
                &mut resident,
                &[event],
            );
        };
        let start = || BreakUiEvent::StartCountdown {
            break_type: BreakType::Eyesight,
            due_in_secs: 30,
        };
        dispatch(start());
        assert!(ui.window.is_visible());
        assert!(ui.label.text().contains("30"));
        assert!(ui.hide.borrow().is_some());
        let first = ui.current.borrow().as_ref().unwrap().clone();
        (hooks.show_countdown)("Break in 29 seconds".into(), 0.52);
        assert!(!ui.cancel_owned(&first), "refresh supersedes old token");
        assert!(ui.window.is_visible());
        dispatch(BreakUiEvent::StopCountdown);
        assert!(
            !ui.window.is_visible(),
            "actual StopCountdown must hide OSD"
        );
        assert!(ui.hide.borrow().is_none(), "owned timer must be removed");
        assert!(ui.current.borrow().is_none());
        dispatch(BreakUiEvent::StopCountdown);
        assert!(!ui.window.is_visible());
        assert!(ui.hide.borrow().is_none());

        for (icon, label) in [
            ("audio-volume-high-symbolic", "Volume"),
            ("display-brightness-symbolic", "Brightness"),
        ] {
            dispatch(start());
            let stale = ui.current.borrow().as_ref().unwrap().clone();
            ui.show(&OsdRequest {
                icon: Some(icon.into()),
                label: Some(label.into()),
                level: Some(0.75),
                max_level: Some(1.0),
            });
            assert!(!ui.cancel_owned(&stale));
            dispatch(BreakUiEvent::StopCountdown);
            dispatch(BreakUiEvent::StopCountdown);
            assert!(ui.window.is_visible(), "stale stop hid foreign OSD");
            assert_eq!(ui.label.text().as_str(), label);
            assert!(ui.hide.borrow().is_some(), "foreign timer was cancelled");
        }
        let foreign = ui.current.borrow().as_ref().unwrap().clone();
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        while ui.window.is_visible() && std::time::Instant::now() < deadline {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !ui.window.is_visible(),
            "foreign OSD auto-hide must still execute"
        );
        assert!(ui.hide.borrow().is_none());
        assert!(ui.current.borrow().is_none());
        assert!(
            !ui.cancel_owned(&foreign),
            "expired token must remain invalid"
        );
        ui.window.close();
        println!("COUNTDOWN-OSD pass actual GTK StopCountdown hides owned display/timer; refreshed and foreign volume/brightness tokens preserved; repeated stop inert; foreign timeout executes");
    }
}
