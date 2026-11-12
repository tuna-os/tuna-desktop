//! Override-redirect X11 windows: menus, tooltips, dropdowns, combo
//! popups (P-SY-04, #1076).
//!
//! An X11 client that sets `override_redirect` on a window takes it out
//! of the window manager's hands: the map is not redirected, the client
//! places and stacks the window itself, and it drives it through its own
//! pointer and keyboard grabs. Mutter keeps such a window as an
//! *unmanaged* `MetaWindow` (`src/x11/window-x11.c`
//! `meta_window_x11_new`, `window->override_redirect`), and so does this
//! module:
//!
//! - It is drawn where the X server has it. Root coordinates are the
//!   compositor's global logical coordinates, the same space a managed
//!   X11 window is configured in (`sync_x11_positions`), at Smithay's
//!   Xwayland client scale. `window-x11.c`
//!   `meta_window_x11_configure_notify` follows each `ConfigureNotify`;
//!   here Smithay keeps the rectangle and the event only says the scene
//!   changed and where the window now stacks.
//! - Stacking: Mutter puts every override-redirect window in the layer
//!   `META_LAYER_OVERRIDE_REDIRECT` and its actor in the *top window
//!   group* (`src/compositor/compositor.c`
//!   `meta_compositor_real_add_window`), above every managed window.
//!   GNOME Shell stacks that group above the chrome it adds with
//!   `addChrome` (the top bar) and under what it adds on top of it (its
//!   own menus, the OSD, system dialogs) (`js/ui/layout.js`). Here: above
//!   every window and the shell's `Top` layer surfaces, below its
//!   `Overlay` ones, in the X server's own order among themselves.
//! - It shows with the window it belongs to (its `WM_TRANSIENT_FOR`
//!   chain, else a window of the same client process): not while that
//!   window is minimized or on another workspace. One belonging to no
//!   window always shows. This is narrower than Mutter, which makes
//!   every override-redirect window sticky (`src/core/window.c`
//!   `always_sticky`): a menu left open by a window that was then
//!   minimized or left on another workspace does not hang over the
//!   windows shown instead. Like the top window group (`layout.js`
//!   `_updateVisibility`), none shows in the overview or while the
//!   session is locked.
//! - It takes the pointer where its input region covers it, and nothing
//!   else: no keyboard focus (`window.c` `meta_window_focus` refuses an
//!   override-redirect window; the client's grabs route keys on the X
//!   side), no frame, no model entry, so no window-list, overview,
//!   switcher or workspace entry, and no tiling.
//!
//! One thing goes beyond Mutter. An X11 pointer grab promises the client
//! every press, wherever the pointer is: that is how a menu learns of
//! the click outside it that closes it. Xwayland only hears of the
//! pointer on its own surfaces, so a press on a native window or the
//! desktop never reaches it and the menu stays open (Mutter leaves it at
//! that). [`WindowManager::x11_grab_press`] gives such a press to
//! the X server instead while an unmanaged window shows and a client
//! holds the pointer grab, with the position it really has; the X server
//! then delivers it by the grab's own rules. Presses on the shell's own
//! surfaces stay the shell's.

use smithay::backend::input::ButtonState;
use smithay::input::pointer::{ButtonEvent, MotionEvent};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER};
use smithay::xwayland::{xwm::WmWindowType, X11Surface};

use super::WindowManager;
use crate::State;

/// How many `WM_TRANSIENT_FOR` links are followed to find an owner: a
/// submenu of a submenu of a menu of a window, with room to spare. A
/// client can build a cycle; this ends it.
const OWNER_DEPTH: usize = 8;

/// The managed window an unmanaged window belongs to through
/// `WM_TRANSIENT_FOR`: the first managed window on the chain that starts
/// at `transient_for`. `managed` resolves an X11 window id to a model id,
/// `parent` gives the `WM_TRANSIENT_FOR` of another unmanaged window.
pub(crate) fn transient_owner(
    transient_for: Option<u32>,
    managed: impl Fn(u32) -> Option<u64>,
    parent: impl Fn(u32) -> Option<u32>,
) -> Option<u64> {
    let mut next = transient_for;
    for _ in 0..OWNER_DEPTH {
        let xid = next?;
        if let Some(id) = managed(xid) {
            return Some(id);
        }
        next = parent(xid);
    }
    None
}

/// The owner among a client's managed windows (`candidates`, bottom to
/// top) when no `WM_TRANSIENT_FOR` names one: the focused one, else the
/// topmost.
pub(crate) fn client_owner(candidates: &[u64], focused: Option<u64>) -> Option<u64> {
    focused
        .filter(|id| candidates.contains(id))
        .or_else(|| candidates.last().copied())
}

