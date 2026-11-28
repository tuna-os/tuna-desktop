//! GNOME 51's screenshot UI (screenshot.js), opened by Print: the screen
//! frozen and dimmed outside the selection, a white-bordered selection
//! with round corner handles, and the panel: Selection / Screen / Window,
//! the photo and video switch, the capture button, the pointer toggle,
//! and the close button. Capturing saves to ~/Pictures/Screenshots, puts
//! the image on the clipboard and says so, as GNOME does.
//!
//! Window shows GNOME's window selector (`UIWindowSelector`): the active
//! workspace's windows, pictured when the UI opened and spread out as
//! the overview spreads them, on the dark system background; the focused
//! one starts selected, the selection ringed in the accent color with a
//! check, and Capture saves that window alone at full size. The video
//! switch records the selection or the screen instead
//! (`screencast.rs`), with GNOME's recording indicator in the panel.
//!
//! Screen shows GNOME's per-monitor screen selectors
//! (`_rebuildMonitorBins`, `screenshot_screens.rs`): every output frozen
//! on its own overlay, the others dimmed and the selected one framed in
//! white; the primary starts selected, the arrows move between monitors
//! and Capture saves the selected output alone, in its own pixels.
//!
//! Not yet: the pointer in screenshots and recordings, and one window
//! selector per monitor (the primary one shows them all); the area
//! selection stays on the primary monitor.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::screencast::Recorder;
use crate::screenshot_screens::{
    layout, neighbour, parse_outputs, recording_area, selected_on_open, ShotOutput,
};
pub use crate::screenshot_selection::{initial_selection, Rect};
use crate::screenshot_selection::{Direction, SelectionKeys};

/// `.screenshot-ui-area-selector-handle`: 24px.
const HANDLE: f64 = 24.0;
/// The panel: 329x168, 4em above the bottom.
const PANEL_W: i32 = 329;
const PANEL_H: i32 = 168;
const PANEL_BOTTOM: i32 = 59;
/// `.screenshot-ui-window-selector-window-border`: 6px, outside the
/// window.
const WINDOW_BORDER: i32 = 6;
/// `.screenshot-ui-window-selector`: `$system_base_color` (dark).
const SELECTOR_BACKGROUND: (f64, f64, f64) = (34.0 / 255.0, 34.0 / 255.0, 38.0 / 255.0);

/// What a press at a point grabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    /// A corner handle: 0 top-left, 1 top-right, 2 bottom-left, 3 bottom-right.
    Corner(u8),
    /// Inside the selection: move it.
    Move,
    /// Elsewhere: draw a new one.
    New,
}

/// What a press at (`x`, `y`) grabs on `sel`.
pub fn hit(sel: &Rect, x: f64, y: f64) -> Grab {
    let corners = [
        (sel.x, sel.y),
        (sel.x + sel.w, sel.y),
        (sel.x, sel.y + sel.h),
        (sel.x + sel.w, sel.y + sel.h),
    ];
    for (i, (cx, cy)) in corners.iter().enumerate() {
        if (x - cx).hypot(y - cy) <= HANDLE / 2.0 + 4.0 {
            return Grab::Corner(i as u8);
        }
    }
    if x >= sel.x && x <= sel.x + sel.w && y >= sel.y && y <= sel.y + sel.h {
        Grab::Move
    } else {
        Grab::New
    }
}

/// The selection after dragging `grab` from `start` (the selection
/// then), pressed at `press` and moved by `delta`, kept on a `screen`
/// sized output.
pub fn drag(
    grab: Grab,
    start: Rect,
    (px, py): (f64, f64),
    (dx, dy): (f64, f64),
    (width, height): (f64, f64),
) -> Rect {
    let span = |a: f64, b: f64| (a.min(b), (a - b).abs());
    match grab {
        Grab::Move => Rect {
            x: (start.x + dx).clamp(0.0, (width - start.w).max(0.0)),
            y: (start.y + dy).clamp(0.0, (height - start.h).max(0.0)),
            ..start
        },
        Grab::New => {
            let (x, w) = span(px, (px + dx).clamp(0.0, width));
            let (y, h) = span(py, (py + dy).clamp(0.0, height));
            Rect { x, y, w, h }
        }
        Grab::Corner(c) => {
            // The opposite corner stays put.
            let (fx, fy) = (
                if c % 2 == 0 {
                    start.x + start.w
                } else {
                    start.x
                },
                if c < 2 { start.y + start.h } else { start.y },
            );
            let (mx, my) = (
                if c % 2 == 0 {
                    start.x
                } else {
                    start.x + start.w
                },
                if c < 2 { start.y } else { start.y + start.h },
            );
            let (x, w) = span(fx, (mx + dx).clamp(0.0, width));
            let (y, h) = span(fy, (my + dy).clamp(0.0, height));
            Rect { x, y, w, h }
        }
    }
}

/// GNOME's file name: `Screenshot From 2026-10-02 03-18-00.png`.
pub fn file_name(now: &jiff::civil::DateTime) -> String {
    format!("Screenshot From {}.png", now.strftime("%Y-%m-%d %H-%M-%S"))
}

/// One window as the compositor's `ScreenshotWindows` answers it.
#[derive(Debug, Clone, PartialEq)]
pub struct ShotWindow {
    pub id: u64,
    pub title: String,
    pub focused: bool,
    /// Its slot in the selector (logical pixels).
    pub slot: Rect,
    pub path: String,
}

/// Unpack a `ScreenshotWindows` reply, `(a(tsbiiiis))`.
pub fn parse_windows(reply: &glib::Variant) -> Vec<ShotWindow> {
    let Some(list) = reply.try_child_value(0) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|w| w.get::<(u64, String, bool, i32, i32, i32, i32, String)>())
        .map(|(id, title, focused, x, y, w, h, path)| ShotWindow {
            id,
            title,
            focused,
            slot: Rect {
                x: f64::from(x),
                y: f64::from(y),
                w: f64::from(w),
                h: f64::from(h),
            },
            path,
        })
        .collect()
}

/// Which window starts selected: the focused one, else the first (as
/// GNOME checks the window with focus).
pub fn initially_selected(windows: &[ShotWindow]) -> Option<usize> {
    windows
        .iter()
        .position(|w| w.focused)
        .or((!windows.is_empty()).then_some(0))
}

/// Says a screenshot was taken: (summary, body).
pub type Notify = Rc<dyn Fn(&str, &str)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Selection,
    Screen,
    Window,
}

/// What a key press does in the screenshot UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Binding {
    /// Escape: the grab's ungrab closes the UI.
    Close,
    /// `activate`: Return, Space or Ctrl+C.
    Capture,
    /// `selection` (S), `screen` (C), `window` (W).
    Mode(Mode),
    /// `screencast` (V).
    ToggleCast,
    /// `pointer` (P).
    TogglePointer,
    /// `reset` (R).
    Reset,
    /// `move-*` (plain), `resize-selection-*-control/-shift` and
    /// `move-selection-*` with their Ctrl and Shift variants.
    Arrow {
        direction: Direction,
        alt: bool,
        ctrl: bool,
        shift: bool,
    },
    /// An arrow with modifiers no binding takes (Ctrl+Shift, Super):
    /// nothing happens, and the key stays inside the UI's grab.
    Unbound,
}

