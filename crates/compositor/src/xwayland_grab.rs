//! `zwp_xwayland_keyboard_grab_manager_v1` version 1, as Mutter 51's
//! `meta-xwayland-grab-keyboard.c` (#89, #364).
//!
//! Rootless Xwayland asks for a grab whenever an X11 client actively
//! grabs the keyboard (`XGrabKeyboard`). The global is offered to the
//! Xwayland client only (Smithay's `can_view` keeps Smithay's
//! `XWaylandClientData` clients, as Mutter's `xwayland_grab_keyboard_filter`
//! keeps `xwayland_manager->client`).
//!
//! A grab is granted as `meta_xwayland_grab_is_granted` decides, once
//! the surface is associated with its X11 window: denied when WM_CLASS's
//! class or instance matches a `!` rule; else granted when the client
//! sent `_XWAYLAND_MAY_GRAB_KEYBOARD` for the window, or when class or
//! instance matches an allow rule. The rules are Mutter's built-in list
//! ([`DEFAULT_ACCESS_RULES`]) followed by `org.gnome.mutter.wayland
//! xwayland-grab-access-rules`, matched as `GPatternSpec` globs
//! (`meta-settings.c` `update_xwayland_grab_access_rules`). A granted
//! grab inhibits the compositor's shortcuts on that surface
//! (`meta_wayland_surface_inhibit_shortcuts`): while it holds keyboard
//! focus every key reaches it. Super+Escape (restore-shortcuts) ends
//! the grab (`shortcuts_restored_cb`); destroying the grab or its
//! surface releases it. A denied grab changes nothing.
//!
//! Not implemented: Mutter's focus lock for override-redirect windows
//! (`meta_xwayland_grab_should_lock_focus`, gated on
//! `xwayland-allow-grabs`). Tuna Desktop does not present
//! override-redirect X11 windows, so there is nothing to lock onto.

use smithay::{
    delegate_xwayland_keyboard_grab,
    input::Seat,
    reexports::{
        wayland_protocols::xwayland::keyboard_grab::zv1::server::zwp_xwayland_keyboard_grab_v1::ZwpXwaylandKeyboardGrabV1,
        wayland_server::{protocol::wl_surface::WlSurface, DisplayHandle, Resource},
    },
    wayland::{
        compositor::{get_role, with_states},
        xwayland_keyboard_grab::{
            XWaylandKeyboardGrab, XWaylandKeyboardGrabHandler, XWaylandKeyboardGrabState,
        },
    },
    xwayland::X11Surface,
};
use std::sync::Mutex;

use crate::State;

/// Mutter 51's built-in access rules: the `xwayland_grab_default_access_rules`
/// meson option (meson.options), compiled in as
/// `XWAYLAND_GRAB_DEFAULT_ACCESS_RULES` and split on `,`.
pub const DEFAULT_ACCESS_RULES: &str =
    "gnome-boxes,remote-viewer,virt-viewer,virt-manager,vinagre,vncviewer,Xephyr";

/// Allow and deny patterns, Mutter's `xwayland_grab_allow_list_patterns`
/// and `xwayland_grab_deny_list_patterns`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessRules {
    allow: Vec<String>,
    deny: Vec<String>,
}

impl Default for AccessRules {
    /// The built-in rules alone (an empty `xwayland-grab-access-rules`,
    /// its schema default, or no `org.gnome.mutter.wayland` schema).
    fn default() -> Self {
        Self::new(&[])
    }
}

impl AccessRules {
    /// `update_xwayland_grab_access_rules`: the built-in rules, then the
    /// `xwayland-grab-access-rules` entries.
    pub fn new(user: &[String]) -> Self {
        let mut rules = Self {
            allow: Vec::new(),
            deny: Vec::new(),
        };
        for item in DEFAULT_ACCESS_RULES
            .split(',')
            .chain(user.iter().map(String::as_str))
        {
            rules.add(item);
        }
        rules
    }

    /// `xwayland_grab_list_add_item`: a leading `!` denies the rest; a
    /// lone `!` is ignored.
    fn add(&mut self, item: &str) {
        match item.strip_prefix('!') {
            None => self.allow.push(item.to_owned()),
            Some("") => {}
            Some(pattern) => self.deny.push(pattern.to_owned()),
        }
    }