/// Apply a `ConfigureNotify` to the unmanaged windows' order (X11 ids,
/// bottom to top). `above` is the sibling the window now sits directly
/// above. Only an order among unmanaged windows is kept: when the sibling
/// is one of them the window goes right above it; a sibling that is no
/// unmanaged window (or none, the bottom of the X stack) says nothing
/// about them and the order stays, as it does for every plain move.
pub(crate) fn restack(order: &mut Vec<u32>, window: u32, above: Option<u32>) {
    let Some(sibling) = above.filter(|sibling| *sibling != window) else {
        return;
    };
    if !order.contains(&window) || !order.contains(&sibling) {
        return;
    }
    order.retain(|other| *other != window);
    let at = order
        .iter()
        .position(|other| *other == sibling)
        .map_or(order.len(), |index| index + 1);
    order.insert(at, window);
}

/// The EWMH type of an unmanaged window, for the state file.
fn type_name(kind: Option<WmWindowType>) -> &'static str {
    match kind {
        None => "none",
        Some(WmWindowType::DropdownMenu) => "dropdown_menu",
        Some(WmWindowType::PopupMenu) => "popup_menu",
        Some(WmWindowType::Menu) => "menu",
        Some(WmWindowType::Tooltip) => "tooltip",
        Some(WmWindowType::Combo) => "combo",
        Some(WmWindowType::Notification) => "notification",
        Some(WmWindowType::Dnd) => "dnd",
        Some(WmWindowType::Utility) => "utility",
        Some(WmWindowType::Dialog) => "dialog",
        Some(WmWindowType::Splash) => "splash",
        Some(WmWindowType::Toolbar) => "toolbar",
        Some(WmWindowType::Dock) => "dock",
        Some(WmWindowType::Desktop) => "desktop",
        Some(WmWindowType::Normal) => "normal",
    }
}

impl WindowManager {
    /// An override-redirect window was mapped: it stacks on top, as a
    /// newly mapped window does in the X server.
    pub(super) fn x11_unmanaged_mapped(&mut self, surface: X11Surface) {
        let xid = surface.window_id();
        self.x11_unmanaged
            .retain(|other| other.window_id() != xid && other.alive());
        if !surface.alive() || !surface.is_override_redirect() {
            return;
        }
        self.x11_unmanaged.push(surface);
        self.pointer_scene_dirty.set(true);
    }

    /// An X11 window was unmapped or destroyed: no longer drawn if it
    /// was an unmanaged one.
    pub(super) fn x11_unmanaged_gone(&mut self, xid: u32) {
        let before = self.x11_unmanaged.len();
        self.x11_unmanaged
            .retain(|other| other.window_id() != xid && other.alive());
        if self.x11_unmanaged.len() != before {
            self.pointer_scene_dirty.set(true);
        }
    }

    /// A mapped override-redirect window moved, resized or restacked
    /// itself.
    pub(super) fn x11_unmanaged_configured(&mut self, xid: u32, above: Option<u32>) {
        let Some(at) = self
            .x11_unmanaged
            .iter()
            .position(|other| other.window_id() == xid)
        else {
            return;
        };
        // It may have given up override-redirect; the window manager then
        // manages it from its next MapRequest.
        if !self.x11_unmanaged[at].is_override_redirect() {
            self.x11_unmanaged.remove(at);
        } else {
            let mut order: Vec<u32> = self
                .x11_unmanaged
                .iter()
                .map(X11Surface::window_id)
                .collect();
            restack(&mut order, xid, above);
            self.x11_unmanaged.sort_by_key(|surface| {
                order
                    .iter()
                    .position(|other| *other == surface.window_id())
                    .unwrap_or(usize::MAX)
            });
        }
        self.pointer_scene_dirty.set(true);
    }

    /// Xwayland is gone, and every window of it.
    pub(super) fn x11_unmanaged_clear(&mut self) {
        if !self.x11_unmanaged.is_empty() {
            self.x11_unmanaged.clear();
            self.pointer_scene_dirty.set(true);
        }
    }

    /// The managed window an unmanaged window belongs to, if any.
    fn x11_unmanaged_owner(&self, surface: &X11Surface) -> Option<u64> {
        let by_transient = transient_owner(
            surface.is_transient_for(),
            |xid| self.x11_managed_id(xid),
            |xid| {
                self.x11_unmanaged
                    .iter()
                    .find(|other| other.window_id() == xid)
                    .and_then(X11Surface::is_transient_for)
            },
        );
        by_transient.or_else(|| {
            let pid = surface.pid()?;
            let candidates: Vec<u64> = self
                .stacking
                .iter()
                .copied()
                .filter(|id| {
                    self.windows
                        .get(id)
                        .and_then(|window| window.surface.x11_surface())
                        .is_some_and(|other| other.pid() == Some(pid))
                })
                .collect();
            client_owner(&candidates, self.model.focused())
        })
    }

