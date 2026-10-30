//! tuna-shell-gtk: the Tuna Desktop shell drawn with GTK4 and libadwaita
//! (ADR 0006, #53).
//!
//! The first surface is the GNOME 51 top panel as a layer-shell strip:
//! the workspace pill on the left (Activities), the date and time in the
//! center opening the calendar and notification list, and the system
//! indicators on the right opening the quick settings grid. Everything
//! the user sees is a real GTK widget, so text uses the system interface
//! font and every control is reachable through AT-SPI for screen
//! readers. Compositor state arrives over the same versioned control
//! socket the current shell uses (`TUNA_CONTROL_SOCKET`).
//!
//! The compositor runs one supervised shell; select this one with
//! `TUNA_SHELL_BIN=tuna-shell-gtk` while it grows to parity.

mod accessibility;
mod anim_clock;
mod audio_selection;
mod audio_state;
mod background_settings;
mod brightness;
mod brightness_service;
mod bt_menu;
mod calendar;
mod display_change;
mod events;
mod fd_watch;
mod folder_dialog;
mod folders;
mod grid_animation;
mod grid_pager;
mod group_animation;
mod ibus_panel;
mod ibus_property_menu;
mod input_sources;
mod kbd_a11y;
mod keybindings;
mod live_apps;
mod location;
mod lock;
mod lock_auth;
mod lock_curtain;
mod lock_wiggle;
mod lockdown;
mod logic;
mod magnifier_sync;
mod media_keys;
mod motion;
mod network_agent;
mod network_secrets;
mod notify;
mod orca;
mod osd;
mod osk;
mod osk_layout;
mod osk_model;
mod overview;
mod pad_osd;
mod pad_osd_dbus;
mod panel_focus;
mod pointer_a11y;
mod polkit;
mod portal_opt_out;
mod power;
mod preview_chrome;
mod printers;
mod privacy;
mod privacy_portal;
mod providers;
mod public_search_diagnostic;
mod renderer;
mod run_dialog;
mod screencast;
mod screensaver;
mod screenshot_selection;
mod screenshot_ui;
mod services;
mod shell_dbus;
mod shortcut_consent;
mod sound_feedback;
mod switcher;
mod transient;
mod tray;
mod wall_clock;
mod wellbeing;
mod wifi;
mod window_menu;
mod wired;
mod ws_dots;
mod ws_popup;
mod wwan;

use std::cell::{Cell, RefCell};
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use libadwaita as adw;
use tuna_shell_host::control::{ControlClient, ControlError, Handled};

use logic::ClockFormat;

const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";
const A11Y_INTERFACE_SCHEMA: &str = "org.gnome.desktop.a11y.interface";
const NOTIFICATIONS_SCHEMA: &str = "org.gnome.desktop.notifications";
const COLOR_SCHEMA: &str = "org.gnome.settings-daemon.plugins.color";
const SESSION_SCHEMA: &str = "org.gnome.desktop.session";
const SHELL_SCHEMA: &str = "org.gnome.shell";
const SCREENSAVER_SCHEMA: &str = "org.gnome.desktop.screensaver";

fn release_version() -> &'static str {
    match option_env!("TUNA_VERSION") {
        Some(v) if !v.is_empty() => v.strip_prefix('v').unwrap_or(v),
        _ => env!("CARGO_PKG_VERSION"),
    }
}

/// GSettings for `schema` when installed (Marlin ships the GNOME desktop
/// schemas); `None` keeps the shell running without them.
fn settings(schema: &str) -> Option<gio::Settings> {
    let source = gio::SettingsSchemaSource::default()?;
    source.lookup(schema, true)?;
    Some(gio::Settings::new(schema))
}

/// Nothing to read now: the socket would block, or the client deferred
/// (or just made) a redial whose answer is still on its way.
fn is_would_block(e: &ControlError) -> bool {
    matches!(e, ControlError::WouldBlock)
        || matches!(e, ControlError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock)
}

/// What one drain of the control socket brought that the views act on
/// once the model is current.
#[derive(Default)]
struct Drained {
    results: Vec<(u64, bool)>,
    /// Lock-screen authentication steps `(request, service, event)`, in order.
    auth_events: Vec<(
        u64,
        tuna_shell_control::AuthService,
        tuna_shell_control::AuthEvent,
    )>,
    /// Grabbed accelerator presses and releases `(action, time, mode, released)`.
    accelerators: Vec<(u32, u32, u32, bool)>,
    menus: Vec<tuna_shell_host::control::WindowMenuRequest>,
    consent: Option<Option<(u64, String)>>,
    /// The latest display-change confirmation countdown, in seconds.
    display_change: Option<u32>,
    popups: Vec<(u32, u32)>,
    aid_switches: Vec<(tuna_shell_control::KeyboardAid, bool)>,
    /// The click the next hover click makes, when the compositor changed it.
    dwell_click: Option<tuna_shell_control::DwellClick>,
    /// Pointer accessibility timeouts for the pie timer, in order.
    pointer_timeouts: Vec<PointerTimeout>,
    /// The compositor's on-screen keyboard updates, in order: focus, the
    /// input-panel request, touch-mode auto-show.
    osk: Vec<(Option<tuna_shell_control::TextFieldFocus>, bool, bool)>,
    /// Drawing-tablet OSD messages, in arrival order.
    tablet: Vec<TabletUi>,
}

/// One compositor request for the tablet OSDs (wire 0.45).
enum TabletUi {
    /// `meta_display_show_osd` on this connector (`None` = current).
    Osd {
        connector: Option<String>,
        icon: String,
        label: String,
    },
    ShowPad(Box<tuna_shell_control::PadOsd>),
    Labels {
        pad: u32,
        labels: tuna_shell_control::PadOsdLabels,
    },
    Input {
        pad: u32,
        event: tuna_shell_control::PadOsdEvent,
    },
    Close(u32),
}

/// One pointer accessibility timeout report: kind, duration when it
/// started, whether it clicked when it stopped, and where.
type PointerTimeout = (
    tuna_shell_control::PointerTimeoutKind,
    Option<u32>,
    bool,
    i32,
    i32,
);

/// The control connection after a drain.
enum Link {
    /// Frames arrive on `fd`, the stream of this connection generation.
    Open {
        generation: u64,
        fd: std::os::fd::RawFd,
    },
    /// The compositor hung up: the client redials on its next poll.
    Redial,
    /// Nothing more can be read: no client, or a frame that cannot be
    /// decoded.
    Lost,
}

/// Handle every frame the control socket holds. Returns whether any
/// arrived, and whether the connection is still usable: not once it
/// closed, nor after a frame that cannot be decoded (it never leaves
/// the buffer, so reading on would spin). Either is logged once.
fn drain(
    control: &mut ControlClient,
    notify: &Rc<notify::NotifyUi>,
    out: &mut Drained,
) -> (bool, bool) {
    let mut handled = false;
    loop {
        let message = match control.poll() {
            Ok(message) => message,
            Err(e) if is_would_block(&e) => {
                // A reply written while handling a frame can block too;
                // frames already read still wait in the buffer.
                if control.has_buffered_frame() {
                    continue;
                }
                return (handled, true);
            }
            Err(e) => {
                eprintln!("tuna-shell-gtk: control error: {e}");
                if control.is_closed() || matches!(e, ControlError::Decode(_)) {
                    return (handled, false);
                }
                // The frame was read (an unexpected message, a failed
                // reply): go on with the rest.
                continue;
            }
        };
        handled = true;
        match message {
            Handled::Gap { .. } => {
                let _ = control.request_snapshot();
            }
            Handled::WindowMenu(request) => out.menus.push(request),
            Handled::ShortcutConsent(request) => out.consent = Some(request),
            Handled::ConfirmDisplayChange(secs) => out.display_change = Some(secs),
            Handled::ScreenReader(state) => {
                if matches!(
                    state,
                    tuna_shell_control::ScreenReaderState::Unavailable
                        | tuna_shell_control::ScreenReaderState::Conflict
                ) {
                    notify.post("Screen Reader", "preferences-desktop-accessibility-symbolic", "Screen Reader unavailable", "Orca could not start for this session. Check its installation and other active sessions.");
                }
            }
            Handled::KeyboardAidToggled { aid, enabled } => out.aid_switches.push((aid, enabled)),
            Handled::DwellClickType(click) => out.dwell_click = Some(click),
            Handled::OnScreenKeyboard {
                focus,
                show,
                touch_auto,
            } => out.osk.push((focus, show, touch_auto)),
            Handled::PointerTimeout {
                kind,
                duration_ms,
                clicked,
                x,
                y,
            } => out
                .pointer_timeouts
                .push((kind, duration_ms, clicked, x, y)),
            Handled::WorkspacePopup { index, count } => out.popups.push((index, count)),
            Handled::Accelerator { action, time, mode } => {
                out.accelerators.push((action, time, mode, false));
            }
            Handled::AcceleratorDeactivated { action, time, mode } => {
                out.accelerators.push((action, time, mode, true));
            }
            Handled::ShowOsd {
                connector,
                icon,
                label,
            } => out.tablet.push(TabletUi::Osd {
                connector,
                icon,
                label,
            }),
            Handled::ShowPadOsd(osd) => out.tablet.push(TabletUi::ShowPad(osd)),
            Handled::PadOsdLabels { pad, labels } => {
                out.tablet.push(TabletUi::Labels { pad, labels });
            }
            Handled::PadOsdInput { pad, event } => {
                out.tablet.push(TabletUi::Input { pad, event });
            }
            Handled::ClosePadOsd { pad } => out.tablet.push(TabletUi::Close(pad)),
            Handled::CommandResult { id, status } => out.results.push((
                id,
                matches!(status, tuna_shell_control::CommandStatus::Applied),
            )),
            Handled::AuthEvent {
                request,
                service,
                event,
            } => out.auth_events.push((request, service, event)),
            _ => {}
        }
    }
}

/// Connect, greet, and wait (bounded) for the first snapshot.
fn attach_control() -> Option<ControlClient> {
    let path = std::env::var_os("TUNA_CONTROL_SOCKET")?;
    let mut control = ControlClient::connect(std::path::Path::new(&path)).ok()?;
    control.send_hello().ok()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut greeted = false;
    while Instant::now() < deadline {
        let step = if greeted {
            control
                .poll()
                .map(|h| matches!(h, Handled::Snapshot { .. }))
        } else {
            control.await_hello().map(|()| {
                greeted = true;
                false
            })
        };
        match step {
            Ok(true) => return Some(control),
            Ok(false) => {}
            Err(e) if is_would_block(&e) => std::thread::sleep(Duration::from_millis(2)),
            Err(e) => {
                eprintln!("tuna-shell-gtk: control handshake failed: {e}");
                return None;
            }
        }
    }
    eprintln!("tuna-shell-gtk: control handshake timed out");
    None
}

struct Shell {
    control: Option<ControlClient>,
    pills: Rc<ws_dots::Indicator>,
    pill_state: Vec<bool>,
}

/// Overview actions routed through the control socket.
struct ShellActions(Rc<RefCell<Shell>>);

impl overview::OverviewActions for ShellActions {
    fn public_navigation_unlocked(&self) -> bool {
        self.0.try_borrow().ok().is_some_and(|shell| {
            shell
                .control
                .as_ref()
                .is_some_and(|control| !control.locked())
        })
    }

    fn set_window_icon_geometries(&self, geometries: Vec<tuna_shell_control::WindowIconGeometry>) {
        if let Some(control) = self.0.borrow_mut().control.as_mut() {
            let _ = control.set_window_icon_geometries(geometries);
        }
    }
    fn close_overview(&self) {
        if let Some(control) = self.0.borrow_mut().control.as_mut() {
            if control.model().is_overview_open() {
                let _ = control.toggle_overview();
            }
        }
    }

    fn activate_window(&self, id: u64) {
        if let Some(control) = self.0.borrow_mut().control.as_mut() {
            let _ = control.activate_window(id);
        }
    }

    fn window_icon(&self, id: u64) -> Option<String> {
        self.0
            .borrow()
            .control
            .as_ref()?
            .model()
            .windows()
            .iter()
            .find(|w| w.id == id)?
            .icon
            .clone()
            .filter(|icon| usable_window_icon(icon))
    }

