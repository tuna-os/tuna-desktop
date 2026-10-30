//! GNOME 51's transient-surface animations (#501): the OSD, notification
//! banners and list items, modal dialogs with their lightbox, the panel
//! menus and the quick-settings submenus.
//!
//! Every animation is split into a fade part (opacity) and a motion part
//! (slide, rise, scale, height). GNOME keeps the fade under Reduced
//! Motion and drops the motion; [`motion_allowed`] is the one place that
//! decides it. With animations off everything lands on its final frame at
//! once.
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// `osdWindow.js` FADE_TIME and LEVEL_ANIMATION_TIME.
pub const OSD_MS: f64 = 100.0;
/// `messageTray.js` ANIMATION_TIME: banners in and out.
pub const BANNER_MS: f64 = 200.0;
/// A banner grows from 90% as it drops in.
const BANNER_SCALE: f64 = 0.9;
/// `messageList.js` MESSAGE_ANIMATION_TIME: list items added or removed.
pub const MESSAGE_MS: f64 = 100.0;
/// `modalDialog.js` OPEN_AND_CLOSE_TIME, also the lightbox fade.
pub const MODAL_MS: f64 = 100.0;
/// `boxpointer.js` POPUP_ANIMATION_TIME: panel menus.
pub const MENU_MS: f64 = 150.0;
/// `-arrow-rise` of `.popup-menu-boxpointer` (`$base_padding`,
/// `_popovers.scss`): how far a panel menu rises toward the panel.
pub const MENU_RISE: f64 = 6.0;
/// `BoxPointer.open` grows a panel menu from 96%.
const MENU_SCALE: f64 = 0.96;
/// `quickSettings.js` POPUP_ANIMATION_TIME: a submenu opens in two
/// halves, height then fade (closing: fade then height).
pub const SUBMENU_MS: f64 = 400.0;
/// `PopupSubMenu.open` and `close` (`popupMenu.js:1292,1326` at gnome-shell
/// 51.0): a popup submenu's height and its arrow ease over 250 ms.
pub const POPUP_SUBMENU_MS: f64 = 250.0;
/// `PopupSubMenuMenuItem`'s triangle turns about (0.5, 0.6)
/// (`popupMenu.js:1428`).
const SUBMENU_ARROW_PIVOT: (f64, f64) = (0.5, 0.6);

/// Whether transient animations play at all: the motion policy (#493)
/// allows fades unless animations are off. (GTK's enable-animations
/// follows full motion only, so it is not the switch here.)
pub fn animations() -> bool {
    crate::motion::current().allows_fades()
}

/// Whether the motion part plays beside the fade. GNOME 51 keeps fades
/// under Reduced Motion and drops the slide, scale and height.
pub fn motion_allowed() -> bool {
    crate::motion::current().allows_motion()
}

/// Clutter's EASE_OUT_QUAD.
pub fn ease_out_quad(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t)
}

/// Clutter's EASE_OUT_BACK: overshoots by about 10% before settling.
pub fn ease_out_back(t: f64) -> f64 {
    const S: f64 = 1.70158;
    if t <= 0.0 {
        return 0.0;
    }
    let p = t.min(1.0) - 1.0;
    p * p * ((S + 1.0) * p + S) + 1.0
}

/// Clutter's EASE_OUT_EXPO (`clutter/clutter/clutter-easing.c:171-175`
/// at mutter 51.0): `1 - 2^(-10t)`, exactly 1 at the end.
pub fn ease_out_expo(t: f64) -> f64 {
    if t >= 1.0 {
        1.0
    } else if t <= 0.0 {
        0.0
    } else {
        1.0 - 2f64.powf(-10.0 * t)
    }
}

/// Linear progress through a `duration_ms` animation; complete at once
/// when animations are off.
pub fn progress(elapsed_ms: f64, duration_ms: f64, enabled: bool) -> f64 {
    if !enabled || duration_ms <= 0.0 {
        1.0
    } else {
        (elapsed_ms / duration_ms).clamp(0.0, 1.0)
    }
}

pub fn lerp(from: f64, to: f64, t: f64) -> f64 {
    from + (to - from) * t
}

/// One frame of a transient surface: the fade part (`opacity`) and the
/// motion part (vertical offset, scale about the pivot, and the share of
/// its height it takes up in layout).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub opacity: f64,
    pub dy: f64,
    pub scale: f64,
    pub extent: f64,
}

impl Frame {
    pub const REST: Frame = Frame {
        opacity: 1.0,
        dy: 0.0,
        scale: 1.0,
        extent: 1.0,
    };

    /// Only the fade part, as GNOME plays it under Reduced Motion.
    pub fn without_motion(self) -> Frame {
        Frame {
            opacity: self.opacity,
            ..Frame::REST
        }
    }

    fn gated(self, motion: bool) -> Frame {
        if motion {
            self
        } else {
            self.without_motion()
        }
    }
}

/// A banner dropping in (`_showNotification`): it fades in (ease-out
/// quad) while sliding down from its own height above and growing from
/// 90% (ease-out back).
pub fn banner_enter(t: f64, height: f64) -> Frame {
    let back = ease_out_back(t);
    Frame {
        opacity: ease_out_quad(t),
        dy: -height * (1.0 - back),
        scale: lerp(BANNER_SCALE, 1.0, back),
        extent: 1.0,
    }
}

/// A banner leaving (`_hideNotification`): fade and slide back up, both
/// ease-out back.
pub fn banner_leave(t: f64, height: f64) -> Frame {
    let back = ease_out_back(t);
    Frame {
        opacity: (1.0 - back).clamp(0.0, 1.0),
        dy: -height * back,
        scale: 1.0,
        extent: 1.0,
    }
}

/// The banner shown again after an update (`_updateShowingNotification`
/// on the notification already up): from wherever it stands, it fades
/// back to full (ease-out quad) and returns to its place, jumping to 90%
/// and growing back (ease-out back).
pub fn banner_reshow(from: Frame, t: f64) -> Frame {
    let back = ease_out_back(t);
    Frame {
        opacity: lerp(from.opacity, 1.0, ease_out_quad(t)),
        dy: lerp(from.dy, 0.0, back),
        scale: lerp(BANNER_SCALE, 1.0, back),
        extent: 1.0,
    }
}

/// A list item zooming in (`_addMessage`) or out (`_removeMessage`):
/// GNOME scales the item, its layout share with it.
pub fn message(t: f64, adding: bool) -> Frame {
    let e = ease_out_quad(t);
    let shown = if adding { e } else { 1.0 - e };
    Frame {
        opacity: shown,
        dy: 0.0,
        scale: shown,
        extent: shown,
    }
}

/// A quick-settings submenu at `t` of [`SUBMENU_MS`]. Opening grows the
/// height over the first half, then fades the content in; closing
/// fades it out, then shrinks.
pub fn submenu(t: f64, opening: bool) -> Frame {
    let first = ease_out_quad(t * 2.0);
    let second = ease_out_quad(t * 2.0 - 1.0);
    let (extent, opacity) = if opening {
        (first, second)
    } else {
        (1.0 - second, 1.0 - first)
    };
    Frame {
        opacity,
        dy: 0.0,
        scale: 1.0,
        extent,
    }
}