/// The binding a key press activates, as gnome-shell 51.0's
/// `ScreenshotUI` binding pool (`js/ui/screenshot.js`) matches it:
/// Clutter's `clutter_binding_pool_activate` compares the keyval and the
/// held Shift, Ctrl, Alt, Super and Hyper modifiers exactly, lock
/// modifiers ignored, so Ctrl+C captures while C picks Screen, and an
/// uppercase keyval (Shift or Caps Lock held) or KP_Enter has no binding.
/// Escape closes through the UI's grab whatever is held (`GrabHelper`).
/// Meta is left out of the comparison: GTK can report it for Alt.
fn binding(key: gdk::Key, modifiers: gdk::ModifierType) -> Option<Binding> {
    use gdk::ModifierType as M;
    let held =
        modifiers & (M::SHIFT_MASK | M::CONTROL_MASK | M::ALT_MASK | M::SUPER_MASK | M::HYPER_MASK);
    let plain = held.is_empty();
    let direction = match key {
        gdk::Key::Left => Some(Direction::Left),
        gdk::Key::Right => Some(Direction::Right),
        gdk::Key::Up => Some(Direction::Up),
        gdk::Key::Down => Some(Direction::Down),
        _ => None,
    };
    if let Some(direction) = direction {
        let (alt, ctrl, shift) = (
            held.contains(M::ALT_MASK),
            held.contains(M::CONTROL_MASK),
            held.contains(M::SHIFT_MASK),
        );
        let bound = !held.intersects(M::SUPER_MASK | M::HYPER_MASK) && !(ctrl && shift);
        return Some(if bound {
            Binding::Arrow {
                direction,
                alt,
                ctrl,
                shift,
            }
        } else {
            Binding::Unbound
        });
    }
    Some(match key {
        gdk::Key::Escape => Binding::Close,
        gdk::Key::Return | gdk::Key::space if plain => Binding::Capture,
        gdk::Key::c if held == M::CONTROL_MASK => Binding::Capture,
        gdk::Key::s if plain => Binding::Mode(Mode::Selection),
        gdk::Key::c if plain => Binding::Mode(Mode::Screen),
        gdk::Key::w if plain => Binding::Mode(Mode::Window),
        gdk::Key::v if plain => Binding::ToggleCast,
        gdk::Key::p if plain => Binding::TogglePointer,
        gdk::Key::r if plain => Binding::Reset,
        _ => return None,
    })
}

/// One window in the selector: its picture and its button.
struct PickerWindow {
    texture: gdk::Texture,
    button: gtk::ToggleButton,
}

/// One monitor as the UI froze it: its output and its picture.
struct Screen {
    output: ShotOutput,
    texture: gdk::Texture,
}

/// The overlay over a monitor other than the primary: its screen
/// selector (`UIMonitorBin`), never taking the keyboard.
struct ScreenWindow {
    window: gtk::Window,
    area: gtk::DrawingArea,
}

pub struct ScreenshotUi {
    window: RefCell<Option<gtk::Window>>,
    app: gtk::Application,
    overlay: gtk::Overlay,
    canvas: gtk::DrawingArea,
    fixed: gtk::Fixed,
    panel: gtk::Box,
    close: gtk::Button,
    types: Vec<(Mode, gtk::ToggleButton)>,
    cast: gtk::ToggleButton,
    capture: gtk::Button,
    pointer: gtk::ToggleButton,
    /// Every monitor, frozen on open, in the compositor's order.
    screens: RefCell<Vec<Screen>>,
    /// The screen the main overlay (panel, area and window selectors)
    /// covers: the primary monitor, where GNOME puts the panel.
    primary: Cell<usize>,
    /// The checked screen selector.
    selected_screen: Cell<usize>,
    /// The hovered screen selector (`:hover` dims it less).
    hovered_screen: Cell<Option<usize>>,
    /// The monitors the selectors were built for and the checked one,
    /// kept across openings as GNOME keeps its monitor bins.
    built: RefCell<Vec<(String, Rect)>>,
    selected_name: RefCell<Option<String>>,
    /// The other monitors' overlays.
    screen_windows: RefCell<Vec<ScreenWindow>>,
    /// The window selector's windows, pictured on open.
    windows: RefCell<Vec<PickerWindow>>,
    selection: Cell<Option<Rect>>,
    selection_keys: Cell<Option<SelectionKeys>>,
    mode: Cell<Mode>,
    grab: Cell<Option<(Grab, Rect, f64, f64)>>,
    /// Takes the focused window's screenshot (Window mode, when the
    /// compositor cannot picture every window).
    shoot_window: Rc<dyn Fn()>,
    notify: Notify,
    recorder: Rc<Recorder>,
    move_pointer: Rc<dyn Fn((f64, f64))>,
}