    /// The model id of the managed window whose client window, or the
    /// window carrying its surface (its frame), is X11 window `xid`.
    fn x11_managed_id(&self, xid: u32) -> Option<u64> {
        self.x11_index.get(&xid).copied().or_else(|| {
            self.windows.iter().find_map(|(id, window)| {
                window
                    .surface
                    .x11_surface()
                    .is_some_and(|surface| surface.mapped_window_id() == Some(xid))
                    .then_some(*id)
            })
        })
    }

    /// Whether an unmanaged window shows: with its owner, or always when
    /// it has none.
    fn x11_unmanaged_shows(&self, owner: Option<u64>, visible: &[u64]) -> bool {
        owner.is_none_or(|id| visible.contains(&id))
    }

    /// Whether unmanaged windows are on screen at all: on the desktop
    /// only, not in the overview (GNOME hides the top window group
    /// there) and not behind the lock screen.
    fn x11_unmanaged_on_screen(&self) -> bool {
        !self.x11_unmanaged.is_empty() && !self.overview_open && !self.lock_input_active
    }

    /// The unmanaged windows on screen, bottom to top: each one's
    /// surface and the rectangle the X server has it at, in global
    /// logical coordinates. What a frame draws and what the pointer can
    /// reach.
    pub fn x11_unmanaged_entries(&self) -> Vec<(WlSurface, Rectangle<i32, Logical>)> {
        if !self.x11_unmanaged_on_screen() {
            return Vec::new();
        }
        let visible: Vec<u64> = self
            .visible_entries()
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        self.x11_unmanaged
            .iter()
            .filter(|surface| surface.alive() && surface.is_override_redirect())
            .filter(|surface| self.x11_unmanaged_shows(self.x11_unmanaged_owner(surface), &visible))
            .filter_map(|surface| Some((surface.wl_surface()?, surface.geometry())))
            .collect()
    }