/// A panel menu opening (`BoxPointer.open`, `PopupAnimation.FULL`, the
/// arrow on top): it fades in while dropping from [`MENU_RISE`] above
/// and growing from 96% about its top centre, all ease-out quad.
pub fn menu_open(t: f64) -> Frame {
    let e = ease_out_quad(t);
    Frame {
        opacity: e,
        dy: lerp(-MENU_RISE, 0.0, e),
        scale: lerp(MENU_SCALE, 1.0, e),
        extent: 1.0,
    }
}

/// A panel or window menu closing (`BoxPointer.close`, `PopupAnimation.FULL`,
/// arrow on top, `boxpointer.js:132-181`): from wherever it stands it
/// fades out while rising [`MENU_RISE`] and shrinking to 96% about its top
/// centre, all ease-out quad.
pub fn menu_close(from: Frame, t: f64) -> Frame {
    let e = ease_out_quad(t);
    Frame {
        opacity: lerp(from.opacity, 0.0, e),
        dy: lerp(from.dy, -MENU_RISE, e),
        scale: lerp(from.scale, MENU_SCALE, e),
        extent: 1.0,
    }
}

/// The submenu arrow's resting angle: it turns to point down when open,
/// the other way round in right-to-left text (`popupMenu.js:1290`).
pub fn submenu_arrow_angle(open: bool, rtl: bool) -> f64 {
    match (open, rtl) {
        (false, _) => 0.0,
        (true, false) => 90.0,
        (true, true) => -90.0,
    }
}

/// `PopupSubMenu._needsScrollbar` (`popupMenu.js:1241-1248`): the top
/// menu has a max height and its natural height reaches it. GNOME then
/// opens and closes the submenu without animation (`popupMenu.js:1285-1288,
/// 1322-1324`).
pub fn needs_scrollbar(top_natural: i32, top_max: i32) -> bool {
    top_max >= 0 && top_natural >= top_max
}

/// A popup submenu's motion: the share of its height it shows and its
/// arrow's rotation in degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubmenuFrame {
    pub extent: f64,
    pub angle: f64,
}

impl SubmenuFrame {
    pub const CLOSED: SubmenuFrame = SubmenuFrame {
        extent: 0.0,
        angle: 0.0,
    };
}

/// A popup submenu `t` (linear) of the way from `from`, wherever it stood
/// (a reversal mid-way starts from the drawn frame), to open or closed:
/// height and arrow both ease out expo (`PopupSubMenu.open`/`close`,
/// `popupMenu.js:1265-1342`).
pub fn popup_submenu(from: SubmenuFrame, open: bool, rtl: bool, t: f64) -> SubmenuFrame {
    let e = ease_out_expo(t);
    SubmenuFrame {
        extent: lerp(from.extent, if open { 1.0 } else { 0.0 }, e),
        angle: lerp(from.angle, submenu_arrow_angle(open, rtl), e),
    }
}

/// A plain ease-out-quad fade from `from` to `to` (OSD, modals, menus).
pub fn fade(from: f64, to: f64, t: f64) -> f64 {
    lerp(from, to, ease_out_quad(t))
}

/// Drives one animation on a widget's frame clock. Starting another on
/// the same player supersedes the running one, whose `done` never runs.
///
/// With `TUNA_TRANSIENT_TRACE` set, each animation logs whether it ran
/// instantly or when it settled (the GTK shell proof's G-TRANSIENT gates).
#[derive(Clone)]
pub struct Player {
    generation: Rc<Cell<u64>>,
    name: &'static str,
}

fn trace(name: &str, what: std::fmt::Arguments) {
    if std::env::var_os("TUNA_TRANSIENT_TRACE").is_some() {
        eprintln!("tuna-shell-gtk: transient {name} {what}");
    }
}

impl Player {
    pub fn new(name: &'static str) -> Self {
        Self {
            generation: Rc::default(),
            name,
        }
    }

    /// Cancel the running animation, if any.
    pub fn stop(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    /// Call `step` with linear progress 0..=1 each frame for
    /// `duration_ms` (scaled by the slow-down factor), then `done`. With
    /// animations off, or on a hidden
    /// widget (which gets no frames), `step(1.0)` and `done` run now.
    pub fn play(
        &self,
        widget: &impl IsA<gtk::Widget>,
        duration_ms: f64,
        step: impl Fn(f64) + 'static,
        done: impl FnOnce() + 'static,
    ) {
        self.stop();
        let generation = self.generation.get();
        let name = self.name;
        // GNOME's slow-down factor stretches every duration.
        let nominal_ms = duration_ms;
        let duration_ms = crate::motion::current().adjust_ms(duration_ms);
        if !animations() || !widget.is_visible() {
            trace(name, format_args!("instant"));
            step(1.0);
            done();
            return;
        }
        step(0.0);
        let current = self.generation.clone();
        let started = Cell::new(None);
        let done = RefCell::new(Some(done));
        widget.add_tick_callback(move |_, clock| {
            if current.get() != generation {
                return glib::ControlFlow::Break;
            }
            let start = started.get().unwrap_or_else(|| clock.frame_time());
            started.set(Some(start));
            let elapsed_ms = (clock.frame_time() - start) as f64 / 1000.0;
            let t = progress(elapsed_ms, duration_ms, animations());
            step(t);
            if t < 1.0 {
                return glib::ControlFlow::Continue;
            }
            trace(
                name,
                format_args!("settled after {elapsed_ms:.0} ms of {nominal_ms:.0} ms"),
            );
            if let Some(done) = done.borrow_mut().take() {
                done();
            }
            glib::ControlFlow::Break
        });
    }
}

mod imp {
    use super::*;

    pub struct MotionBin {
        pub dy: Cell<f64>,
        pub scale: Cell<f64>,
        pub extent: Cell<f64>,
        /// Degrees about the pivot (a submenu's arrow).
        pub rotation: Cell<f64>,
        /// The scale and rotation pivot, as a fraction of the child's
        /// size (Clutter's `pivot_point`).
        pub pivot: Cell<(f64, f64)>,
        /// Clip to the bin while it moves (off for a BoxPointer, whose
        /// rise and shrink stay inside its surface).
        pub clip: Cell<bool>,
    }