    fn running(&self) -> Vec<(u64, Option<String>)> {
        self.0
            .borrow()
            .control
            .as_ref()
            .map(|c| {
                c.model()
                    .windows()
                    .iter()
                    .map(|w| (w.id, w.app_id.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn set_app_grid(&self, active: bool) {
        if let Some(control) = self.0.borrow_mut().control.as_mut() {
            let _ = control.set_overview_app_grid(active);
        }
    }

    fn set_search(&self, active: bool) {
        if let Some(control) = self.0.borrow_mut().control.as_mut() {
            let _ = control.set_overview_search(active);
        }
    }

    fn open_overview(&self) {
        if let Some(control) = self.0.borrow_mut().control.as_mut() {
            if !control.model().is_overview_open() {
                let _ = control.toggle_overview();
            }
        }
    }
}

/// org.gnome.Shell's methods, routed to the OSD and the overview.
struct GnomeShellDbus {
    osd: Rc<osd::OsdUi>,
    overview: Rc<RefCell<overview::OverviewUi>>,
    shell: Rc<RefCell<Shell>>,
}

impl shell_dbus::ShellActions for GnomeShellDbus {
    fn show_osd(&self, request: &osd::OsdRequest) {
        self.osd.show(request);
    }

    fn focus_search(&self) {
        overview::OverviewUi::focus_search(&self.overview);
    }

    fn show_applications(&self) {
        overview::OverviewUi::show_apps(&self.overview);
    }

    fn overview_active(&self) -> bool {
        self.shell
            .borrow()
            .control
            .as_ref()
            .is_some_and(|c| c.model().is_overview_open())
    }

    fn set_accelerator_grabs(&self, grabs: Vec<tuna_shell_control::AcceleratorGrab>) {
        if let Some(control) = self.shell.borrow_mut().control.as_mut() {
            let locked = control.locked();
            let subset = media_keys::trace_enabled().then(|| {
                let accelerators: Vec<_> = grabs.iter().map(|g| g.accelerator).collect();
                media_keys::media_subset(&accelerators)
            });
            let result = control.set_accelerator_grabs(grabs);
            if let Some(subset) = subset {
                media_keys::trace(
                    "accelerators-send",
                    serde_json::json!({
                        "request_id": result.as_ref().ok(), "write_ok": result.is_ok(),
                        "media": subset,
                    }),
                    locked,
                );
            }
        }
    }
}

/// GNOME's PanelMenu.Button is an ATK MENU named by its label
/// (panelMenu.js), so Orca says "System menu". The focusable toggle inside
/// GTK's menu button takes that role; call this before the button is
/// realized, while the role can still change.
pub(crate) fn panel_menu_role(button: &gtk::MenuButton) {
    if let Some(toggle) = button.first_child() {
        toggle.set_property("accessible-role", gtk::AccessibleRole::Menu);
    }
}

fn panel_menu_button(
    child: &impl IsA<gtk::Widget>,
    label: &str,
    popover: &gtk::Popover,
) -> gtk::MenuButton {
    let button = gtk::MenuButton::builder()
        .child(child)
        .popover(popover)
        .always_show_arrow(false)
        .build();
    button.add_css_class("panel-button");
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    panel_menu_role(&button);
    // GNOME focuses a panel menu's first item only when the keyboard
    // opened it: a click shows no focus ring (keyboard opens set it
    // back, see the keybinding toggle).
    popover.connect_show(|popover| {
        if let Some(window) = popover.root().and_downcast::<gtk::Window>() {
            window.set_focus_visible(false);
        }
    });
    button
}

/// The panel's live tray menu buttons, in strip order.
fn tray_menu_buttons(tray: &gtk::Box) -> Vec<gtk::MenuButton> {
    let mut buttons = Vec::new();
    let mut child = tray.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Ok(button) = widget.downcast::<gtk::MenuButton>() {
            buttons.push(button);
        }
    }
    buttons
}

/// Whether any panel menu (clock, system, accessibility, tray) is open.
fn panel_menu_open(
    clock: &gtk::MenuButton,
    system: &gtk::MenuButton,
    a11y: &gtk::MenuButton,
    input: &gtk::MenuButton,
    tray: &gtk::Box,
) -> bool {
    clock.is_active()
        || system.is_active()
        || a11y.is_active()
        || input.is_active()
        || tray_menu_buttons(tray).iter().any(|b| b.is_active())
}

/// Close every open panel menu; a button that owned one keeps focus.
/// Each popdown of a visible popover banks one guard credit before
/// the popdown, so the matching `closed` signal keeps the hold
/// whenever it lands (G-PANEL-FOCUS).
fn close_panel_menus(
    clock: &gtk::MenuButton,
    system: &gtk::MenuButton,
    a11y: &gtk::MenuButton,
    input: &gtk::MenuButton,
    tray: &gtk::Box,
    guard: &Cell<panel_focus::EscapeGuard>,
) {
    let mut buttons = vec![clock.clone(), system.clone(), a11y.clone(), input.clone()];
    buttons.extend(tray_menu_buttons(tray));
    for button in buttons {
        if button.is_active() {
            if let Some(popover) = button.popover() {
                if popover.is_visible() {
                    let mut state = guard.get();
                    state.note_escape_close();
                    guard.set(state);
                }
            }
            button.popdown();
            button.grab_focus();
        }
    }
}

/// Give the panel's Ctrl+Alt+Tab keyboard back to the client.
fn release_panel_focus(window: &gtk::ApplicationWindow, held: &Rc<Cell<bool>>) {
    held.set(false);
    window.set_keyboard_mode(KeyboardMode::OnDemand);
}

/// The panel owns exclusive keyboard (idempotent: layer-shell ignores
/// an unchanged mode, so re-asserting a live hold is a no-op).
fn assert_panel_hold(window: &gtk::ApplicationWindow, held: &Rc<Cell<bool>>) {
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    held.set(true);
}

/// Ctrl+Alt+Tab (`switch-panels`): the panel takes exclusive keyboard
/// and the first (forward) or last (backward) control takes GTK focus.
fn focus_panel(
    window: &gtk::ApplicationWindow,
    activities: &gtk::Button,
    system: &gtk::MenuButton,
    held: &Rc<Cell<bool>>,
    backward: bool,
) {
    assert_panel_hold(window, held);
    window.present();
    window.set_focus_visible(true);
    match panel_focus::focus_end(backward) {
        panel_focus::FocusEnd::First => activities.grab_focus(),
        panel_focus::FocusEnd::Last => system.grab_focus(),
    };
}

fn render_pills(shell: &mut Shell) {
    let wanted = match shell.control.as_ref() {
        Some(control) => {
            let model = control.model();
            if let Some(count) = model.fixed_workspace_count() {
                (0..count)
                    .map(|workspace| workspace == model.active_workspace())
                    .collect()
            } else {
                let occupied: Vec<u32> = model
                    .windows()
                    .iter()
                    .filter(|w| !w.on_all_workspaces)
                    .map(|w| w.workspace)
                    .collect();
                logic::workspace_pills(model.workspaces(), model.active_workspace(), &occupied)
            }
        }
        None => vec![true],
    };
    if wanted == shell.pill_state {
        return;
    }
    shell.pills.show(&wanted);
    shell.pill_state = wanted;
}

/// GNOME 51's date menu: the notification list beside the calendar
/// column, back on today each time it opens (dateMenu.js).
fn calendar_popover(notes: &gtk::Box) -> (gtk::Popover, Rc<calendar::CalendarUi>) {
    let popover = transient::MenuPopover::new_popover();
    popover.add_css_class("tuna-shell-popover");
    popover.add_css_class("tuna-cal");
    popover.set_has_arrow(false);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("calendar-area");
    let cal = calendar::CalendarUi::new(wall_clock::now().date());
    row.append(notes);
    row.append(&cal.column);
    popover.set_child(Some(&row));
    {
        let cal = cal.clone();
        popover.connect_show(move |_| cal.reset());
    }
    {
        let popover = popover.downgrade();
        cal.connect_open(move || {
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
        });
    }
    (popover, cal)
}

/// One GNOME 51 quick toggle (`.quick-toggle`): icon, bold title and an
/// optional subtitle in a 176x48 pill.
fn qs_tile(icon: &str, title: &str, subtitle: Option<&str>) -> services::Tile {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    let image = gtk::Image::from_icon_name(icon);
    image.set_valign(gtk::Align::Center);
    image.add_css_class("qs-icon");
    content.append(&image);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_valign(gtk::Align::Center);
    // Fixed-width toggles (GNOME's 12em): labels ellipsize instead of
    // widening the panel.
    let t = gtk::Label::new(Some(title));
    t.set_xalign(0.0);
    t.set_max_width_chars(1);
    t.set_hexpand(true);
    t.set_ellipsize(gtk::pango::EllipsizeMode::End);
    t.add_css_class("qs-title");
    text.append(&t);
    let s = gtk::Label::new(subtitle);
    s.set_xalign(0.0);
    s.set_max_width_chars(1);
    s.set_hexpand(true);
    s.set_ellipsize(gtk::pango::EllipsizeMode::End);
    s.add_css_class("qs-subtitle");
    s.set_visible(subtitle.is_some());
    text.append(&s);
    content.append(&text);
    let toggle = gtk::ToggleButton::builder().child(&content).build();
    toggle.add_css_class("qs-toggle");
    toggle.update_property(&[gtk::accessible::Property::Label(title)]);
    services::Tile::new(toggle, s, image)
}

/// A GNOME quick toggle with a menu (`.quick-toggle-has-menu`): the
/// toggle, a 1px separator, and an arrow that opens `menu` in place.
fn qs_menu_tile(icon: &str, title: &str, subtitle: Option<&str>, menu: &QsMenu) -> services::Tile {
    let mut tile = qs_tile(icon, title, subtitle);
    tile.button.add_css_class("has-menu");
    let outer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    outer.add_css_class("qs-toggle-has-menu");
    outer.append(&tile.button);
    tile.button.set_hexpand(true);
    let separator = gtk::Separator::new(gtk::Orientation::Vertical);
    separator.add_css_class("qs-separator");
    outer.append(&separator);
    let arrow = gtk::ToggleButton::new();
    arrow.set_icon_name("go-next-symbolic");
    arrow.add_css_class("qs-menu-button");
    arrow.update_property(&[gtk::accessible::Property::Label(&format!("{title} Menu"))]);
    outer.append(&arrow);
    {
        let motion = menu.motion.clone();
        arrow.connect_toggled(move |a| motion.set_open(a.is_active()));
    }
    {
        let arrow = arrow.clone();
        menu.revealer.connect_visible_notify(move |r| {
            if arrow.is_active() != r.is_visible() {
                arrow.set_active(r.is_visible());
            }
        });
    }
    // The separator and arrow follow the toggle's checked state.
    {
        let (outer, button) = (outer.clone(), tile.button.clone());
        let sync = move || {
            if button.is_active() {
                outer.add_css_class("checked");
            } else {
                outer.remove_css_class("checked");
            }
        };
        sync();
        tile.button.connect_toggled(move |_| sync());
    }
    tile.outer = outer.upcast();
    tile
}

/// A quick toggle's menu (`.quick-toggle-menu`): a header with a round
/// icon and title, then a section of items. Shown in place, spanning
/// the grid under its toggle's row.
struct QsMenu {
    /// The menu's open state: visible while open (or closing).
    revealer: gtk::Box,
    /// Grows and fades the menu open and closed (GNOME 51's
    /// QuickToggleMenu).
    motion: Rc<transient::Submenu>,
    header_icon: gtk::Image,
    section: gtk::Box,
}

impl QsMenu {
    fn new(icon: &str, title: &str) -> Self {
        let revealer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        revealer.set_visible(false);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.add_css_class("qs-menu");
        let bin = transient::MotionBin::new(&body);
        revealer.append(&bin);
        let motion = transient::Submenu::new(&revealer, &bin);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header.add_css_class("qs-menu-header");
        let header_icon = gtk::Image::from_icon_name(icon);
        header_icon.add_css_class("qs-menu-icon");
        header_icon.set_pixel_size(24);
        header.append(&header_icon);
        let label = gtk::Label::new(Some(title));
        label.add_css_class("qs-menu-title");
        header.append(&label);
        body.append(&header);
        let section = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&section);
        Self {
            revealer,
            motion,
            header_icon,
            section,
        }
    }

    /// One item: icon, label and a check ornament (hidden until set).
    fn item(&self, icon: Option<&str>, label: &str) -> (gtk::Button, gtk::Image) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        if let Some(icon) = icon {
            row.append(&gtk::Image::from_icon_name(icon));
        }
        row.append(&gtk::Label::new(Some(label)));
        let ornament = gtk::Image::from_icon_name("ornament-check-symbolic");
        ornament.set_visible(false);
        row.append(&ornament);
        let button = gtk::Button::builder().child(&row).build();
        button.add_css_class("qs-menu-item");
        button.update_property(&[gtk::accessible::Property::Label(label)]);
        self.section.append(&button);
        (button, ornament)
    }

    /// A box for items rebuilt at run time (the Wi-Fi networks).
    fn list(&self) -> gtk::Box {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        self.section.append(&list);
        list
    }

    fn separator(&self) -> gtk::Separator {
        let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        separator.add_css_class("qs-menu-separator");
        self.section.append(&separator);
        separator
    }
}

/// GNOME's quick-settings grid (`QuickSettingsLayout`): two 176px
/// columns, 12px apart; hidden toggles leave no gap; an open toggle
/// menu takes a full-width row under its toggle's row.
struct QsGrid {
    grid: gtk::Grid,
    /// Each toggle (whose visibility decides placement), the wrapper
    /// the grid holds, and its menu.
    items: RefCell<Vec<(gtk::Widget, gtk::Box, Option<gtk::Box>)>>,
}

impl QsGrid {
    fn new() -> Rc<Self> {
        let grid = gtk::Grid::builder()
            .row_spacing(12)
            .column_spacing(12)
            .column_homogeneous(true)
            .build();
        grid.add_css_class("qs-grid");
        Rc::new(Self {
            grid,
            items: RefCell::new(Vec::new()),
        })
    }

    fn add(self: &Rc<Self>, tile: &services::Tile, menu: Option<&QsMenu>) {
        let menu = menu.map(|m| m.revealer.clone());
        // GtkGrid lines toggles up by baseline, and a toggle with a
        // subtitle has a different one: the row grows 2px past GNOME's
        // 48. A vertical box reports no baseline.
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrapper.append(&tile.outer);
        self.items
            .borrow_mut()
            .push((tile.outer.clone(), wrapper, menu.clone()));
        // A strong reference: the grid lives as long as its tiles do
        // (the quick-settings popover, for the whole session).
        let this = self.clone();
        let relayout = move || this.relayout();
        let r = relayout.clone();
        tile.outer.connect_visible_notify(move |_| r());
        if let Some(menu) = menu {
            menu.connect_visible_notify(move |_| relayout());
        }
        self.relayout();
    }

    fn relayout(&self) {
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        let items = self.items.borrow();
        let visible: Vec<_> = items.iter().filter(|(w, _, _)| w.is_visible()).collect();
        let mut row = 0;
        for pair in visible.chunks(2) {
            for (col, (_, wrapper, _)) in pair.iter().enumerate() {
                self.grid.attach(wrapper, col as i32, row, 1, 1);
            }
            row += 1;
            for (_, _, menu) in pair {
                if let Some(menu) = menu {
                    self.grid.attach(menu, 0, row, 2, 1);
                    row += 1;
                }
            }
        }
    }
}

fn qs_round(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("qs-round");
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

/// The panel's status icons that quick-settings services drive.
struct PanelIcons {
    mic: gtk::Image,
    network: gtk::Image,
    location: gtk::Image,
    dnd: gtk::Image,
    volume: gtk::Image,
    power_profile: gtk::Image,
    battery: gtk::Box,
    battery_icon: gtk::Image,
    battery_percentage: gtk::Label,
}

fn quick_settings_popover(
    app: &adw::Application,
    shell: &Rc<RefCell<Shell>>,
    notify: &Rc<notify::NotifyUi>,
    power_ui: &Rc<power::PowerUi>,
    brightness_controller: &Rc<brightness_service::Controller>,
    icons: PanelIcons,
) -> gtk::Popover {
    let popover = transient::MenuPopover::new_popover();
    popover.add_css_class("tuna-shell-popover");
    popover.add_css_class("tuna-qs");
    popover.set_has_arrow(false);
    let col = gtk::Box::new(gtk::Orientation::Vertical, 12);

    // Top row: screenshot and settings left, lock and power right.
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let screenshot = qs_round("screenshooter-symbolic", "Take Screenshot");
    // GNOME shows the Settings app's own icon, in its symbolic form.
    let settings_btn = qs_round("emblem-system-symbolic", "Settings");
    settings_btn.set_child(Some(&gtk::Image::from_gicon(&gio::ThemedIcon::from_names(
        &["org.gnome.Settings-symbolic", "emblem-system-symbolic"],
    ))));
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let lock = qs_round("system-lock-screen-symbolic", "Lock Screen");
    let power = qs_round("system-shutdown-symbolic", "Power Off Menu");
    top.append(&screenshot);
    top.append(&settings_btn);
    top.append(&spacer);
    top.append(&lock);
    top.append(&power);
    {
        // GNOME's Screenshot button, through org.gnome.Shell.Screenshot
        // (#61): close the menu first so it is not in the shot.
        let (popover, notify) = (popover.clone(), notify.clone());
        screenshot.connect_clicked(move |_| {
            popover.popdown();
            take_screenshot(false, notify.clone());
        });
    }
    settings_btn.connect_clicked(|_| {
        let context = tuna_shell_host::apps::application_launch_context();
        let _ = gio::AppInfo::launch_default_for_uri("settings://", Some(&context)).or_else(|_| {
            tuna_shell_host::apps::application_command("gnome-control-center")
                .spawn()
                .map(|_| ())
                .map_err(|e| glib::Error::new(gio::IOErrorEnum::Failed, &e.to_string()))
        });
    });
    {
        let shell = shell.clone();
        let popover = popover.clone();
        lock.connect_clicked(move |_| {
            popover.popdown();
            // Administrative lockdown (#357): a disabled lock screen
            // refuses the click (live read per activation, so no stale
            // shell path can bypass the policy).
            if !lockdown::read().entry_visibility().lock_screen {
                eprintln!("tuna-shell-gtk: lock refused by administrator policy");
                return;
            }
            if let Some(control) = shell.borrow_mut().control.as_mut() {
                if let Err(e) = control.lock() {
                    eprintln!("tuna-shell-gtk: lock failed: {e}");
                }
            }
        });
    }

    // Power: GNOME's shutdown menu (status/system.js ShutdownItem), a
    // quick toggle menu opening in place under the top row.
    let power_menu_box = QsMenu::new("system-shutdown-symbolic", "Power Off");
    // Kept for the lockdown watcher below, which hides the rows
    // `disable-log-out` denies (Restart…, Power Off…, Log Out…, as
    // gnome-session's CanReboot/CanShutdown/Logout refuse them) and the
    // separator once nothing follows it (popupMenu.js hides a trailing
    // separator).
    let mut power_rows: Vec<(power::Action, gtk::Button)> = Vec::new();
    let mut log_out_separator: Option<gtk::Separator> = None;
    for action in power_ui.actions() {
        let label = match action {
            power::Action::Suspend => "Suspend",
            power::Action::Restart => "Restart…",
            power::Action::PowerOff => "Power Off…",
            power::Action::LogOut => "Log Out…",
        };
        // GNOME's menu always separates the session actions, even when
        // no power action sits above them.
        if *action == power::Action::LogOut {
            log_out_separator = Some(power_menu_box.separator());
        }
        let (item, _) = power_menu_box.item(None, label);
        power_rows.push((*action, item.clone()));
        let (power_ui, popover, action) = (power_ui.clone(), popover.clone(), *action);
        item.connect_clicked(move |_| {
            popover.popdown();
            power_ui.activate(action);
        });
    }
    let power_menu = power_menu_box.revealer.clone();
    {
        let motion = power_menu_box.motion.clone();
        power.connect_clicked(move |_| motion.set_open(!motion.is_open()));
    }
    {
        let popover = popover.clone();
        power_menu.connect_visible_notify(move |menu| {
            if menu.is_visible() {
                popover.add_css_class("qs-dimmed");
            } else {
                popover.remove_css_class("qs-dimmed");
            }
        });
    }
    {
        let menu = power_menu.clone();
        popover.connect_closed(move |_| menu.set_visible(false));
    }
    // Administrative lockdown (#357): the lock button and the
    // session-ending rows follow the live policy; a nested session
    // with no rows shows no power menu at all.
    {
        let actions = power_ui.actions().to_vec();
        let apply: Rc<dyn Fn(lockdown::EntryVisibility)> =
            Rc::new(move |visible: lockdown::EntryVisibility| {
                lock.set_visible(visible.lock_screen);
                for (action, row) in &power_rows {
                    row.set_visible(power::action_allowed(*action, visible.log_out));
                }
                if let Some(separator) = log_out_separator.as_ref() {
                    separator.set_visible(visible.log_out);
                }
                power.set_visible(power::power_menu_visible_for(&actions, visible.log_out));
            });
        apply(lockdown::read().entry_visibility());
        if let Some(lockdown_settings) = settings(lockdown::SCHEMA) {
            lockdown_settings.connect_changed(None, move |_, _| {
                apply(lockdown::read().entry_visibility());
            });
            // Keep the settings object (and its signal) alive.
            std::mem::forget(lockdown_settings);
        }
    }

    // Volume: mute button plus slider (PipeWire via wpctl).
    let volume_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let mute = gtk::Button::from_icon_name("audio-volume-high-symbolic");
    mute.add_css_class("flat");
    mute.update_property(&[gtk::accessible::Property::Label("Mute")]);
    volume_row.append(&mute);
    let slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    slider.set_value(100.0);
    slider.set_hexpand(true);
    slider.update_property(&[gtk::accessible::Property::Label("Volume")]);
    volume_row.append(&slider);
    // GNOME's sound output menu (volume.js): an arrow beside the slider,
    // offered with more than one output.
    let sound_menu_ui = QsMenu::new("audio-headphones-symbolic", "Sound Output");
    let sound_list = sound_menu_ui.list();
    sound_menu_ui.separator();
    let (sound_settings, _) = sound_menu_ui.item(None, "Sound Settings");
    sound_settings.connect_clicked(|_| {
        open_settings_panel("sound");
    });
    let sound_arrow = gtk::ToggleButton::new();
    sound_arrow.set_icon_name("go-next-symbolic");
    sound_arrow.add_css_class("qs-slider-menu-button");
    sound_arrow.update_property(&[gtk::accessible::Property::Label("Open sound output menu")]);
    sound_arrow.set_visible(false);
    {
        let motion = sound_menu_ui.motion.clone();
        sound_arrow.connect_toggled(move |a| motion.set_open(a.is_active()));
    }
    {
        let arrow = sound_arrow.clone();
        sound_menu_ui.revealer.connect_visible_notify(move |r| {
            if arrow.is_active() != r.is_visible() {
                arrow.set_active(r.is_visible());
            }
        });
    }
    volume_row.append(&sound_arrow);

    // GNOME's microphone slider (volume.js `InputStreamSlider`): shown
    // only while an app records; the icon button mutes.
    let mic_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let mic_mute = gtk::Button::from_icon_name("microphone-sensitivity-medium-symbolic");
    mic_mute.add_css_class("flat");
    mic_mute.update_property(&[gtk::accessible::Property::Label("Mute")]);
    mic_row.append(&mic_mute);
    let mic = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    mic.set_hexpand(true);
    mic.update_property(&[gtk::accessible::Property::Label("Microphone")]);
    mic_row.append(&mic);
    mic_row.set_visible(false);

    // Brightness: hidden without a backlight.
    let brightness_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    brightness_row.append(&gtk::Image::from_icon_name("display-brightness-symbolic"));
    let brightness = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    brightness.set_hexpand(true);
    brightness.update_property(&[gtk::accessible::Property::Label("Brightness")]);
    brightness_row.append(&brightness);

    // Toggle grid, two columns, GNOME 51 order (panel.js
    // `_setupIndicators`). Tiles whose service is absent hide, and the
    // grid closes the gap.
    let grid = QsGrid::new();
    // GNOME's Wi-Fi menu: the networks, then All Networks (Settings).
    let wifi_menu_ui = QsMenu::new("network-wireless-symbolic", "Wi\u{2013}Fi");
    let wifi_menu = wifi::WifiMenu::new(wifi_menu_ui.list());
    wifi_menu_ui.separator();
    let (all_networks, _) = wifi_menu_ui.item(None, "All Networks");
    all_networks.connect_clicked(|_| {
        open_settings_panel("wifi");
    });
    {
        let wifi_menu = wifi_menu.clone();
        wifi_menu_ui
            .revealer
            .connect_visible_notify(move |r| wifi_menu.set_open(r.is_visible()));
    }
    let wifi = qs_menu_tile("network-wireless-symbolic", "Wi-Fi", None, &wifi_menu_ui);
    wifi_menu.set_tile(wifi.clone());
    // GNOME's wired menu: the devices' profiles, then Wired Settings.
    let wired_menu_ui = QsMenu::new("network-wired-symbolic", "Wired Connections");
    let wired_menu = wired::WiredMenu::new(wired_menu_ui.list());
    wired_menu_ui.separator();
    let (wired_settings, _) = wired_menu_ui.item(None, "Wired Settings");
    wired_settings.connect_clicked(|_| {
        open_settings_panel("network");
    });
    let wired = qs_menu_tile("network-wired-symbolic", "Wired", None, &wired_menu_ui);
    wired_menu.set_tile(wired.clone());
    // GNOME's Bluetooth menu: devices, a placeholder, Bluetooth Settings.
    let bt_menu_ui = QsMenu::new("bluetooth-active-symbolic", "Bluetooth");
    let bt_list = bt_menu_ui.list();
    let bt_placeholder = gtk::Label::new(None);
    bt_placeholder.add_css_class("bt-menu-placeholder");
    bt_placeholder.set_wrap(true);
    bt_placeholder.set_justify(gtk::Justification::Center);
    bt_menu_ui.section.append(&bt_placeholder);
    let bt_menu = bt_menu::BtMenu::new(bt_list, bt_placeholder);
    bt_menu_ui.separator();
    let (bt_settings, _) = bt_menu_ui.item(None, "Bluetooth Settings");
    bt_settings.connect_clicked(|_| {
        open_settings_panel("bluetooth");
    });
    let bluetooth = qs_menu_tile("bluetooth-active-symbolic", "Bluetooth", None, &bt_menu_ui);
    bt_menu.set_tile(bluetooth.clone());
    // Printers (#362 P-ST-10): the live CUPS queue. The tile's
    // subtitle tracks the print service; its menu lists one compact
    // row per printer, refreshed whenever the menu opens. A row, Print
    // Settings or the toggle opens the queue window with the full
    // panel; printers have no on/off, so the toggle is momentary.
    let printers_menu_ui = QsMenu::new("printer-symbolic", "Printers");
    let printers_menu = printers::PrintersMenu::new(printers_menu_ui.list());
    printers_menu_ui.separator();
    let (print_settings, _) = printers_menu_ui.item(None, "Print Settings");
    print_settings.connect_clicked({
        let menu = printers_menu.clone();
        move |_| menu.open_window()
    });
    let printers_tile = qs_menu_tile("printer-symbolic", "Printers", None, &printers_menu_ui);
    printers_menu.set_tile(printers_tile.clone());
    printers_menu.set_on_open({
        let popover = popover.clone();
        move || popover.popdown()
    });
    {
        let menu = printers_menu.clone();
        printers_menu_ui.revealer.connect_visible_notify(move |r| {
            if r.is_visible() {
                menu.refresh();
            }
        });
    }
    printers_tile.on_user_toggle({
        let menu = printers_menu.clone();
        move |on| {
            if on {
                menu.open_window();
            }
        }
    });
    // GNOME's mobile broadband menu: the modems, then Mobile Network
    // Settings. Opening it re-reads the modems, so a freshly appeared
    // one lists without waiting for a daemon signal.
    let wwan_menu_ui = QsMenu::new(
        "network-cellular-signal-excellent-symbolic",
        "Mobile Network",
    );
    let wwan_menu = wwan::WwanMenu::new(wwan_menu_ui.list());
    wwan_menu_ui.separator();
    let (wwan_settings, _) = wwan_menu_ui.item(None, "Mobile Network Settings");
    wwan_settings.connect_clicked(|_| {
        open_settings_panel("wwan");
    });
    let wwan = qs_menu_tile(
        "network-cellular-signal-excellent-symbolic",
        "Mobile Network",
        None,
        &wwan_menu_ui,
    );
    wwan_menu.set_tile(wwan.clone());
    {
        let wwan_menu = wwan_menu.clone();
        wwan_menu_ui.revealer.connect_visible_notify(move |r| {
            if r.is_visible() {
                wwan_menu.refresh();
            }
        });
    }
    let power_menu_ui = QsMenu::new("power-profile-balanced-symbolic", "Power Mode");
    let power_items: Vec<(&'static str, gtk::Button, gtk::Image)> = [
        ("performance", "Performance"),
        ("balanced", "Balanced"),
        ("power-saver", "Power Saver"),
    ]
    .into_iter()
    .map(|(name, label)| {
        let (button, ornament) = power_menu_ui.item(Some(logic::power_mode_icon(name)), label);
        (name, button, ornament)
    })
    .collect();
    power_menu_ui.separator();
    let (power_settings, _) = power_menu_ui.item(None, "Power Settings");
    power_settings.connect_clicked(|_| {
        open_settings_panel("power");
    });
    let power_mode = qs_menu_tile(
        "power-profile-balanced-symbolic",
        "Power Mode",
        Some("Balanced"),
        &power_menu_ui,
    );
    let night = qs_tile("night-light-symbolic", "Night Light", None);
    let dark = qs_tile("dark-mode-symbolic", "Dark Style", None);
    let dnd_tile = qs_tile("notifications-disabled-symbolic", "Do Not Disturb", None);
    // Location services: the GeoClue2 agent behind the panel's in-use
    // indicator. GNOME puts the master switch in Settings, not quick
    // settings, so there is no tile: Settings (and the proof) writes
    // the switch, the agent honors it. First-use consent is the
    // overlay dialog (unknown apps prompt, the answer is saved).
    let location_agent = location::LocationAgent::new();
    location_agent.set_indicator(icons.location);
    {
        let prompt = location::LocationPrompt::new(app.upcast_ref());
        location_agent.on_prompt(move |app_id, req, respond| {
            prompt.show(app_id, req, respond);
        });
    }
    grid.add(&wifi, Some(&wifi_menu_ui));
    grid.add(&wired, Some(&wired_menu_ui));
    grid.add(&bluetooth, Some(&bt_menu_ui));
    grid.add(&printers_tile, Some(&printers_menu_ui));
    grid.add(&wwan, Some(&wwan_menu_ui));
    grid.add(&power_mode, Some(&power_menu_ui));
    for tile in [&night, &dark, &dnd_tile] {
        grid.add(tile, None);
    }
    let (
        wifi_outer,
        wired_outer,
        bluetooth_outer,
        power_outer,
        night_outer,
        dark_outer,
        dnd_outer,
        wwan_outer,
    ) = (
        wifi.outer.clone(),
        wired.outer.clone(),
        bluetooth.outer.clone(),
        power_mode.outer.clone(),
        night.outer.clone(),
        dark.outer.clone(),
        dnd_tile.outer.clone(),
        wwan.outer.clone(),
    );
    // While a toggle menu is open GNOME dims everything else in the
    // panel (brightness -0.4); selecting an item closes the panel, as
    // PopupMenu activation does.
    {
        let popover = popover.clone();
        power_menu_ui.revealer.connect_visible_notify(move |menu| {
            if menu.is_visible() {
                popover.add_css_class("qs-dimmed");
            } else {
                popover.remove_css_class("qs-dimmed");
            }
        });
    }
    for (_, button, _) in &power_items {
        let popover = popover.clone();
        button.connect_clicked(move |_| popover.popdown());
    }
    {
        let popover = popover.clone();
        power_settings.connect_clicked(move |_| popover.popdown());
    }
    {
        let menu = power_menu_ui.revealer.clone();
        popover.connect_closed(move |_| menu.set_visible(false));
    }
    // The Wi-Fi and Bluetooth menus dim the panel and reset on close the
    // same way; their rows close the panel themselves.
    for menu in [
        &wifi_menu_ui.revealer,
        &wired_menu_ui.revealer,
        &bt_menu_ui.revealer,
        &wwan_menu_ui.revealer,
        &sound_menu_ui.revealer,
    ] {
        {
            let popover = popover.clone();
            menu.connect_visible_notify(move |menu| {
                if menu.is_visible() {
                    popover.add_css_class("qs-dimmed");
                } else {
                    popover.remove_css_class("qs-dimmed");
                }
            });
        }
        let menu = menu.clone();
        popover.connect_closed(move |_| menu.set_visible(false));
    }
    for settings in [
        &all_networks,
        &wired_settings,
        &bt_settings,
        &wwan_settings,
        &sound_settings,
    ] {
        let popover = popover.clone();
        settings.connect_clicked(move |_| popover.popdown());
    }
    // The printers menu dims and resets like the other toggle menus;
    // kept as its own block so the printers wiring stays additive.
    {
        let popover = popover.clone();
        printers_menu_ui
            .revealer
            .connect_visible_notify(move |menu| {
                if menu.is_visible() {
                    popover.add_css_class("qs-dimmed");
                } else {
                    popover.remove_css_class("qs-dimmed");
                }
            });
    }
    {
        let menu = printers_menu_ui.revealer.clone();
        popover.connect_closed(move |_| menu.set_visible(false));
    }
    let dnd = dnd_tile.button.clone();

    // GNOME 51 darkMode.js / nightLight.js: the tiles follow their keys
    // whoever changes them (Settings, gsettings), not only the tile.
    if let Some(iface) = settings(INTERFACE_SCHEMA) {
        dark.button
            .set_active(logic::is_dark(&iface.string("color-scheme")));
        {
            let button = dark.button.clone();
            iface.connect_changed(Some("color-scheme"), move |s, key| {
                button.set_active(logic::is_dark(&s.string(key)));
            });
        }
        dark.button.connect_toggled(move |t| {
            if logic::is_dark(&iface.string("color-scheme")) != t.is_active() {
                let _ = iface.set_string("color-scheme", logic::color_scheme_for(t.is_active()));
            }
        });
    }
    match settings(COLOR_SCHEMA) {
        Some(color) => {
            night
                .button
                .set_active(color.boolean("night-light-enabled"));
            {
                let button = night.button.clone();
                color.connect_changed(Some("night-light-enabled"), move |s, key| {
                    button.set_active(s.boolean(key));
                });
            }
            night.button.connect_toggled(move |t| {
                if color.boolean("night-light-enabled") != t.is_active() {
                    let _ = color.set_boolean("night-light-enabled", t.is_active());
                }
            });
        }
        None => night.present(false),
    }
    let battery_summary = gtk::Label::new(None);
    battery_summary.set_xalign(0.0);
    battery_summary.set_visible(false);
    services::attach(&Rc::new(services::Widgets {
        wifi,
        wifi_menu,
        wired_menu,
        bt_menu,
        wired,
        bluetooth,
        wwan,
        wwan_menu,
        location_agent,
        power_mode,
        power_header: power_menu_ui.header_icon.clone(),
        power_items,
        panel_mic: icons.mic,
        mic,
        mic_mute,
        mic_row: mic_row.clone(),
        panel_network: icons.network,
        panel_volume: icons.volume,
        panel_power_profile: icons.power_profile,
        panel_battery: icons.battery,
        battery_icon: icons.battery_icon,
        battery_percentage: icons.battery_percentage,
        battery_summary: battery_summary.clone(),
        volume: slider,
        mute,
        brightness_row: brightness_row.clone(),
        sound_list,
        sound_arrow,
        brightness,
        brightness_controller: brightness_controller.clone(),
    }));
    // Do Not Disturb drives the shell's notification store (banners
    // held back) and mirrors GNOME's show-banners key.
    let notes_settings = settings(NOTIFICATIONS_SCHEMA).filter(|notes| {
        notes
            .settings_schema()
            .is_some_and(|schema| schema.has_key("show-banners"))
    });
    if let Some(notes) = notes_settings.as_ref() {
        notify.set_dnd(!notes.boolean("show-banners"));
        let notify = notify.clone();
        notes.connect_changed(Some("show-banners"), move |notes, _| {
            notify.set_dnd(!notes.boolean("show-banners"));
        });
    }
    dnd.set_active(notify.dnd());
    {
        let notify = notify.clone();
        dnd.connect_toggled(move |t| {
            notify.set_dnd(t.is_active());
            // This closure retains Settings and its external-change subscription.
            // Write only a user-originated change. An external show-banners
            // flip reaches this toggle through the DND listener below; echoing
            // it back makes the keyfile backend rewrite its whole cached file
            // and can revert other keys written meanwhile (lost break enable).
            if let Some(notes) = notes_settings.as_ref() {
                let banners = !t.is_active();
                if notes.boolean("show-banners") != banners {
                    let _ = notes.set_boolean("show-banners", banners);
                }
            }
        });
    }
    {
        // Keep the tile, and the panel's DND icon, in step when DND
        // changes elsewhere (GNOME's show-banners key).
        let (dnd, dnd_icon) = (dnd.clone(), icons.dnd.clone());
        dnd_icon.set_visible(notify.dnd());
        notify.connect_dnd(move |on| {
            if dnd.is_active() != on {
                dnd.set_active(on);
            }
            dnd_icon.set_visible(on);
        });
    }

    col.append(&top);
    col.append(&battery_summary);
    col.append(&power_menu);
    col.append(&volume_row);
    col.append(&sound_menu_ui.revealer);
    col.append(&mic_row);
    col.append(&brightness_row);
    col.append(&grid.grid);
    // Everything but an open toggle menu dims with the panel.
    for widget in [
        top.upcast_ref::<gtk::Widget>(),
        volume_row.upcast_ref(),
        mic_row.upcast_ref(),
        brightness_row.upcast_ref(),
    ] {
        widget.add_css_class("qs-dimmable");
    }
    for tile in [
        &wifi_outer,
        &wired_outer,
        &bluetooth_outer,
        &wwan_outer,
        &power_outer,
        &night_outer,
        &dark_outer,
        &dnd_outer,
    ] {
        tile.add_css_class("qs-dimmable");
    }
    // Service-backed tiles may add rows while an inline submenu is open.
    // Keep the popup on its monitor instead of letting GTK dismiss an
    // oversized popup; keyboard focus and scrolling still reach every row.
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(600)
        .child(&col)
        .build();
    {
        let scroll = scroll.clone();
        popover.connect_show(move |popup| {
            let parent = popup.parent();
            let surface = parent
                .as_ref()
                .and_then(|widget| widget.native())
                .and_then(|native| native.surface());
            if let Some(monitor) = surface.as_ref().and_then(|surface| {
                gtk::prelude::WidgetExt::display(popup).monitor_at_surface(surface)
            }) {
                // Reserve the panel and the popup's padding, border and gap.
                let panel_height = parent
                    .as_ref()
                    .map_or(tuna_shell_control::PANEL_HEIGHT as i32, |widget| {
                        widget.height()
                    });
                scroll.set_max_content_height(
                    (monitor.geometry().height() - panel_height - 64).max(1),
                );
            }
            scroll.vadjustment().set_value(0.0);
        });
    }
    popover.set_child(Some(&scroll));
    popover
}

/// Open a GNOME Settings panel, as the quick-settings menus' "… Settings"
/// rows do in GNOME 51.
fn open_settings_panel(panel: &str) {
    if let Err(e) = tuna_shell_host::apps::application_command("gnome-control-center")
        .arg(panel)
        .spawn()
    {
        eprintln!("tuna-shell-gtk: could not open {panel} settings: {e}");
    }
}

/// Save a screenshot of the screen (or the focused window) through
/// org.gnome.Shell.Screenshot and say so in a notification, as GNOME's
/// screenshot button and keys do. Waits a moment so a closing menu is
/// not in the shot.
///
/// Administrative lockdown (#357): while `disable-save-to-disk` denies,
/// the shot goes to the clipboard only, as GNOME 51's
/// `screenshot.js` `_storeScreenshot` does — captured into a private
/// runtime file that is read back and removed at once, never into
/// Pictures.
fn take_screenshot(window: bool, notify: Rc<notify::NotifyUi>) {
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let to_disk = lockdown::read().entry_visibility().save_to_disk;
        let scratch = (!to_disk).then(|| glib::user_runtime_dir().join("tuna-screenshot-clip.png"));
        let target = scratch
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        gio::bus_get(
            gio::BusType::Session,
            None::<&gio::Cancellable>,
            move |conn| {
                let Ok(conn) = conn else {
                    return;
                };
                let (method, args) = if window {
                    (
                        "ScreenshotWindow",
                        (true, false, true, target.as_str()).to_variant(),
                    )
                } else {
                    ("Screenshot", (false, true, target.as_str()).to_variant())
                };
                conn.call(
                    Some("org.gnome.Shell.Screenshot"),
                    "/org/gnome/Shell/Screenshot",
                    "org.gnome.Shell.Screenshot",
                    method,
                    Some(&args),
                    glib::VariantTy::new("(bs)").ok(),
                    gio::DBusCallFlags::NONE,
                    10_000,
                    None::<&gio::Cancellable>,
                    move |reply| match reply {
                        Ok(reply) => {
                            let path = reply
                                .try_child_value(1)
                                .and_then(|v| v.get::<String>())
                                .unwrap_or_default();
                            if let Some(scratch) = scratch {
                                let texture = gtk::gdk::Texture::from_filename(&scratch).ok();
                                let _ = std::fs::remove_file(&scratch);
                                let (Some(texture), Some(display)) =
                                    (texture, gtk::gdk::Display::default())
                                else {
                                    eprintln!("tuna-shell-gtk: screenshot failed");
                                    return;
                                };
                                display.clipboard().set_texture(&texture);
                                eprintln!(
                                    "tuna-shell-gtk: screenshot kept on the clipboard (disable-save-to-disk)"
                                );
                                notify.post(
                                    "Screenshot",
                                    "screenshot-recorded-symbolic",
                                    "Screenshot captured",
                                    "You can paste the image from the clipboard.",
                                );
                                return;
                            }
                            let name = std::path::Path::new(&path)
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or(path);
                            notify.post(
                                "Screenshot",
                                "screenshot-recorded-symbolic",
                                "Screenshot captured",
                                &name,
                            );
                        }
                        Err(e) => eprintln!("tuna-shell-gtk: screenshot failed: {e}"),
                    },
                );
            },
        );
    });
}

fn build(app: &adw::Application) {
    accessibility::start_focus_policy();
    magnifier_sync::start_sync();
    let provider = gtk::CssProvider::new();
    // Shell CSS has GNOME's baseline face/size, but follows the user's font live.
    let apply_font = {
        let provider = provider.clone();
        move |iface: Option<&gio::Settings>| {
            let name = iface
                .filter(|i| i.settings_schema().is_some_and(|s| s.has_key("font-name")))
                .map(|i| i.string("font-name").to_string())
                .unwrap_or_else(|| "Adwaita Sans 11".into());
            let font = gtk::pango::FontDescription::from_string(&name);
            let family = font
                .family()
                .unwrap_or_else(|| "Adwaita Sans".into())
                .replace('\\', "\\\\")
                .replace('"', "\\\"");
            let size = if font.size() > 0 {
                f64::from(font.size()) / f64::from(gtk::pango::SCALE)
            } else {
                11.0
            };
            let unit = if font.is_size_absolute() { "px" } else { "pt" };
            let weight = gtk::glib::translate::IntoGlib::into_glib(font.weight());
            let style = match font.style() {
                gtk::pango::Style::Italic => "italic",
                gtk::pango::Style::Oblique => "oblique",
                _ => "normal",
            };
            let stretch = match font.stretch() {
                gtk::pango::Stretch::UltraCondensed => "ultra-condensed",
                gtk::pango::Stretch::ExtraCondensed => "extra-condensed",
                gtk::pango::Stretch::Condensed => "condensed",
                gtk::pango::Stretch::SemiCondensed => "semi-condensed",
                gtk::pango::Stretch::SemiExpanded => "semi-expanded",
                gtk::pango::Stretch::Expanded => "expanded",
                gtk::pango::Stretch::ExtraExpanded => "extra-expanded",
                gtk::pango::Stretch::UltraExpanded => "ultra-expanded",
                _ => "normal",
            };
            let css = include_str!("style.css")
                .replace(
                    "font-family: \"Adwaita Sans\", \"Cantarell\", sans-serif;",
                    &format!(
                        "font-family: \"{family}\", sans-serif; font-weight: {weight}; font-style: {style}; font-stretch: {stretch};"
                    ),
                )
                .replace("font-size: 11pt;", &format!("font-size: {size}{unit};"))
                .replace("font-weight: normal;", &format!("font-weight: {weight};"))
                .replace("font-weight: 400;", &format!("font-weight: {weight};"))
                // Preserve semantic emphasis without weakening a heavy user font.
                .replace("font-weight: bold;", &format!("font-weight: {};", weight.max(700)))
                .replace("font-weight: 700;", &format!("font-weight: {};", weight.max(700)))
                .replace("font-weight: 800;", &format!("font-weight: {};", weight.max(800)));
            provider.load_from_string(&css);
        }
    };
    let iface = settings(INTERFACE_SCHEMA);
    apply_font(iface.as_ref());
    if let Some(iface) = iface {
        iface.connect_changed(Some("font-name"), move |i, _| apply_font(Some(i)));
        // Keep the live subscription for the shell process lifetime.
        std::mem::forget(iface);
    }
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        // GNOME Shell's bundled icons (dark-mode, screenshooter...).
        gio::resources_register_include!("icons.gresource").ok();
        gtk::IconTheme::for_display(&display).add_resource_path("/org/tuna/Shell/icons");
    }
    // GNOME Shell's on-screen keyboard layouts and emoji list.
    gio::resources_register_include!("osk-layouts.gresource").ok();
    // GNOME's icon theme key, applied the way gnome-settings-daemon's
    // xsettings plugin applies it to GTK.
    if let (Some(gtk_settings), Some(iface)) =
        (gtk::Settings::default(), settings(INTERFACE_SCHEMA))
    {
        gtk_settings.set_gtk_icon_theme_name(Some(&iface.string("icon-theme")));
        iface.connect_changed(Some("icon-theme"), move |i, _| {
            gtk_settings.set_gtk_icon_theme_name(Some(&i.string("icon-theme")));
        });
        // Keep the live subscription for the shell process lifetime.
        std::mem::forget(iface);
    }
    // Text renders with GNOME's font hinting and antialiasing keys
    // (slight, grayscale by default), mapped the way
    // gnome-settings-daemon's xsettings plugin maps them for GTK.
    if let Some(gtk_settings) = gtk::Settings::default() {
        let apply = {
            let gtk_settings = gtk_settings.clone();
            move |iface: Option<&gio::Settings>| {
                let (hinting, antialias) = iface
                    .map(|i| {
                        (
                            i.string("font-hinting").to_string(),
                            i.string("font-antialiasing").to_string(),
                        )
                    })
                    .unwrap_or_else(|| ("slight".into(), "grayscale".into()));
                let (hint, style, aa, rgba) = logic::font_rendering(&hinting, &antialias);
                gtk_settings.set_property("gtk-xft-hinting", hint);
                gtk_settings.set_property("gtk-xft-hintstyle", style);
                gtk_settings.set_property("gtk-xft-antialias", aa);
                gtk_settings.set_property("gtk-xft-rgba", rgba);
            }
        };
        let iface = settings(INTERFACE_SCHEMA);
        apply(iface.as_ref());
        if let Some(iface) = iface {
            iface.connect_changed(None, move |i, key| {
                if key.starts_with("font-") {
                    apply(Some(i));
                }
            });
            // GSettings backend watches do not retain the settings object.
            std::mem::forget(iface);
        }
    }
    // The shell chrome is always dark, as in GNOME.
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);

    let window = gtk::ApplicationWindow::new(app);
    window.add_css_class("tuna-panel");
    window.set_title(Some("Top Bar"));
    window.init_layer_shell();
    window.set_layer(Layer::Top);
    window.set_namespace(Some(tuna_shell_control::GTK_PANEL_NAMESPACE));
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);
    window.auto_exclusive_zone_enable();
    window.set_keyboard_mode(KeyboardMode::OnDemand);

    let pills = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    pills.set_valign(gtk::Align::Center);
    pills.add_css_class("ws-pills");
    let shell = Rc::new(RefCell::new(Shell {
        control: attach_control(),
        pills: ws_dots::Indicator::new(pills.clone()),
        pill_state: Vec::new(),
    }));
    render_pills(&mut shell.borrow_mut());
    orca::start(shell.clone());

    // The compositor toggles the overview on presses over the
    // Activities control (ACTIVITIES_WIDTH_PX); sending a toggle here
    // as well would cancel it out.
    // GNOME's ActivitiesButton: role toggle button, named Activities.
    let activities = gtk::Button::builder()
        .child(&pills)
        .accessible_role(gtk::AccessibleRole::ToggleButton)
        .build();
    activities.add_css_class("panel-button");
    activities.update_property(&[gtk::accessible::Property::Label("Activities")]);
    activities.update_state(&[gtk::accessible::State::Pressed(
        gtk::AccessibleTristate::False,
    )]);

    let notify = notify::NotifyUi::new(app.upcast_ref());
    let clock_label = gtk::Label::new(None);
    let (cal_popover, calendar_ui) = calendar_popover(notify.pane());
    // GNOME's calendar events through its calendar server.
    calendar_ui.set_event_source(events::EventSource::new());
    let clock = panel_menu_button(&clock_label, "Date and Time", &cal_popover);
    {
        let notify = notify.clone();
        cal_popover.connect_visible_notify(move |p| notify.set_banner_blocked(p.is_visible()));
    }
    clock.add_css_class("clock-display");

    // System indicators in GNOME's order (panel.js `_indicators`): each
    // shows only while it means something; the power icon always does.
    let indicators = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    indicators.add_css_class("panel-status-indicators-box");
    let status_icon = |icon: &str, visible: bool| {
        let image = gtk::Image::from_icon_name(icon);
        image.add_css_class("system-status-icon");
        image.set_visible(visible);
        indicators.append(&image);
        image
    };
    // Privacy indicators lead (the microphone in use, orange unless
    // muted).
    let panel_mic = status_icon("microphone-sensitivity-medium-symbolic", false);
    panel_mic.update_property(&[gtk::accessible::Property::Label("Microphone in use")]);
    let panel_network = status_icon("network-wired-symbolic", false);
    let panel_location = status_icon("location-services-active-symbolic", false);
    panel_location.update_property(&[gtk::accessible::Property::Label("Location in use")]);
    let panel_dnd = status_icon("notifications-disabled-symbolic", false);
    let panel_volume = status_icon("audio-volume-high-symbolic", false);
    let panel_power_profile = status_icon("power-profile-balanced-symbolic", false);
    let panel_battery = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let battery_icon = gtk::Image::from_icon_name("battery-level-100-symbolic");
    battery_icon.add_css_class("system-status-icon");
    let battery_percentage = gtk::Label::new(None);
    panel_battery.append(&battery_icon);
    panel_battery.append(&battery_percentage);
    panel_battery.set_visible(false);
    indicators.append(&panel_battery);
    status_icon("system-shutdown-symbolic", true);
    let power_ui = {
        let shell = shell.clone();
        power::PowerUi::new(
            app.upcast_ref(),
            Rc::new(move || {
                // Administrative lockdown (#357): a disabled lock
                // screen suspends unlocked, as in a GNOME kiosk.
                if !lockdown::read().entry_visibility().lock_screen {
                    return;
                }
                if let Some(control) = shell.borrow_mut().control.as_mut() {
                    let _ = control.lock();
                }
            }),
        )
    };
    // gsd-media-keys asks which device an ambiguous jack plug is.
    audio_selection::start(app.upcast_ref(), {
        let shell = shell.clone();
        Rc::new(move || {
            if let Some(control) = shell.borrow_mut().control.as_mut() {
                if control.model().is_overview_open() {
                    let _ = control.toggle_overview();
                }
            }
        })
    });
    // GNOME's lock screen on ext-session-lock (the compositor decides
    // when, draws the blurred background, and runs the authentication
    // conversations).
    let lock_ui = {
        let interface = settings(INTERFACE_SCHEMA);
        // GNOME's login-screen schema may be absent: fingerprint stays on,
        // as its default.
        let login_screen = settings("org.gnome.login-screen").filter(|s| {
            s.settings_schema()
                .is_some_and(|schema| schema.has_key("enable-fingerprint-authentication"))
        });
        let port = lock::AuthPort {
            unlock: {
                let shell = shell.clone();
                Rc::new(move |password| {
                    shell
                        .borrow_mut()
                        .control
                        .as_mut()
                        .and_then(|c| c.unlock(password).ok())
                })
            },
            begin: {
                let shell = shell.clone();
                Rc::new(move |service| {
                    shell
                        .borrow_mut()
                        .control
                        .as_mut()
                        .and_then(|c| c.begin_verification(service).ok())
                })
            },
            answer: {
                let shell = shell.clone();
                Rc::new(move |service, sequence, answer| {
                    shell
                        .borrow_mut()
                        .control
                        .as_mut()
                        .and_then(|c| c.answer_query(service, sequence, answer).ok())
                })
            },
            cancel: {
                let shell = shell.clone();
                Rc::new(move || {
                    shell
                        .borrow_mut()
                        .control
                        .as_mut()
                        .and_then(|c| c.cancel_verification().ok())
                })
            },
            fingerprint_enabled: Rc::new(move || {
                login_screen
                    .as_ref()
                    .is_none_or(|s| s.boolean("enable-fingerprint-authentication"))
            }),
        };
        lock::LockUi::new(
            app.upcast_ref(),
            port,
            Box::new(move || {
                interface
                    .as_ref()
                    .map(|s| ClockFormat::from_setting(&s.string("clock-format")))
                    .unwrap_or(ClockFormat::TwentyFourHour)
            }),
        )
    };
    let brightness_policy_settings =
        settings("org.gnome.settings-daemon.plugins.power").filter(|s| {
            s.settings_schema()
                .is_some_and(|schema| schema.has_key("idle-brightness"))
        });
    let brightness_controller = brightness_service::start(
        {
            let weak = Rc::downgrade(&shell);
            Rc::new(move || {
                weak.upgrade()
                    .and_then(|shell| {
                        let shell = shell.try_borrow().ok()?;
                        shell.control.as_ref().map(|c| c.native_outputs().to_vec())
                    })
                    .unwrap_or_default()
            })
        },
        brightness_policy_settings
            .as_ref()
            .map(|s| f64::from(s.int("idle-brightness")) / 100.0)
            .unwrap_or(0.3),
    );
    if let Some(settings) = brightness_policy_settings {
        brightness_controller.settings(settings);
    }
    let qs = quick_settings_popover(
        app,
        &shell,
        &notify,
        &power_ui,
        &brightness_controller,
        PanelIcons {
            mic: panel_mic,
            network: panel_network,
            location: panel_location,
            dnd: panel_dnd,
            volume: panel_volume,
            power_profile: panel_power_profile,
            battery: panel_battery,
            battery_icon,
            battery_percentage,
        },
    );
    let system = panel_menu_button(&indicators, "System", &qs);

    // GNOME's screen recorder (org.gnome.Shell.Screencast) and its
    // indicator, first in the panel's right box as in panel.js.
    let recorder = {
        let notify = notify.clone();
        screencast::Recorder::new(Rc::new(move |summary: &str, body: &str| {
            notify.post("Screenshot", "screencast-recorded-symbolic", summary, body);
        }))
    };
    screencast::serve(recorder.clone());

    // Ctrl+Alt+Tab panel focus (P-A11Y-02): `held` while the bar
    // owns exclusive keyboard; `escape_guard` banks one credit per
    // Escape popdown so the matching `closed` signal keeps focus
    // whenever it lands (G-PANEL-FOCUS).
    let panel_held: Rc<Cell<bool>> = Rc::default();
    let panel_escape_guard: Rc<Cell<panel_focus::EscapeGuard>> = Rc::default();
    // GNOME's hover-click chooser (dwellClick.js), before the system
    // indicators.
    let dwell_chooser = {
        let shell = shell.clone();
        pointer_a11y::DwellChooser::new(Rc::new(move |click| {
            if let Some(control) = shell.borrow_mut().control.as_mut() {
                let _ = control.set_dwell_click_type(click);
            }
        }))
    };
    let bar = gtk::CenterBox::new();
    bar.set_start_widget(Some(&activities));
    bar.set_center_widget(Some(&clock));
    // AppIndicator items sit left of the system indicators.
    let end = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    end.append(&screencast::indicator(&recorder));
    let a11y = accessibility::menu();
    let ibus_properties = ibus_property_menu::PropertyMenu::new({
        let shell = shell.clone();
        // No action while compositor identity/lock state is unavailable.
        Rc::new(move || {
            shell
                .borrow()
                .control
                .as_ref()
                .is_none_or(|c| !c.input_method_actions_allowed())
        })
    });
    let input = ibus_properties.button.clone();
    // Escape inside a tray popover: the popover's grab delivers the key
    // to the popover, never to our window controller — each tray menu
    // calls this instead of closing natively, so the hold survives and
    // focus returns to the tray button (G-PANEL-FOCUS). Other menus
    // cannot be open under the grab, but the built-ins close too.
    let on_tray_escape = {
        let (window, held, guard) = (
            window.clone(),
            panel_held.clone(),
            panel_escape_guard.clone(),
        );
        let (clock, system, a11y, input) =
            (clock.clone(), system.clone(), a11y.clone(), input.clone());
        Rc::new(move |button: &gtk::MenuButton| {
            if !held.get() {
                return glib::Propagation::Proceed;
            }
            for other in [&clock, &system, &a11y, &input] {
                if other.is_active() {
                    if let Some(popover) = other.popover() {
                        if popover.is_visible() {
                            let mut state = guard.get();
                            state.note_escape_close();
                            guard.set(state);
                        }
                    }
                    other.popdown();
                }
            }
            if let Some(popover) = button.popover() {
                if popover.is_visible() {
                    let mut state = guard.get();
                    state.note_escape_close();
                    guard.set(state);
                }
            }
            button.popdown();
            button.grab_focus();
            assert_panel_hold(&window, &held);
            glib::Propagation::Stop
        })
    };
    let tray_box = {
        let (window, held, guard) = (
            window.clone(),
            panel_held.clone(),
            panel_escape_guard.clone(),
        );
        tray::tray(
            Rc::new(move || {
                let mut state = guard.get();
                let release = state.closed(held.get());
                guard.set(state);
                if release {
                    release_panel_focus(&window, &held);
                }
            }),
            panel_escape_guard.clone(),
            on_tray_escape.clone(),
        )
    };
    end.append(&dwell_chooser.button);
    end.append(&tray_box);
    end.append(&input);
    end.append(&a11y);
    end.append(&system);
    bar.set_end_widget(Some(&end));
    window.set_child(Some(&bar));
    // A panel menu closing any other way (item activation, outside
    // click) returns focus to the client, as in GNOME. No-op unless
    // Ctrl+Alt+Tab holds the bar.
    for button in [&clock, &system, &a11y, &input] {
        if let Some(popover) = button.popover() {
            // Cloned before the shadowing bindings below are moved
            // into the signal closures.
            let (window_e, held_e, guard_e) = (
                window.clone(),
                panel_held.clone(),
                panel_escape_guard.clone(),
            );
            let (clock_e, system_e, a11y_e, input_e, tray_box_e) = (
                clock.clone(),
                system.clone(),
                a11y.clone(),
                input.clone(),
                tray_box.clone(),
            );
            let guard = panel_escape_guard.clone();
            popover.connect_show(move |_| {
                let mut state = guard.get();
                state.note_shown();
                guard.set(state);
            });
            let (window, held, guard) = (
                window.clone(),
                panel_held.clone(),
                panel_escape_guard.clone(),
            );
            popover.connect_closed(move |_| {
                let mut state = guard.get();
                let release = state.closed(held.get());
                guard.set(state);
                if release {
                    release_panel_focus(&window, &held);
                }
            });
            // Escape inside the popover: its grab delivers the key here,
            // never to our window controller — intercept it or the menu
            // closes natively, the hold is lost, and focus leaves the
            // bar (G-PANEL-FOCUS). Without the hold, Proceed: normal
            // popover behavior is unchanged.
            let escape_keys = gtk::EventControllerKey::new();
            escape_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            escape_keys.connect_key_pressed(move |_, key, _, _| {
                use gtk::gdk::Key;
                if key != Key::Escape || !held_e.get() {
                    return glib::Propagation::Proceed;
                }
                close_panel_menus(
                    &clock_e,
                    &system_e,
                    &a11y_e,
                    &input_e,
                    &tray_box_e,
                    &guard_e,
                );
                assert_panel_hold(&window_e, &held_e);
                glib::Propagation::Stop
            });
            popover.add_controller(escape_keys);
        }
    }
    // Panel-focus keys: Escape unwinds the open panel menu, then
    // returns focus to the client; Enter/Space on Activities toggles
    // the overview (the pointer path stays the compositor's press
    // region); Left/Right walk the bar while no menu owns the arrows.
    {
        let (window, bar, activities, clock, system, a11y, input, tray_box, held, guard) = (
            window.clone(),
            bar.clone(),
            activities.clone(),
            clock.clone(),
            system.clone(),
            a11y.clone(),
            input.clone(),
            tray_box.clone(),
            panel_held.clone(),
            panel_escape_guard.clone(),
        );
        let shell = shell.clone();
        let keys = gtk::EventControllerKey::new();
        // Fallback for Escape with no popover shown (no grab in the
        // way): unwind or release. An open popover's grab delivers
        // Escape to the popover instead, which carries its own
        // interceptor above — this controller never sees those keys.
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let window_keys = window.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            use gtk::gdk::Key;
            if !held.get() {
                return glib::Propagation::Proceed;
            }
            match key {
                Key::Escape => {
                    match panel_focus::escape(
                        true,
                        panel_menu_open(&clock, &system, &a11y, &input, &tray_box),
                    ) {
                        panel_focus::EscapeOutcome::CloseMenus => {
                            close_panel_menus(&clock, &system, &a11y, &input, &tray_box, &guard);
                            // Closing keeps panel focus by contract: the
                            // hold is re-asserted unconditionally (a no-op
                            // when the guard already held it), so the bar
                            // keeps exclusive keyboard on every path out
                            // of this branch (G-PANEL-FOCUS).
                            assert_panel_hold(&window_keys, &held);
                            glib::Propagation::Stop
                        }
                        panel_focus::EscapeOutcome::Release => {
                            release_panel_focus(&window_keys, &held);
                            glib::Propagation::Stop
                        }
                        panel_focus::EscapeOutcome::Ignore => glib::Propagation::Proceed,
                    }
                }
                Key::Return | Key::KP_Enter | Key::space
                    if gtk::prelude::GtkWindowExt::focus(&window_keys).as_ref()
                        == Some(activities.upcast_ref()) =>
                {
                    if let Some(control) = shell.borrow_mut().control.as_mut() {
                        let _ = control.toggle_overview();
                    }
                    glib::Propagation::Stop
                }
                Key::Left | Key::Right
                    if panel_focus::arrows_walk_bar(
                        true,
                        panel_menu_open(&clock, &system, &a11y, &input, &tray_box),
                    ) =>
                {
                    bar.child_focus(if key == Key::Left {
                        gtk::DirectionType::TabBackward
                    } else {
                        gtk::DirectionType::TabForward
                    });
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        window.add_controller(keys);
    }

    // Clock: GNOME's panel text, redrawn on each minute (each second
    // with clock-show-seconds), after any wall-clock change, and when
    // its settings change.
    let interface = settings(INTERFACE_SCHEMA);
    let clock_button = clock.clone();
    let refresh_clock: Rc<dyn Fn()> = {
        let interface = interface.clone();
        let shown = RefCell::new(String::new());
        Rc::new(move || {
            let now = wall_clock::now().datetime();
            let format = interface
                .as_ref()
                .map(|s| ClockFormat::from_setting(&s.string("clock-format")))
                .unwrap_or(ClockFormat::TwentyFourHour);
            // GNOME's clock keys; without the schema, weekday and date.
            let parts = interface
                .as_ref()
                .map(|s| logic::ClockParts {
                    weekday: s.boolean("clock-show-weekday"),
                    date: s.boolean("clock-show-date"),
                    seconds: s.boolean("clock-show-seconds"),
                })
                .unwrap_or_default();
            let text = logic::clock_text(&now, format, parts);
            if *shown.borrow() != text {
                clock_label.set_label(&text);
                clock.update_property(&[gtk::accessible::Property::Description(&text)]);
                *shown.borrow_mut() = text;
            }
            calendar_ui.set_clock_format(format);
            calendar_ui.set_today(now.date());
        })
    };
    refresh_clock();
    let wall_clock = {
        let seconds = interface
            .as_ref()
            .is_some_and(|s| s.boolean("clock-show-seconds"));
        let refresh_clock = refresh_clock.clone();
        Rc::new(wall_clock::WallClock::new(seconds, move || refresh_clock()))
    };
    if let Some(interface) = interface {
        let (wall_clock, lock_ui) = (wall_clock.clone(), lock_ui.clone());
        interface.connect_changed(None, move |s, key| {
            if key.starts_with("clock-") {
                wall_clock.set_seconds(s.boolean("clock-show-seconds"));
                refresh_clock();
                lock_ui.refresh_clock();
            }
        });
        // `refresh_clock` holds the settings, so the handler lives as
        // long as the shell.
    }
    // The clock ticks for the shell's life.
    std::mem::forget(wall_clock);

    // Overview search, dash, and app grid (#54, #57).
    let favorites = {
        let pinned = tuna_shell_host::favorites::Favorites::system()
            .ids()
            .to_vec();
        // Tuna Desktop's own pins first, then the user's GNOME dash
        // (org.gnome.shell favorite-apps, #63), then GNOME's defaults.
        let gnome: Vec<String> = settings(SHELL_SCHEMA)
            .map(|s| {
                s.strv("favorite-apps")
                    .iter()
                    .map(|v| v.to_string())
                    .collect()
            })
            .unwrap_or_default();
        if !pinned.is_empty() {
            pinned
        } else if !gnome.is_empty() {
            gnome
        } else {
            logic::DEFAULT_FAVORITES
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        }
    };
    let apps = live_apps::LiveApps::new();
    let dash_apps = favorites.clone();
    let overview_ui = overview::OverviewUi::new(
        app.upcast_ref(),
        apps.clone(),
        favorites,
        Rc::new(ShellActions(shell.clone())),
    );
    let switcher_ui = switcher::SwitcherUi::new(app.upcast_ref(), apps.clone());
    // Keep both switcher policies live, including while a popup is open.
    {
        let app_settings = settings("org.gnome.shell.app-switcher");
        let window_settings = settings("org.gnome.shell.window-switcher");
        let apply: Rc<dyn Fn()> = {
            let shell = shell.clone();
            let app_settings = app_settings.clone();
            let window_settings = window_settings.clone();
            Rc::new(move || {
                let defaults = tuna_shell_host::model::SwitcherWorkspacePolicy::default();
                let read = |settings: &Option<gio::Settings>, fallback| {
                    settings
                        .as_ref()
                        .filter(|settings| {
                            settings
                                .settings_schema()
                                .is_some_and(|schema| schema.has_key("current-workspace-only"))
                        })
                        .map(|settings| settings.boolean("current-workspace-only"))
                        .unwrap_or(fallback)
                };
                if let Some(control) = shell.borrow_mut().control.as_mut() {
                    control.set_switcher_workspace_policy(
                        tuna_shell_host::model::SwitcherWorkspacePolicy {
                            applications_current_workspace_only: read(
                                &app_settings,
                                defaults.applications_current_workspace_only,
                            ),
                            windows_current_workspace_only: read(
                                &window_settings,
                                defaults.windows_current_workspace_only,
                            ),
                        },
                    );
                }
            })
        };
        apply();
        for settings in [app_settings, window_settings].into_iter().flatten() {
            let apply = apply.clone();
            settings.connect_changed(Some("current-workspace-only"), move |_, _| apply());
            std::mem::forget(settings);
        }
    }
    // GNOME's WindowTracker: a window belongs to the desktop entry its
    // app id names (as the switcher finds its icon), shown in the grid
    // or not; a window no desktop file claims is an app of its own.
    if let Some(control) = shell.borrow_mut().control.as_mut() {
        let apps = apps.clone();
        let on_disk: RefCell<std::collections::HashMap<String, bool>> = RefCell::default();
        control.set_app_resolver(move |app_id| {
            let id = app_id.trim_end_matches(".desktop");
            if let Some(entry) = {
                let apps = apps.get();
                apps.entry(id)
                    .or_else(|| apps.entry(app_id))
                    .map(|e| e.app_id.clone())
            } {
                return Some(entry);
            }
            let found = *on_disk
                .borrow_mut()
                .entry(id.to_owned())
                .or_insert_with(|| tuna_shell_host::apps::desktop_file_exists(id));
            found.then(|| id.to_owned())
        });
    }
    // The folder dialog's shade reaches under the top bar, as GNOME's.
    {
        let panel = window.clone();
        overview::OverviewUi::set_folder_shade(
            &overview_ui,
            Rc::new(move |on| {
                if on {
                    panel.add_css_class("shaded");
                } else {
                    panel.remove_css_class("shaded");
                }
            }),
        );
    }
    // org.gnome.Shell for the rest of GNOME: the OSD gnome-settings-daemon
    // shows for volume and brightness keys, search and the app grid.
    let osd_ui = osd::OsdUi::new(app.upcast_ref());
    let media_keys = media_keys::MediaKeys::new({
        let osd = osd_ui.clone();
        let sound = media_keys::sound_settings();
        Rc::new(move |percent, muted, microphone| {
            osd.show(&osd::OsdRequest {
                icon: Some(
                    if microphone {
                        logic::mic_icon(percent, muted)
                    } else {
                        logic::volume_icon(percent, muted)
                    }
                    .into(),
                ),
                level: Some(if muted { 0.0 } else { percent / 100.0 }),
                max_level: Some(if microphone {
                    1.0
                } else {
                    media_keys::output_limit(sound.as_ref()) / 100.0
                }),
                ..Default::default()
            });
        })
    });
    let screensaver = screensaver::start(
        {
            let shell = shell.clone();
            Rc::new(move || {
                // Administrative lockdown (#357): a disabled lock
                // screen refuses screensaver and D-Bus lock requests.
                if !lockdown::read().entry_visibility().lock_screen {
                    return Err("lock screen disabled by administrator");
                }
                let mut shell = shell.borrow_mut();
                let control = shell.control.as_mut().ok_or("compositor disconnected")?;
                control
                    .lock()
                    .map(|_| ())
                    .map_err(|_| "lock request failed")
            })
        },
        {
            let shell = shell.clone();
            Rc::new(move || shell.borrow().control.as_ref().is_some_and(|c| c.locked()))
        },
    );
    // Wellbeing (#354): screen-time history/limits and break reminders.
    // The module owns its timers and settings watches; the shell only
    // lends it the message list, the OSD countdown and the lock action.
    // Test builds compile the module for its unit tests but never start
    // the live runtime (it owns process timers and the real history file).
    // The 0.35 control client reconciles effects with Applied responses.
    #[cfg(not(test))]
    {
        let notices = notify.clone();
        let (show_countdown, hide_countdown) = osd_ui.countdown_hooks();
        let lock_shell = shell.clone();
        let effect_shell = shell.clone();
        let action_notices = notify.clone();
        let register_notices = notify.clone();
        let drop_notices = notify.clone();
        let remove_notices = notify.clone();
        // GNOME's wellbeing sources post `Urgency.HIGH`, or `LOW` for a
        // break type whose `notify` toggle is off.
        let notice_urgency = |urgent: bool| {
            if urgent {
                tuna_shell_host::notifications::Urgency::High
            } else {
                tuna_shell_host::notifications::Urgency::Low
            }
        };
        let break_queue: Rc<RefCell<Vec<wellbeing::BreakActionKind>>> =
            Rc::new(RefCell::new(Vec::new()));
        let break_push = break_queue.clone();
        wellbeing::start(
            app.upcast_ref(),
            wellbeing::ShellHooks {
                post_notice: Rc::new(
                    move |title: String, body: String, urgent: bool, replaces: Option<u64>| {
                        notices.post_replacing(
                            "Wellbeing",
                            "alarm-symbolic",
                            &title,
                            &body,
                            notice_urgency(urgent),
                            replaces,
                        )
                    },
                ),
                remove_notice: Rc::new(move |id: u64| remove_notices.remove_notification(id)),
                post_action_notice: Rc::new(
                    move |title: String,
                          body: String,
                          actions: Vec<(String, String)>,
                          urgent: bool,
                          replaces: Option<u64>| {
                        action_notices.post_with_actions(
                            "Wellbeing",
                            "alarm-symbolic",
                            &title,
                            &body,
                            &actions,
                            notice_urgency(urgent),
                            replaces,
                        )
                    },
                ),
                on_notice_action: Rc::new(move |id: u64, key: String, handler: Rc<dyn Fn()>| {
                    register_notices.on_notification_action(id, &key, handler);
                }),
                drop_notice_actions: Rc::new(move |id: u64| {
                    drop_notices.drop_notification_actions(id);
                }),
                break_action: Rc::new(move |kind: wellbeing::BreakActionKind| {
                    break_push.borrow_mut().push(kind);
                }),
                // Reconcile on every wellbeing tick. ControlClient bounds
                // outstanding sends, correlates Applied, and invalidates the
                // pair when it replaces a dead connection.
                effect_hook: Rc::new(move |grayscale: bool, fade: bool| {
                    if let Some(control) = effect_shell.borrow_mut().control.as_mut() {
                        match control.reconcile_wellbeing_effects(grayscale, fade) {
                            Ok(applied) => applied,
                            Err(e) => {
                                eprintln!("tuna-shell-gtk: wellbeing: effects send failed: {e}");
                                false
                            }
                        }
                    } else {
                        false
                    }
                }),
                show_countdown,
                hide_countdown,
                lock: Rc::new(move || {
                    if let Some(control) = lock_shell.borrow_mut().control.as_mut() {
                        if let Err(e) = control.lock() {
                            eprintln!("tuna-shell-gtk: wellbeing: lock failed: {e}");
                        }
                    }
                }),
            },
            break_queue,
        );
    }
    let gnome_shell = shell_dbus::start(Rc::new(GnomeShellDbus {
        osd: osd_ui.clone(),
        overview: overview_ui.clone(),
        shell: shell.clone(),
    }));

    // GNOME's screenshot UI (Print).
    let screenshot_ui = {
        let notify_shot = notify.clone();
        let notify_post = notify.clone();
        let pointer_shell = shell.clone();
        screenshot_ui::ScreenshotUi::new(
            app.upcast_ref(),
            Rc::new(move || take_screenshot(true, notify_shot.clone())),
            Rc::new(move |summary: &str, body: &str| {
                notify_post.post("Screenshot", "screenshot-recorded-symbolic", summary, body);
            }),
            recorder.clone(),
            Rc::new(move |(x, y)| {
                if let Some(control) = pointer_shell.borrow_mut().control.as_mut() {
                    let _ = control.move_screenshot_pointer(x, y);
                }
            }),
        )
    };

    // GNOME Shell is the session's polkit agent.
    polkit::start(app.upcast_ref());
    // GNOME's NetworkManager secret agent (Wi-Fi passwords).
    network_agent::start(app.upcast_ref());

    // GNOME Shell is IBus's panel: it draws the candidate window.
    ibus_panel::start(app.upcast_ref(), ibus_properties.clone());

    // GNOME's workspace switcher popup.
    let workspace_popup = ws_popup::WorkspacePopup::new(app.upcast_ref());
    // GNOME's Run a Command dialog (Alt+F2, #357 lockdown gate inside).
    let run_dialog = run_dialog::RunDialog::new(app.upcast_ref());

    // GNOME's input-source policy (#346): MRU switching with its popup,
    // per-window sources, saved order. It drives the compositor's keymap
    // layouts with relative SwitchInputSource steps.
    let input_sources = {
        let shell = shell.clone();
        input_sources::InputSources::new(
            app.upcast_ref(),
            Rc::new(move |backward| {
                if let Some(control) = shell.borrow_mut().control.as_mut() {
                    let _ = control.switch_input_source(backward);
                }
            }),
        )
    };
    // GNOME's on-screen keyboard (#349): typed through the compositor,
    // following its input-method focus and the current input source.
    let osk = {
        let shell = shell.clone();
        osk::Osk::new(
            input_sources.clone(),
            Rc::new(move |input| {
                if let Some(control) = shell.borrow_mut().control.as_mut() {
                    if let Err(e) = control.osk_input(input) {
                        eprintln!("tuna-shell-gtk: on-screen keyboard input: {e}");
                    }
                }
            }),
        )
    };
    lock_ui.set_osk(osk.clone());

    // GNOME Shell's own keybindings (org.gnome.shell.keybindings).
    {
        // The accelerator that fired, for the input-source popup's
        // modifier (SwitcherPopup.show's binding mask).
        let fired_accelerator: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let run: Rc<dyn Fn(keybindings::Action, u32)> = {
            let input_sources = input_sources.clone();
            let fired_accelerator = fired_accelerator.clone();
            let (shell, overview_ui, clock, system) = (
                shell.clone(),
                overview_ui.clone(),
                clock_button.clone(),
                system.clone(),
            );
            let (apps, notify, osd_ui) = (apps.clone(), notify.clone(), osd_ui.clone());
            let screenshot_ui = screenshot_ui.clone();
            let brightness_controller = brightness_controller.clone();
            let workspace_popup_keys = workspace_popup.clone();
            let run_dialog = run_dialog.clone();
            let (panel_window_keys, activities_keys, panel_held_keys) =
                (window.clone(), activities.clone(), panel_held.clone());
            Rc::new(move |action, mode| {
                use keybindings::Action;
                // Ctrl+Alt+Tab owns the keyboard while held: a session
                // chord that fires then is spurious (G-PANEL-FOCUS).
                if panel_held_keys.get() && !keybindings::runs_while_panel_held(action, mode) {
                    return;
                }
                let toggle = |button: &gtk::MenuButton| {
                    if button.is_active() {
                        button.popdown();
                    } else {
                        button.popup();
                        // Opened from the keyboard: GNOME shows the
                        // first item's focus.
                        if let Some(window) = button.root().and_downcast::<gtk::Window>() {
                            window.set_focus_visible(true);
                        }
                    }
                };
                let dash_entry = |n: u8| {
                    let id = dash_apps.get(usize::from(n) - 1)?;
                    let id = id.trim_end_matches(".desktop");
                    apps.get().entry(id).cloned()
                };
                if media_keys::trace_enabled()
                    && matches!(
                        action,
                        Action::VolumeUp { .. }
                            | Action::VolumeDown { .. }
                            | Action::VolumeUpQuiet
                            | Action::VolumeDownQuiet
                            | Action::VolumeMute
                            | Action::VolumeMuteQuiet
                            | Action::MicrophoneMute
                    )
                {
                    let locked = shell.borrow().control.as_ref().is_some_and(|c| c.locked());
                    media_keys::trace(
                        "media-action",
                        serde_json::json!({
                            "action": format!("{action:?}"), "mode": mode,
                        }),
                        locked,
                    );
                }
                match action {
                    Action::LockScreen => {
                        // Administrative lockdown (#357): the keybinding
                        // refuses too, so no alternate shell path bypasses
                        // the hidden button.
                        if !lockdown::read().entry_visibility().lock_screen {
                            eprintln!("tuna-shell-gtk: lock refused by administrator policy");
                            return;
                        }
                        if let Some(control) = shell.borrow_mut().control.as_mut() {
                            if let Err(e) = control.lock() {
                                eprintln!("tuna-shell-gtk: lock failed: {e}");
                            }
                        }
                    }
                    Action::ToggleOverview => {
                        if let Some(control) = shell.borrow_mut().control.as_mut() {
                            let _ = control.toggle_overview();
                        }
                    }
                    Action::ToggleApplicationView => {
                        overview::OverviewUi::toggle_apps(&overview_ui);
                    }
                    Action::FocusActiveNotification => notify.focus_active(),
                    Action::ShiftOverviewUp | Action::ShiftOverviewDown => {
                        overview::OverviewUi::shift(
                            &overview_ui,
                            action == Action::ShiftOverviewUp,
                        );
                    }
                    Action::ToggleMessageTray => toggle(&clock),
                    Action::ToggleQuickSettings => toggle(&system),
                    Action::FocusPanel { backward } => {
                        focus_panel(
                            &panel_window_keys,
                            &activities_keys,
                            &system,
                            &panel_held_keys,
                            backward,
                        );
                    }
                    Action::SwitchToApplication(n) => {
                        let Some(entry) = dash_entry(n) else { return };
                        // Its newest window when it runs, else a launch;
                        // the overview steps aside either way.
                        let window = shell.borrow().control.as_ref().and_then(|c| {
                            c.model()
                                .windows()
                                .iter()
                                .rev()
                                .find(|w| w.app_id.as_deref() == Some(entry.app_id.as_str()))
                                .map(|w| w.id)
                        });
                        let mut shell = shell.borrow_mut();
                        if let Some(control) = shell.control.as_mut() {
                            match window {
                                Some(id) => {
                                    let _ = control.activate_window(id);
                                }
                                None => {
                                    let _ = tuna_shell_host::apps::launch(&entry);
                                }
                            }
                            if control.model().is_overview_open() {
                                let _ = control.toggle_overview();
                            }
                        }
                    }
                    Action::OpenNewWindow(n) => {
                        if let Some(entry) = dash_entry(n) {
                            let _ = tuna_shell_host::apps::launch(&entry);
                        }
                    }
                    Action::RunDialog => {
                        run_dialog.open();
                    }
                    Action::ShowScreenshotUi => screenshot_ui.open(),
                    Action::ShowScreenRecordingUi => screenshot_ui.open_recording(),
                    Action::Screenshot => take_screenshot(false, notify.clone()),
                    Action::ScreenshotWindow => take_screenshot(true, notify.clone()),
                    Action::NextInputSource | Action::PreviousInputSource => {
                        let accelerator = fired_accelerator.borrow().clone();
                        input_sources.binding(
                            action == Action::PreviousInputSource,
                            mode,
                            &accelerator,
                        );
                    }
                    Action::Close => {
                        let mut shell = shell.borrow_mut();
                        if let Some(control) = shell.control.as_mut() {
                            let focused = control
                                .model()
                                .windows()
                                .iter()
                                .find(|w| w.active)
                                .map(|w| w.id);
                            if let Some(id) = focused {
                                let _ = control.close_window(id);
                            }
                        }
                    }
                    Action::WindowMenu
                    | Action::ToggleMaximized
                    | Action::Unmaximize
                    | Action::Maximize
                    | Action::Minimize
                    | Action::TileLeft
                    | Action::TileRight
                    | Action::BeginMove
                    | Action::BeginResize => {
                        use tuna_shell_control::WindowAction as W;
                        let what = match action {
                            Action::WindowMenu => W::ShowMenu,
                            Action::ToggleMaximized => W::ToggleMaximize,
                            Action::Unmaximize => W::Unmaximize,
                            Action::Maximize => W::Maximize,
                            Action::Minimize => W::Minimize,
                            Action::TileLeft => W::ToggleTiledLeft,
                            Action::TileRight => W::ToggleTiledRight,
                            Action::BeginMove => W::Move,
                            _ => W::Resize,
                        };
                        let mut shell = shell.borrow_mut();
                        if let Some(control) = shell.control.as_mut() {
                            let focused = control
                                .model()
                                .windows()
                                .iter()
                                .find(|w| w.active)
                                .map(|w| w.id);
                            if let Some(id) = focused {
                                let _ = control.window_action(id, what);
                            }
                        }
                    }
                    Action::Workspace(target) | Action::MoveToWorkspace(target) => {
                        let mut shell = shell.borrow_mut();
                        let Some(control) = shell.control.as_mut() else {
                            return;
                        };
                        let model = control.model();
                        let occupied: Vec<u32> = model
                            .windows()
                            .iter()
                            .filter(|w| !w.on_all_workspaces)
                            .map(|w| w.workspace)
                            .collect();
                        let Some(workspace) =
                            target.resolve(model.active_workspace(), model.workspaces(), &occupied)
                        else {
                            return;
                        };
                        let workspace = if let Some(count) = model.fixed_workspace_count() {
                            if target == keybindings::Target::Last {
                                count - 1
                            } else if workspace >= count {
                                return;
                            } else {
                                workspace
                            }
                        } else {
                            workspace
                        };
                        let focused = model.windows().iter().find(|w| w.active).map(|w| w.id);
                        if matches!(action, Action::MoveToWorkspace(_)) {
                            // Mutter moves the window and follows it.
                            let Some(id) = focused else { return };
                            let _ = control.window_action(
                                id,
                                tuna_shell_control::WindowAction::MoveToWorkspace { workspace },
                            );
                        }
                        let _ = control.focus_workspace(workspace);
                        // GNOME's switcher popup, outside the overview.
                        if !control.model().is_overview_open() {
                            let moved = matches!(action, Action::MoveToWorkspace(_));
                            let occupied = control
                                .model()
                                .windows()
                                .iter()
                                .map(|w| {
                                    if moved && w.active {
                                        workspace
                                    } else {
                                        w.workspace
                                    }
                                })
                                .max();
                            workspace_popup_keys.display(
                                workspace,
                                control.model().fixed_workspace_count().unwrap_or_else(|| {
                                    tuna_shell_control::dynamic_workspace_count(occupied, workspace)
                                }),
                            );
                        }
                    }
                    Action::VolumeUp { .. }
                    | Action::VolumeDown { .. }
                    | Action::VolumeUpQuiet
                    | Action::VolumeDownQuiet => {
                        let up = matches!(action, Action::VolumeUp { .. } | Action::VolumeUpQuiet);
                        let precise = matches!(
                            action,
                            Action::VolumeUp { precise: true }
                                | Action::VolumeDown { precise: true }
                        );
                        let quiet =
                            matches!(action, Action::VolumeUpQuiet | Action::VolumeDownQuiet);
                        let step = if precise {
                            2.0
                        } else {
                            keybindings::volume_step(keybindings::media_settings().as_ref())
                        };
                        media_keys.change(
                            if up {
                                media_keys::VolumeKey::Up
                            } else {
                                media_keys::VolumeKey::Down
                            },
                            step,
                            false,
                            quiet,
                        );
                    }
                    Action::ToggleMagnifier => magnifier_sync::toggle(),
                    Action::ToggleScreenReader => orca::toggle(),
                    Action::MagnifierZoom { zoom_in } => magnifier_sync::zoom(zoom_in),
                    Action::VolumeMute | Action::VolumeMuteQuiet | Action::MicrophoneMute => {
                        media_keys.change(
                            media_keys::VolumeKey::Mute,
                            0.0,
                            action == Action::MicrophoneMute,
                            action == Action::VolumeMuteQuiet,
                        );
                    }
                    Action::BrightnessUp
                    | Action::BrightnessDown
                    | Action::BrightnessUpMonitor
                    | Action::BrightnessDownMonitor
                    | Action::BrightnessCycle
                    | Action::BrightnessCycleMonitor => {
                        let monitor = matches!(
                            action,
                            Action::BrightnessUpMonitor
                                | Action::BrightnessDownMonitor
                                | Action::BrightnessCycleMonitor
                        );
                        let output = shell
                            .borrow()
                            .control
                            .as_ref()
                            .and_then(|c| c.pointer_output().map(str::to_owned));
                        // No pointer-output/backlight match means no write to a different screen.
                        if monitor && output.is_none() {
                            return;
                        }
                        let step = match action {
                            Action::BrightnessUp | Action::BrightnessUpMonitor => {
                                services::BrightnessStep::Up
                            }
                            Action::BrightnessDown | Action::BrightnessDownMonitor => {
                                services::BrightnessStep::Down
                            }
                            _ => services::BrightnessStep::Cycle,
                        };
                        let output = output.as_deref().filter(|_| monitor);
                        let osd = osd_ui.clone();
                        let show = move |result: Result<f64, String>| match result {
                            Ok(level) => osd.show(&osd::OsdRequest {
                                icon: Some("display-brightness-symbolic".into()),
                                level: Some(level),
                                ..Default::default()
                            }),
                            Err(e) => eprintln!("tuna-shell-gtk: brightness key: {e}"),
                        };
                        if brightness_controller.has_output(output) {
                            brightness_controller.step(output, step, Box::new(show));
                        } else if monitor {
                            eprintln!(
                                "tuna-shell-gtk: no authoritative native pointer-output backlight"
                            );
                        } else {
                            // Preserve explicitly global legacy access, but this
                            // fallback cannot establish the new interface capability.
                            services::step_brightness_for(step, output, Box::new(show));
                        }
                    }
                }
            })
        };
        let settings = keybindings::settings();
        let wm_settings = keybindings::wm_settings();
        let mutter_settings = keybindings::mutter_settings();
        let media_settings = keybindings::media_settings();
        // gsd-media-keys owns the media-keys schema's bindings whenever it
        // runs (every hardware session, tuna-session.target). Until the
        // watch has answered the shell binds none of them, so a daemon
        // that grabs first never meets a key the shell reserved; its
        // NameOwnerChanged reaches the shell before its GrabAccelerators.
        let gsd_media_keys: Rc<Cell<Option<bool>>> = Rc::new(Cell::new(None));
        let apply: Rc<dyn Fn()> = {
            let (settings, wm_settings, mutter_settings, media_settings, gnome_shell) = (
                settings.clone(),
                wm_settings.clone(),
                mutter_settings.clone(),
                media_settings.clone(),
                gnome_shell.clone(),
            );
            let shell = shell.clone();
            let gsd_media_keys = gsd_media_keys.clone();
            Rc::new(move || {
                // The switcher's chords go to the compositor, which holds
                // the popup open while their modifiers are held.
                let switcher: Vec<tuna_shell_control::SwitcherKey> =
                    keybindings::switcher_keys(wm_settings.as_ref())
                        .iter()
                        .filter_map(|(a, kind)| keybindings::parse_switcher_key(a, *kind))
                        .take(tuna_shell_control::MAX_SWITCHER_KEYS)
                        .collect();
                if let Some(control) = shell.borrow_mut().control.as_mut() {
                    let _ = control.set_switcher_keys(switcher);
                }
                let list = keybindings::bindings(
                    settings.as_ref(),
                    wm_settings.as_ref(),
                    mutter_settings.as_ref(),
                    media_settings.as_ref(),
                    gsd_media_keys.get().unwrap_or(true),
                );
                let accels: Vec<(String, u32, u32)> = list
                    .iter()
                    .map(|b| (b.accelerator.clone(), b.modes, b.flags))
                    .collect();
                eprintln!(
                    "tuna-shell-gtk: keybindings: {} grabs, maximize on {:?}",
                    accels.len(),
                    list.iter()
                        .filter(|b| b.action == keybindings::Action::Maximize)
                        .map(|b| b.accelerator.as_str())
                        .collect::<Vec<_>>()
                );
                let actions: Vec<keybindings::Action> = list.iter().map(|b| b.action).collect();
                let accels_for_run = accels.clone();
                let run = run.clone();
                let fired_accelerator = fired_accelerator.clone();
                gnome_shell.set_internal(
                    &accels,
                    Rc::new(move |i, mode| {
                        if let Some(action) = actions.get(i) {
                            if let Some((accelerator, _, _)) = accels_for_run.get(i) {
                                *fired_accelerator.borrow_mut() = accelerator.clone();
                            }
                            run(*action, mode);
                        }
                    }),
                );
            })
        };
        apply();
        {
            let owner_changed = {
                let (gsd_media_keys, apply) = (gsd_media_keys.clone(), apply.clone());
                move |running: bool| {
                    if gsd_media_keys.replace(Some(running)) == Some(running) {
                        return;
                    }
                    eprintln!(
                        "tuna-shell-gtk: media keys: {}",
                        if running {
                            "gsd-media-keys owns the media-keys bindings"
                        } else {
                            "no gsd-media-keys, native volume and lock bindings"
                        }
                    );
                    apply();
                }
            };
            let vanished = owner_changed.clone();
            // Watched for the whole session (a WatcherId unwatches
            // nothing when it drops): a restarted daemon takes its keys
            // back, a crashed one leaves the native subset.
            let _ = gio::bus_watch_name(
                gio::BusType::Session,
                keybindings::MEDIA_KEYS_DAEMON,
                gio::BusNameWatcherFlags::NONE,
                move |_, _, _| owner_changed(true),
                move |_, _| vanished(false),
            );
        }
        for settings in [settings, wm_settings, mutter_settings, media_settings]
            .into_iter()
            .flatten()
        {
            let apply = apply.clone();
            settings.connect_changed(None, move |_, _| apply());
            std::mem::forget(settings);
        }
    }

    // GNOME's background settings: the picture (the dark variant under
    // the dark style, as GNOME picks it) and primary-color, published to
    // the compositor's wallpaper drop file now and on every change.
    {
        let background = settings("org.gnome.desktop.background");
        let interface = settings(INTERFACE_SCHEMA);
        let screensaver = settings(SCREENSAVER_SCHEMA);
        let publish: Rc<dyn Fn()> = {
            let (background, interface, screensaver) =
                (background.clone(), interface.clone(), screensaver.clone());
            Rc::new(move || {
                let Some(bg) = background.as_ref() else {
                    return;
                };
                let dark = interface
                    .as_ref()
                    .is_some_and(|i| i.string("color-scheme") == "prefer-dark");
                let text = logic::wallpaper_drop(
                    &bg.string("picture-uri"),
                    &bg.string("picture-uri-dark"),
                    dark,
                    &bg.string("picture-options"),
                    &bg.string("primary-color"),
                    // GNOME 47's accent; older schemas have none (blue).
                    &interface
                        .as_ref()
                        .filter(|i| {
                            i.settings_schema()
                                .is_some_and(|schema| schema.has_key("accent-color"))
                        })
                        .map(|i| i.string("accent-color").to_string())
                        .unwrap_or_default(),
                );
                let lock_uri = screensaver
                    .as_ref()
                    .filter(|s| {
                        s.settings_schema()
                            .is_some_and(|schema| schema.has_key("picture-uri"))
                    })
                    .map(|s| {
                        let uri = s.string("picture-uri");
                        if !uri.starts_with("file://") {
                            return String::new();
                        }
                        // Validate a local native path, then keep a canonical
                        // escaped URI across the line-based drop. The compositor
                        // decodes once; literal percent signs remain filename bytes.
                        gio::File::for_uri(&uri)
                            .path()
                            .filter(|path| path.is_absolute())
                            .map(|path| gio::File::for_path(path).uri().to_string())
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();
                let picture_settings = |s: &gio::Settings| {
                    background_settings::picture_settings(
                        s.string("picture-options").as_str(),
                        s.string("color-shading-type").as_str(),
                        s.string("primary-color").as_str(),
                        s.string("secondary-color").as_str(),
                    )
                };
                let Some(desktop) = picture_settings(bg) else {
                    return;
                };
                let lock = if let Some(settings) = screensaver.as_ref() {
                    let Some(lock) = picture_settings(settings) else {
                        return;
                    };
                    lock
                } else {
                    desktop
                };
                let Ok(text) = background_settings::append_metadata(text, &lock_uri, desktop, lock)
                else {
                    return;
                };

                let dir = std::env::var_os("XDG_RUNTIME_DIR")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(std::env::temp_dir);
                let path = dir.join("tuna-wallpaper");
                let tmp = dir.join(".tuna-wallpaper.tmp");
                if std::fs::write(&tmp, text).is_ok() {
                    let _ = std::fs::rename(&tmp, &path);
                }
            })
        };
        publish();
        for s in [background, interface, screensaver].into_iter().flatten() {
            let publish = publish.clone();
            // `publish` holds both settings objects, so they live as long
            // as this handler does.
            s.connect_changed(None, move |_, _| publish());
        }
    }

    // GNOME's input settings (#60): keymap, repeat, touchpad, mouse and
    // hot corner, sent now and on every change.
    {
        let schemas = [
            "org.gnome.desktop.input-sources",
            "org.gnome.desktop.peripherals.keyboard",
            "org.gnome.desktop.peripherals.touchpad",
            "org.gnome.desktop.peripherals.mouse",
            INTERFACE_SCHEMA,
            A11Y_INTERFACE_SCHEMA,
            "org.gnome.desktop.wm.preferences",
            "org.gnome.desktop.sound",
            "org.gnome.mutter",
            kbd_a11y::SCHEMA,
            pointer_a11y::SCHEMA,
            "org.gnome.desktop.peripherals.trackball",
            "org.gnome.desktop.peripherals.pointingstick",
        ];
        let all: Rc<Vec<Option<gio::Settings>>> =
            Rc::new(schemas.iter().map(|s| settings(s)).collect());
        let send: Rc<dyn Fn()> = {
            let (shell, all) = (shell.clone(), all.clone());
            let input_sources = input_sources.clone();
            Rc::new(move || {
                let mut out = tuna_shell_control::InputSettings::default();
                if let Some(sources) = all[0].as_ref() {
                    let list: Vec<(String, String)> =
                        sources.value("sources").get().unwrap_or_default();
                    let (layout, variant) = logic::xkb_from_sources(&list);
                    out.xkb_layout = layout;
                    out.xkb_variant = variant;
                    out.xkb_options = sources
                        .strv("xkb-options")
                        .iter()
                        .map(|o| o.to_string())
                        .collect::<Vec<_>>()
                        .join(",");
                }
                if let Some(kbd) = all[1].as_ref() {
                    out.repeat = kbd.boolean("repeat");
                    out.repeat_delay_ms = kbd.uint("delay");
                    out.repeat_interval_ms = kbd.uint("repeat-interval");
                }
                if let Some(pad) = all[2].as_ref() {
                    out.tap_to_click = pad.boolean("tap-to-click");
                    out.touchpad_natural_scroll = pad.boolean("natural-scroll");
                    out.touchpad_speed_milli = (pad.double("speed") * 1000.0).round() as i32;
                    out.disable_while_typing = pad.boolean("disable-while-typing");
                    out.touchpad = tuna_shell_control::TouchpadSettings {
                        send_events: tuna_shell_control::SendEvents::from_gsettings(
                            &pad.string("send-events"),
                        ),
                        click_method: tuna_shell_control::TouchpadClickMethod::from_gsettings(
                            &pad.string("click-method"),
                        ),
                        edge_scrolling: pad.boolean("edge-scrolling-enabled"),
                        two_finger_scrolling: pad.boolean("two-finger-scrolling-enabled"),
                        tap_and_drag: pad.boolean("tap-and-drag"),
                        tap_and_drag_lock: pad.boolean("tap-and-drag-lock"),
                        tap_button_map: tuna_shell_control::TapButtonMap::from_gsettings(
                            &pad.string("tap-button-map"),
                        ),
                        middle_click_emulation: pad.boolean("middle-click-emulation"),
                        accel_profile: tuna_shell_control::PointerAccelProfile::from_gsettings(
                            &pad.string("accel-profile"),
                        ),
                    };
                }
                // GNOME's 0..24 `scroll-wheel-emulation-button` and its lock;
                // the mouse schema gained them in gsettings-desktop-schemas 51.
                let wheel_emulation = |s: &gio::Settings| {
                    let has = |key| {
                        s.settings_schema()
                            .is_some_and(|schema| schema.has_key(key))
                    };
                    tuna_shell_control::ScrollWheelEmulation {
                        button: if has("scroll-wheel-emulation-button") {
                            u32::try_from(s.int("scroll-wheel-emulation-button")).unwrap_or(0)
                        } else {
                            0
                        },
                        lock: has("scroll-wheel-emulation-button-lock")
                            && s.boolean("scroll-wheel-emulation-button-lock"),
                    }
                };
                if let Some(mouse) = all[3].as_ref() {
                    out.mouse_natural_scroll = mouse.boolean("natural-scroll");
                    out.mouse_speed_milli = (mouse.double("speed") * 1000.0).round() as i32;
                    out.mouse_left_handed = mouse.boolean("left-handed");
                    out.mouse_accel_profile =
                        tuna_shell_control::PointerAccelProfile::from_gsettings(
                            &mouse.string("accel-profile"),
                        );
                    out.mouse = tuna_shell_control::MouseSettings {
                        middle_click_emulation: mouse.boolean("middle-click-emulation"),
                        scroll_wheel_emulation: wheel_emulation(mouse),
                    };
                }
                if let Some(trackball) = all[11].as_ref() {
                    out.trackball = tuna_shell_control::TrackballSettings {
                        accel_profile: tuna_shell_control::PointerAccelProfile::from_gsettings(
                            &trackball.string("accel-profile"),
                        ),
                        middle_click_emulation: trackball.boolean("middle-click-emulation"),
                        scroll_wheel_emulation: wheel_emulation(trackball),
                    };
                }
                if let Some(stick) = all[12].as_ref() {
                    out.pointing_stick = tuna_shell_control::PointingStickSettings {
                        speed_milli: (stick.double("speed") * 1000.0).round() as i32,
                        accel_profile: tuna_shell_control::PointerAccelProfile::from_gsettings(
                            &stick.string("accel-profile"),
                        ),
                        scroll_method:
                            tuna_shell_control::PointingStickScrollMethod::from_gsettings(
                                &stick.string("scroll-method"),
                            ),
                    };
                }
                out.touchpad_left_handed = logic::touchpad_left_handed(
                    all[2]
                        .as_ref()
                        .map(|pad| pad.string("left-handed"))
                        .as_deref()
                        .unwrap_or("mouse"),
                    out.mouse_left_handed,
                );
                if let Some(iface) = all[4].as_ref() {
                    out.hot_corners = iface.boolean("enable-hot-corners");
                    // GNOME's cursor and locate-pointer keys (#342): the
                    // compositor draws its own cursors from them.
                    out.cursor_theme = iface.string("cursor-theme").to_string();
                    out.cursor_size = u32::try_from(iface.int("cursor-size")).unwrap_or(0);
                    out.locate_pointer = iface.boolean("locate-pointer");
                }
                out.right_to_left = gtk::Widget::default_direction() == gtk::TextDirection::Rtl;
                out.motion = motion::from_settings(all[4].as_ref(), all[5].as_ref());
                if let Some(mutter) = all[8].as_ref() {
                    if mutter
                        .settings_schema()
                        .is_some_and(|schema| schema.has_key("attach-modal-dialogs"))
                    {
                        out.attach_modal_dialogs = mutter.boolean("attach-modal-dialogs");
                    }
                    if mutter
                        .settings_schema()
                        .is_some_and(|schema| schema.has_key("center-new-windows"))
                    {
                        out.center_new_windows = mutter.boolean("center-new-windows");
                    }
                }
                if let Some(mutter) = all[8].as_ref() {
                    if let Some(schema) = mutter.settings_schema() {
                        if schema.has_key("locate-pointer-key") {
                            out.locate_pointer_key =
                                mutter.string("locate-pointer-key").to_string();
                        }
                        if schema.has_key("dynamic-workspaces") {
                            out.workspace_policy.dynamic = mutter.boolean("dynamic-workspaces");
                        }
                        if schema.has_key("workspaces-only-on-primary") {
                            out.workspace_policy.only_on_primary =
                                mutter.boolean("workspaces-only-on-primary");
                        }
                        if schema.has_key("edge-tiling") {
                            out.workspace_policy.edge_tiling = mutter.boolean("edge-tiling");
                        }
                    }
                }
                motion::set(out.motion);
                if let Some(wm) = all[6].as_ref() {
                    out.workspace_policy.fixed_count = wm.int("num-workspaces").clamp(1, 36) as u32;
                    out.bell.audible = wm.boolean("audible-bell");
                    out.bell.visual = wm.boolean("visual-bell");
                    out.bell.frame_flash = wm.string("visual-bell-type") == "frame-flash";
                }
                if let Some(sound) = all[7].as_ref() {
                    out.bell.event_sounds = sound.boolean("event-sounds");
                    out.bell.theme = sound.string("theme-name").to_string();
                }
                out.keyboard_aids = kbd_a11y::read(all[9].as_ref());
                out.pointer_aids = pointer_a11y::read(all[10].as_ref());
                let keymap = (
                    out.xkb_layout.clone(),
                    out.xkb_variant.clone(),
                    out.xkb_options.clone(),
                );
                if let Some(control) = shell.borrow_mut().control.as_mut() {
                    let _ = control.set_input_settings(out);
                }
                // After the keymap it may have changed: MRU, per-window.
                input_sources.sync(keymap);
            })
        };
        send();
        for settings in all.iter().flatten() {
            let send = send.clone();
            settings.connect_changed(None, move |_, _| send());
        }
        std::mem::forget(all);
    }

    // GNOME's idle and lock settings drive the compositor's idle lock
    // (#63): sent now and on every change.
    {
        let session = settings(SESSION_SCHEMA);
        let screensaver = settings(SCREENSAVER_SCHEMA);
        let interface = settings(INTERFACE_SCHEMA);
        let a11y = settings(A11Y_INTERFACE_SCHEMA);
        let lockdown_settings = settings(lockdown::SCHEMA);
        let send: Rc<dyn Fn()> = {
            let (shell, session, screensaver, interface, a11y) = (
                shell.clone(),
                session.clone(),
                screensaver.clone(),
                interface.clone(),
                a11y.clone(),
            );
            Rc::new(move || {
                let idle = session
                    .as_ref()
                    .map(|s| s.uint("idle-delay"))
                    .unwrap_or(300);
                let (enabled, delay) = screensaver
                    .as_ref()
                    .map(|s| (s.boolean("lock-enabled"), s.uint("lock-delay")))
                    .unwrap_or((true, 0));
                // GNOME's idle shield is a fade: reduced motion keeps it,
                // and the slow-down factor stretches it (screenShield.js).
                let fade_ms = motion::from_settings(interface.as_ref(), a11y.as_ref())
                    .adjust_ms(logic::IDLE_FADE_MS as f64)
                    .round() as u64;
                let ms = logic::idle_lock_ms(idle, enabled, delay, fade_ms);
                // Administrative lockdown (#357): a disabled lock screen
                // never locks on idle (the compositor backstops this
                // mapping by denying `Lock` and idle engagement itself).
                let ms = if lockdown::read().entry_visibility().lock_screen {
                    ms
                } else {
                    0
                };
                if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
                    let dir = std::path::PathBuf::from(dir);
                    let temporary = dir.join(".tuna-idle-blank.tmp");
                    let policy = format!("{}\n{}\n", u64::from(idle) * 1000, fade_ms);
                    if std::fs::write(&temporary, policy).is_ok() {
                        let _ = std::fs::rename(temporary, dir.join("tuna-idle-blank"));
                    }
                }
                if let Some(control) = shell.borrow_mut().control.as_mut() {
                    let _ = control.set_idle_timeout(ms);
                }
            })
        };
        send();
        for settings in [session, screensaver, interface, a11y, lockdown_settings]
            .into_iter()
            .flatten()
        {
            let send = send.clone();
            settings.connect_changed(None, move |_, _| send());
            // Keep the settings object (and its signal) alive.
            std::mem::forget(settings);
        }
    }

    // GNOME's window menu (header-bar right click).
    let window_menu = {
        let (shell, notify) = (shell.clone(), notify.clone());
        window_menu::WindowMenu::new(
            app.upcast_ref(),
            Rc::new(move |window, item| {
                let mut shell = shell.borrow_mut();
                let Some(control) = shell.control.as_mut() else {
                    return;
                };
                match item {
                    window_menu::Item::Screenshot => {
                        // The menu's window is the focused one.
                        let _ = control.activate_window(window);
                        take_screenshot(true, notify.clone());
                    }
                    window_menu::Item::Action(action) => {
                        let _ = control.window_action(window, action);
                    }
                    window_menu::Item::Close => {
                        let _ = control.close_window(window);
                    }
                }
            }),
        )
    };

    let shortcut_consent = {
        let shell = shell.clone();
        shortcut_consent::Consent::new(
            app.upcast_ref(),
            Rc::new(move |request, allow| {
                if let Some(control) = shell.borrow_mut().control.as_mut() {
                    let _ = control.shortcut_consent(request, allow);
                }
            }),
        )
    };
    let display_change_dialog = {
        let shell = shell.clone();
        display_change::DisplayChange::new(
            app.upcast_ref(),
            Rc::new(move |keep| {
                if let Some(control) = shell.borrow_mut().control.as_mut() {
                    let _ = control.complete_display_change(keep);
                }
            }),
        )
    };
    // gnome-shell's pad OSD and its org.gnome.Shell.Wacom.PadOsd service.
    let pad_osd_ui = {
        let (closed_shell, request_shell) = (shell.clone(), shell.clone());
        pad_osd::PadOsdUi::new(
            app.upcast_ref(),
            pad_osd::Callbacks {
                closed: Rc::new(move |pad| {
                    if let Some(control) = closed_shell.borrow_mut().control.as_mut() {
                        let _ = control.pad_osd_closed(pad);
                    }
                }),
                request: Rc::new(move |pad, edition_mode| {
                    if let Some(control) = request_shell.borrow_mut().control.as_mut() {
                        let _ = control.request_pad_osd(
                            tuna_shell_control::PadOsdTarget::Pad(pad),
                            edition_mode,
                        );
                    }
                }),
            },
        )
    };
    let pad_osd_service = {
        let shell = shell.clone();
        pad_osd_dbus::start(Rc::new(move |target, edition_mode| {
            shell
                .borrow_mut()
                .control
                .as_mut()?
                .request_pad_osd(target, edition_mode)
                .ok()
        }))
    };
    let kbd_a11y_dialog = kbd_a11y::KbdA11yDialog::new(app.upcast_ref());
    let pie_timer = pointer_a11y::PieTimer::new(app.upcast_ref());
    // Compositor state: the control socket is drained whenever it is
    // readable, then mirrored into the views. Nothing runs while the
    // session idles: the views re-sync after the compositor said
    // something, on every frame while the switcher is open (its layout
    // and thumbnail delay move with time), and when the app list or the
    // icon theme changes what the overview shows.
    {
        let shell = shell.clone();
        let notify = notify.clone();
        let overview_ui = overview_ui.clone();
        let switcher_ui = switcher_ui.clone();
        let panel_window = window.clone();
        let panel_focus_window = window.clone();
        let panel_focus_held = panel_held.clone();
        let last_open = Cell::new(false);
        let chrome = {
            let shell = shell.clone();
            let actions = ShellActions(shell.clone());
            let chrome = preview_chrome::PreviewChrome::new(
                app.upcast_ref(),
                Rc::new(move |id| {
                    if let Some(control) = shell.borrow_mut().control.as_mut() {
                        let _ = control.close_window(id);
                    }
                }),
                // A preview activated by keyboard: its window, as a click
                // on the compositor's preview picks it.
                Rc::new(move |id| {
                    overview::OverviewActions::activate_window(&actions, id);
                    overview::OverviewActions::close_overview(&actions);
                }),
            );
            overview::OverviewUi::set_previews(&overview_ui, chrome.clone());
            chrome
        };
        let chrome_apps = apps.clone();
        let shell_rc = shell.clone();
        let activities_button = activities.clone();
        let last_frames: RefCell<Vec<tuna_shell_control::SwitcherThumbnail>> = RefCell::default();
        // A later re-sync, however often it is asked for before it runs
        // (filled in below, once `sync` exists).
        let resync = Rc::new(RefCell::new(None::<Rc<dyn Fn()>>));
        let resync_pending: Rc<Cell<bool>> = Rc::default();
        let resync_after = {
            let (resync, pending) = (resync.clone(), resync_pending.clone());
            move |delay: Duration| {
                if pending.replace(true) {
                    return;
                }
                let (resync, pending) = (resync.clone(), pending.clone());
                glib::timeout_add_local_once(delay, move || {
                    pending.set(false);
                    let sync = resync.borrow().clone();
                    if let Some(sync) = sync {
                        sync();
                    }
                });
            }
        };
        let sync: Rc<dyn Fn(Drained)> = {
            let (chrome, notify, switcher_ui) =
                (chrome.clone(), notify.clone(), switcher_ui.clone());
            let input_sources = input_sources.clone();
            let shell_rc = shell_rc.clone();
            let (osd_ui, pad_osd_ui, pad_osd_service) =
                (osd_ui.clone(), pad_osd_ui.clone(), pad_osd_service.clone());
            let resync_after = resync_after.clone();
            Rc::new(move |drained: Drained| {
                let Drained {
                    results,
                    auth_events,
                    accelerators,
                    menus,
                    consent,
                    display_change,
                    popups,
                    aid_switches,
                    dwell_click,
                    pointer_timeouts,
                    tablet,
                    osk: osk_updates,
                } = drained;
                let mut shell = shell.borrow_mut();
                let locked = shell.control.as_ref().is_some_and(|c| c.locked());
                let auth_generation = shell.control.as_ref().map(|c| c.connection_generation());
                let authoritative_lock = shell
                    .control
                    .as_ref()
                    .filter(|c| !c.needs_snapshot())
                    .map(|c| c.locked());
                render_pills(&mut shell);
                if let Some(control) = shell.control.as_ref() {
                    switcher_ui.sync(control.model());
                }
                // The switcher's window thumbnails: the compositor draws the
                // windows into the frames, sent whenever they move.
                let frames = switcher_ui.thumbnail_frames();
                if *last_frames.borrow() != frames {
                    if let Some(control) = shell.control.as_mut() {
                        match control.set_switcher_thumbnails(frames.clone()) {
                            Ok(_) => *last_frames.borrow_mut() = frames,
                            // The socket is full: try again a frame later.
                            Err(e) if is_would_block(&e) => resync_after(Duration::from_millis(16)),
                            Err(_) => {}
                        }
                    }
                }
                let open = shell
                    .control
                    .as_ref()
                    .is_some_and(|c| c.model().is_overview_open());
                // GNOME's per-window source follows the focus window, the
                // overview counting as one more (`_getCurrentWindow`).
                let holder = if open {
                    Some(input_sources::Holder::Overview)
                } else {
                    shell.control.as_ref().and_then(|c| {
                        c.model()
                            .windows()
                            .iter()
                            .find(|w| w.active)
                            .map(|w| input_sources::Holder::Window(w.id))
                    })
                };
                drop(shell);
                input_sources.focus(holder);
                if locked {
                    notify.release_focus();
                    release_panel_focus(&panel_focus_window, &panel_focus_held);
                }
                ibus_properties.sync_lock();
                lock_ui.sync_connection_generation(auth_generation);
                if let Some(locked) = authoritative_lock {
                    lock_ui.sync(locked);
                    lock_ui.resume_verification();
                }
                screensaver.sync(locked);
                // The session keyboard never shows over the lock screen.
                osk.set_locked(locked);
                for (focus, show, touch_auto) in osk_updates {
                    osk.update(focus, show, touch_auto);
                }
                if locked {
                    shortcut_consent.dismiss();
                } else if let Some(request) = consent {
                    shortcut_consent.sync(request);
                }
                if locked {
                    display_change_dialog.close();
                } else if let Some(secs) = display_change {
                    display_change_dialog.show(secs);
                }
                if locked {
                    pad_osd_ui.close();
                }
                for request in tablet {
                    match request {
                        TabletUi::Osd {
                            connector,
                            icon,
                            label,
                        } => osd_ui.show_on(
                            &osd::OsdRequest {
                                icon: Some(icon),
                                label: (!label.is_empty()).then_some(label),
                                ..Default::default()
                            },
                            connector.as_deref(),
                        ),
                        // The lock screen holds the keyboard: no modal OSD
                        // over it, and the compositor learns it closed.
                        TabletUi::ShowPad(info) if locked => pad_osd_ui.refuse(info.pad),
                        TabletUi::ShowPad(info) => pad_osd_ui.show(*info),
                        TabletUi::Labels { pad, labels } => pad_osd_ui.labels(pad, labels),
                        TabletUi::Input { pad, event } => pad_osd_ui.input(pad, event),
                        TabletUi::Close(pad) => pad_osd_ui.close_pad(pad),
                    }
                }
                for (action, time, mode, released) in accelerators {
                    if released {
                        gnome_shell.accelerator_deactivated(action, time, mode);
                    } else {
                        gnome_shell.accelerator_activated(action, time, mode);
                    }
                }
                for request in menus {
                    window_menu.open(&request);
                }
                for (index, count) in popups {
                    workspace_popup.display(index, count);
                }
                for (aid, enabled) in aid_switches {
                    kbd_a11y_dialog.switched(aid, enabled, locked);
                }
                if let Some(click) = dwell_click {
                    dwell_chooser.set_click_type(click);
                }
                for (kind, duration_ms, clicked, x, y) in pointer_timeouts {
                    pointer_a11y::on_timeout(&pie_timer, kind, duration_ms, clicked, x, y);
                }
                if locked {
                    kbd_a11y_dialog.dismiss();
                }
                // Conversation steps first: a step and the result ending its
                // conversation can arrive in one drain, in that order.
                for (request, service, event) in auth_events {
                    lock_ui.auth_event(request, service, &event);
                }
                for (id, applied) in results {
                    // Receipt of Applied precedes the runtime manager replacement.
                    media_keys::trace(
                        "control-ack-not-runtime-application",
                        serde_json::json!({
                            "request_id": id, "applied": applied,
                        }),
                        locked,
                    );
                    lock_ui.command_result(id, applied);
                    pad_osd_service.command_result(id, applied);
                }
                // The bar goes see-through and Activities shows as checked
                // while the overview is open, as GNOME draws them; it is a
                // toggle button (panel.js), pressed while the overview is open.
                if open {
                    panel_window.add_css_class("overview");
                    activities_button.add_css_class("checked");
                } else {
                    panel_window.remove_css_class("overview");
                    activities_button.remove_css_class("checked");
                }
                activities_button.update_state(&[gtk::accessible::State::Pressed(if open {
                    gtk::AccessibleTristate::True
                } else {
                    gtk::AccessibleTristate::False
                })]);
                overview::OverviewUi::set_open(&overview_ui, open);
                if open != last_open.replace(open) {
                    // Panel focus follows the overview: opening hands the
                    // keyboard over; closing releases a hold that never won
                    // it (the overview kept the keyboard), while a hold the
                    // bar actually won survives.
                    if open || !panel_focus_window.is_active() {
                        release_panel_focus(&panel_focus_window, &panel_focus_held);
                    }
                }
                // GNOME's preview chrome over the compositor's previews.
                let shell = shell_rc.borrow();
                if let Some(control) = shell.control.as_ref() {
                    let (previews, hovered) = control.overview_previews();
                    let model = control.model();
                    let apps = chrome_apps.get();
                    let lookup = |id: u64| {
                        let window = model.windows().iter().find(|w| w.id == id)?;
                        let icon = window
                            .app_id
                            .as_deref()
                            .and_then(|app| providers::provider_app(&apps, app))
                            .and_then(|entry| entry.icon.clone())
                            .or_else(|| {
                                window
                                    .app_id
                                    .as_deref()
                                    .and_then(|app| providers::provider_app(&apps, app))
                                    .is_none()
                                    .then(|| {
                                        window.icon.clone().filter(|icon| usable_window_icon(icon))
                                    })
                                    .flatten()
                            })
                            .map(|icon| -> gio::Icon {
                                if icon.starts_with('/') {
                                    gio::FileIcon::new(&gio::File::for_path(&icon)).upcast()
                                } else {
                                    gio::ThemedIcon::new(&icon).upcast()
                                }
                            })
                            .unwrap_or_else(|| {
                                gio::ThemedIcon::new("application-x-executable").upcast()
                            });
                        Some(preview_chrome::PreviewWindow {
                            title: window.title.clone(),
                            icon,
                        })
                    };
                    chrome.update(previews, hovered, &lookup);
                }
            })
        };
        *resync.borrow_mut() = Some({
            let sync = sync.clone();
            Rc::new(move || sync(Drained::default()))
        });
        {
            let sync = sync.clone();
            switcher_ui.on_frame(move || sync(Drained::default()));
        }
        // The overview's dash and preview chrome look icons up in the app
        // list and the icon theme.
        {
            let resync_after = resync_after.clone();
            apps.connect_changed(move || {
                chrome.forget_windows();
                resync_after(Duration::ZERO);
            });
        }
        if let Some(display) = gtk::gdk::Display::default() {
            let resync_after = resync_after.clone();
            gtk::IconTheme::for_display(&display).connect_changed(move |_| {
                resync_after(Duration::ZERO);
            });
        }
        sync(Drained::default());
        // Every frame the socket holds, then what the connection is now.
        let pump = {
            let (shell, notify, sync) = (shell_rc.clone(), notify.clone(), sync.clone());
            move || {
                let mut drained = Drained::default();
                let mut shell = shell.borrow_mut();
                let Some(control) = shell.control.as_mut() else {
                    return Link::Lost;
                };
                let (before, was_closed) = (control.connection_generation(), control.is_closed());
                let (handled, usable) = drain(control, &notify, &mut drained);
                let (generation, closed) = (control.connection_generation(), control.is_closed());
                let link = if closed {
                    Link::Redial
                } else if usable {
                    Link::Open {
                        generation,
                        fd: control.as_raw_fd(),
                    }
                } else {
                    Link::Lost
                };
                drop(shell);
                // A hang-up or a redial changes what the views may trust
                // (lock authority, input-method admission) without a frame.
                if handled || generation != before || closed != was_closed {
                    sync(drained);
                }
                link
            }
        };
        // One readability watch on the current stream. A redial (a poll
        // after the compositor hung up, or a send that met a dead peer)
        // replaces the stream and its descriptor: the old watch ends and
        // the new stream gets its own. While the compositor is gone, a
        // one-shot timeout redials on the client's spacing. The watch
        // lives as long as the shell.
        let watched: Rc<Cell<Option<u64>>> = Rc::default();
        let redial_pending: Rc<Cell<bool>> = Rc::default();
        let tend = Rc::new(RefCell::new(None::<Rc<dyn Fn(Link)>>));
        let pump: Rc<dyn Fn() -> Link> = Rc::new(pump);
        *tend.borrow_mut() = Some({
            let (pump, tend) = (pump.clone(), tend.clone());
            Rc::new(move |link: Link| match link {
                Link::Open { generation, fd } => {
                    if watched.replace(Some(generation)) == Some(generation) {
                        return;
                    }
                    let (pump, tend, watched) = (pump.clone(), tend.clone(), watched.clone());
                    let hangup = glib::IOCondition::HUP | glib::IOCondition::ERR;
                    fd_watch::add_local(fd, glib::IOCondition::IN | hangup, move |_, condition| {
                        if watched.get() != Some(generation) {
                            // A newer stream has its own watch.
                            return glib::ControlFlow::Break;
                        }
                        let link = pump();
                        if matches!(link, Link::Open { generation: now, .. } if now == generation) {
                            if !condition.intersects(hangup) {
                                return glib::ControlFlow::Continue;
                            }
                            eprintln!("tuna-shell-gtk: control error: the socket hung up");
                            watched.set(None);
                            return glib::ControlFlow::Break;
                        }
                        watched.set(None);
                        let next = tend.borrow().clone();
                        if let Some(next) = next {
                            next(link);
                        }
                        glib::ControlFlow::Break
                    });
                }
                Link::Redial => {
                    watched.set(None);
                    if redial_pending.replace(true) {
                        return;
                    }
                    let (pump, tend, pending) =
                        (pump.clone(), tend.clone(), redial_pending.clone());
                    glib::timeout_add_local_once(
                        tuna_shell_host::control::REDIAL_INTERVAL,
                        move || {
                            pending.set(false);
                            let link = pump();
                            let next = tend.borrow().clone();
                            if let Some(next) = next {
                                next(link);
                            }
                        },
                    );
                }
                Link::Lost => watched.set(None),
            })
        });
        // The handshake may have read frames past the first snapshot: the
        // socket will not report those as readable.
        let link = pump();
        let first = tend.borrow().clone();
        if let Some(first) = first {
            first(link);
        }
    }

    window.present();
}

fn usable_window_icon(icon: &str) -> bool {
    if icon.starts_with('/') {
        std::path::Path::new(icon).is_file()
    } else {
        gtk::gdk::Display::default().is_some_and(|d| gtk::IconTheme::for_display(&d).has_icon(icon))
    }
}

fn main() -> glib::ExitCode {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("tuna-shell-gtk {}", release_version());
        return glib::ExitCode::SUCCESS;
    }
    glib::set_prgname(Some("tuna-shell-gtk"));
    // GTK and libadwaita must not block startup on portal activation.
    portal_opt_out::disable();
    // Before any window is realized, so GSK's first renderer follows it.
    if gtk::init().is_ok() {
        if let Some(display) = gtk::gdk::Display::default() {
            renderer::prefer_cairo_on_software_gl(&display);
        }
    }
    let app = adw::Application::builder()
        .application_id("org.tuna.Shell")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    // GApplication's RUN_FIRST startup handler completes GTK/libadwaita's
    // VFS initialization before this worker can construct dconf settings.
    // Otherwise GTK can own the VFS once-init and wait for GIO's module
    // mutex while dconf owns that mutex and waits for the same VFS once-init.
    app.connect_startup(privacy_portal::start);
    app.connect_activate(build);
    app.run_with_args::<&str>(&[])
}