    /// The surfaces of every mapped unmanaged window, shown or not, for
    /// frame callbacks: Xwayland paces its commits on them.
    pub(super) fn x11_unmanaged_surfaces(&self) -> impl Iterator<Item = WlSurface> + '_ {
        self.x11_unmanaged.iter().filter_map(X11Surface::wl_surface)
    }

    /// The topmost unmanaged window taking the pointer at `pos`: its
    /// surface and origin. A window takes it where its buffer is and its
    /// input region covers the point (a tooltip's or a shaped menu's
    /// transparent parts pass it on).
    pub(super) fn x11_unmanaged_at(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<i32, Logical>)> {
        let point = Point::<i32, Logical>::from((pos.x.floor() as i32, pos.y.floor() as i32));
        self.x11_unmanaged_entries()
            .into_iter()
            .rev()
            .find(|(surface, rect)| {
                let local = point - rect.loc;
                super::buffer_size(surface)
                    .is_some_and(|size| Rectangle::from_size(size).contains(local))
                    && crate::layer::accepts_input(surface, (local.x, local.y))
            })
            .map(|(surface, rect)| (surface, rect.loc))
    }

    /// Give a press at the pointer to the X server when it belongs to an
    /// X11 client's pointer grab although no Xwayland surface is under
    /// it (see the module documentation). That is a press that would
    /// land on a native window or the bare desktop (not on a shell
    /// surface, a popup or an X11 window, which the X server hears of
    /// anyway) while an unmanaged window shows and a client holds the
    /// grab. The pointer enters the topmost unmanaged window's surface
    /// at the position it really has, outside the surface: Xwayland
    /// turns it into the root position and the X server delivers the
    /// press by the grab's own rules. Smithay's implicit grab then sends
    /// the release the same way. The press is the grab's alone, as on an
    /// X server: it neither focuses nor reaches what is under it.
    /// Returns whether the press was taken.
    pub(super) fn x11_grab_press(&mut self, state: &mut State, button: u32, time: u32) -> bool {
        if !self.x11_unmanaged_on_screen() || self.grab.is_some() || state.popup_grab_active() {
            return false;
        }
        let Some(pointer) = self.pointer.clone() else {
            return false;
        };
        if pointer.is_grabbed() {
            return false;
        }
        let pos = self.pointer_pos;
        if self.chrome_at(state, pos).is_some()
            || self.popup_at(state, pos).is_some()
            || self.window_at(pos).is_some_and(|id| self.is_x11(id))
        {
            return false;
        }
        let Some((surface, rect)) = self.x11_unmanaged_entries().pop() else {
            return false;
        };
        match state.xwm.as_ref().map(|xwm| xwm.pointer_grabbed()) {
            Some(Ok(true)) => {}
            Some(Ok(false)) | None => return false,
            Some(Err(error)) => {
                eprintln!("tuna-compositor: xwayland: pointer grab query failed: {error}");
                return false;
            }
        }
        pointer.motion(
            state,
            Some((surface, rect.loc.to_f64())),
            &MotionEvent {
                location: pos,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.button(
            state,
            &ButtonEvent {
                serial: SERIAL_COUNTER.next_serial(),
                time,
                button,
                state: ButtonState::Pressed,
            },
        );
        pointer.frame(state);
        // The release ends Smithay's grab; the pointer then goes back to
        // what is under it.
        self.pointer_scene_dirty.set(true);
        true
    }

    /// The unmanaged windows as the state file lists them, bottom to
    /// top: `(X11 window id, rectangle, owner, shown, type)`.
    #[allow(clippy::type_complexity)]
    pub fn x11_unmanaged_state(
        &self,
    ) -> Vec<(
        u32,
        Rectangle<i32, Logical>,
        Option<u64>,
        bool,
        &'static str,
    )> {
        let visible: Vec<u64> = self
            .visible_entries()
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        self.x11_unmanaged
            .iter()
            .filter(|surface| surface.alive() && surface.is_override_redirect())
            .map(|surface| {
                let owner = self.x11_unmanaged_owner(surface);
                (
                    surface.window_id(),
                    surface.geometry(),
                    owner,
                    surface.wl_surface().is_some()
                        && self.x11_unmanaged_on_screen()
                        && self.x11_unmanaged_shows(owner, &visible),
                    type_name(surface.window_type()),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{client_owner, restack, transient_owner};

    #[test]
    fn a_menu_belongs_to_the_window_it_is_transient_for() {
        // GTK sets WM_TRANSIENT_FOR on a menu to its window.
        let managed = |xid| (xid == 0x40_0003).then_some(7);
        assert_eq!(transient_owner(Some(0x40_0003), managed, |_| None), Some(7));
        // The window carrying the surface (a frame) resolves the same
        // way: `managed` knows both ids.
        let managed = |xid| matches!(xid, 0x40_0003 | 0x20_0010).then_some(7);
        assert_eq!(transient_owner(Some(0x20_0010), managed, |_| None), Some(7));
    }

    #[test]
    fn a_submenu_belongs_to_its_menu_s_window() {
        // Submenu → menu → window, the menus both unmanaged.
        let managed = |xid| (xid == 0x40_0003).then_some(7);
        let parent = |xid| match xid {
            0x40_0476 => Some(0x40_0003),
            0x40_04b0 => Some(0x40_0476),
            _ => None,
        };
        assert_eq!(transient_owner(Some(0x40_04b0), managed, parent), Some(7));
    }

    #[test]
    fn a_window_transient_for_nothing_managed_has_no_transient_owner() {
        assert_eq!(transient_owner(None, |_| Some(1), |_| None), None);
        assert_eq!(transient_owner(Some(5), |_| None, |_| None), None);
        // A cycle a client built ends.
        assert_eq!(
            transient_owner(Some(5), |_| None, |xid| Some(if xid == 5 { 6 } else { 5 })),
            None
        );
    }

    #[test]
    fn a_client_s_focused_window_owns_before_its_topmost() {
        // Bottom to top.
        assert_eq!(client_owner(&[3, 9, 4], Some(9)), Some(9));
        // The focus is another client's: the client's topmost window.
        assert_eq!(client_owner(&[3, 9, 4], Some(1)), Some(4));
        assert_eq!(client_owner(&[3, 9, 4], None), Some(4));
        assert_eq!(client_owner(&[], Some(9)), None);
    }

    #[test]
    fn a_raise_above_another_unmanaged_window_reorders() {
        // A submenu raised above its menu, which was mapped later.
        let mut order = vec![10, 20, 30];
        restack(&mut order, 10, Some(30));
        assert_eq!(order, [20, 30, 10]);
        // Directly above the middle one.
        restack(&mut order, 10, Some(20));
        assert_eq!(order, [20, 10, 30]);
    }

    #[test]
    fn a_plain_move_keeps_the_order() {
        // ConfigureNotify names the sibling below on every move: the one
        // already below it, a managed window's wrapper, or nothing.
        let mut order = vec![10, 20, 30];
        restack(&mut order, 30, Some(20));
        assert_eq!(order, [10, 20, 30]);
        restack(&mut order, 20, Some(0x20_0001));
        assert_eq!(order, [10, 20, 30]);
        restack(&mut order, 10, None);
        assert_eq!(order, [10, 20, 30]);
        // Itself, or a window that is not listed: nothing.
        restack(&mut order, 20, Some(20));
        restack(&mut order, 99, Some(10));
        assert_eq!(order, [10, 20, 30]);
    }
}