fn icon_label_button(icon: &str, label: &str) -> gtk::ToggleButton {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(32);
    column.append(&image);
    let text = gtk::Label::new(Some(label));
    text.add_css_class("screenshot-ui-type-label");
    column.append(&text);
    let button = gtk::ToggleButton::builder().child(&column).build();
    button.add_css_class("screenshot-ui-type-button");
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

impl ScreenshotUi {
    pub fn new(
        app: &gtk::Application,
        shoot_window: Rc<dyn Fn()>,
        notify: Notify,
        recorder: Rc<Recorder>,
        move_pointer: Rc<dyn Fn((f64, f64))>,
    ) -> Rc<Self> {
        let canvas = gtk::DrawingArea::new();
        canvas.set_accessible_role(gtk::AccessibleRole::Img);
        canvas.set_hexpand(true);
        canvas.set_vexpand(true);
        let fixed = gtk::Fixed::new();
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&canvas));
        overlay.add_overlay(&fixed);

        // The panel.
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 12);
        panel.add_css_class("screenshot-ui-panel");
        panel.set_size_request(PANEL_W, PANEL_H);
        let type_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let types = vec![
            (
                Mode::Selection,
                icon_label_button("screenshot-ui-area-symbolic", "Selection"),
            ),
            (
                Mode::Screen,
                icon_label_button("screenshot-ui-display-symbolic", "Screen"),
            ),
            (
                Mode::Window,
                icon_label_button("screenshot-ui-window-symbolic", "Window"),
            ),
        ];
        for (_, b) in &types {
            // GNOME's mode buttons: 89x76 each, 12px apart.
            b.set_size_request(89, 76);
            type_row.append(b);
        }
        panel.append(&type_row);
        let bottom = gtk::CenterBox::new();
        let shot_cast = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        shot_cast.add_css_class("screenshot-ui-shot-cast-container");
        shot_cast.set_valign(gtk::Align::Center);
        let shot = gtk::ToggleButton::builder()
            .child(&gtk::Image::from_icon_name("camera-photo-symbolic"))
            .active(true)
            .build();
        shot.add_css_class("screenshot-ui-shot-cast-button");
        shot.update_property(&[gtk::accessible::Property::Label("Screenshot")]);
        let cast = gtk::ToggleButton::builder()
            .child(&gtk::Image::from_icon_name("camera-web-symbolic"))
            .build();
        cast.add_css_class("screenshot-ui-shot-cast-button");
        cast.update_property(&[gtk::accessible::Property::Label("Record Screen")]);
        // The photo and video switch: one of the two is always on.
        cast.set_group(Some(&shot));
        // GNOME shows the switch only where screencasts work.
        cast.set_sensitive(crate::screencast::supported());
        shot_cast.append(&shot);
        shot_cast.append(&cast);
        bottom.set_start_widget(Some(&shot_cast));
        let capture = gtk::Button::new();
        capture.add_css_class("screenshot-ui-capture-button");
        let circle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        circle.add_css_class("screenshot-ui-capture-button-circle");
        capture.set_child(Some(&circle));
        capture.set_valign(gtk::Align::Center);
        capture.set_halign(gtk::Align::Center);
        capture.update_property(&[gtk::accessible::Property::Label("Capture")]);
        bottom.set_center_widget(Some(&capture));
        let pointer = gtk::ToggleButton::builder()
            .child(&gtk::Image::from_icon_name(
                "screenshot-ui-show-pointer-symbolic",
            ))
            .build();
        pointer.add_css_class("screenshot-ui-show-pointer-button");
        pointer.set_valign(gtk::Align::Center);
        pointer.update_property(&[gtk::accessible::Property::Label("Show Pointer")]);
        bottom.set_end_widget(Some(&pointer));
        panel.append(&bottom);
        fixed.put(&panel, 0.0, 0.0);

        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("screenshot-ui-close-button");
        close.update_property(&[gtk::accessible::Property::Label("Close")]);
        fixed.put(&close, 0.0, 0.0);

        let ui = Rc::new(Self {
            window: RefCell::new(None),
            app: app.clone(),
            overlay: overlay.clone(),
            canvas,
            fixed,
            panel,
            close,
            types,
            cast,
            capture: capture.clone(),
            pointer,
            screens: RefCell::new(Vec::new()),
            primary: Cell::new(0),
            selected_screen: Cell::new(0),
            hovered_screen: Cell::new(None),
            built: RefCell::new(Vec::new()),
            selected_name: RefCell::new(None),
            screen_windows: RefCell::new(Vec::new()),
            windows: RefCell::new(Vec::new()),
            selection: Cell::new(None),
            selection_keys: Cell::new(None),
            mode: Cell::new(Mode::Selection),
            grab: Cell::new(None),
            shoot_window,
            notify,
            recorder,
            move_pointer,
        });

        {
            let weak = Rc::downgrade(&ui);
            ui.canvas.set_draw_func(move |_, cr, w, h| {
                if let Some(ui) = weak.upgrade() {
                    ui.draw(cr, f64::from(w), f64::from(h));
                }
            });
        }
        ui.add_screen_controllers(ui.canvas.upcast_ref(), None);
        for (mode, button) in &ui.types {
            let (weak, mode) = (Rc::downgrade(&ui), *mode);
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.set_mode(mode);
                }
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            ui.cast.connect_toggled(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.sync_cast();
                }
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            capture.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.capture();
                }
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            ui.close.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.close();
                }
            });
        }
        // Drawing, moving and resizing the selection. The gesture sits
        // on the overlay, so presses the panel and window buttons leave
        // unclaimed reach it wherever they land.
        let drag_gesture = gtk::GestureDrag::new();
        {
            let weak = Rc::downgrade(&ui);
            drag_gesture.connect_drag_begin(move |_, x, y| {
                let Some(ui) = weak.upgrade() else { return };
                match ui.mode.get() {
                    Mode::Selection => {}
                    Mode::Screen => {
                        // A press on the primary's screen selector checks it.
                        ui.select_screen(ui.primary.get());
                        return;
                    }
                    Mode::Window => return,
                }
                let sel = ui.selection.get().unwrap_or(Rect {
                    x,
                    y,
                    w: 0.0,
                    h: 0.0,
                });
                ui.grab.set(Some((hit(&sel, x, y), sel, x, y)));
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            drag_gesture.connect_drag_update(move |_, dx, dy| {
                let Some(ui) = weak.upgrade() else { return };
                let Some((grab, start, px, py)) = ui.grab.get() else {
                    return;
                };
                let (w, h) = (f64::from(ui.canvas.width()), f64::from(ui.canvas.height()));
                ui.selection
                    .set(Some(drag(grab, start, (px, py), (dx, dy), (w, h))));
                ui.selection_keys.set(None);
                ui.describe_selection();
                ui.canvas.queue_draw();
            });
        }
        {
            let weak = Rc::downgrade(&ui);
            drag_gesture.connect_drag_end(move |_, _, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.grab.set(None);
                }
            });
        }
        overlay.add_controller(drag_gesture);
        // GNOME's keys ([`binding`]): Escape closes, Enter/Space/Ctrl+C
        // capture, S/C/W pick the mode, V switches between screenshot and
        // screencast, P toggles the pointer. Arrows resize the current
        // edge; Alt moves, Ctrl adjusts by one pixel, Shift reaches the
        // edge, and R resets.
        let keys = gtk::EventControllerKey::new();
        // The mode toggle can own focus and consume Return/Space itself.
        // GNOME's overlay shortcuts precede focused button activation.
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let weak = Rc::downgrade(&ui);
            keys.connect_key_pressed(move |_, key, _, modifiers| {
                let Some(ui) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                let Some(action) = binding(key, modifiers) else {
                    return glib::Propagation::Proceed;
                };
                match action {
                    Binding::Close => ui.close(),
                    Binding::Capture => ui.capture(),
                    Binding::Mode(Mode::Window) => {
                        if !ui.cast.is_active() {
                            ui.set_mode(Mode::Window);
                        }
                    }
                    Binding::Mode(mode) => ui.set_mode(mode),
                    Binding::ToggleCast => {
                        if ui.cast.is_sensitive() {
                            ui.set_cast(!ui.cast.is_active());
                        }
                    }
                    Binding::TogglePointer => ui.pointer.set_active(!ui.pointer.is_active()),
                    Binding::Reset => {
                        ui.selection.set(Some(initial_selection(
                            f64::from(ui.canvas.width()),
                            f64::from(ui.canvas.height()),
                        )));
                        // GNOME resetArea preserves the currently selected edge.
                        if let (Some(previous), Some(rect)) =
                            (ui.selection_keys.get(), ui.selection.get())
                        {
                            ui.selection_keys.set(Some(previous.reset_area(rect)));
                        }
                        ui.describe_selection();
                        ui.canvas.queue_draw();
                    }
                    Binding::Arrow {
                        direction,
                        alt,
                        ctrl,
                        shift,
                    } => {
                        if ui.mode.get() == Mode::Selection {
                            let Some(rect) = ui.selection.get() else {
                                return glib::Propagation::Proceed;
                            };
                            let mut selection = ui
                                .selection_keys
                                .get()
                                .unwrap_or_else(|| SelectionKeys::new(rect));
                            let rect = selection.adjust(
                                direction,
                                alt,
                                ctrl,
                                shift,
                                (f64::from(ui.canvas.width()), f64::from(ui.canvas.height())),
                            );
                            ui.selection_keys.set(Some(selection));
                            ui.selection.set(Some(rect));
                            ui.describe_selection();
                            ui.canvas.queue_draw();
                            (ui.move_pointer)(selection.cursor(alt));
                        } else if alt || ctrl || shift {
                            // Only the plain `move-*` bindings move
                            // between selectors.
                        } else if ui.mode.get() == Mode::Screen {
                            // `_moveFocus` on the screen selectors.
                            ui.move_screen(direction);
                        } else {
                            // `_moveFocus` on the window selector.
                            ui.overlay.child_focus(match direction {
                                Direction::Left => gtk::DirectionType::Left,
                                Direction::Right => gtk::DirectionType::Right,
                                Direction::Up => gtk::DirectionType::Up,
                                Direction::Down => gtk::DirectionType::Down,
                            });
                        }
                    }
                    Binding::Unbound => {}
                }
                glib::Propagation::Stop
            });
        }
        ui.overlay.add_controller(keys);
        ui.set_mode(Mode::Selection);
        ui
    }

    fn describe_selection(&self) {
        if let Some(rect) = self.selection.get() {
            self.canvas
                .update_property(&[gtk::accessible::Property::Label(&format!(
                    "Selected area at x {:.0}, y {:.0}, width {:.0}, height {:.0}",
                    rect.x, rect.y, rect.w, rect.h,
                ))]);
        }
    }

    /// The checked screen selector, for screen readers and proofs.
    fn describe_screen(&self) {
        let screens = self.screens.borrow();
        if let Some(screen) = screens.get(self.selected_screen.get()) {
            let r = screen.output.rect;
            self.canvas
                .update_property(&[gtk::accessible::Property::Label(&format!(
                    "Selected screen {} at x {:.0}, y {:.0}, width {:.0}, height {:.0}",
                    screen.output.name, r.x, r.y, r.w, r.h,
                ))]);
        }
    }

    fn set_mode(&self, mode: Mode) {
        self.mode.set(mode);
        for (m, button) in &self.types {
            button.set_active(*m == mode);
        }
        for w in self.windows.borrow().iter() {
            w.button.set_visible(mode == Mode::Window);
        }
        match mode {
            Mode::Screen => self.describe_screen(),
            Mode::Selection => self.describe_selection(),
            Mode::Window => {}
        }
        self.queue_draw_screens();
    }

    /// Redraw every monitor's overlay.
    fn queue_draw_screens(&self) {
        self.canvas.queue_draw();
        for other in self.screen_windows.borrow().iter() {
            other.area.queue_draw();
        }
    }

    /// Check screen selector `index`, unchecking the others
    /// (`notify::checked` in `_rebuildMonitorBins`).
    fn select_screen(&self, index: usize) {
        let Some(name) = self
            .screens
            .borrow()
            .get(index)
            .map(|s| s.output.name.clone())
        else {
            return;
        };
        self.selected_screen.set(index);
        *self.selected_name.borrow_mut() = Some(name);
        if self.mode.get() == Mode::Screen {
            self.describe_screen();
        }
        self.queue_draw_screens();
    }

    /// An arrow in Screen mode: the check moves to the monitor that way
    /// (`_moveFocus`), or stays.
    fn move_screen(&self, direction: Direction) {
        let rects: Vec<Rect> = self
            .screens
            .borrow()
            .iter()
            .map(|s| s.output.rect)
            .collect();
        if let Some(next) = neighbour(&rects, self.selected_screen.get(), direction) {
            self.select_screen(next);
        }
    }

    /// A screen selector's pointer handling on `widget`, the overlay of
    /// screen `index` (`None`: the primary's main overlay, whose drag
    /// gesture takes the press): a click checks it and hovering dims it
    /// less, in Screen mode, as the selector buttons GNOME shows there.
    fn add_screen_controllers(self: &Rc<Self>, widget: &gtk::Widget, index: Option<usize>) {
        if let Some(index) = index {
            let click = gtk::GestureClick::new();
            let weak = Rc::downgrade(self);
            click.connect_pressed(move |_, _, _, _| {
                let Some(ui) = weak.upgrade() else { return };
                if ui.mode.get() == Mode::Screen {
                    ui.select_screen(index);
                }
            });
            widget.add_controller(click);
        }
        let motion = gtk::EventControllerMotion::new();
        {
            let weak = Rc::downgrade(self);
            motion.connect_enter(move |_, _, _| {
                let Some(ui) = weak.upgrade() else { return };
                ui.hovered_screen
                    .set(Some(index.unwrap_or_else(|| ui.primary.get())));
                ui.queue_draw_screens();
            });
        }
        {
            let weak = Rc::downgrade(self);
            motion.connect_leave(move |_| {
                let Some(ui) = weak.upgrade() else { return };
                ui.hovered_screen.set(None);
                ui.queue_draw_screens();
            });
        }
        widget.add_controller(motion);
    }

    /// Switch between screenshot and screencast.
    fn set_cast(&self, cast: bool) {
        if cast {
            self.cast.set_active(true);
        } else if let Some(shot) = self.shot_button() {
            shot.set_active(true);
        }
        self.sync_cast();
    }

    fn shot_button(&self) -> Option<gtk::ToggleButton> {
        self.cast
            .prev_sibling()
            .and_then(|w| w.downcast::<gtk::ToggleButton>().ok())
    }

    /// GNOME's screencast mode (`_onCastButtonToggled`): the frozen
    /// screen gives way to the live one, the capture button turns red,
    /// and Window is off (recording a window is not supported).
    fn sync_cast(&self) {
        let cast = self.cast.is_active();
        if cast {
            self.capture.add_css_class("cast");
            if self.mode.get() == Mode::Window {
                self.set_mode(Mode::Selection);
            }
        } else {
            self.capture.remove_css_class("cast");
        }
        for (m, button) in &self.types {
            if *m == Mode::Window {
                button.set_sensitive(!cast);
            }
        }
        self.queue_draw_screens();
    }

    /// Freeze the screen and show the UI.
    pub fn open(self: &Rc<Self>) {
        self.open_in(false);
    }

    /// GNOME's `show-screen-recording-ui`: the UI in screencast mode, or
    /// the recording stopped when one is running.
    pub fn open_recording(self: &Rc<Self>) {
        if self.recorder.in_progress() {
            self.recorder.stop(|_| {});
            return;
        }
        self.open_in(true);
    }

    fn open_in(self: &Rc<Self>, cast: bool) {
        if self
            .window
            .borrow()
            .as_ref()
            .is_some_and(|w| w.is_visible())
        {
            return;
        }
        let dir = glib::user_runtime_dir();
        let outputs_dir = dir.join("tuna-screenshot-outputs");
        let windows_dir = dir.join("tuna-screenshot-windows");
        let weak = Rc::downgrade(self);
        gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, move |conn| {
            let Ok(conn) = conn else { return };
            let conn2 = conn.clone();
            // Every monitor frozen, each in its own pixels, as GNOME's
            // stage screenshot freezes them all.
            conn.call(
                Some("org.gnome.Shell.Screenshot"),
                "/org/gnome/Shell/Screenshot",
                "org.tuna.Screenshot",
                "ScreenshotOutputs",
                Some(&(outputs_dir.to_string_lossy().as_ref(),).to_variant()),
                glib::VariantTy::new("(a(siiiidbs))").ok(),
                gio::DBusCallFlags::NONE,
                10_000,
                gio::Cancellable::NONE,
                move |reply| {
                    let Some(ui) = weak.upgrade() else { return };
                    let outputs = match reply {
                        Ok(reply) => parse_outputs(&reply),
                        Err(e) => {
                            eprintln!(
                                "tuna-shell-gtk: screenshot UI could not freeze the screen: {e}"
                            );
                            return;
                        }
                    };
                    let screens: Vec<Screen> = outputs
                        .into_iter()
                        .filter_map(|output| {
                            let texture = gdk::Texture::from_filename(&output.path).ok();
                            let _ = std::fs::remove_file(&output.path);
                            Some(Screen {
                                output,
                                texture: texture?,
                            })
                        })
                        .collect();
                    if screens.is_empty() {
                        eprintln!("tuna-shell-gtk: screenshot UI could not freeze the screen");
                        return;
                    }
                    *ui.screens.borrow_mut() = screens;
                    // Every window's picture for the window selector, as
                    // GNOME takes them when the UI opens.
                    let weak = Rc::downgrade(&ui);
                    conn2.call(
                        Some("org.gnome.Shell.Screenshot"),
                        "/org/gnome/Shell/Screenshot",
                        "org.tuna.Screenshot",
                        "ScreenshotWindows",
                        Some(&(windows_dir.to_string_lossy().as_ref(),).to_variant()),
                        glib::VariantTy::new("(a(tsbiiiis))").ok(),
                        gio::DBusCallFlags::NONE,
                        10_000,
                        gio::Cancellable::NONE,
                        move |reply| {
                            let Some(ui) = weak.upgrade() else { return };
                            let windows = match reply {
                                Ok(reply) => parse_windows(&reply),
                                Err(e) => {
                                    eprintln!("tuna-shell-gtk: no window selector: {e}");
                                    Vec::new()
                                }
                            };
                            ui.set_windows(windows);
                            ui.show(cast);
                        },
                    );
                },
            );
        });
    }

    /// Build the window selector's buttons (screenshot.js
    /// `UIWindowSelectorWindow`): the picture in its slot, the 6px border
    /// around it and the check in the middle.
    fn set_windows(self: &Rc<Self>, shots: Vec<ShotWindow>) {
        for old in self.windows.borrow_mut().drain(..) {
            self.fixed.remove(&old.button);
        }
        let selected = initially_selected(&shots);
        let mut group: Option<gtk::ToggleButton> = None;
        let mut windows = Vec::new();
        for (index, shot) in shots.into_iter().enumerate() {
            let texture = gdk::Texture::from_filename(&shot.path).ok();
            let _ = std::fs::remove_file(&shot.path);
            let Some(texture) = texture else { continue };
            let (w, h) = (
                shot.slot.w.round().max(1.0) as i32,
                shot.slot.h.round().max(1.0) as i32,
            );
            let picture = gtk::DrawingArea::new();
            picture.set_content_width(w);
            picture.set_content_height(h);
            if let Ok(surface) = texture_surface(&texture) {
                let (tw, th) = (f64::from(texture.width()), f64::from(texture.height()));
                picture.set_draw_func(move |_, cr, w, h| {
                    cr.scale(f64::from(w) / tw, f64::from(h) / th);
                    let _ = cr.set_source_surface(&surface, 0.0, 0.0);
                    cr.source().set_filter(gtk::cairo::Filter::Good);
                    let _ = cr.paint();
                });
            }
            let check = gtk::Image::from_icon_name("object-select-symbolic");
            check.add_css_class("screenshot-ui-window-selector-check");
            check.set_halign(gtk::Align::Center);
            check.set_valign(gtk::Align::Center);
            let content = gtk::Overlay::new();
            content.set_child(Some(&picture));
            content.add_overlay(&check);
            let button = gtk::ToggleButton::builder().child(&content).build();
            button.add_css_class("screenshot-ui-window-selector-window");
            let title = if shot.title.is_empty() {
                "Window".to_owned()
            } else {
                shot.title.clone()
            };
            button.update_property(&[gtk::accessible::Property::Label(&title)]);
            button.set_tooltip_text(Some(&title));
            button.set_size_request(w + 2 * WINDOW_BORDER, h + 2 * WINDOW_BORDER);
            match &group {
                Some(first) => button.set_group(Some(first)),
                None => group = Some(button.clone()),
            }
            button.set_active(Some(index) == selected);
            // Focusing a window selects it, as in GNOME.
            button.connect_has_focus_notify(|b| {
                if b.has_focus() {
                    b.set_active(true);
                }
            });
            button.set_visible(self.mode.get() == Mode::Window);
            self.fixed.put(
                &button,
                shot.slot.x - f64::from(WINDOW_BORDER),
                shot.slot.y - f64::from(WINDOW_BORDER),
            );
            windows.push(PickerWindow { texture, button });
        }
        *self.windows.borrow_mut() = windows;
        // The panel and the close button stay above the windows.
        self.panel.insert_before(&self.fixed, None::<&gtk::Widget>);
        self.close.insert_before(&self.fixed, None::<&gtk::Widget>);
    }

    fn show(self: &Rc<Self>, cast: bool) {
        let monitors: Vec<gdk::Monitor> = WidgetExt::display(&self.canvas)
            .monitors()
            .iter::<gdk::Monitor>()
            .filter_map(Result::ok)
            .collect();
        let monitor_for = |name: &str| {
            monitors
                .iter()
                .find(|m| m.connector().as_deref() == Some(name))
                .cloned()
        };
        // The main overlay covers the primary, as GNOME's panel sits in
        // its primary monitor bin; another monitor needs a GDK monitor
        // to put its own overlay on.
        let outputs: Vec<ShotOutput> = {
            let mut screens = self.screens.borrow_mut();
            let main = screens.iter().position(|s| s.output.primary).unwrap_or(0);
            let mut index = 0;
            screens.retain(|s| {
                let keep = index == main || monitor_for(&s.output.name).is_some();
                index += 1;
                keep
            });
            screens.iter().map(|s| s.output.clone()).collect()
        };
        let primary = outputs.iter().position(|o| o.primary).unwrap_or(0);
        self.primary.set(primary);
        let selected = {
            let previous = self.selected_name.borrow().clone();
            selected_on_open(&self.built.borrow(), previous.as_deref(), &outputs).unwrap_or(primary)
        };
        self.selected_screen.set(selected);
        *self.selected_name.borrow_mut() = outputs.get(selected).map(|o| o.name.clone());
        *self.built.borrow_mut() = layout(&outputs);
        self.hovered_screen.set(None);
        let monitor = outputs
            .get(primary)
            .map(|o| (o.rect.w.round() as i32, o.rect.h.round() as i32))
            .or_else(|| {
                monitors.first().map(|m| {
                    let g = m.geometry();
                    (g.width(), g.height())
                })
            })
            .unwrap_or((1280, 800));
        if self.selection.get().is_none_or(|r| {
            r.x < 0.0
                || r.y < 0.0
                || r.x + r.w > f64::from(monitor.0)
                || r.y + r.h > f64::from(monitor.1)
        }) {
            self.selection.set(Some(initial_selection(
                f64::from(monitor.0),
                f64::from(monitor.1),
            )));
        }
        self.selection_keys.set(None);
        self.describe_selection();
        let px = (monitor.0 - PANEL_W) / 2;
        let py = monitor.1 - PANEL_BOTTOM - PANEL_H;
        self.fixed.move_(&self.panel, f64::from(px), f64::from(py));
        self.fixed.move_(
            &self.close,
            f64::from(px + PANEL_W - 30),
            f64::from(py - 18),
        );
        let others: Vec<ScreenWindow> = outputs
            .iter()
            .enumerate()
            .filter(|&(index, _)| index != primary)
            .filter_map(|(index, output)| {
                Some(self.new_screen_window(index, output, &monitor_for(&output.name)?))
            })
            .collect();
        *self.screen_windows.borrow_mut() = others;
        self.set_cast(cast);
        self.set_mode(self.mode.get());
        let window = self
            .window
            .borrow()
            .clone()
            .unwrap_or_else(|| self.new_window());
        if let Some(output) = outputs.get(primary) {
            window.set_monitor(monitor_for(&output.name).as_ref());
        }
        *self.window.borrow_mut() = Some(window.clone());
        let others: Vec<gtk::Window> = self
            .screen_windows
            .borrow()
            .iter()
            .map(|o| o.window.clone())
            .collect();
        for other in others {
            other.present();
        }
        window.present();
    }

    /// The overlay over a monitor other than the primary: its frozen
    /// picture and its screen selector (`UIMonitorBin`). The main overlay
    /// keeps the keyboard; Escape there closes this one too.
    fn new_screen_window(
        self: &Rc<Self>,
        index: usize,
        output: &ShotOutput,
        monitor: &gdk::Monitor,
    ) -> ScreenWindow {
        let window = gtk::Window::new();
        window.set_application(Some(&self.app));
        window.add_css_class("tuna-screenshot-ui");
        window.set_title(Some("Screenshot"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::SCREENSHOT_NAMESPACE));
        window.set_monitor(Some(monitor));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::None);
        let area = gtk::DrawingArea::new();
        area.set_accessible_role(gtk::AccessibleRole::Img);
        area.update_property(&[gtk::accessible::Property::Label(&format!(
            "Screen {}",
            output.name
        ))]);
        area.set_hexpand(true);
        area.set_vexpand(true);
        {
            let weak = Rc::downgrade(self);
            area.set_draw_func(move |_, cr, w, h| {
                if let Some(ui) = weak.upgrade() {
                    ui.draw_other(cr, index, f64::from(w), f64::from(h));
                }
            });
        }
        self.add_screen_controllers(area.upcast_ref(), Some(index));
        window.set_child(Some(&area));
        // Closed from outside (its output went away): the UI closes, as
        // through Escape.
        let weak = Rc::downgrade(self);
        window.connect_close_request(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.close();
            }
            glib::Propagation::Stop
        });
        ScreenWindow { window, area }
    }

    fn new_window(self: &Rc<Self>) -> gtk::Window {
        let window = gtk::Window::new();
        window.set_application(Some(&self.app));
        window.add_css_class("tuna-screenshot-ui");
        window.set_title(Some("Screenshot"));
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_namespace(Some(tuna_shell_control::SCREENSHOT_NAMESPACE));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::Exclusive);

        window.set_child(Some(&self.overlay));
        // The layer-shell `closed` event becomes an xdg-toplevel close
        // request. Retire that surface through the same path as Escape.
        let weak = Rc::downgrade(self);
        window.connect_close_request(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        // GTK's destroy signal waits for the final object reference, while
        // direct GtkWindow.destroy removes its native surface immediately.
        // Retire on unrealize so the held reference cannot keep a dead
        // window in the slot. Finish destruction after signal emission.
        let weak = Rc::downgrade(self);
        window.connect_unrealize(move |window| {
            if let Some(ui) = weak.upgrade() {
                let current = ui
                    .window
                    .borrow()
                    .as_ref()
                    .is_some_and(|current| current == window);
                if current {
                    ui.window.borrow_mut().take();
                    window.set_child(None::<&gtk::Widget>);
                    ui.close();
                    let retired = window.clone();
                    glib::idle_add_local_once(move || retired.destroy());
                }
            }
        });
        window
    }

    fn close(&self) {
        // Remove the retired toplevel before GTK's AT-SPI root can
        // return its unrealized context for a stale child index. Content
        // and shortcuts persist; each opening gets a fresh layer surface.
        let retired = self.window.borrow_mut().take();
        if let Some(window) = retired {
            window.set_child(None::<&gtk::Widget>);
            window.destroy();
        }
        let others: Vec<ScreenWindow> = self.screen_windows.borrow_mut().drain(..).collect();
        for other in others {
            other.window.destroy();
        }
        self.screens.borrow_mut().clear();
        for old in self.windows.borrow_mut().drain(..) {
            self.fixed.remove(&old.button);
        }
    }

    /// Screen `index` frozen, scaled to its overlay's logical size.
    fn paint_frozen(&self, cr: &gtk::cairo::Context, index: usize, w: f64, h: f64) {
        let Some(texture) = self.screens.borrow().get(index).map(|s| s.texture.clone()) else {
            return;
        };
        let sx = w / f64::from(texture.width());
        let sy = h / f64::from(texture.height());
        cr.save().ok();
        cr.scale(sx, sy);
        if let Ok(surface) = texture_surface(&texture) {
            let _ = cr.set_source_surface(&surface, 0.0, 0.0);
            let _ = cr.paint();
        }
        cr.restore().ok();
    }

    /// `.screenshot-ui-screen-selector` over screen `index`: the checked
    /// one a 2px white frame, the others dimmed to 50% black (30% while
    /// hovered).
    fn draw_screen_selector(&self, cr: &gtk::cairo::Context, index: usize, w: f64, h: f64) {
        if index == self.selected_screen.get() {
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.set_line_width(2.0);
            cr.rectangle(1.0, 1.0, w - 2.0, h - 2.0);
            let _ = cr.stroke();
        } else {
            let alpha = if self.hovered_screen.get() == Some(index) {
                0.3
            } else {
                0.5
            };
            cr.set_source_rgba(0.0, 0.0, 0.0, alpha);
            let _ = cr.paint();
        }
    }

    /// Another monitor's overlay: its frozen picture under its screen
    /// selector, shaded outside the area selection (which is on the
    /// primary) or covered by the window selector's background.
    fn draw_other(&self, cr: &gtk::cairo::Context, index: usize, w: f64, h: f64) {
        if self.mode.get() == Mode::Window {
            let (r, g, b) = SELECTOR_BACKGROUND;
            cr.set_source_rgb(r, g, b);
            let _ = cr.paint();
            return;
        }
        if !self.cast.is_active() {
            self.paint_frozen(cr, index, w, h);
        }
        match self.mode.get() {
            Mode::Selection => {
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
                let _ = cr.paint();
            }
            Mode::Screen => self.draw_screen_selector(cr, index, w, h),
            Mode::Window => {}
        }
    }

    fn draw(&self, cr: &gtk::cairo::Context, w: f64, h: f64) {
        if self.mode.get() == Mode::Window {
            // The window selector covers the screen.
            let (r, g, b) = SELECTOR_BACKGROUND;
            cr.set_source_rgb(r, g, b);
            let _ = cr.paint();
            return;
        }
        // Screencast mode shows the live screen through the UI.
        if !self.cast.is_active() {
            self.paint_frozen(cr, self.primary.get(), w, h);
        }
        match self.mode.get() {
            Mode::Selection => {
                let Some(sel) = self.selection.get() else {
                    return;
                };
                // Shade outside the selection at 50% (four rectangles).
                cr.set_source_rgba(0.0, 0.0, 0.0, 0.5);
                cr.rectangle(0.0, 0.0, w, sel.y);
                cr.rectangle(0.0, sel.y + sel.h, w, h - sel.y - sel.h);
                cr.rectangle(0.0, sel.y, sel.x, sel.h);
                cr.rectangle(sel.x + sel.w, sel.y, w - sel.x - sel.w, sel.h);
                let _ = cr.fill();
                // The 2px white border, inside the selection.
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.set_line_width(2.0);
                cr.rectangle(
                    sel.x + 1.0,
                    sel.y + 1.0,
                    (sel.w - 2.0).max(0.0),
                    (sel.h - 2.0).max(0.0),
                );
                let _ = cr.stroke();
                // The handles: 24px white discs on a soft shadow.
                for (cx, cy) in [
                    (sel.x, sel.y),
                    (sel.x + sel.w, sel.y),
                    (sel.x, sel.y + sel.h),
                    (sel.x + sel.w, sel.y + sel.h),
                ] {
                    cr.set_source_rgba(0.0, 0.0, 0.0, 0.2);
                    cr.arc(cx, cy + 1.0, HANDLE / 2.0 + 2.0, 0.0, std::f64::consts::TAU);
                    let _ = cr.fill();
                    cr.set_source_rgb(1.0, 1.0, 1.0);
                    cr.arc(cx, cy, HANDLE / 2.0, 0.0, std::f64::consts::TAU);
                    let _ = cr.fill();
                }
            }
            Mode::Screen => self.draw_screen_selector(cr, self.primary.get(), w, h),
            Mode::Window => {}
        }
    }

    fn capture(self: &Rc<Self>) {
        let (w, h) = (self.canvas.width().max(1), self.canvas.height().max(1));
        let full = Rect {
            x: 0.0,
            y: 0.0,
            w: f64::from(w),
            h: f64::from(h),
        };
        let rect_of = |index: usize| self.screens.borrow().get(index).map(|s| s.output.rect);
        if self.cast.is_active() {
            let selection = match self.mode.get() {
                Mode::Selection => match self.selection.get() {
                    Some(sel) if sel.w >= 1.0 && sel.h >= 1.0 => Some(sel),
                    _ => return,
                },
                _ => None,
            };
            let primary = rect_of(self.primary.get()).unwrap_or(full);
            let selected = rect_of(self.selected_screen.get()).unwrap_or(primary);
            let area = recording_area(selection, primary, selected);
            // Close first, so the UI is not recorded.
            self.close();
            let recorder = self.recorder.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                recorder.start(Some(area), crate::screencast::TEMPLATE, None, |result| {
                    if let Err(e) = result {
                        eprintln!("tuna-shell-gtk: screencast did not start: {e}");
                    }
                });
            });
            return;
        }
        match self.mode.get() {
            Mode::Window => {
                let chosen = self
                    .windows
                    .borrow()
                    .iter()
                    .find(|w| w.button.is_active())
                    .map(|w| w.texture.clone());
                self.close();
                match chosen {
                    // The window's own picture, at its full size.
                    Some(texture) => {
                        let (tw, th) = (f64::from(texture.width()), f64::from(texture.height()));
                        let whole = Rect {
                            x: 0.0,
                            y: 0.0,
                            w: tw,
                            h: th,
                        };
                        self.saved(save(&texture, whole, tw, th, saves_to_disk()));
                    }
                    // No selector (the compositor could not picture the
                    // windows): the focused window, as before.
                    None => (self.shoot_window)(),
                }
            }
            Mode::Screen => {
                // The checked monitor alone, in its own pixels (at its
                // scale), as `_getSelectedGeometry` cuts the stage.
                let Some(texture) = self
                    .screens
                    .borrow()
                    .get(self.selected_screen.get())
                    .map(|s| s.texture.clone())
                else {
                    return;
                };
                self.close();
                let (tw, th) = (f64::from(texture.width()), f64::from(texture.height()));
                let whole = Rect {
                    x: 0.0,
                    y: 0.0,
                    w: tw,
                    h: th,
                };
                self.saved(save(&texture, whole, tw, th, saves_to_disk()));
            }
            Mode::Selection => {
                let Some(texture) = self
                    .screens
                    .borrow()
                    .get(self.primary.get())
                    .map(|s| s.texture.clone())
                else {
                    return;
                };
                let crop = match self.selection.get() {
                    Some(sel) if sel.w >= 1.0 && sel.h >= 1.0 => sel,
                    _ => return,
                };
                self.close();
                self.saved(save(
                    &texture,
                    crop,
                    f64::from(w),
                    f64::from(h),
                    saves_to_disk(),
                ));
            }
        }
    }

    /// Put a captured screenshot on the clipboard and say so.
    fn saved(&self, result: Option<gdk::Texture>) {
        match result {
            Some(cropped) => {
                if let Some(display) = gdk::Display::default() {
                    display.clipboard().set_texture(&cropped);
                }
                (self.notify)(
                    "Screenshot captured",
                    "You can paste the image from the clipboard.",
                );
            }
            None => eprintln!("tuna-shell-gtk: screenshot UI could not save"),
        }
    }
}