    impl Default for MotionBin {
        fn default() -> Self {
            Self {
                dy: Cell::new(0.0),
                scale: Cell::new(1.0),
                extent: Cell::new(1.0),
                rotation: Cell::new(0.0),
                pivot: Cell::new((0.5, 0.5)),
                clip: Cell::new(true),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MotionBin {
        const NAME: &'static str = "TunaMotionBin";
        type Type = super::MotionBin;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for MotionBin {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for MotionBin {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.obj().first_child() else {
                return (0, 0, -1, -1);
            };
            let (min, natural, _, _) = child.measure(orientation, for_size);
            if orientation == gtk::Orientation::Vertical {
                let extent = self.extent.get();
                let share = |v: i32| (f64::from(v) * extent).round() as i32;
                (share(min), share(natural), -1, -1)
            } else {
                (min, natural, -1, -1)
            }
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let Some(child) = self.obj().first_child() else {
                return;
            };
            // The child keeps its full height while the bin's layout
            // share shrinks; the bin clips the rest.
            let (_, natural, _, _) = child.measure(gtk::Orientation::Vertical, width);
            let child_height = height.max(natural);
            // Scale and turn about the pivot (GNOME's pivot_point,
            // the centre unless set).
            let (fx, fy) = self.pivot.get();
            let (cx, cy) = (f64::from(width) * fx, f64::from(child_height) * fy);
            let scale = self.scale.get().max(0.0) as f32;
            let transform = gtk::gsk::Transform::new()
                .translate(&gtk::graphene::Point::new(
                    cx as f32,
                    (cy + self.dy.get()) as f32,
                ))
                .rotate(self.rotation.get() as f32)
                .scale(scale, scale)
                .translate(&gtk::graphene::Point::new(-cx as f32, -cy as f32));
            child.allocate(width, child_height, -1, Some(transform));
        }
    }
}

glib::wrapper! {
    /// A one-child container that shows its child at a [`Frame`].
    pub struct MotionBin(ObjectSubclass<imp::MotionBin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MotionBin {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let bin: Self = glib::Object::new();
        child.set_parent(&bin);
        bin
    }

    /// A BoxPointer's bin (arrow on top): it scales about its top centre
    /// (`boxpointer.js:93-95`) and never clips its own rise or shrink.
    pub fn box_pointer(child: &impl IsA<gtk::Widget>) -> Self {
        let bin = Self::new(child);
        bin.imp().pivot.set((0.5, 0.0));
        bin.imp().clip.set(false);
        bin
    }

    /// Turn the child `degrees` about `pivot` (fractions of its size).
    pub fn set_rotation(&self, degrees: f64, pivot: (f64, f64)) {
        let imp = self.imp();
        imp.pivot.set(pivot);
        if imp.rotation.replace(degrees) != degrees {
            self.queue_allocate();
        }
    }

    pub fn rotation(&self) -> f64 {
        self.imp().rotation.get()
    }

    /// The frame the bin shows now.
    pub fn frame(&self) -> Frame {
        let imp = self.imp();
        Frame {
            opacity: self.opacity(),
            dy: imp.dy.get(),
            scale: imp.scale.get(),
            extent: imp.extent.get(),
        }
    }

    pub fn set_frame(&self, frame: Frame) {
        let imp = self.imp();
        self.set_opacity(frame.opacity.clamp(0.0, 1.0));
        imp.dy.set(frame.dy);
        imp.scale.set(frame.scale);
        let resized = imp.extent.replace(frame.extent) != frame.extent;
        // Clip only while something is moving, and only where asked.
        self.set_overflow(if frame == Frame::REST || !imp.clip.get() {
            gtk::Overflow::Visible
        } else {
            gtk::Overflow::Hidden
        });
        if resized {
            self.queue_resize();
        } else {
            self.queue_allocate();
        }
    }

    /// Play `frame_at` (given the bin and linear progress) over
    /// `duration_ms`, motion gated by [`motion_allowed`].
    pub fn animate(
        &self,
        player: &Player,
        duration_ms: f64,
        frame_at: impl Fn(&MotionBin, f64) -> Frame + 'static,
        done: impl FnOnce() + 'static,
    ) {
        let bin = self.downgrade();
        let motion = motion_allowed();
        player.play(
            self,
            duration_ms,
            move |t| {
                if let Some(bin) = bin.upgrade() {
                    bin.set_frame(frame_at(&bin, t).gated(motion));
                }
            },
            done,
        );
    }
}

/// A modal window (polkit, end-session, network secrets) that fades in
/// and out with its lightbox, as `ModalDialog` and `Lightbox` do. The
/// window's own background is the lightbox, so one opacity covers both.
pub struct ModalFade {
    window: gtk::Window,
    player: Player,
}

impl ModalFade {
    pub fn new(window: &impl IsA<gtk::Window>) -> Self {
        Self {
            window: window.clone().upcast(),
            player: Player::new("modal"),
        }
    }

    /// Present the window, fading in from wherever it stands.
    pub fn present(&self) {
        let from = if self.window.is_visible() {
            self.window.opacity()
        } else {
            0.0
        };
        self.window.set_can_target(true);
        self.window.set_opacity(from);
        self.window.present();
        let window = self.window.downgrade();
        self.player.play(
            &self.window,
            MODAL_MS,
            move |t| {
                if let Some(w) = window.upgrade() {
                    w.set_opacity(fade(from, 1.0, t));
                }
            },
            || {},
        );
    }

    /// Fade out, then hide. The dialog takes no more clicks meanwhile.
    pub fn hide(&self) {
        if !self.window.is_visible() {
            return;
        }
        self.window.set_can_target(false);
        let from = self.window.opacity();
        let (step, done) = (self.window.downgrade(), self.window.downgrade());
        self.player.play(
            &self.window,
            MODAL_MS,
            move |t| {
                if let Some(w) = step.upgrade() {
                    w.set_opacity(fade(from, 0.0, t));
                }
            },
            move || {
                if let Some(w) = done.upgrade() {
                    w.set_visible(false);
                    w.set_opacity(1.0);
                    w.set_can_target(true);
                }
            },
        );
    }
}

/// `PopupMenuManager._changeMenu` opens a menu fade-only when another menu
/// is active: here, another panel menu is still mapped, or one unmapped in
/// the same main-loop turn (GTK closes the old one as it opens the next).
pub fn open_fade_only(others_mapped: usize, closed_this_turn: bool) -> bool {
    others_mapped > 0 || closed_this_turn
}

thread_local! {
    static MENUS_MAPPED: Cell<usize> = const { Cell::new(0) };
    static MENU_CLOSED_THIS_TURN: Cell<bool> = const { Cell::new(false) };
    static UNANIMATED_CLOSE: RefCell<Option<Rc<dyn Fn()>>> = RefCell::new(None);
}

/// What tells the compositor that the panel menu closing now goes without
/// BoxPointer's close (`CloseMenuUnanimated`): run as a panel button is
/// hidden under its open menu, before GTK drops the menu's popup.
pub fn on_unanimated_menu_close(hook: impl Fn() + 'static) {
    UNANIMATED_CLOSE.with(|h| *h.borrow_mut() = Some(Rc::new(hook)));
}

/// `PanelMenu.Button.vfunc_hide` (`panelMenu.js:149-154`): a menu whose
/// button is no longer mapped closes with `animate: false`. Hiding the
/// button unmaps it before GTK unmaps its popover.
pub fn closes_unanimated(button_mapped: Option<bool>) -> bool {
    button_mapped == Some(false)
}

mod menu_imp {
    use super::*;

    pub struct MenuPopover {
        pub frame: Cell<Frame>,
        pub player: Player,
        /// Whether the last open played BoxPointer's rise (tests).
        pub opened_with_motion: Cell<Option<bool>>,
        /// Whether the last close went unanimated (tests).
        pub closed_unanimated: Cell<bool>,
    }

    impl Default for MenuPopover {
        fn default() -> Self {
            Self {
                frame: Cell::new(Frame::REST),
                player: Player::new("menu"),
                opened_with_motion: Cell::new(None),
                closed_unanimated: Cell::new(false),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MenuPopover {
        const NAME: &'static str = "TunaMenuPopover";
        type Type = super::MenuPopover;
        type ParentType = gtk::Popover;
    }

    impl ObjectImpl for MenuPopover {}

    impl WidgetImpl for MenuPopover {
        fn map(&self) {
            self.parent_map();
            let fade_only = open_fade_only(
                MENUS_MAPPED.with(Cell::get),
                MENU_CLOSED_THIS_TURN.with(Cell::get),
            );
            MENUS_MAPPED.with(|n| n.set(n.get() + 1));
            self.obj().play_open(fade_only);
        }

        /// The close itself is the compositor's (`menu_close.rs`): GTK
        /// has already closed the menu logically when this runs. A hidden
        /// button's menu closes unanimated (`PanelMenu.Button.vfunc_hide`).
        fn unmap(&self) {
            let hidden = closes_unanimated(self.obj().parent().map(|b| b.is_mapped()));
            self.closed_unanimated.set(hidden);
            if hidden {
                trace("menu", format_args!("close unanimated (button hidden)"));
                let hook = UNANIMATED_CLOSE.with(|h| h.borrow().clone());
                if let Some(hook) = hook {
                    hook();
                }
            }
            MENUS_MAPPED.with(|n| n.set(n.get().saturating_sub(1)));
            if !MENU_CLOSED_THIS_TURN.with(|c| c.replace(true)) {
                glib::idle_add_local_once(|| MENU_CLOSED_THIS_TURN.with(|c| c.set(false)));
            }
            self.parent_unmap();
            if hidden {
                // GNOME's menu is closed for good: GTK would map the
                // still-visible popover again with its button.
                let popover = self.obj().downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(popover) = popover.upgrade() {
                        if popover.is_visible() && !popover.is_mapped() {
                            popover.popdown();
                        }
                    }
                });
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let frame = self.frame.get();
            if frame.dy == 0.0 && frame.scale == 1.0 {
                self.parent_snapshot(snapshot);
                return;
            }
            // BoxPointer pivots about (0.5, 0) for a top arrow; the
            // contents' 6px margin is the rise.
            let cx = self.obj().width() as f32 / 2.0;
            let scale = frame.scale.max(0.0) as f32;
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(cx, frame.dy as f32));
            snapshot.scale(scale, scale);
            snapshot.translate(&gtk::graphene::Point::new(-cx, 0.0));
            self.parent_snapshot(snapshot);
            snapshot.restore();
        }
    }

    impl PopoverImpl for MenuPopover {}
}

glib::wrapper! {
    /// A panel menu: a popover that opens as GNOME's BoxPointer does.
    pub struct MenuPopover(ObjectSubclass<menu_imp::MenuPopover>)
        @extends gtk::Popover, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
            gtk::Native, gtk::ShortcutManager;
}

impl MenuPopover {
    /// A panel menu popover, to hand to [`gtk::MenuButton`] as usual.
    pub fn new_popover() -> gtk::Popover {
        glib::Object::new::<Self>().upcast()
    }

    fn set_frame(&self, frame: Frame) {
        self.set_opacity(frame.opacity.clamp(0.0, 1.0));
        self.imp().frame.set(frame);
        self.queue_draw();
    }

    /// `BoxPointer.open` on every map: fade, rise and grow over
    /// [`MENU_MS`]; Reduced Motion, or taking over from another menu,
    /// keeps only the fade.
    fn play_open(&self, fade_only: bool) {
        let motion = motion_allowed() && !fade_only;
        self.imp().opened_with_motion.set(Some(motion));
        trace(
            "menu",
            format_args!(
                "open motion={motion} rise={MENU_RISE} scale={MENU_SCALE} switch={fade_only}"
            ),
        );
        let weak = self.downgrade();
        self.imp().player.play(
            self,
            MENU_MS,
            move |t| {
                if let Some(popover) = weak.upgrade() {
                    popover.set_frame(menu_open(t).gated(motion));
                }
            },
            || {},
        );
    }
}

/// A quick-settings submenu that grows open and shrinks closed. `outer`
/// is the menu's visibility (what the rest of the panel follows);
/// `bin` wraps its content.
pub struct Submenu {
    outer: gtk::Box,
    bin: MotionBin,
    player: Player,
    closing: Cell<bool>,
}

impl Submenu {
    pub fn new(outer: &gtk::Box, bin: &MotionBin) -> Rc<Self> {
        let menu = Rc::new(Self {
            outer: outer.clone(),
            bin: bin.clone(),
            player: Player::new("submenu"),
            closing: Cell::new(false),
        });
        let weak = Rc::downgrade(&menu);
        outer.connect_visible_notify(move |outer| {
            let Some(menu) = weak.upgrade() else {
                return;
            };
            menu.closing.set(false);
            if outer.is_visible() {
                menu.play_open();
            } else {
                // Closed outright (the panel went away): settle.
                menu.player.stop();
                menu.bin.set_frame(Frame::REST);
            }
        });
        menu
    }

    fn play_open(&self) {
        self.bin.set_frame(submenu(0.0, true));
        self.bin
            .animate(&self.player, SUBMENU_MS, |_, t| submenu(t, true), || {});
    }

    /// Open, and not on its way closed.
    pub fn is_open(&self) -> bool {
        self.outer.is_visible() && !self.closing.get()
    }

    pub fn set_open(&self, open: bool) {
        if open {
            if self.outer.is_visible() {
                // Reopened while closing: settle open.
                self.closing.set(false);
                self.player.stop();
                self.bin.set_frame(Frame::REST);
            } else {
                self.outer.set_visible(true);
            }
            return;
        }
        if !self.is_open() {
            return;
        }
        self.closing.set(true);
        let outer = self.outer.downgrade();
        self.bin.animate(
            &self.player,
            SUBMENU_MS,
            |_, t| submenu(t, false),
            move || {
                if let Some(outer) = outer.upgrade() {
                    outer.set_visible(false);
                }
            },
        );
    }
}

mod popup_submenu_imp {
    use super::*;

    pub struct PopupSubmenu {
        pub header: gtk::Button,
        pub label: gtk::Label,
        pub arrow: MotionBin,
        pub bin: RefCell<Option<MotionBin>>,
        pub player: Player,
        pub open: Cell<bool>,
        /// Whether the last open or close animated (tests).
        pub animated: Cell<Option<bool>>,
    }

    impl Default for PopupSubmenu {
        fn default() -> Self {
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            label.set_hexpand(true);
            // arrowIcon(St.Side.RIGHT), turned as the submenu opens.
            let arrow = MotionBin::new(&gtk::Image::from_icon_name("pan-end-symbolic"));
            arrow.add_css_class("popup-submenu-arrow");
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            line.append(&label);
            line.append(&arrow);
            let header = gtk::Button::builder().child(&line).build();
            header.add_css_class("popup-submenu-menu-item");
            Self {
                header,
                label,
                arrow,
                bin: RefCell::new(None),
                player: Player::new("popup-submenu"),
                open: Cell::new(false),
                animated: Cell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PopupSubmenu {
        const NAME: &'static str = "TunaPopupSubmenu";
        type Type = super::PopupSubmenu;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for PopupSubmenu {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_orientation(gtk::Orientation::Vertical);
            obj.append(&self.header);
        }
    }

    impl WidgetImpl for PopupSubmenu {}
    impl BoxImpl for PopupSubmenu {}
}

glib::wrapper! {
    /// GNOME's `PopupSubMenuMenuItem` with its `PopupSubMenu`
    /// (`popupMenu.js:1216-1484`): an item that expands its submenu in
    /// place, the height and the arrow easing out expo over 250 ms.
    pub struct PopupSubmenu(ObjectSubclass<popup_submenu_imp::PopupSubmenu>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
            gtk::Orientable;
}

impl PopupSubmenu {
    /// An item labelled `label` whose submenu is `content`, closed.
    pub fn new(label: &str, content: &impl IsA<gtk::Widget>) -> Self {
        // A group holding the item and its submenu, so assistive
        // technology finds the submenu's rows beside the item they belong
        // to. The item alone carries the name.
        let menu: Self = glib::Object::builder()
            .property("accessible-role", gtk::AccessibleRole::Group)
            .build();
        let imp = menu.imp();
        imp.label.set_text(label);
        imp.header
            .update_property(&[gtk::accessible::Property::Label(label)]);
        let bin = MotionBin::new(content);
        bin.add_css_class("popup-sub-menu");
        bin.set_frame(Frame {
            extent: 0.0,
            ..Frame::REST
        });
        bin.set_visible(false);
        menu.append(&bin);
        *imp.bin.borrow_mut() = Some(bin.clone());
        menu.sync_state();
        let weak = menu.downgrade();
        imp.header.connect_clicked(move |_| {
            if let Some(menu) = weak.upgrade() {
                menu.set_open(!menu.is_open(), true);
            }
        });
        // Right on the item opens the submenu and moves into it
        // (`popupMenu.js:1393-1400`).
        let keys = gtk::EventControllerKey::new();
        let weak = menu.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| match weak.upgrade() {
            Some(menu) if key == gtk::gdk::Key::Right => {
                menu.set_open(true, true);
                if let Some(bin) = menu.bin() {
                    bin.child_focus(gtk::DirectionType::TabForward);
                }
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        imp.header.add_controller(keys);
        // Left inside closes it and returns to the item
        // (`PopupSubMenu._onKeyPress`, `popupMenu.js:1344-1354`).
        let keys = gtk::EventControllerKey::new();
        let weak = menu.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| match weak.upgrade() {
            Some(menu) if key == gtk::gdk::Key::Left => menu.close_to_item(),
            _ => glib::Propagation::Proceed,
        });
        bin.add_controller(keys);
        // syncSensitive: an insensitive item hides its arrow and closes.
        menu.connect_sensitive_notify(|menu| {
            let sensitive = menu.is_sensitive();
            menu.imp().arrow.set_visible(sensitive);
            if !sensitive {
                menu.set_open(false, true);
            }
        });
        menu
    }

    fn bin(&self) -> Option<MotionBin> {
        self.imp().bin.borrow().clone()
    }

    /// The item that opens and closes the submenu.
    pub fn header(&self) -> gtk::Button {
        self.imp().header.clone()
    }

    pub fn is_open(&self) -> bool {
        self.imp().open.get()
    }

    /// Left inside an open submenu: close it and focus its item.
    pub fn close_to_item(&self) -> glib::Propagation {
        if !self.is_open() {
            return glib::Propagation::Proceed;
        }
        self.set_open(false, true);
        self.imp().header.grab_focus();
        glib::Propagation::Stop
    }

    /// What the submenu shows now: a hidden one shows nothing.
    pub fn submenu_frame(&self) -> SubmenuFrame {
        SubmenuFrame {
            extent: self
                .bin()
                .filter(|bin| bin.is_visible())
                .map_or(0.0, |bin| bin.frame().extent),
            angle: self.imp().arrow.rotation(),
        }
    }

    /// The expanded state, for screen readers (`Atk.StateType.EXPANDED`)
    /// and the `:open` style.
    fn sync_state(&self) {
        let imp = self.imp();
        let open = imp.open.get();
        imp.header
            .update_state(&[gtk::accessible::State::Expanded(Some(open))]);
        if open {
            imp.header.add_css_class("open");
        } else {
            imp.header.remove_css_class("open");
        }
    }

    /// `_needsScrollbar` against the scrolled menu holding the submenu:
    /// its max height and its natural height with the submenu open.
    fn needs_scrollbar_now(&self) -> bool {
        let Some(scroll) = self
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>()
        else {
            return false;
        };
        let (Some(child), Some(bin)) = (scroll.child(), self.bin()) else {
            return false;
        };
        let for_width = |w: i32| if w > 0 { w } else { -1 };
        let natural = child
            .measure(gtk::Orientation::Vertical, for_width(scroll.width()))
            .1;
        let shown = bin
            .measure(gtk::Orientation::Vertical, for_width(bin.width()))
            .1;
        let full = bin.first_child().map_or(0, |c| {
            c.measure(gtk::Orientation::Vertical, for_width(bin.width()))
                .1
        });
        needs_scrollbar(natural - shown + full, scroll.max_content_height())
    }

    /// `PopupSubMenu.open`/`close`: height and arrow ease out expo over
    /// [`POPUP_SUBMENU_MS`] from where they stand, then a closed submenu
    /// hides. No animation when asked for none or when the menu must
    /// scroll. GNOME eases the height whatever Reduced Motion says
    /// (`popupMenu.js:1265-1342` never reads it; `adjustAnimationTime`
    /// only zeroes durations with animations off,
    /// `misc/animationUtils.js:23-33`), so only animations off lands it
    /// at once here (through [`Player::play`]).
    pub fn set_open(&self, open: bool, animate: bool) {
        let imp = self.imp();
        let Some(bin) = self.bin() else { return };
        if imp.open.replace(open) == open {
            return;
        }
        self.sync_state();
        let from = self.submenu_frame();
        if open {
            bin.set_visible(true);
        }
        let animate = animate && !self.needs_scrollbar_now();
        imp.animated.set(Some(animate));
        let rtl = self.direction() == gtk::TextDirection::Rtl;
        let (step_bin, arrow) = (bin.downgrade(), imp.arrow.downgrade());
        let step = move |t: f64| {
            let f = popup_submenu(from, open, rtl, t);
            if let Some(bin) = step_bin.upgrade() {
                bin.set_frame(Frame {
                    extent: f.extent,
                    ..Frame::REST
                });
            }
            if let Some(arrow) = arrow.upgrade() {
                arrow.set_rotation(f.angle, SUBMENU_ARROW_PIVOT);
            }
        };
        let done_bin = bin.downgrade();
        let done = move || {
            if let Some(bin) = done_bin.upgrade().filter(|_| !open) {
                bin.set_visible(false);
            }
        };
        if animate {
            imp.player.play(self, POPUP_SUBMENU_MS, step, done);
        } else {
            imp.player.stop();
            step(1.0);
            done();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn easing_curves_match_clutter() {
        assert_eq!(ease_out_quad(0.0), 0.0);
        assert_eq!(ease_out_quad(0.5), 0.75);
        assert_eq!(ease_out_quad(1.0), 1.0);
        assert_eq!(ease_out_quad(2.0), 1.0);
        assert!(close(ease_out_back(0.0), 0.0));
        assert!(close(ease_out_back(1.0), 1.0));
        // Ease-out back overshoots by about 10% before settling.
        let peak = (0..=100)
            .map(|i| ease_out_back(f64::from(i) / 100.0))
            .fold(0.0, f64::max);
        assert!(peak > 1.09 && peak < 1.11, "{peak}");
    }

    #[test]
    fn progress_is_linear_and_instant_when_animations_are_off() {
        assert_eq!(progress(0.0, 100.0, true), 0.0);
        assert_eq!(progress(50.0, 100.0, true), 0.5);
        assert_eq!(progress(150.0, 100.0, true), 1.0);
        assert_eq!(progress(0.0, 100.0, false), 1.0);
        assert_eq!(progress(0.0, 0.0, true), 1.0);
    }

    #[test]
    fn gnome_51_durations() {
        assert_eq!(OSD_MS, 100.0);
        assert_eq!(BANNER_MS, 200.0);
        assert_eq!(MESSAGE_MS, 100.0);
        assert_eq!(MODAL_MS, 100.0);
        assert_eq!(MENU_MS, 150.0);
        // Two 200 ms halves.
        assert_eq!(SUBMENU_MS, 400.0);
    }

    #[test]
    fn banners_drop_in_scaled_and_slide_back_up() {
        let start = banner_enter(0.0, 80.0);
        assert_eq!(start.opacity, 0.0);
        assert!(close(start.dy, -80.0));
        assert!(close(start.scale, 0.9));
        let end = banner_enter(1.0, 80.0);
        assert!(close(end.opacity, 1.0) && close(end.dy, 0.0) && close(end.scale, 1.0));
        // Ease-out back overshoots the rest position mid-way.
        assert!(banner_enter(0.7, 80.0).scale > 1.0);
        assert!(banner_enter(0.7, 80.0).dy > 0.0);
        let gone = banner_leave(1.0, 80.0);
        assert!(close(gone.opacity, 0.0) && close(gone.dy, -80.0));
        assert_eq!(banner_leave(0.0, 80.0).opacity, 1.0);
        // Opacity never leaves 0..=1 despite the overshoot.
        for i in 0..=20 {
            let t = f64::from(i) / 20.0;
            assert!((0.0..=1.0).contains(&banner_leave(t, 80.0).opacity));
        }
    }

    #[test]
    fn an_updated_banner_pops_again_from_where_it_stands() {
        // On screen: it stays put and opaque, jumps to 90% and grows back.
        let start = banner_reshow(Frame::REST, 0.0);
        assert_eq!((start.opacity, start.dy), (1.0, 0.0));
        assert!(close(start.scale, 0.9));
        assert!(banner_reshow(Frame::REST, 0.7).scale > 1.0);
        assert_eq!(banner_reshow(Frame::REST, 1.0), Frame::REST);
        // Caught sliding out: it comes back from there.
        let leaving = banner_leave(0.5, 80.0);
        let back = banner_reshow(leaving, 0.0);
        assert!(close(back.opacity, leaving.opacity) && close(back.dy, leaving.dy));
        let end = banner_reshow(leaving, 1.0);
        assert!(close(end.opacity, 1.0) && close(end.dy, 0.0) && close(end.scale, 1.0));
    }

    #[test]
    fn list_items_zoom_in_and_out() {
        assert_eq!(message(0.0, true).scale, 0.0);
        assert_eq!(message(0.5, true).extent, 0.75);
        assert_eq!(message(1.0, true), Frame::REST);
        assert_eq!(message(0.0, false), Frame::REST);
        assert_eq!(message(0.5, false).scale, 0.25);
        assert_eq!(message(1.0, false).extent, 0.0);
    }

    #[test]
    fn submenus_grow_then_fade_and_fade_then_shrink() {
        assert_eq!(submenu(0.0, true).extent, 0.0);
        assert_eq!(submenu(0.25, true).extent, 0.75);
        assert_eq!(submenu(0.25, true).opacity, 0.0);
        assert_eq!(submenu(0.5, true).extent, 1.0);
        assert_eq!(submenu(0.75, true).opacity, 0.75);
        assert_eq!(submenu(1.0, true), Frame::REST);
        assert_eq!(submenu(0.0, false), Frame::REST);
        assert_eq!(submenu(0.25, false).opacity, 0.25);
        assert_eq!(submenu(0.25, false).extent, 1.0);
        assert_eq!(submenu(0.75, false).extent, 0.25);
        assert_eq!(submenu(1.0, false).extent, 0.0);
    }

    #[test]
    fn reduced_motion_keeps_only_the_fade() {
        let frame = banner_enter(0.5, 80.0);
        let faded = frame.without_motion();
        assert_eq!(faded.opacity, frame.opacity);
        assert_eq!((faded.dy, faded.scale, faded.extent), (0.0, 1.0, 1.0));
        assert_eq!(message(0.5, true).gated(false).extent, 1.0);
        assert_eq!(message(0.5, true).gated(true), message(0.5, true));
    }

    #[test]
    fn the_motion_policy_gates_fades_and_motion() {
        // What Player and MotionBin::animate read from the policy.
        let at = |enable, reduced| {
            let policy = crate::motion::derive(Some(enable), Some(reduced), None);
            let t = progress(50.0, policy.adjust_ms(BANNER_MS), policy.allows_fades());
            (t, banner_enter(t, 80.0).gated(policy.allows_motion()))
        };
        let (t, full) = at(true, "no-preference");
        assert_eq!(full, banner_enter(t, 80.0));
        // Reduced Motion: the fade still runs, the slide and scale do not.
        let (t, faded) = at(true, "reduce");
        assert_eq!(t, 0.25);
        assert_eq!(faded, banner_enter(0.25, 80.0).without_motion());
        // Off: the last frame at once.
        let (t, off) = at(false, "no-preference");
        assert_eq!((t, off), (1.0, Frame::REST));
        // The slow-down factor stretches durations.
        let slow = crate::motion::derive(Some(true), None, Some("2"));
        assert_eq!(progress(100.0, slow.adjust_ms(MODAL_MS), true), 0.5);
    }

    #[test]
    fn a_menu_taking_over_from_another_opens_fade_only() {
        // PopupMenuManager._changeMenu: fadeOnly when a menu is active.
        assert!(!open_fade_only(0, false));
        assert!(open_fade_only(1, false));
        assert!(open_fade_only(0, true));
    }

    #[test]
    fn panel_menus_fade_rise_and_grow_open() {
        // BoxPointer.open: opacity 0, translation_y -rise, scale 0.96.
        let start = menu_open(0.0);
        assert_eq!(start.opacity, 0.0);
        assert_eq!(start.dy, -MENU_RISE);
        assert_eq!(start.scale, 0.96);
        assert_eq!(start.extent, 1.0);
        // All three ease out quad together.
        let mid = menu_open(0.5);
        assert!(close(mid.opacity, 0.75));
        assert!(close(mid.dy, -1.5));
        assert!(close(mid.scale, 0.99));
        assert_eq!(menu_open(1.0), Frame::REST);
        assert_eq!(MENU_RISE, 6.0);
        // Reduced Motion: useMotion is false, so only the fade plays.
        let faded = menu_open(0.5).gated(false);
        assert_eq!(faded.opacity, mid.opacity);
        assert_eq!((faded.dy, faded.scale), (0.0, 1.0));
        // Animations off and the slow-down factor, as Player reads them.
        let policy = crate::motion::derive(Some(false), None, None);
        let t = progress(0.0, policy.adjust_ms(MENU_MS), policy.allows_fades());
        assert_eq!(menu_open(t), Frame::REST);
        let slow = crate::motion::derive(Some(true), None, Some("2"));
        assert_eq!(progress(150.0, slow.adjust_ms(MENU_MS), true), 0.5);
    }

    #[test]
    fn fades_ease_out_quad_from_where_they_stand() {
        assert_eq!(fade(0.0, 1.0, 0.0), 0.0);
        assert_eq!(fade(0.0, 1.0, 0.5), 0.75);
        assert_eq!(fade(0.4, 0.0, 1.0), 0.0);
        assert_eq!(fade(0.5, 1.0, 0.5), 0.875);
    }

    #[test]
    fn ease_out_expo_matches_clutter() {
        // clutter-easing.c: -2^(-10 t/d) + 1, exactly 1 at t == d.
        assert_eq!(ease_out_expo(0.0), 0.0);
        assert!(close(ease_out_expo(0.5), 1.0 - 2f64.powi(-5)));
        assert!(close(ease_out_expo(0.1), 0.5));
        assert_eq!(ease_out_expo(1.0), 1.0);
        assert_eq!(ease_out_expo(3.0), 1.0);
    }

    #[test]
    fn popup_submenus_grow_and_turn_their_arrow_over_250_ms_out_expo() {
        assert_eq!(POPUP_SUBMENU_MS, 250.0);
        let open = |t| popup_submenu(SubmenuFrame::CLOSED, true, false, t);
        assert_eq!(open(0.0), SubmenuFrame::CLOSED);
        // Height and arrow share the curve: half the way 25 ms in.
        let early = open(0.1);
        assert!(close(early.extent, 0.5) && close(early.angle, 45.0));
        let mid = open(0.5);
        assert!(close(mid.extent, ease_out_expo(0.5)));
        assert!(close(mid.angle, 90.0 * ease_out_expo(0.5)));
        assert_eq!(
            open(1.0),
            SubmenuFrame {
                extent: 1.0,
                angle: 90.0
            }
        );
        // Right to left the arrow turns the other way.
        assert_eq!(
            popup_submenu(SubmenuFrame::CLOSED, true, true, 1.0).angle,
            -90.0
        );
        assert_eq!(submenu_arrow_angle(false, true), 0.0);
        // Closing shrinks back to nothing and turns the arrow back.
        let shown = open(1.0);
        assert_eq!(popup_submenu(shown, false, false, 0.0), shown);
        assert!(close(popup_submenu(shown, false, false, 0.1).extent, 0.5));
        assert_eq!(
            popup_submenu(shown, false, false, 1.0),
            SubmenuFrame::CLOSED
        );
    }

    #[test]
    fn a_reversed_popup_submenu_starts_from_its_drawn_frame() {
        // Closed 40 ms into opening: the close starts where it stood.
        let drawn = popup_submenu(SubmenuFrame::CLOSED, true, false, 40.0 / 250.0);
        let back = popup_submenu(drawn, false, false, 0.0);
        assert_eq!(back, drawn);
        // Every frame on the way back lies between the drawn one and shut.
        for i in 0..=20 {
            let f = popup_submenu(drawn, false, false, f64::from(i) / 20.0);
            assert!(f.extent <= drawn.extent && f.extent >= 0.0);
            assert!(f.angle <= drawn.angle && f.angle >= 0.0);
        }
        // Reopened mid-close: it grows on from there.
        let again = popup_submenu(back, true, false, 0.0);
        assert_eq!(again, drawn);
    }

    #[test]
    fn popup_submenus_skip_the_animation_when_the_menu_scrolls() {
        // _needsScrollbar: a max height the natural height reaches.
        assert!(!needs_scrollbar(400, -1));
        assert!(!needs_scrollbar(400, 600));
        assert!(needs_scrollbar(600, 600));
        assert!(needs_scrollbar(900, 600));
    }

    #[test]
    fn popup_submenus_keep_their_motion_under_reduced_motion() {
        // GNOME never reads reducedMotion here: only animations off (a
        // zero duration in adjustAnimationTime) lands the submenu at once.
        let at = |enable, reduced, slowdown| {
            let policy = crate::motion::derive(Some(enable), Some(reduced), slowdown);
            progress(
                125.0,
                policy.adjust_ms(POPUP_SUBMENU_MS),
                policy.allows_fades(),
            )
        };
        assert_eq!(at(true, "no-preference", None), 0.5);
        assert_eq!(at(true, "reduce", None), 0.5);
        assert_eq!(at(false, "no-preference", None), 1.0);
        assert_eq!(at(true, "no-preference", Some("2")), 0.25);
    }

    #[test]
    fn menus_close_as_boxpointer_from_where_they_stand() {
        // BoxPointer.close, FULL with the arrow on top: fade out, rise
        // 6px and shrink to 96%, all ease-out quad.
        assert_eq!(menu_close(Frame::REST, 0.0), Frame::REST);
        let mid = menu_close(Frame::REST, 0.5);
        assert!(close(mid.opacity, 0.25) && close(mid.dy, -4.5) && close(mid.scale, 0.97));
        let end = menu_close(Frame::REST, 1.0);
        assert!(close(end.opacity, 0.0) && close(end.dy, -MENU_RISE) && close(end.scale, 0.96));
        // Closed while still fading in: from that opacity, no jump.
        let opening = menu_open(0.3).without_motion();
        assert_eq!(menu_close(opening, 0.0), opening);
        // Reduced Motion keeps only the fade.
        let faded = menu_close(Frame::REST, 0.5).gated(false);
        assert_eq!((faded.dy, faded.scale), (0.0, 1.0));
        assert!(close(faded.opacity, 0.25));
    }

    #[test]
    fn a_hidden_buttons_menu_closes_unanimated() {
        // panelMenu.js:149-154: only a button no longer mapped.
        assert!(closes_unanimated(Some(false)));
        assert!(!closes_unanimated(Some(true)));
        assert!(!closes_unanimated(None));
    }
}

#[cfg(test)]
mod gtk_menu_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn pump_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
        let context = glib::MainContext::default();
        let start = Instant::now();
        while start.elapsed() < limit {
            while context.pending() {
                context.iteration(false);
            }
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        false
    }

    /// Real GTK popovers: a panel menu closes logically at once (hidden,
    /// `closed` emitted, button released) so the compositor alone draws
    /// the close; a menu opened in the same turn another closes, or while
    /// another is up, opens fade-only, and a later open rises again. A
    /// button hidden under its open menu closes it unanimated, once, and
    /// for good (`PanelMenu.Button.vfunc_hide`).
    #[test]
    #[ignore = "requires owned Xvfb/private D-Bus and explicit menu admission"]
    fn actual_gtk_menus_close_at_once_and_switch_fade_only() {
        assert_eq!(
            std::env::var("TUNA_REQUIRE_MENU_MOTION").as_deref(),
            Ok("1")
        );
        assert!(std::env::var_os("WAYLAND_DISPLAY").is_none());
        gtk::init().expect("actual GTK display required");
        crate::motion::set(tuna_shell_control::MotionPolicy::default());
        let window = gtk::Window::new();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let menus: Vec<(gtk::MenuButton, MenuPopover)> = ["Clock", "System"]
            .into_iter()
            .map(|label| {
                let popover = MenuPopover::new_popover();
                popover.set_child(Some(&gtk::Label::new(Some(label))));
                let button = gtk::MenuButton::builder()
                    .label(label)
                    .popover(&popover)
                    .build();
                bar.append(&button);
                (button, popover.downcast().unwrap())
            })
            .collect();
        window.set_child(Some(&bar));
        window.present();
        assert!(pump_until(Duration::from_secs(5), || window.is_mapped()));
        let (clock, clock_menu) = &menus[0];
        let (system, system_menu) = &menus[1];
        let closed = Rc::new(Cell::new(0));
        {
            let closed = closed.clone();
            clock_menu.connect_closed(move |_| closed.set(closed.get() + 1));
        }
        clock.popup();
        assert!(pump_until(Duration::from_secs(5), || clock_menu.is_mapped()));
        assert_eq!(clock_menu.imp().opened_with_motion.get(), Some(true));
        // Switching: the old menu closes logically in this very call.
        clock.popdown();
        assert!(!clock_menu.is_visible() && !clock_menu.is_mapped());
        assert_eq!(closed.get(), 1, "closed is emitted synchronously");
        assert!(!clock.is_active());
        system.popup();
        assert!(pump_until(Duration::from_secs(5), || system_menu.is_mapped()));
        assert_eq!(
            system_menu.imp().opened_with_motion.get(),
            Some(false),
            "a menu taking over opens fade-only"
        );
        system.popdown();
        // A later, separate open rises again.
        assert!(pump_until(Duration::from_secs(1), || {
            !MENU_CLOSED_THIS_TURN.with(Cell::get)
        }));
        clock.popup();
        assert!(pump_until(Duration::from_secs(5), || clock_menu.is_mapped()));
        assert_eq!(clock_menu.imp().opened_with_motion.get(), Some(true));
        assert!(!clock_menu.imp().closed_unanimated.get());
        // PanelMenu.Button.vfunc_hide: hiding the button closes its menu
        // with animate: false, telling the compositor first.
        let unanimated = Rc::new(Cell::new(0));
        {
            let unanimated = unanimated.clone();
            on_unanimated_menu_close(move || unanimated.set(unanimated.get() + 1));
        }
        clock.popdown();
        assert_eq!(unanimated.get(), 0, "an ordinary close animates");
        assert!(pump_until(Duration::from_secs(1), || {
            !MENU_CLOSED_THIS_TURN.with(Cell::get)
        }));
        clock.popup();
        assert!(pump_until(Duration::from_secs(5), || clock_menu.is_mapped()));
        clock.set_visible(false);
        assert!(!clock_menu.is_mapped(), "hiding the button unmaps its menu");
        assert_eq!(unanimated.get(), 1);
        assert!(clock_menu.imp().closed_unanimated.get());
        assert!(pump_until(Duration::from_secs(1), || !clock_menu.is_visible()));
        clock.set_visible(true);
        assert!(pump_until(Duration::from_secs(5), || clock.is_mapped()));
        assert!(!pump_until(Duration::from_millis(300), || clock_menu.is_mapped()));
        assert!(!clock_menu.is_visible(), "the closed menu stays closed");
        assert_eq!(MENUS_MAPPED.with(Cell::get), 0);
        UNANIMATED_CLOSE.with(|h| h.borrow_mut().take());
        window.close();
        println!("MENU-MOTION pass actual GTK menus close at once (closed/visibility/button immediate); takeover opens fade-only; later open rises; a hidden button closes its menu unanimated and for good");
    }

    /// Real GTK popup submenus (`PopupSubMenu`): height and arrow ease out
    /// expo over 250 ms from where they stand, a reversal mid-way starts
    /// from the drawn frame, a scrolling menu opens them at once, Reduced
    /// Motion keeps the height, animations off land it, and Left closes
    /// back to the item with the expanded state following.
    #[test]
    #[ignore = "requires owned Xvfb/private D-Bus and explicit menu admission"]
    fn actual_gtk_popup_submenus_ease_open_and_closed() {
        use tuna_shell_control::{MotionLevel, MotionPolicy};
        assert_eq!(
            std::env::var("TUNA_REQUIRE_MENU_MOTION").as_deref(),
            Ok("1")
        );
        assert!(std::env::var_os("WAYLAND_DISPLAY").is_none());
        gtk::init().expect("actual GTK display required");
        crate::motion::set(MotionPolicy::default());
        let rows = || {
            let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
            for label in ["Wired 1", "Wired 2", "Turn Off"] {
                rows.append(&gtk::Button::with_label(label));
            }
            rows
        };
        let submenu = PopupSubmenu::new("Ethernet (enp1s0)", &rows());
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&submenu);
        let window = gtk::Window::new();
        window.set_child(Some(&column));
        window.present();
        assert!(pump_until(Duration::from_secs(5), || submenu.is_mapped()));
        let bin = submenu.bin().unwrap();
        assert!(!bin.is_visible() && !submenu.is_open());
        let shut = SubmenuFrame::CLOSED;
        let open = SubmenuFrame {
            extent: 1.0,
            angle: 90.0,
        };
        // Opening: frames strictly between shut and open on the way.
        let started = Instant::now();
        submenu.set_open(true, true);
        assert_eq!(submenu.imp().animated.get(), Some(true));
        assert_eq!(submenu.submenu_frame(), shut);
        let mut between = 0;
        assert!(pump_until(Duration::from_secs(5), || {
            let f = submenu.submenu_frame();
            if f.extent > 0.0 && f.extent < 1.0 {
                assert!(f.angle > 0.0 && f.angle < 90.0, "{f:?}");
                between += 1;
            }
            f == open
        }));
        let took = started.elapsed();
        assert!(between > 0, "no intermediate frame");
        assert!(took >= Duration::from_millis(240), "{took:?}");
        assert!(bin.height() > 0);
        // Reversed mid-opening: the close starts from the drawn frame.
        submenu.set_open(false, true);
        assert!(pump_until(Duration::from_secs(5), || !bin.is_visible()));
        submenu.set_open(true, true);
        assert!(pump_until(Duration::from_secs(5), || {
            submenu.submenu_frame().extent > 0.3
        }));
        let drawn = submenu.submenu_frame();
        assert!(drawn.extent < 1.0);
        submenu.set_open(false, true);
        assert_eq!(submenu.submenu_frame(), drawn, "the reversal jumped");
        assert!(pump_until(Duration::from_secs(5), || !bin.is_visible()));
        assert_eq!(submenu.submenu_frame(), shut);
        // Left inside an open submenu closes it and focuses its item.
        submenu.set_open(true, false);
        assert_eq!(submenu.submenu_frame(), open);
        assert_eq!(submenu.close_to_item(), glib::Propagation::Stop);
        assert!(!submenu.is_open());
        assert!(pump_until(Duration::from_secs(5), || submenu
            .header()
            .has_focus()));
        assert_eq!(submenu.close_to_item(), glib::Propagation::Proceed);
        assert!(pump_until(Duration::from_secs(5), || !bin.is_visible()));
        // Reduced Motion: GNOME still eases the height.
        crate::motion::set(MotionPolicy::new(MotionLevel::FadeOnly, 1.0));
        submenu.set_open(true, true);
        assert_eq!(submenu.submenu_frame(), shut);
        assert!(pump_until(Duration::from_secs(5), || submenu
            .submenu_frame()
            == open));
        // Animations off: it lands at once.
        crate::motion::set(MotionPolicy::new(MotionLevel::Off, 1.0));
        submenu.set_open(false, true);
        assert_eq!(submenu.submenu_frame(), shut);
        assert!(!bin.is_visible());
        crate::motion::set(MotionPolicy::default());
        // A menu that must scroll opens it without animation.
        let scrolled = PopupSubmenu::new("Input Mode", &rows());
        let scroll = gtk::ScrolledWindow::builder()
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(true)
            .max_content_height(20)
            .child(&scrolled)
            .build();
        column.append(&scroll);
        assert!(pump_until(Duration::from_secs(5), || scrolled.is_mapped()));
        scrolled.set_open(true, true);
        assert_eq!(scrolled.imp().animated.get(), Some(false));
        assert_eq!(scrolled.submenu_frame(), open);
        window.close();
        println!("POPUP-SUBMENU pass actual GTK popup submenus ease height and arrow out-expo over 250 ms, reverse from the drawn frame, skip motion when scrolling, keep it under Reduced Motion, land with animations off, Left returns to the item");
    }
}