    /// `meta_xwayland_grab_is_granted` for a window with WM_CLASS
    /// `instance`/`class` (Mutter's res_name/res_class, empty when unset):
    /// a deny match refuses; otherwise the client's own
    /// `_XWAYLAND_MAY_GRAB_KEYBOARD` or an allow match grants.
    pub fn is_granted(&self, class: &str, instance: &str, may_grab: bool) -> bool {
        if in_patterns(&self.deny, class, instance) {
            return false;
        }
        may_grab || in_patterns(&self.allow, class, instance)
    }
}

/// `application_is_in_pattern_array`: class or instance matches a pattern.
fn in_patterns(patterns: &[String], class: &str, instance: &str) -> bool {
    patterns
        .iter()
        .any(|pattern| pattern_matches(pattern, class) || pattern_matches(pattern, instance))
}

/// GLib's `GPatternSpec` over the whole string: `*` matches any run of
/// characters (none included), `?` exactly one character, everything
/// else itself; there is no escape.
pub fn pattern_matches(pattern: &str, string: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let string: Vec<char> = string.chars().collect();
    let (mut p, mut s) = (0, 0);
    // The latest `*` and the string position it currently covers up to.
    let mut star: Option<(usize, usize)> = None;
    while s < string.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, s));
                p += 1;
            }
            Some(&c) if c == '?' || c == string[s] => {
                p += 1;
                s += 1;
            }
            _ => match star {
                // Let the last `*` take one more character and retry.
                Some((star_p, star_s)) => {
                    star = Some((star_p, star_s + 1));
                    p = star_p + 1;
                    s = star_s + 1;
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// One `zwp_xwayland_keyboard_grab_v1`.
struct Grab {
    resource: ZwpXwaylandKeyboardGrabV1,
    surface: WlSurface,
    /// `None` until the surface's X11 window is known (Mutter waits for
    /// `window-associated`); then whether it inhibits shortcuts. A
    /// restore-shortcuts ends it for good.
    granted: Option<bool>,
}

impl Grab {
    fn live(&self) -> bool {
        self.resource.is_alive() && self.surface.is_alive()
    }
}

/// The global, the access rules, and the grabs Xwayland holds.
pub(crate) struct XwaylandGrabs {
    _global: XWaylandKeyboardGrabState,
    rules: AccessRules,
    grabs: Vec<Grab>,
}

impl XwaylandGrabs {
    pub(crate) fn new(dh: &DisplayHandle) -> Self {
        Self {
            _global: XWaylandKeyboardGrabState::new::<State>(dh),
            rules: AccessRules::default(),
            grabs: Vec::new(),
        }
    }

    /// Whether a granted grab holds `surface`'s shortcuts.
    pub(crate) fn inhibits(&self, surface: &WlSurface) -> bool {
        self.grabs
            .iter()
            .any(|grab| grab.granted == Some(true) && &grab.surface == surface && grab.live())
    }

    /// restore-shortcuts on `surface`: its granted grabs end. Returns
    /// whether one was granted.
    pub(crate) fn restore(&mut self, surface: &WlSurface) -> bool {
        let mut restored = false;
        for grab in &mut self.grabs {
            if grab.granted == Some(true) && &grab.surface == surface && grab.live() {
                grab.granted = Some(false);
                restored = true;
            }
        }
        restored
    }

    /// Rules for grabs decided from now on (Mutter reads its settings
    /// when a grab activates; earlier grabs keep their decision).
    pub(crate) fn set_rules(&mut self, rules: AccessRules) {
        self.rules = rules;
    }

    fn decide(&self, window: &X11Surface) -> bool {
        let (class, instance) = (window.class(), window.instance());
        let granted = self
            .rules
            .is_granted(&class, &instance, window.xwayland_may_grab_keyboard());
        eprintln!(
            "tuna-compositor: xwayland: keyboard grab {} for window {} (class {class:?}, instance {instance:?})",
            if granted { "granted" } else { "denied" },
            window.window_id(),
        );
        granted
    }
}

/// The X11 window associated with an Xwayland `surface`, if any.
fn x11_window(surface: &WlSurface) -> Option<X11Surface> {
    with_states(surface, |states| {
        states
            .data_map
            .get::<Mutex<Option<X11Surface>>>()
            .and_then(|window| window.lock().unwrap().clone())
    })
}

impl State {
    /// An Xwayland surface was associated with its X11 window: remember
    /// the window for later grabs and decide the grabs waiting for it
    /// (Mutter's `meta_xwayland_keyboard_window_associated`).
    pub(crate) fn xwayland_surface_associated(&mut self, surface: &WlSurface, window: &X11Surface) {
        with_states(surface, |states| {
            *states
                .data_map
                .get_or_insert_threadsafe(|| Mutex::new(None::<X11Surface>))
                .lock()
                .unwrap() = Some(window.clone());
        });
        let grabs = &mut self.protocols.xwayland_grabs;
        grabs.grabs.retain(Grab::live);
        let pending = grabs
            .grabs
            .iter()
            .any(|grab| grab.granted.is_none() && &grab.surface == surface);
        if pending {
            let granted = grabs.decide(window);
            for grab in &mut grabs.grabs {
                if grab.granted.is_none() && &grab.surface == surface {
                    grab.granted = Some(granted);
                }
            }
        }
    }

    /// Replace the user's `xwayland-grab-access-rules` entries.
    pub(crate) fn set_xwayland_grab_access_rules(&mut self, rules: AccessRules) {
        self.protocols.xwayland_grabs.set_rules(rules);
    }
}

impl XWaylandKeyboardGrabHandler for State {
    /// Every request yields a grab object; whether it does anything is
    /// decided in [`Self::grab`].
    fn keyboard_focus_for_xsurface(&self, surface: &WlSurface) -> Option<WlSurface> {
        Some(surface.clone())
    }

    /// `zwp_xwayland_keyboard_grab_manager_grab`. Unlike Smithay's
    /// default, no keyboard grab is set: a granted grab inhibits
    /// shortcuts as Mutter's does.
    fn grab(&mut self, surface: WlSurface, _seat: Seat<Self>, grab: XWaylandKeyboardGrab<Self>) {
        let grabs = &mut self.protocols.xwayland_grabs;
        grabs.grabs.retain(Grab::live);
        let granted = match x11_window(&surface) {
            Some(window) => Some(grabs.decide(&window)),
            None if get_role(&surface).is_some() => None,
            None => {
                eprintln!("tuna-compositor: xwayland: cannot grant a keyboard grab to a surface without a role");
                Some(false)
            }
        };
        grabs.grabs.push(Grab {
            resource: grab.grab().clone(),
            surface,
            granted,
        });
    }
}
delegate_xwayland_keyboard_grab!(State);

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(user: &[&str]) -> AccessRules {
        AccessRules::new(&user.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn gpatternspec_globs_match_the_whole_string() {
        assert!(pattern_matches("Xephyr", "Xephyr"));
        assert!(!pattern_matches("Xephyr", "xephyr"), "case-sensitive");
        assert!(!pattern_matches("virt", "virt-viewer"), "whole string only");
        assert!(pattern_matches("virt-*", "virt-viewer"));
        assert!(pattern_matches("*viewer", "remote-viewer"));
        assert!(pattern_matches("*", ""));
        assert!(pattern_matches("**", "anything"));
        assert!(pattern_matches("v?ncviewer", "vincviewer"));
        assert!(
            !pattern_matches("v?ncviewer", "vncviewer"),
            "? is one character"
        );
        assert!(pattern_matches("?", "é"), "? is one UTF-8 character");
        assert!(pattern_matches("a*b*c", "aXXbYYbc"));
        assert!(!pattern_matches("a*b*c", "aXXbYYb"));
        assert!(pattern_matches("", ""));
        assert!(!pattern_matches("", "x"));
    }

    #[test]
    fn mutters_default_list_grants_its_viewers_by_class_or_instance() {
        let rules = AccessRules::default();
        for name in [
            "gnome-boxes",
            "remote-viewer",
            "virt-viewer",
            "virt-manager",
            "vinagre",
            "vncviewer",
            "Xephyr",
        ] {
            assert!(rules.is_granted(name, "", false), "class {name}");
            assert!(rules.is_granted("", name, false), "instance {name}");
        }
        assert!(!rules.is_granted("XTerm", "xterm", false));
        assert!(!rules.is_granted("Remote-viewer", "remote-viewer-x", false));
        assert!(!rules.is_granted("", "", false));
    }

    #[test]
    fn deny_rules_win_over_allow_rules_and_the_client_message() {
        let rules = rules(&["!remote-viewer", "XTerm", "!Fire*"]);
        assert!(!rules.is_granted("Remote-viewer", "remote-viewer", false));
        assert!(!rules.is_granted("Remote-viewer", "remote-viewer", true));
        assert!(rules.is_granted("XTerm", "xterm", false));
        assert!(!rules.is_granted("Firefox", "Navigator", true));
        // Other defaults stay allowed.
        assert!(rules.is_granted("Virt-viewer", "virt-viewer", false));
    }

    #[test]
    fn the_client_message_grants_an_unlisted_window() {
        let rules = AccessRules::default();
        assert!(!rules.is_granted("Spice", "spicy", false));
        assert!(rules.is_granted("Spice", "spicy", true));
    }

    #[test]
    fn a_lone_bang_is_ignored_and_globs_apply_to_rules() {
        let rules = rules(&["!", "my-*-vm", "!*-secret-vm"]);
        assert_eq!(rules.deny, vec!["*-secret-vm".to_owned()]);
        assert!(rules.is_granted("", "my-test-vm", false));
        assert!(!rules.is_granted("", "my-secret-vm", false));
        assert_eq!(AccessRules::default().allow.len(), 7);
        assert!(AccessRules::default().deny.is_empty());
    }
}

/// The real path: a spawned Xwayland, Smithay's X11 window manager on
/// this crate's handlers, and an X11 client issuing `XGrabKeyboard`.
#[cfg(test)]
mod xwayland_tests {
    use super::*;
    use smithay::{
        input::keyboard::XkbConfig,
        output::{Mode, Output, PhysicalProperties, Subpixel},
        reexports::{
            calloop::EventLoop,
            wayland_server::{backend::GlobalId, Display},
        },
        utils::SERIAL_COUNTER,
        xwayland::{X11Wm, XWayland, XWaylandEvent},
    };
    use std::time::{Duration, Instant};
    use x11rb::{
        connection::Connection,
        protocol::xproto::{
            AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, GrabMode,
            GrabStatus, PropMode, WindowClass,
        },
        rust_connection::RustConnection,
        wrapper::ConnectionExt as _,
        COPY_DEPTH_FROM_PARENT, CURRENT_TIME,
    };

    use crate::xwayland::X11ManagerEvent;

    const LIMIT: Duration = Duration::from_secs(30);

    struct Session {
        // Dropped first: XWayland's drop disconnects (and so ends) the server.
        event_loop: EventLoop<'static, State>,
        display: Display<State>,
        state: State,
        x: RustConnection,
        root: u32,
        may_grab: u32,
        _output: GlobalId,
    }

    impl Session {
        fn start() -> Self {
            let display = Display::<State>::new().unwrap();
            let dh = display.handle();
            let mut state = State::new(&dh);
            // Xwayland routes grabs only for a seat with a keyboard.
            state
                .seat
                .add_keyboard(XkbConfig::default(), 200, 25)
                .unwrap();
            let output = Output::new(
                "grab-test".to_owned(),
                PhysicalProperties {
                    size: (0, 0).into(),
                    subpixel: Subpixel::Unknown,
                    make: "Tuna Desktop".to_owned(),
                    model: "Test".to_owned(),
                },
            );
            let mode = Mode {
                size: (800, 600).into(),
                refresh: 60_000,
            };
            output.change_current_state(Some(mode), None, None, Some((0, 0).into()));
            output.set_preferred(mode);
            let output_global = output.create_global::<State>(&dh);
            let event_loop = EventLoop::<State>::try_new().unwrap();
            let (xwayland, client) = XWayland::spawn(
                &dh,
                None,
                std::iter::empty::<(&str, &str)>(),
                true,
                std::process::Stdio::null(),
                std::process::Stdio::null(),
                |_| {},
            )
            .expect("Xwayland must be installed for the real grab path");
            let handle = event_loop.handle();
            let wm_handle = handle.clone();
            let started = std::rc::Rc::new(std::cell::Cell::new(None));
            let ready = started.clone();
            handle
                .insert_source(xwayland, move |event, _, state: &mut State| match event {
                    XWaylandEvent::Ready {
                        x11_socket,
                        display_number,
                    } => {
                        ready.set(Some(display_number));
                        state.xwm = Some(
                            X11Wm::start_wm(wm_handle.clone(), x11_socket, client.clone()).unwrap(),
                        );
                    }
                    XWaylandEvent::Error => panic!("Xwayland exited during startup"),
                })
                .unwrap();
            let (mut event_loop, mut display) = (event_loop, display);
            let deadline = Instant::now() + LIMIT;
            let display_number = loop {
                if let Some(number) = started.get().filter(|_| state.xwm.is_some()) {
                    break number;
                }
                assert!(Instant::now() < deadline, "Xwayland never became ready");
                pump(&mut event_loop, &mut display, &mut state);
            };
            let display_name = format!(":{display_number}");
            let (x, screen) = RustConnection::connect(Some(&display_name)).unwrap();
            let root = x.setup().roots[screen].root;
            let may_grab = x
                .intern_atom(false, b"_XWAYLAND_MAY_GRAB_KEYBOARD")
                .unwrap()
                .reply()
                .unwrap()
                .atom;
            Self {
                event_loop,
                display,
                state,
                x,
                root,
                may_grab,
                _output: output_global,
            }
        }

        fn pump(&mut self) {
            pump(&mut self.event_loop, &mut self.display, &mut self.state);
            while self.x.poll_for_event().unwrap().is_some() {}
        }

        fn until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
            let deadline = Instant::now() + LIMIT;
            while !done(self) {
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                self.pump();
            }
        }

        /// Map a window with WM_CLASS `instance`/`class`, wait for its
        /// Wayland surface, and give that surface keyboard focus.
        fn window(&mut self, instance: &str, class: &str) -> (u32, WlSurface) {
            let x = &self.x;
            let window = x.generate_id().unwrap();
            let screen = &x.setup().roots[0];
            x.create_window(
                COPY_DEPTH_FROM_PARENT,
                window,
                self.root,
                0,
                0,
                200,
                100,
                0,
                WindowClass::INPUT_OUTPUT,
                screen.root_visual,
                &CreateWindowAux::new()
                    .background_pixel(screen.white_pixel)
                    .event_mask(EventMask::STRUCTURE_NOTIFY | EventMask::EXPOSURE),
            )
            .unwrap();
            let wm_class = format!("{instance}\0{class}\0");
            x.change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                wm_class.as_bytes(),
            )
            .unwrap();
            x.map_window(window).unwrap();
            x.flush().unwrap();
            let mut mapped = None;
            self.until("the window manager's map", |session| {
                for event in session.state.take_x11_events() {
                    if let X11ManagerEvent::MapRequest(surface) = event {
                        if surface.window_id() == window {
                            mapped = Some(*surface);
                        }
                    }
                }
                mapped.is_some()
            });
            let mapped = mapped.unwrap();
            self.until("the X11 window's surface association", |_| {
                mapped.wl_surface().is_some()
            });
            let surface = mapped.wl_surface().unwrap();
            self.until("the class Smithay reads", |_| mapped.class() == class);
            let keyboard = self.state.seat.get_keyboard().unwrap();
            keyboard.set_focus(
                &mut self.state,
                Some(surface.clone()),
                SERIAL_COUNTER.next_serial(),
            );
            (window, surface)
        }

        /// `XGrabKeyboard` on `window`, then the compositor's decision.
        fn grab(&mut self, window: u32, surface: &WlSurface) -> bool {
            let reply = self
                .x
                .grab_keyboard(
                    false,
                    window,
                    CURRENT_TIME,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )
                .unwrap()
                .reply()
                .unwrap();
            assert_eq!(reply.status, GrabStatus::SUCCESS);
            let mut decided = None;
            self.until("the grab decision", |session| {
                decided = session.decision(surface);
                decided.is_some()
            });
            decided.unwrap()
        }

        fn ungrab(&mut self, surface: &WlSurface) {
            self.x.ungrab_keyboard(CURRENT_TIME).unwrap();
            self.x.flush().unwrap();
            self.until("the grab's release", |session| {
                !session
                    .state
                    .protocols
                    .xwayland_grabs
                    .grabs
                    .iter()
                    .any(|grab| &grab.surface == surface && grab.live())
            });
        }

        fn decision(&self, surface: &WlSurface) -> Option<bool> {
            self.state
                .protocols
                .xwayland_grabs
                .grabs
                .iter()
                .rev()
                .find(|grab| &grab.surface == surface && grab.live())
                .and_then(|grab| grab.granted)
        }
    }

    fn pump(
        event_loop: &mut EventLoop<'static, State>,
        display: &mut Display<State>,
        state: &mut State,
    ) {
        event_loop
            .dispatch(Some(Duration::from_millis(5)), state)
            .unwrap();
        display.dispatch_clients(state).unwrap();
        display.flush_clients().unwrap();
    }

    #[test]
    fn xwayland_grabs_follow_mutters_access_rules_end_to_end() {
        let mut session = Session::start();

        // A built-in rule (instance remote-viewer): granted, and every
        // shortcut goes to the focused window.
        let (viewer, viewer_surface) = session.window("remote-viewer", "Remote-viewer");
        assert!(!session.state.shortcuts_inhibited());
        assert!(session.grab(viewer, &viewer_surface));
        assert!(session.state.shortcuts_inhibited());
        // Super+Escape (restore-shortcuts) ends the grab for good.
        assert!(session.state.restore_shortcuts());
        assert!(!session.state.shortcuts_inhibited());
        assert!(!session.state.restore_shortcuts());
        session.ungrab(&viewer_surface);
        // A new X11 grab is a new decision; releasing it gives the
        // shortcuts back.
        assert!(session.grab(viewer, &viewer_surface));
        assert!(session.state.shortcuts_inhibited());
        session.ungrab(&viewer_surface);
        assert!(!session.state.shortcuts_inhibited());

        // Not listed: the grab is a no-op.
        let (term, term_surface) = session.window("xterm", "XTerm");
        assert!(!session.grab(term, &term_surface));
        assert!(!session.state.shortcuts_inhibited());
        session.ungrab(&term_surface);

        // The client allows itself with _XWAYLAND_MAY_GRAB_KEYBOARD, sent
        // to the root window as spice-gtk does.
        let event = ClientMessageEvent::new(32, term, session.may_grab, [1, 0, 0, 0, 0]);
        session
            .x
            .send_event(
                false,
                session.root,
                EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
                event,
            )
            .unwrap();
        session.x.flush().unwrap();
        session.until("the client message", |_| {
            x11_window(&term_surface).is_some_and(|window| window.xwayland_may_grab_keyboard())
        });
        assert!(session.grab(term, &term_surface));
        assert!(session.state.shortcuts_inhibited());
        session.ungrab(&term_surface);

        // A `!` rule denies even a built-in or self-allowed window.
        session
            .state
            .set_xwayland_grab_access_rules(AccessRules::new(&[
                "!remote-viewer".to_owned(),
                "!XTerm".to_owned(),
            ]));
        let keyboard = session.state.seat.get_keyboard().unwrap();
        keyboard.set_focus(
            &mut session.state,
            Some(viewer_surface.clone()),
            SERIAL_COUNTER.next_serial(),
        );
        assert!(!session.grab(viewer, &viewer_surface));
        assert!(!session.state.shortcuts_inhibited());
        session.ungrab(&viewer_surface);
        keyboard.set_focus(
            &mut session.state,
            Some(term_surface.clone()),
            SERIAL_COUNTER.next_serial(),
        );
        assert!(!session.grab(term, &term_surface));
        assert!(!session.state.shortcuts_inhibited());
    }
}