/// A cairo surface holding `texture`'s pixels.
fn texture_surface(texture: &gdk::Texture) -> Result<gtk::cairo::ImageSurface, gtk::cairo::Error> {
    let (w, h) = (texture.width(), texture.height());
    let mut surface = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, w, h)?;
    let stride = surface.stride() as usize;
    {
        let mut data = surface
            .data()
            .map_err(|_| gtk::cairo::Error::SurfaceFinished)?;
        texture.download(&mut data, stride);
    }
    surface.mark_dirty();
    Ok(surface)
}

/// Whether captures may land on disk: GNOME 51's `screenshot.js`
/// `_storeScreenshot` re-reads `disable-save-to-disk` on every capture
/// and, while it denies, keeps the shot on the clipboard only.
fn saves_to_disk() -> bool {
    crate::lockdown::read().entry_visibility().save_to_disk
}

/// Crop `texture` (shown at `w` x `h`) to `crop` and return the cropped
/// image, saved where GNOME saves screenshots when `to_disk` (see
/// [`saves_to_disk`]).
fn save(texture: &gdk::Texture, crop: Rect, w: f64, h: f64, to_disk: bool) -> Option<gdk::Texture> {
    let sx = f64::from(texture.width()) / w;
    let sy = f64::from(texture.height()) / h;
    let (x, y) = ((crop.x * sx).round() as i32, (crop.y * sy).round() as i32);
    let cw = ((crop.w * sx).round() as i32).clamp(1, texture.width() - x);
    let ch = ((crop.h * sy).round() as i32).clamp(1, texture.height() - y);
    let surface = texture_surface(texture).ok()?;
    let mut out = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, cw, ch).ok()?;
    {
        let cr = gtk::cairo::Context::new(&out).ok()?;
        cr.set_source_surface(&surface, -f64::from(x), -f64::from(y))
            .ok()?;
        cr.paint().ok()?;
    }
    out.flush();
    let stride = out.stride() as usize;
    let bytes = glib::Bytes::from(&out.data().ok()?[..]);
    let cropped: gdk::Texture = gdk::MemoryTexture::new(
        cw,
        ch,
        gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &bytes,
        stride,
    )
    .upcast();
    if !to_disk {
        return Some(cropped);
    }
    // Where the compositor's screenshots go too: XDG_PICTURES_DIR, the
    // user's Pictures directory, else ~/Pictures.
    let pictures = std::env::var_os("XDG_PICTURES_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| glib::user_special_dir(glib::UserDirectory::Pictures))
        .unwrap_or_else(|| glib::home_dir().join("Pictures"));
    let dir = pictures.join("Screenshots");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(file_name(&crate::wall_clock::now().datetime()));
    cropped.save_to_png(&path).ok()?;
    Some(cropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_gnome_bindings_exactly() {
        use gdk::ModifierType as M;
        let none = M::empty();
        assert_eq!(
            binding(gdk::Key::c, M::CONTROL_MASK),
            Some(Binding::Capture)
        );
        assert_eq!(
            binding(gdk::Key::c, none),
            Some(Binding::Mode(Mode::Screen))
        );
        assert_eq!(binding(gdk::Key::Return, none), Some(Binding::Capture));
        assert_eq!(binding(gdk::Key::space, none), Some(Binding::Capture));
        assert_eq!(
            binding(gdk::Key::s, none),
            Some(Binding::Mode(Mode::Selection))
        );
        assert_eq!(
            binding(gdk::Key::w, none),
            Some(Binding::Mode(Mode::Window))
        );
        assert_eq!(binding(gdk::Key::v, none), Some(Binding::ToggleCast));
        assert_eq!(binding(gdk::Key::p, none), Some(Binding::TogglePointer));
        assert_eq!(binding(gdk::Key::r, none), Some(Binding::Reset));
        // Lock modifiers do not count.
        assert_eq!(
            binding(gdk::Key::s, M::LOCK_MASK),
            Some(Binding::Mode(Mode::Selection))
        );
        // Bindings take only their own modifiers, and only lowercase
        // keyvals: no binding for these.
        for (key, modifiers) in [
            (gdk::Key::S, M::SHIFT_MASK),
            (gdk::Key::S, M::LOCK_MASK),
            (gdk::Key::s, M::CONTROL_MASK),
            (gdk::Key::r, M::ALT_MASK),
            (gdk::Key::c, M::CONTROL_MASK | M::SHIFT_MASK),
            (gdk::Key::Return, M::SHIFT_MASK),
            (gdk::Key::KP_Enter, none),
            (gdk::Key::a, none),
        ] {
            assert_eq!(binding(key, modifiers), None, "{key:?} {modifiers:?}");
        }
        // Escape closes whatever is held.
        assert_eq!(
            binding(gdk::Key::Escape, M::SHIFT_MASK),
            Some(Binding::Close)
        );
    }

    #[test]
    fn arrows_take_gnomes_modifier_combinations_only() {
        use gdk::ModifierType as M;
        let arrow = |alt, ctrl, shift| {
            Some(Binding::Arrow {
                direction: Direction::Left,
                alt,
                ctrl,
                shift,
            })
        };
        assert_eq!(
            binding(gdk::Key::Left, M::empty()),
            arrow(false, false, false)
        );
        assert_eq!(
            binding(gdk::Key::Left, M::CONTROL_MASK),
            arrow(false, true, false)
        );
        assert_eq!(
            binding(gdk::Key::Left, M::SHIFT_MASK),
            arrow(false, false, true)
        );
        assert_eq!(
            binding(gdk::Key::Left, M::ALT_MASK),
            arrow(true, false, false)
        );
        assert_eq!(
            binding(gdk::Key::Left, M::ALT_MASK | M::CONTROL_MASK),
            arrow(true, true, false)
        );
        assert_eq!(
            binding(gdk::Key::Left, M::ALT_MASK | M::SHIFT_MASK | M::META_MASK),
            arrow(true, false, true)
        );
        for modifiers in [
            M::CONTROL_MASK | M::SHIFT_MASK,
            M::ALT_MASK | M::CONTROL_MASK | M::SHIFT_MASK,
            M::SUPER_MASK,
            M::HYPER_MASK | M::ALT_MASK,
        ] {
            assert_eq!(binding(gdk::Key::Left, modifiers), Some(Binding::Unbound));
        }
        assert_eq!(
            binding(gdk::Key::Down, M::empty()),
            Some(Binding::Arrow {
                direction: Direction::Down,
                alt: false,
                ctrl: false,
                shift: false,
            })
        );
    }

    #[test]
    #[ignore = "requires a supervised GTK test process inside a Wayland layer-shell session"]
    fn external_close_and_destroy_retire_the_screenshot_window() {
        let _control = crate::attach_control().expect("supervised shell control handshake");
        gtk::init().expect("GTK display");
        let app = gtk::Application::builder()
            .application_id("org.tuna.ScreenshotLifecycleTest")
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        let notify: Notify = Rc::new(|_, _| {});
        let ui = ScreenshotUi::new(
            &app,
            Rc::new(|| {}),
            notify.clone(),
            Recorder::new(notify),
            Rc::new(|_| {}),
        );
        let context = glib::MainContext::default();
        let mapped = |window: &gtk::Window| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !window.is_mapped() && std::time::Instant::now() < deadline {
                context.iteration(false);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(window.is_mapped(), "screenshot layer must map");
        };
        ui.show(false);
        let first = ui.window.borrow().clone().unwrap();
        mapped(&first);
        first.close();
        while context.pending() {
            context.iteration(false);
        }
        assert!(
            ui.window.borrow().is_none(),
            "close-request must retire window"
        );
        assert!(ui.overlay.parent().is_none());

        ui.show(false);
        let second = ui.window.borrow().clone().unwrap();
        assert_ne!(first, second, "reopen must create a fresh layer window");
        mapped(&second);
        second.destroy();
        assert!(
            ui.window.borrow().is_none(),
            "direct destroy must retire window"
        );
        assert!(ui.overlay.parent().is_none());

        ui.show(false);
        let third = ui.window.borrow().clone().unwrap();
        assert_ne!(second, third);
        mapped(&third);
        ui.close();
        if let Some(path) = std::env::var_os("TUNA_SCREENSHOT_LIFECYCLE_RECEIPT") {
            std::fs::write(
                path,
                "external close, direct destroy and fresh reopen passed\n",
            )
            .unwrap();
        }
    }

    #[test]
    fn the_first_selection_is_a_centred_quarter() {
        assert_eq!(
            initial_selection(1280.0, 800.0),
            Rect {
                x: 480.0,
                y: 300.0,
                w: 320.0,
                h: 200.0
            }
        );
    }

    #[test]
    fn drags_move_resize_and_draw() {
        let sel = initial_selection(1280.0, 800.0);
        assert_eq!(hit(&sel, 480.0, 300.0), Grab::Corner(0));
        assert_eq!(hit(&sel, 800.0, 500.0), Grab::Corner(3));
        assert_eq!(hit(&sel, 640.0, 400.0), Grab::Move);
        assert_eq!(hit(&sel, 100.0, 100.0), Grab::New);
        let moved = drag(
            Grab::Move,
            sel,
            (640.0, 400.0),
            (10.0, -20.0),
            (1280.0, 800.0),
        );
        assert_eq!((moved.x, moved.y, moved.w), (490.0, 280.0, 320.0));
        // Pulling the bottom-right corner past the top-left flips it.
        let flipped = drag(
            Grab::Corner(3),
            sel,
            (800.0, 500.0),
            (-400.0, -300.0),
            (1280.0, 800.0),
        );
        assert_eq!(
            flipped,
            Rect {
                x: 400.0,
                y: 200.0,
                w: 80.0,
                h: 100.0
            }
        );
        let drawn = drag(
            Grab::New,
            sel,
            (100.0, 100.0),
            (50.0, 40.0),
            (1280.0, 800.0),
        );
        assert_eq!(
            drawn,
            Rect {
                x: 100.0,
                y: 100.0,
                w: 50.0,
                h: 40.0
            }
        );
        // Moves stay on the screen.
        let edge = drag(
            Grab::Move,
            sel,
            (640.0, 400.0),
            (2000.0, 0.0),
            (1280.0, 800.0),
        );
        assert_eq!(edge.x, 960.0);
    }

    #[test]
    fn files_are_named_like_gnome() {
        let t = jiff::civil::date(2026, 10, 2).at(3, 18, 0, 0);
        assert_eq!(file_name(&t), "Screenshot From 2026-10-02 03-18-00.png");
    }

    #[test]
    fn the_window_selector_reads_the_compositors_answer() {
        let windows = vec![
            (
                3u64,
                "Alpha".to_owned(),
                false,
                100i32,
                120i32,
                400i32,
                300i32,
                "/run/a.png".to_owned(),
            ),
            (
                5u64,
                "Beta".to_owned(),
                true,
                520,
                120,
                400,
                300,
                "/run/b.png".to_owned(),
            ),
        ];
        let reply = (windows,).to_variant();
        assert_eq!(reply.type_().as_str(), "(a(tsbiiiis))");
        let parsed = parse_windows(&reply);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].title, "Beta");
        assert_eq!(
            parsed[1].slot,
            Rect {
                x: 520.0,
                y: 120.0,
                w: 400.0,
                h: 300.0
            }
        );
        // The focused window starts selected; with none, the first.
        assert_eq!(initially_selected(&parsed), Some(1));
        let mut unfocused = parsed.clone();
        unfocused[1].focused = false;
        assert_eq!(initially_selected(&unfocused), Some(0));
        assert_eq!(initially_selected(&[]), None);
    }
}
