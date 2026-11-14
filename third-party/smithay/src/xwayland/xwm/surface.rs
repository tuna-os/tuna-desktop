use crate::{
    backend::input::KeyState,
    input::{
        keyboard::{KeyboardTarget, KeysymHandle, ModifiersState},
        pointer::{
            AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
            GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
            GestureSwipeUpdateEvent, MotionEvent, PointerTarget, RelativeMotionEvent,
        },
        touch::TouchTarget,
        Seat, SeatHandler,
    },
    utils::{user_data::UserDataMap, Client, IsAlive, Logical, Point, Rectangle, Serial, Size},
    wayland::compositor,
};
use atomic_float::AtomicF64;
use encoding_rs::WINDOWS_1252;
use std::{
    collections::HashSet,
    sync::{atomic::Ordering, Arc, Mutex, Weak},
};
use tracing::warn;
use wayland_server::protocol::wl_surface::WlSurface;

use x11rb::{
    connection::Connection as _,
    errors::ReplyOrIdError,
    properties::{WmClass, WmHints, WmSizeHints},
    protocol::{
        res::{query_client_ids, ClientIdSpec},
        xproto::{
            Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ColormapAlloc, ConfigureWindowAux,
            ConnectionExt as _, CreateWindowAux, EventMask, InputFocus, MapState, PropMode, StackMode,
            Window as X11Window, WindowClass,
        },
    },
    rust_connection::{ConnectionError, RustConnection},
    wrapper::ConnectionExt,
    COPY_DEPTH_FROM_PARENT,
};

use super::{send_configure_notify, X11Wm, XwmId};

/// X11 window managed by an [`X11Wm`](super::X11Wm)
#[derive(Debug, Clone)]
pub struct X11Surface {
    xwm: Option<XwmId>,
    client_scale: Option<Arc<AtomicF64>>,
    window: X11Window,
    /// Root window of the managing [`X11Wm`]: where a window that loses its
    /// frame is wrapped again.
    root: Option<X11Window>,
    conn: Weak<RustConnection>,
    atoms: super::Atoms,
    pub(crate) state: Arc<Mutex<SharedSurfaceState>>,
    user_data: Arc<UserDataMap>,
}

const MWM_HINTS_FLAGS_FIELD: usize = 0;
const MWM_HINTS_DECORATIONS_FIELD: usize = 2;
const MWM_HINTS_DECORATIONS: u32 = 1 << 1;

/// Border widths in the order the X11 frame-extents properties
/// (`_NET_FRAME_EXTENTS`, `_GTK_FRAME_EXTENTS`, `_MUTTER_FRAME_EXTENTS`)
/// list them: left, right, top, bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FrameBorder {
    /// Width of the left border
    pub left: i32,
    /// Width of the right border
    pub right: i32,
    /// Height of the top border
    pub top: i32,
    /// Height of the bottom border
    pub bottom: i32,
}

impl FrameBorder {
    /// Largest border accepted from a client: X11 window sizes are 16 bit.
    const MAX: u32 = u16::MAX as u32;

    /// Parse a frame-extents property value. Only a value of exactly four
    /// CARDINALs counts, as in Mutter's `meta_frame_query_borders`
    /// (`nitems == 4`); anything else is no border at all.
    pub fn from_cardinals(values: &[u32]) -> Option<Self> {
        let &[left, right, top, bottom] = values else {
            return None;
        };
        Some(Self {
            left: left.min(Self::MAX) as i32,
            right: right.min(Self::MAX) as i32,
            top: top.min(Self::MAX) as i32,
            bottom: bottom.min(Self::MAX) as i32,
        })
    }

    /// The property value for these borders: left, right, top, bottom.
    pub fn to_cardinals(self) -> [u32; 4] {
        [
            self.left.max(0) as u32,
            self.right.max(0) as u32,
            self.top.max(0) as u32,
            self.bottom.max(0) as u32,
        ]
    }

    /// Whether every border is zero.
    pub fn is_zero(self) -> bool {
        self == Self::default()
    }

    /// Borders read from the X server (client pixels) in logical pixels,
    /// as Mutter's `meta_window_protocol_to_stage_size`.
    pub fn to_logical(self, client_scale: f64) -> Self {
        let scale = |value: i32| (f64::from(value) / client_scale).round() as i32;
        Self {
            left: scale(self.left),
            right: scale(self.right),
            top: scale(self.top),
            bottom: scale(self.bottom),
        }
    }
}

/// The borders of a framed X11 window, as Mutter's `MetaFrameBorders`.
///
/// Three rectangles describe such a window:
///
///  - the *frame rectangle*: what the user sees as the window, title bar
///    included (Mutter's `meta_window_get_frame_rect`);
///  - the *buffer rectangle*: the X11 window carrying the Wayland surface
///    (the frame window), which is the frame rectangle grown by the
///    [`invisible`](Self::invisible) borders (shadows, resize handles);
///  - the *client rectangle*: the application's own window, which is the
///    frame rectangle shrunk by the [`visible`](Self::visible) borders.
///
/// The same arithmetic serves a client-decorated window without a frame:
/// its own `_GTK_FRAME_EXTENTS` are the invisible borders, no border is
/// visible, and the client is the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FrameExtents {
    /// Decoration drawn inside the frame rectangle (`_MUTTER_FRAME_EXTENTS`
    /// on the frame window).
    pub visible: FrameBorder,
    /// Part of the buffer outside the frame rectangle (`_GTK_FRAME_EXTENTS`).
    pub invisible: FrameBorder,
}

impl FrameExtents {
    /// Visible plus invisible borders: the client's inset in the frame
    /// window (Mutter's `borders.total`).
    pub fn total(&self) -> FrameBorder {
        FrameBorder {
            left: self.visible.left + self.invisible.left,
            right: self.visible.right + self.invisible.right,
            top: self.visible.top + self.invisible.top,
            bottom: self.visible.bottom + self.invisible.bottom,
        }
    }

    /// The buffer rectangle of a frame rectangle: `frame->rect` in
    /// Mutter's `meta_window_x11_move_resize_internal`.
    pub fn frame_rect_to_buffer_rect<Kind>(&self, rect: Rectangle<i32, Kind>) -> Rectangle<i32, Kind> {
        grow(rect, self.invisible)
    }

    /// The frame rectangle of a buffer rectangle
    /// (`meta_window_x11_surface_rect_to_frame_rect`).
    pub fn buffer_rect_to_frame_rect<Kind>(&self, rect: Rectangle<i32, Kind>) -> Rectangle<i32, Kind> {
        shrink(rect, self.invisible)
    }

    /// The buffer rectangle of a frame put around a window that filled
    /// `bare` (its bare wrapper). The client keeps its size and the frame
    /// rectangle starts where the bare window did: the title bar goes in
    /// above the client. Mutter does not resize a client for its frame
    /// either (`meta_window_x11_set_frame_xwindow` adopts it at offset
    /// 0,0 and the queued move-resize grows the frame around it).
    pub fn bare_rect_to_buffer_rect<Kind>(&self, bare: Rectangle<i32, Kind>) -> Rectangle<i32, Kind> {
        self.frame_rect_to_buffer_rect(Rectangle::new(bare.loc, grow(bare, self.visible).size))
    }

    /// The rectangle of the bare wrapper a window goes back to when its
    /// frame of `buffer` goes: the frame rectangle's origin and the
    /// client's size (`meta_window_destroy_frame` reparents the client to
    /// the frame's origin past the invisible border and leaves its size).
    pub fn buffer_rect_to_bare_rect<Kind>(&self, buffer: Rectangle<i32, Kind>) -> Rectangle<i32, Kind> {
        let frame = self.buffer_rect_to_frame_rect(buffer);
        Rectangle::new(frame.loc, shrink(frame, self.visible).size)
    }

    /// The client rectangle of a framed window's frame rectangle
    /// (`meta_window_frame_rect_to_client_rect` with a frame).
    pub fn frame_rect_to_client_rect<Kind>(&self, rect: Rectangle<i32, Kind>) -> Rectangle<i32, Kind> {
        shrink(rect, self.visible)
    }

    /// The frame rectangle of a framed window's client rectangle
    /// (`meta_window_client_rect_to_frame_rect` with a frame).
    pub fn client_rect_to_frame_rect<Kind>(&self, rect: Rectangle<i32, Kind>) -> Rectangle<i32, Kind> {
        grow(rect, self.visible)
    }

    /// Where the client sits inside a frame window of `buffer_size`,
    /// relative to the frame window's origin: Mutter's `frame->child_x`,
    /// `frame->child_y` and the client size it configures.
    pub fn child_rect<Kind>(&self, buffer_size: Size<i32, Kind>) -> Rectangle<i32, Kind> {
        let total = self.total();
        Rectangle::new(
            (total.left, total.top).into(),
            (
                (buffer_size.w - total.left - total.right).max(1),
                (buffer_size.h - total.top - total.bottom).max(1),
            )
                .into(),
        )
    }

    /// The value of the client's `_NET_FRAME_EXTENTS`: the visible borders
    /// (Mutter's `update_net_frame_extents`).
    pub fn net_frame_extents(&self) -> [u32; 4] {
        self.visible.to_cardinals()
    }

    /// These extents in logical pixels.
    pub fn to_logical(self, client_scale: f64) -> Self {
        Self {
            visible: self.visible.to_logical(client_scale),
            invisible: self.invisible.to_logical(client_scale),
        }
    }
}

fn read_border(conn: &RustConnection, window: X11Window, atom: Atom) -> Result<FrameBorder, ConnectionError> {
    let reply = match conn
        .get_property(false, window, atom, AtomEnum::CARDINAL, 0, 4)?
        .reply_unchecked()
    {
        Ok(Some(reply)) => reply,
        Ok(None) | Err(ConnectionError::ParseError(_)) => return Ok(FrameBorder::default()),
        Err(err) => return Err(err),
    };
    Ok(reply
        .value32()
        .map(|values| values.collect::<Vec<_>>())
        .and_then(|values| FrameBorder::from_cardinals(&values))
        .unwrap_or_default())
}

/// Mutter's `meta_frame_query_borders`: invisible borders from the frame's
/// `_GTK_FRAME_EXTENTS`, visible ones from its `_MUTTER_FRAME_EXTENTS`.
fn read_frame_extents(
    conn: &RustConnection,
    atoms: &super::Atoms,
    frame: X11Window,
) -> Result<FrameExtents, ConnectionError> {
    Ok(FrameExtents {
        invisible: read_border(conn, frame, atoms._GTK_FRAME_EXTENTS)?,
        visible: read_border(conn, frame, atoms._MUTTER_FRAME_EXTENTS)?,
    })
}

fn grow<Kind>(rect: Rectangle<i32, Kind>, border: FrameBorder) -> Rectangle<i32, Kind> {
    Rectangle::new(
        Point::from((rect.loc.x - border.left, rect.loc.y - border.top)),
        (
            (rect.size.w + border.left + border.right).max(1),
            (rect.size.h + border.top + border.bottom).max(1),
        )
            .into(),
    )
}

fn shrink<Kind>(rect: Rectangle<i32, Kind>, border: FrameBorder) -> Rectangle<i32, Kind> {
    Rectangle::new(
        Point::from((rect.loc.x + border.left, rect.loc.y + border.top)),
        (
            (rect.size.w - border.left - border.right).max(1),
            (rect.size.h - border.top - border.bottom).max(1),
        )
            .into(),
    )
}

/// Create the bare wrapper a window without a frames-client frame is
/// reparented into. Returns the sequence number of the `CreateWindow`
/// request.
///
/// The wrapper is the X11 window Xwayland gives the Wayland surface, so
/// it takes the client's depth and visual: a client with an alpha channel
/// (a client-decorated GTK window draws its shadow there) keeps it. In a
/// wrapper of the root's depth the X server composites such a client
/// without alpha, black where it is transparent. A window of another
/// depth than its parent needs a colormap of its visual and a border
/// pixel; the client's own colormap serves.
pub(super) fn create_wrapper(
    conn: &RustConnection,
    root: X11Window,
    wrapper: X11Window,
    client: X11Window,
    depth: u8,
    rect: Rectangle<i32, Client>,
) -> Result<u64, ReplyOrIdError> {
    let aux = CreateWindowAux::new().event_mask(
        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT | EventMask::PROPERTY_CHANGE,
    );
    let root_depth = conn
        .setup()
        .roots
        .iter()
        .find(|screen| screen.root == root)
        .map(|screen| screen.root_depth);
    let mut own_colormap = None;
    let (depth, visual, aux) = if Some(depth) == root_depth {
        (COPY_DEPTH_FROM_PARENT, x11rb::COPY_FROM_PARENT, aux)
    } else {
        let attributes = conn.get_window_attributes(client)?.reply()?;
        let colormap = if attributes.colormap == x11rb::NONE {
            let colormap = conn.generate_id()?;
            conn.create_colormap(ColormapAlloc::NONE, colormap, root, attributes.visual)?;
            own_colormap = Some(colormap);
            colormap
        } else {
            attributes.colormap
        };
        (depth, attributes.visual, aux.colormap(colormap).border_pixel(0))
    };
    let cookie = conn.create_window(
        depth,
        wrapper,
        root,
        rect.loc.x as i16,
        rect.loc.y as i16,
        rect.size.w as u16,
        rect.size.h as u16,
        0,
        WindowClass::INPUT_OUTPUT,
        visual,
        &aux,
    )?;
    if let Some(colormap) = own_colormap {
        // The window keeps drawing in its true-colour visual.
        conn.free_colormap(colormap)?;
    }
    Ok(cookie.sequence_number())
}

/// Whether a client message naming `target` is addressed to the window
/// `client` whose frames-client frame is `frame`.
///
/// GTK sends `_NET_WM_STATE`, `WM_CHANGE_STATE`, `_NET_WM_MOVERESIZE` and
/// `_GTK_SHOW_WINDOW_MENU` for the title bar's own window, the frame;
/// Mutter resolves either window to the one `MetaWindow`
/// (`meta_x11_display_register_x_window` for both ids).
pub fn addresses_window(target: X11Window, client: X11Window, frame: Option<X11Window>) -> bool {
    target == client || frame == Some(target)
}

/// The two `CARD32` of a managed window's ICCCM `WM_STATE` property
/// (section 4.1.3.1): `IconicState` or `NormalState`, and no icon window
/// (Mutter's `set_wm_state_on_xwindow`: "Mutter doesn't use icon windows,
/// so data\[1\] should be None").
pub fn wm_state_property(iconic: bool) -> [u32; 2] {
    const NORMAL_STATE: u32 = 1;
    const ICONIC_STATE: u32 = 3;
    [if iconic { ICONIC_STATE } else { NORMAL_STATE }, x11rb::NONE]
}

fn connection_error(error: ReplyOrIdError) -> ConnectionError {
    match error {
        ReplyOrIdError::ConnectionError(error) => error,
        ReplyOrIdError::IdsExhausted | ReplyOrIdError::X11Error(_) => ConnectionError::UnknownError,
    }
}

#[derive(Debug)]
pub(crate) struct SharedSurfaceState {
    pub(super) alive: bool,
    pub(super) wl_surface_id: Option<u32>,
    pub(super) wl_surface_serial: Option<u64>,
    pub(super) mapped_onto: Option<X11Window>,
    pub(super) geometry: Rectangle<i32, Logical>,
    pub(super) override_redirect: bool,
    /// The compositor wants a frames-client frame (`_MUTTER_NEEDS_FRAME`).
    pub(super) needs_frame: bool,
    /// `mapped_onto` is a frames-client frame, not the bare wrapper.
    pub(super) framed: bool,
    /// The frame's borders in client pixels; zero unless `framed`.
    frame_extents: FrameExtents,
    /// The window's own `_GTK_FRAME_EXTENTS` in client pixels.
    gtk_frame_extents: FrameBorder,
    /// `set_mapped(true)` was the last request.
    mapped: bool,
    /// UnmapNotify events still to come from reparents by this window
    /// manager (Mutter's `unmaps_pending`): not withdrawals.
    pub(super) unmaps_pending: u32,

    // The associated wl_surface.
    pub(crate) wl_surface: Option<WlSurface>,

    title: String,
    class: String,
    instance: String,
    startup_id: Option<String>,
    pid: Option<u32>,
    protocols: Protocols,
    hints: Option<WmHints>,
    normal_hints: Option<WmSizeHints>,
    transient_for: Option<X11Window>,
    net_state: HashSet<Atom>,
    motif_hints: Vec<u32>,
    window_type: Vec<Atom>,
}

pub(super) type Protocols = Vec<WMProtocol>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum WMProtocol {
    TakeFocus,
    DeleteWindow,
}

/// https://x.org/releases/X11R7.6/doc/xorg-docs/specs/ICCCM/icccm.html#input_focus
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum InputMode {
    None,
    Passive,
    LocallyActive,
    GloballyActive,
}

impl PartialEq for X11Surface {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        let self_alive = self.state.lock().unwrap().alive;
        let other_alive = other.state.lock().unwrap().alive;
        self.xwm == other.xwm && self.window == other.window && self_alive && other_alive
    }
}

/// Errors that can happen for operations on an [`X11Surface`]
#[derive(Debug, thiserror::Error)]
pub enum X11SurfaceError {
    /// Error on the underlying X11 Connection
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    /// Operation was unsupported for an override_redirect window
    #[error("Operation was unsupported for an override_redirect window")]
    UnsupportedForOverrideRedirect,
}

/// Window types of [`X11Surface`]s
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum WmWindowType {
    Desktop,
    Dock,
    Combo,
    Dnd,
    DropdownMenu,
    Dialog,
    Menu,
    Notification,
    Normal,
    PopupMenu,
    Splash,
    Toolbar,
    Tooltip,
    Utility,
}

/// Window properties of [`X11Surface`]s
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum WmWindowProperty {
    Title,
    Class,
    Protocols,
    Hints,
    NormalHints,
    TransientFor,
    WindowType,
    MotifHints,
    StartupId,
    Pid,
}

impl X11Surface {
    /// Create a new [`X11Surface`] usually handled by an [`X11Wm`](super::X11Wm)
    ///
    /// ## Arguments
    ///
    /// - `window` X11 window id
    /// - `override_redirect` set if the X11 window has the override redirect flag set
    /// - `conn` Weak reference on the X11 connection
    /// - `atoms` Atoms struct as defined by the [xwm module](super).
    /// - `geometry` Initial geometry of the window
    pub fn new(
        xwm: Option<&X11Wm>,
        window: u32,
        override_redirect: bool,
        conn: Weak<RustConnection>,
        atoms: super::Atoms,
        geometry: Rectangle<i32, Logical>,
    ) -> X11Surface {
        X11Surface {
            xwm: xwm.map(|wm| wm.id),
            client_scale: xwm.map(|wm| wm.client_scale.clone()),
            window,
            root: xwm.map(|wm| wm.screen.root),
            conn,
            atoms,
            state: Arc::new(Mutex::new(SharedSurfaceState {
                alive: true,
                wl_surface_id: None,
                wl_surface_serial: None,
                wl_surface: None,
                mapped_onto: None,
                geometry,
                override_redirect,
                needs_frame: false,
                framed: false,
                frame_extents: FrameExtents::default(),
                gtk_frame_extents: FrameBorder::default(),
                mapped: false,
                unmaps_pending: 0,
                title: String::from(""),
                class: String::from(""),
                instance: String::from(""),
                startup_id: None,
                pid: None,
                protocols: Vec::new(),
                hints: None,
                normal_hints: None,
                transient_for: None,
                net_state: HashSet::new(),
                motif_hints: vec![0; 5],
                window_type: Vec::new(),
            })),
            user_data: Arc::new(UserDataMap::new()),
        }
    }

    /// Returns the id of the X11Wm responsible for this surface, if any
    pub fn xwm_id(&self) -> Option<XwmId> {
        self.xwm
    }

    /// X11 protocol id of the underlying window
    pub fn window_id(&self) -> X11Window {
        self.window
    }

    /// X11 protocol id of the reparented window, if any
    ///
    /// This is the frames-client frame for a framed window (see
    /// [`X11Surface::frame_window_id`]) and the window manager's own bare
    /// wrapper otherwise.
    pub fn mapped_window_id(&self) -> Option<X11Window> {
        self.state.lock().unwrap().mapped_onto
    }

    fn scale(&self) -> f64 {
        self.client_scale
            .as_ref()
            .map(|s| s.load(Ordering::Acquire))
            .unwrap_or(1.)
    }

    /// Ask for, or stop asking for, a server-side frame from the frames
    /// client, with Mutter's `_MUTTER_NEEDS_FRAME` protocol.
    ///
    /// `true` is Mutter's `meta_window_ensure_frame`: the property is set
    /// on the window once it is managed (from its `MapRequest` on), a
    /// frames client answers with a frame window carrying
    /// `_MUTTER_FRAME_FOR`, and the window manager adopts that frame when
    /// it asks to be mapped ([`XwmHandler::frame_changed`](super::XwmHandler::frame_changed)).
    /// Without a frames client nothing answers and the window stays in its
    /// bare wrapper.
    ///
    /// `false` is `meta_window_destroy_frame`: the window leaves its frame
    /// for a bare wrapper before the property is deleted, because the
    /// frames client destroys its frame window on that deletion.
    ///
    /// It is an error to call this function on override redirect windows
    pub fn set_needs_frame(&self, needs_frame: bool) -> Result<(), X11SurfaceError> {
        if self.is_override_redirect() {
            return Err(X11SurfaceError::UnsupportedForOverrideRedirect);
        }
        let Some(conn) = self.conn.upgrade() else {
            return Ok(());
        };
        let (managed, framed) = {
            let mut state = self.state.lock().unwrap();
            state.needs_frame = needs_frame;
            (state.mapped_onto.is_some(), state.framed)
        };
        if needs_frame {
            if managed {
                self.announce_needs_frame(&conn)?;
            }
        } else if framed {
            self.unframe().map_err(connection_error)?;
        } else {
            conn.delete_property(self.window, self.atoms._MUTTER_NEEDS_FRAME)?;
        }
        conn.flush()?;
        Ok(())
    }

    /// Returns whether a frame was asked for with [`X11Surface::set_needs_frame`]
    pub fn needs_frame(&self) -> bool {
        self.state.lock().unwrap().needs_frame
    }

    /// Returns whether the window sits in a frames-client frame
    pub fn is_framed(&self) -> bool {
        self.state.lock().unwrap().framed
    }

    /// X11 protocol id of the frames-client frame, if the window has one
    pub fn frame_window_id(&self) -> Option<X11Window> {
        let state = self.state.lock().unwrap();
        state.mapped_onto.filter(|_| state.framed)
    }

    /// Returns whether a client message naming `target` is addressed to
    /// this window: its own id or its frame's (see [`addresses_window`])
    pub fn is_message_target(&self, target: X11Window) -> bool {
        addresses_window(target, self.window, self.frame_window_id())
    }

    /// Returns the borders around this window's frame rectangle in logical
    /// pixels.
    ///
    /// For a framed window these are the frame's `_MUTTER_FRAME_EXTENTS`
    /// (visible) and `_GTK_FRAME_EXTENTS` (invisible), as Mutter's
    /// `meta_frame_query_borders` reads them. Without a frame nothing is
    /// visible and the window's own `_GTK_FRAME_EXTENTS`, if any, are the
    /// invisible borders (Mutter's custom frame extents of a
    /// client-decorated window).
    ///
    /// [`X11Surface::geometry`] and [`X11Surface::configure`] deal in the
    /// buffer rectangle; see [`FrameExtents`] for the conversions.
    pub fn frame_extents(&self) -> FrameExtents {
        let scale = self.scale();
        let state = self.state.lock().unwrap();
        let extents = if state.framed {
            state.frame_extents
        } else {
            FrameExtents {
                visible: FrameBorder::default(),
                invisible: state.gtk_frame_extents,
            }
        };
        extents.to_logical(scale)
    }

    /// Mutter's `meta_window_ensure_frame`.
    pub(super) fn announce_needs_frame(&self, conn: &RustConnection) -> Result<(), ConnectionError> {
        conn.change_property32(
            PropMode::REPLACE,
            self.window,
            self.atoms._MUTTER_NEEDS_FRAME,
            AtomEnum::CARDINAL,
            &[1],
        )?;
        Ok(())
    }

    /// Adopt `frame`, a frames-client window announced with
    /// `_MUTTER_FRAME_FOR` naming this window, as Mutter's
    /// `meta_window_x11_set_frame_xwindow` does: select the window
    /// manager's events on it, reparent the client into it at the extents
    /// offset and map it. The bare wrapper is destroyed; the frame takes
    /// its stacking position and role as [`X11Surface::mapped_window_id`],
    /// so Xwayland's `WL_SURFACE_SERIAL` for the frame associates the
    /// Wayland surface. The client keeps its size: the buffer rectangle
    /// ([`X11Surface::geometry`]) grows by the frame's borders
    /// ([`FrameExtents::bare_rect_to_buffer_rect`]).
    ///
    /// Returns false, leaving everything as it was, unless this is a
    /// managed window that wants a frame and has none (Mutter's
    /// `window->decorated && !meta_window_x11_is_ssd (window)`).
    pub(super) fn adopt_frame(&self, frame: X11Window) -> Result<bool, ReplyOrIdError> {
        let Some(conn) = self.conn.upgrade() else {
            return Ok(false);
        };
        let scale = self.scale();
        let mut state = self.state.lock().unwrap();
        let Some(wrapper) = state.mapped_onto else {
            return Ok(false);
        };
        if !state.alive
            || !state.needs_frame
            || state.framed
            || state.override_redirect
            || frame == self.window
        {
            return Ok(false);
        }

        let extents = read_frame_extents(&conn, &self.atoms, frame)?;
        let rect = extents.bare_rect_to_buffer_rect(state.geometry.to_client_precise_round(scale));
        let child = extents.child_rect(rect.size);
        {
            let _guard = scopeguard::guard((), |_| {
                let _ = conn.ungrab_server();
            });
            conn.grab_server()?;
            // Mutter's EVENT_MASK also selects StructureNotify and
            // FocusChange; this window manager already hears a root
            // child's structure events through the root window, as for
            // its bare wrapper, and selecting them twice would deliver
            // every one of them twice.
            conn.change_window_attributes(
                frame,
                &ChangeWindowAttributesAux::new().event_mask(
                    EventMask::SUBSTRUCTURE_NOTIFY
                        | EventMask::SUBSTRUCTURE_REDIRECT
                        | EventMask::PROPERTY_CHANGE,
                ),
            )?;
            let mapped = conn.get_window_attributes(self.window)?.reply()?.map_state != MapState::UNMAPPED;
            conn.configure_window(
                frame,
                &ConfigureWindowAux::new()
                    .x(rect.loc.x)
                    .y(rect.loc.y)
                    .width(rect.size.w as u32)
                    .height(rect.size.h as u32)
                    .border_width(0)
                    .sibling(wrapper)
                    .stack_mode(StackMode::ABOVE),
            )?;
            conn.reparent_window(self.window, frame, child.loc.x as i16, child.loc.y as i16)?;
            if mapped {
                // "the reparent will unmap the window, we don't want to
                // take that as a withdraw"
                state.unmaps_pending += 1;
            }
            // A managed client stays mapped inside what it is mapped
            // onto, whose own map state shows or hides it. The X server
            // maps a mapped window again after a reparent; one it handed
            // back unmapped (see `unframe`) is mapped here.
            conn.map_window(self.window)?;
            conn.configure_window(
                self.window,
                &ConfigureWindowAux::new()
                    .width(child.size.w as u32)
                    .height(child.size.h as u32)
                    .border_width(0),
            )?;
            conn.destroy_window(wrapper)?;
        }

        state.mapped_onto = Some(frame);
        state.framed = true;
        state.frame_extents = extents;
        state.geometry = rect.to_f64().to_logical(scale).to_i32_round();
        // The wrapper's Wayland surface went with it; Xwayland announces
        // the frame's own once it is mapped.
        state.wl_surface = None;
        state.wl_surface_serial = None;
        state.wl_surface_id = None;

        conn.change_property32(
            PropMode::REPLACE,
            self.window,
            self.atoms._NET_FRAME_EXTENTS,
            AtomEnum::CARDINAL,
            &extents.net_frame_extents(),
        )?;
        // window-x11.c meta_window_x11_set_net_wm_state: the frame carries
        // the client's _NET_WM_STATE, from which GTK draws it focused,
        // maximized or without a title bar (fullscreen).
        let net_state = Vec::from_iter(state.net_state.iter().copied());
        conn.change_property32(
            PropMode::REPLACE,
            frame,
            self.atoms._NET_WM_STATE,
            AtomEnum::ATOM,
            &net_state,
        )?;
        send_configure_notify(
            &conn,
            &self.window,
            Rectangle::new(rect.loc + child.loc, child.size),
            false,
        )?;
        if state.mapped {
            conn.map_window(frame)?;
        }
        conn.flush()?;
        Ok(true)
    }

    /// Take the window out of its frames-client frame into a fresh bare
    /// wrapper, as Mutter's `meta_window_destroy_frame` returns it to the
    /// root window: the client is reparented before `_MUTTER_NEEDS_FRAME`
    /// is deleted, since the frames client destroys its frame window on
    /// that deletion. Also the recovery when the frame went away by itself
    /// (the frames client died and the X server's save-set returned the
    /// client to the root). The client keeps its size: the buffer
    /// rectangle shrinks to it ([`FrameExtents::buffer_rect_to_bare_rect`]).
    /// The wish for a frame is kept, for
    /// [`X11Wm::request_frames`](super::X11Wm::request_frames).
    pub(super) fn unframe(&self) -> Result<(), ReplyOrIdError> {
        let Some(conn) = self.conn.upgrade() else {
            return Ok(());
        };
        let scale = self.scale();
        let mut state = self.state.lock().unwrap();
        if !state.framed {
            return Ok(());
        }
        let frame = state.mapped_onto.take();
        let extents = std::mem::take(&mut state.frame_extents);
        state.framed = false;
        state.wl_surface = None;
        state.wl_surface_serial = None;
        state.wl_surface_id = None;
        let Some(root) = self.root else {
            return Ok(());
        };
        // The client may have gone with its frame: nothing to wrap then.
        let Some(attributes) = conn.get_window_attributes(self.window)?.reply_unchecked()? else {
            return Ok(());
        };
        let Some(depth) = conn
            .get_geometry(self.window)?
            .reply_unchecked()?
            .map(|geometry| geometry.depth)
        else {
            return Ok(());
        };

        let rect = extents.buffer_rect_to_bare_rect(state.geometry.to_client_precise_round(scale));
        let wrapper = conn.generate_id()?;
        {
            let _guard = scopeguard::guard((), |_| {
                let _ = conn.ungrab_server();
            });
            conn.grab_server()?;
            create_wrapper(&conn, root, wrapper, self.window, depth, rect)?;
            if let Some(frame) = frame {
                conn.configure_window(
                    wrapper,
                    &ConfigureWindowAux::new()
                        .sibling(frame)
                        .stack_mode(StackMode::ABOVE),
                )?;
            }
            conn.reparent_window(self.window, wrapper, 0, 0)?;
            if attributes.map_state != MapState::UNMAPPED {
                state.unmaps_pending += 1;
            }
            // When the frames client dies, the X server's save-set
            // reparents the client to the root window and maps it again,
            // which the root's SubstructureRedirect turns into a
            // MapRequest for a window that is already managed: nobody
            // would map it. A managed client is mapped inside its wrapper.
            conn.map_window(self.window)?;
            conn.configure_window(
                self.window,
                &ConfigureWindowAux::new()
                    .width(rect.size.w as u32)
                    .height(rect.size.h as u32)
                    .border_width(0),
            )?;
            conn.delete_property(self.window, self.atoms._MUTTER_NEEDS_FRAME)?;
            conn.change_property32(
                PropMode::REPLACE,
                self.window,
                self.atoms._NET_FRAME_EXTENTS,
                AtomEnum::CARDINAL,
                &[0, 0, 0, 0],
            )?;
        }
        state.mapped_onto = Some(wrapper);
        state.geometry = rect.to_f64().to_logical(scale).to_i32_round();
        send_configure_notify(&conn, &self.window, rect, false)?;
        if state.mapped {
            conn.map_window(wrapper)?;
        }
        conn.flush()?;
        Ok(())
    }

    /// The client was withdrawn (unmapped by its owner) and returned to
    /// the root: drop what it was mapped onto. The bare wrapper is this
    /// window manager's and is destroyed; a frame belongs to the frames
    /// client, which destroys it when `_MUTTER_NEEDS_FRAME` goes
    /// (`meta_window_destroy_frame` on unmanage).
    pub(super) fn withdraw_mapped_onto(&self, conn: &RustConnection) -> Result<(), ConnectionError> {
        let mut state = self.state.lock().unwrap();
        let Some(onto) = state.mapped_onto.take() else {
            return Ok(());
        };
        if state.framed {
            state.framed = false;
            state.frame_extents = FrameExtents::default();
            conn.change_property32(
                PropMode::REPLACE,
                self.window,
                self.atoms._NET_FRAME_EXTENTS,
                AtomEnum::CARDINAL,
                &[0, 0, 0, 0],
            )?;
        } else {
            conn.destroy_window(onto)?;
        }
        // A new map request decides again.
        state.needs_frame = false;
        state.mapped = false;
        conn.delete_property(self.window, self.atoms._MUTTER_NEEDS_FRAME)?;
        Ok(())
    }

    /// Whether the X server took this framed window out of its frame:
    /// the frames client died and its save-set returned the client to the
    /// root window, which also unmaps and maps it.
    pub(super) fn lost_frame(&self, conn: &RustConnection) -> Result<bool, ConnectionError> {
        if !self.is_framed() {
            return Ok(false);
        }
        Ok(conn
            .query_tree(self.window)?
            .reply_unchecked()?
            .is_some_and(|tree| tree.parent == tree.root))
    }

    /// Re-read the frame's borders after a `_MUTTER_FRAME_EXTENTS` or
    /// `_GTK_FRAME_EXTENTS` change on it (Mutter's
    /// `meta_frame_handle_xevent`), keep the client at the new offset and
    /// publish `_NET_FRAME_EXTENTS`. Returns whether they changed.
    ///
    /// The buffer rectangle stays and the client is resized inside it,
    /// until the compositor lays the window out again. A window whose map
    /// was not granted yet has no layout: it keeps the client size it
    /// asked for, and the buffer rectangle follows the borders.
    pub(super) fn update_frame_extents(&self) -> Result<bool, ConnectionError> {
        let Some(conn) = self.conn.upgrade() else {
            return Ok(false);
        };
        let Some(frame) = self.frame_window_id() else {
            return Ok(false);
        };
        let extents = read_frame_extents(&conn, &self.atoms, frame)?;
        let scale = self.scale();
        let rect = {
            let mut state = self.state.lock().unwrap();
            if !state.framed || state.frame_extents == extents {
                return Ok(false);
            }
            let old = std::mem::replace(&mut state.frame_extents, extents);
            (!state.mapped).then(|| {
                let bare = old.buffer_rect_to_bare_rect(state.geometry.to_client_precise_round(scale));
                extents
                    .bare_rect_to_buffer_rect(bare)
                    .to_f64()
                    .to_logical(scale)
                    .to_i32_round()
            })
        };
        conn.change_property32(
            PropMode::REPLACE,
            self.window,
            self.atoms._NET_FRAME_EXTENTS,
            AtomEnum::CARDINAL,
            &extents.net_frame_extents(),
        )?;
        match self.configure(rect) {
            Ok(()) | Err(X11SurfaceError::UnsupportedForOverrideRedirect) => Ok(true),
            Err(X11SurfaceError::Connection(error)) => Err(error),
        }
    }

    /// Re-read the window's own `_GTK_FRAME_EXTENTS`. Returns whether they
    /// changed.
    pub(super) fn update_gtk_frame_extents(&self) -> Result<bool, ConnectionError> {
        let conn = self.conn.upgrade().ok_or(ConnectionError::UnknownError)?;
        let border = read_border(&conn, self.window, self.atoms._GTK_FRAME_EXTENTS)?;
        let mut state = self.state.lock().unwrap();
        let changed = state.gtk_frame_extents != border;
        state.gtk_frame_extents = border;
        Ok(changed)
    }

    /// Answer the frame's own `ConfigureRequest` as Mutter's
    /// `send_configure_notify` in meta-x11-frame.c does: "We never get
    /// told by the frames client, just reassert the current frame size."
    pub(super) fn notify_frame_geometry(&self) -> Result<(), ConnectionError> {
        let Some(conn) = self.conn.upgrade() else {
            return Ok(());
        };
        let scale = self.scale();
        let state = self.state.lock().unwrap();
        if let Some(frame) = state.mapped_onto.filter(|_| state.framed) {
            let rect = state.geometry.to_client_precise_round(scale);
            send_configure_notify(&conn, &frame, rect, false)?;
        }
        Ok(())
    }

    /// Set the X11 windows as mapped/unmapped affecting its visibility.
    ///
    /// It is an error to call this function on override redirect windows
    pub fn set_mapped(&self, mapped: bool) -> Result<(), X11SurfaceError> {
        if self.is_override_redirect() {
            return Err(X11SurfaceError::UnsupportedForOverrideRedirect);
        }

        if let Some(conn) = self.conn.upgrade() {
            let mapped_onto = {
                let mut state = self.state.lock().unwrap();
                state.mapped = mapped;
                state.mapped_onto
            };
            if let Some(frame) = mapped_onto {
                if mapped {
                    let property = [1u32 /*NormalState*/, 0 /*WINDOW_NONE*/];
                    conn.change_property32(
                        PropMode::REPLACE,
                        self.window,
                        self.atoms.WM_STATE,
                        self.atoms.WM_STATE,
                        &property,
                    )?;
                    conn.map_window(frame)?;
                } else {
                    let property = [3u32 /*IconicState*/, 0 /*WINDOW_NONE*/];
                    conn.change_property32(
                        PropMode::REPLACE,
                        self.window,
                        self.atoms.WM_STATE,
                        self.atoms.WM_STATE,
                        &property,
                    )?;
                    conn.unmap_window(frame)?;
                }
                conn.flush()?;
            }
        }
        Ok(())
    }

    /// Returns if this window has the override redirect flag set or not
    pub fn is_override_redirect(&self) -> bool {
        self.state.lock().unwrap().override_redirect
    }

    /// Returns if the window is currently mapped or not
    pub fn is_mapped(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.wl_surface.is_some()
    }

    /// Returns if the window is still alive
    #[inline]
    pub fn alive(&self) -> bool {
        self.state.lock().unwrap().alive && self.conn.strong_count() != 0
    }

    /// Send a configure to this window.
    ///
    /// If `rect` is provided the new state will be send to the window.
    /// If `rect` is `None` a synthetic configure event with the existing state will be send.
    pub fn configure(&self, rect: impl Into<Option<Rectangle<i32, Logical>>>) -> Result<(), X11SurfaceError> {
        let rect = rect.into();
        if self.is_override_redirect() && rect.is_some() {
            return Err(X11SurfaceError::UnsupportedForOverrideRedirect);
        }

        if let Some(conn) = self.conn.upgrade() {
            let mut state = self.state.lock().unwrap();
            let client_scale = self
                .client_scale
                .as_ref()
                .map(|s| s.load(Ordering::Acquire))
                .unwrap_or(1.);
            let logical_rect = rect.unwrap_or(state.geometry);
            let rect = logical_rect.to_client_precise_round(client_scale);
            let aux = ConfigureWindowAux::default()
                .x(rect.loc.x)
                .y(rect.loc.y)
                .width(rect.size.w as u32)
                .height(rect.size.h as u32)
                .border_width(0);
            if let Some(frame) = state.mapped_onto {
                // `rect` is the window carrying the Wayland surface. The
                // bare wrapper is filled by the client; a frames-client
                // frame keeps the client at its extents offset
                // (`frame->child_x`, `frame->child_y`), smaller by the
                // borders.
                let (win_aux, client) = if state.framed {
                    let child = state.frame_extents.child_rect(rect.size);
                    (
                        ConfigureWindowAux::default()
                            .x(child.loc.x)
                            .y(child.loc.y)
                            .width(child.size.w as u32)
                            .height(child.size.h as u32)
                            .border_width(0),
                        Rectangle::new(rect.loc + child.loc, child.size),
                    )
                } else {
                    (
                        ConfigureWindowAux::default()
                            .width(rect.size.w as u32)
                            .height(rect.size.h as u32)
                            .border_width(0),
                        rect,
                    )
                };
                conn.configure_window(frame, &aux)?;
                conn.configure_window(self.window, &win_aux)?;
                send_configure_notify(&conn, &self.window, client, false)?;
            } else {
                conn.configure_window(self.window, &aux)?;
            }
            conn.flush()?;

            state.geometry = logical_rect;
        }
        Ok(())
    }

    /// Returns the associated wl_surface.
    ///
    /// This will only return `Some` once:
    ///   - The `WL_SURFACE_SERIAL` has been set on the x11 window, and
    ///   - The wl_surface has been assigned the same serial using the [xwayland
    ///     shell](crate::wayland::xwayland_shell) protocol on the wayland side,
    ///     and then committed.
    #[inline]
    pub fn wl_surface(&self) -> Option<WlSurface> {
        self.state.lock().unwrap().wl_surface.clone()
    }

    /// Returns the associated `wl_surface` id, once it has been set by
    /// xwayland.
    ///
    /// Note that XWayland will only set this if it was unable to bind the
    /// [xwayland shell](crate::wayland::xwayland_shell) protocol on the wayland
    /// side.
    #[deprecated = "Since XWayland 23.1, the recommended approach is to use [wl_surface_serial] and the [xwayland shell](crate::wayland::xwayland_shell) protocol on the wayland side to match X11 windows."]
    pub fn wl_surface_id(&self) -> Option<u32> {
        self.state.lock().unwrap().wl_surface_id
    }

    /// Returns the associated `wl_surface` serial, once it has been set by
    /// xwayland.
    ///
    /// XWayland will set this if it has bound the [xwayland
    /// shell](crate::wayland::xwayland_shell) protocol on the wayland side.
    /// Otherwise, it will set [`wl_surface_id`][Self::wl_surface_id] instead.
    pub fn wl_surface_serial(&self) -> Option<u64> {
        self.state.lock().unwrap().wl_surface_serial
    }

    /// Returns the current geometry of the underlying X11 window
    pub fn geometry(&self) -> Rectangle<i32, Logical> {
        self.state.lock().unwrap().geometry
    }

    /// Returns the current title of the underlying X11 window
    pub fn title(&self) -> String {
        self.state.lock().unwrap().title.clone()
    }

    /// Returns the current window class of the underlying X11 window
    pub fn class(&self) -> String {
        self.state.lock().unwrap().class.clone()
    }

    /// Returns the current window instance of the underlying X11 window
    pub fn instance(&self) -> String {
        self.state.lock().unwrap().instance.clone()
    }

    /// Returns the startup id of the underlying X11 window
    pub fn startup_id(&self) -> Option<String> {
        self.state.lock().unwrap().startup_id.clone()
    }

    /// Returns the PID of the underlying X11 window
    pub fn pid(&self) -> Option<u32> {
        self.state.lock().unwrap().pid
    }

    /// Returns if the window is considered to be a popup.
    ///
    /// Corresponds to the internal `_NET_WM_STATE_MODAL` state of the underlying X11 window.
    pub fn is_popup(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.net_state.contains(&self.atoms._NET_WM_STATE_MODAL)
    }

    /// Returns if the underlying window is transient to another window.
    ///
    /// This might be used as a hint to manage windows in a group.
    pub fn is_transient_for(&self) -> Option<X11Window> {
        self.state.lock().unwrap().transient_for
    }

    /// Returns the hints for the underlying X11 window
    pub fn hints(&self) -> Option<WmHints> {
        self.state.lock().unwrap().hints
    }

    /// Returns the size hints for the underlying X11 window
    pub fn size_hints(&self) -> Option<WmSizeHints> {
        self.state.lock().unwrap().normal_hints
    }

    /// Returns the suggested minimum size of the underlying X11 window
    pub fn min_size(&self) -> Option<Size<i32, Logical>> {
        let client_scale = self
            .client_scale
            .as_ref()
            .map(|s| s.load(Ordering::Acquire))
            .unwrap_or(1.);
        let state = self.state.lock().unwrap();
        state
            .normal_hints
            .as_ref()
            .and_then(|hints| hints.min_size)
            .map(Size::<i32, Client>::from)
            .map(|s| s.to_f64().to_logical(client_scale).to_i32_round())
    }

    /// Returns the suggested minimum size of the underlying X11 window
    pub fn max_size(&self) -> Option<Size<i32, Logical>> {
        let client_scale = self
            .client_scale
            .as_ref()
            .map(|s| s.load(Ordering::Acquire))
            .unwrap_or(1.);
        let state = self.state.lock().unwrap();
        state
            .normal_hints
            .as_ref()
            .and_then(|hints| hints.max_size)
            .map(Size::<i32, Client>::from)
            .map(|s| s.to_f64().to_logical(client_scale).to_i32_round())
    }

    /// Returns the suggested base size of the underlying X11 window
    pub fn base_size(&self) -> Option<Size<i32, Logical>> {
        let client_scale = self
            .client_scale
            .as_ref()
            .map(|s| s.load(Ordering::Acquire))
            .unwrap_or(1.);
        let state = self.state.lock().unwrap();
        let res = state
            .normal_hints
            .as_ref()
            .and_then(|hints| hints.base_size)
            .map(Size::<i32, Client>::from)
            .map(|s| s.to_f64().to_logical(client_scale).to_i32_round());
        std::mem::drop(state);
        res.or_else(|| self.min_size())
    }

    /// Returns if the window is in the maximized state
    pub fn is_maximized(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.net_state.contains(&self.atoms._NET_WM_STATE_MAXIMIZED_HORZ)
            && state.net_state.contains(&self.atoms._NET_WM_STATE_MAXIMIZED_VERT)
    }

    /// Returns if the window is in the fullscreen state
    pub fn is_fullscreen(&self) -> bool {
        self.state
            .lock()
            .unwrap()
            .net_state
            .contains(&self.atoms._NET_WM_STATE_FULLSCREEN)
    }

    /// Returns if the window is in the minimized state
    pub fn is_minimized(&self) -> bool {
        self.state
            .lock()
            .unwrap()
            .net_state
            .contains(&self.atoms._NET_WM_STATE_HIDDEN)
    }

    /// Returns if the window is in the activated state
    pub fn is_activated(&self) -> bool {
        self.state
            .lock()
            .unwrap()
            .net_state
            .contains(&self.atoms._NET_WM_STATE_FOCUSED)
    }

    /// Returns true if the window is client-side decorated
    pub fn is_decorated(&self) -> bool {
        let state = self.state.lock().unwrap();
        if (state.motif_hints[MWM_HINTS_FLAGS_FIELD] & MWM_HINTS_DECORATIONS) != 0 {
            return state.motif_hints[MWM_HINTS_DECORATIONS_FIELD] == 0;
        }
        false
    }

    /// Sets the window as maximized or not.
    ///
    /// Allows the client to reflect this state in their UI.
    pub fn set_maximized(&self, maximized: bool) -> Result<(), ConnectionError> {
        if maximized {
            self.change_net_state(
                &[
                    self.atoms._NET_WM_STATE_MAXIMIZED_HORZ,
                    self.atoms._NET_WM_STATE_MAXIMIZED_VERT,
                ],
                &[],
            )?;
        } else {
            self.change_net_state(
                &[],
                &[
                    self.atoms._NET_WM_STATE_MAXIMIZED_HORZ,
                    self.atoms._NET_WM_STATE_MAXIMIZED_VERT,
                ],
            )?;
        }
        Ok(())
    }

    /// Sets the window as fullscreen or not.
    ///
    /// Allows the client to reflect this state in their UI.
    pub fn set_fullscreen(&self, fullscreen: bool) -> Result<(), ConnectionError> {
        if fullscreen {
            self.change_net_state(&[self.atoms._NET_WM_STATE_FULLSCREEN], &[])?;
        } else {
            self.change_net_state(&[], &[self.atoms._NET_WM_STATE_FULLSCREEN])?;
        }
        Ok(())
    }

    /// Sets the window as suspended/hidden or not.
    ///
    /// Allows the client to e.g. stop rendering.
    pub fn set_suspended(&self, suspended: bool) -> Result<(), ConnectionError> {
        if suspended {
            self.change_net_state(&[self.atoms._NET_WM_STATE_HIDDEN], &[])?;
        } else {
            self.change_net_state(&[], &[self.atoms._NET_WM_STATE_HIDDEN])?;
        }
        Ok(())
    }

    /// Marks a mapped window minimized or restored without unmapping it,
    /// for a compositor that keeps drawing a minimized window's surface
    /// (in an overview, a switcher, the minimize animation).
    ///
    /// Mutter's `meta_window_x11_set_wm_state` and
    /// `meta_window_x11_set_net_wm_state`: `WM_STATE` on the client is
    /// `IconicState` while the window is hidden and `NormalState` again
    /// when it shows, and `_NET_WM_STATE` gains or loses
    /// `_NET_WM_STATE_HIDDEN`, on the client and on its frame. That is how
    /// an X11 client knows to pause what nobody sees, and how a pager
    /// shows the window's state. A window that is not mapped keeps the
    /// `WM_STATE` [`set_mapped`](Self::set_mapped) gave it.
    pub fn set_minimized(&self, minimized: bool) -> Result<(), ConnectionError> {
        let managed = {
            let state = self.state.lock().unwrap();
            state.mapped && state.mapped_onto.is_some() && !state.override_redirect
        };
        if managed {
            let conn = self.conn.upgrade().ok_or(ConnectionError::UnknownError)?;
            conn.change_property32(
                PropMode::REPLACE,
                self.window,
                self.atoms.WM_STATE,
                self.atoms.WM_STATE,
                &wm_state_property(minimized),
            )?;
            conn.flush()?;
        }
        self.set_suspended(minimized)
    }

    /// Sets the window as activated or not.
    ///
    /// Allows the client to reflect this state in their UI.
    pub fn set_activated(&self, activated: bool) -> Result<(), ConnectionError> {
        if activated {
            self.change_net_state(&[self.atoms._NET_WM_STATE_FOCUSED], &[])?;
        } else {
            self.change_net_state(&[], &[self.atoms._NET_WM_STATE_FOCUSED])?;
        }
        Ok(())
    }

    /// The X11 half of keyboard focus, per the window's ICCCM input mode:
    /// `SetInputFocus` to this window (Passive, LocallyActive) and/or
    /// `WM_TAKE_FOCUS` (LocallyActive, GloballyActive); unfocusing sets the
    /// X input focus to None. [`KeyboardTarget`] for `X11Surface` does this
    /// on enter/leave; compositors whose keyboard focus is the window's
    /// `wl_surface` call it directly, as Mutter's
    /// meta_x11_display_set_input_focus does for X11 windows.
    pub fn set_input_focus(&self, focused: bool) -> Result<(), ConnectionError> {
        let mode = self.input_mode();
        if mode == InputMode::None || self.is_override_redirect() {
            return Ok(());
        }
        let Some(conn) = self.conn.upgrade() else {
            return Ok(());
        };
        if !focused {
            conn.set_input_focus(InputFocus::NONE, x11rb::NONE, x11rb::CURRENT_TIME)?;
            conn.flush()?;
            return Ok(());
        }
        if matches!(mode, InputMode::Passive | InputMode::LocallyActive) {
            conn.set_input_focus(InputFocus::NONE, self.window, x11rb::CURRENT_TIME)?;
        }
        if matches!(mode, InputMode::LocallyActive | InputMode::GloballyActive) {
            let event = ClientMessageEvent::new(
                32,
                self.window,
                self.atoms.WM_PROTOCOLS,
                [self.atoms.WM_TAKE_FOCUS, x11rb::CURRENT_TIME, 0, 0, 0],
            );
            conn.send_event(false, self.window, EventMask::NO_EVENT, event)?;
        }
        conn.flush()?;
        Ok(())
    }

    /// Returns the reported window type of the underlying X11 window if set.
    ///
    /// Windows without a window type set should be considered to be of type `Normal` for
    /// backwards compatibility.
    pub fn window_type(&self) -> Option<WmWindowType> {
        self.state
            .lock()
            .unwrap()
            .window_type
            .iter()
            .find_map(|atom| match atom {
                x if *x == self.atoms._NET_WM_WINDOW_TYPE_DESKTOP => Some(WmWindowType::Desktop),
                x if *x == self.atoms._NET_WM_WINDOW_TYPE_DOCK => Some(WmWindowType::Dock),
                x if *x == self.atoms._NET_WM_WINDOW_TYPE_COMBO => Some(WmWindowType::Combo),
                x if *x == self.atoms._NET_WM_WINDOW_TYPE_DND => Some(WmWindowType::Dnd),
                x if *x == self.atoms._NET_WM_WINDOW_TYPE_DROPDOWN_MENU => Some(WmWindowType::DropdownMenu),
                x if *x == self.atoms._NET_WM_WINDOW_TYPE_DIALOG => Some(WmWindowType::Dialog),
                x if *x == self.atoms._NET_WM_WINDOW_TYPE_MENU => Some(WmWindowType::Menu),
                x if *x == self.atoms._NET_WM_WINDOW_TYPE_NOTIFICATION => Some(WmWindowType::Notification),
                x if *x == self.atoms._NET_WM_WINDOW_TYPE_NORMAL => Some(WmWindowType::Normal),
                x if *x == self.atoms._NET_WM_WINDOW_TYPE_POPUP_MENU => Some(WmWindowType::PopupMenu),
                x if *x == self.atoms._NET_WM_WINDOW_TYPE_SPLASH => Some(WmWindowType::Splash),
                x if *x == self.atoms._NET_WM_WINDOW_TYPE_TOOLBAR => Some(WmWindowType::Toolbar),
                x if *x == self.atoms._NET_WM_WINDOW_TYPE_TOOLTIP => Some(WmWindowType::Tooltip),
                x if *x == self.atoms._NET_WM_WINDOW_TYPE_UTILITY => Some(WmWindowType::Utility),
                _ => None,
            })
    }

    fn change_net_state(&self, added: &[Atom], removed: &[Atom]) -> Result<(), ConnectionError> {
        let mut state = self.state.lock().unwrap();

        let mut changed = false;
        for atom in removed {
            changed |= state.net_state.remove(atom);
        }
        for atom in added {
            changed |= state.net_state.insert(*atom);
        }

        if changed {
            let new_props = Vec::from_iter(state.net_state.iter().copied());

            let conn = self.conn.upgrade().ok_or(ConnectionError::UnknownError)?;
            conn.grab_server()?;
            let _guard = scopeguard::guard((), |_| {
                let _ = conn.ungrab_server();
                let _ = conn.flush();
            });

            conn.change_property32(
                PropMode::REPLACE,
                self.window,
                self.atoms._NET_WM_STATE,
                AtomEnum::ATOM,
                &new_props,
            )?;
            // window-x11.c meta_window_x11_set_net_wm_state writes the
            // same atoms to the frame window: GTK reads its own window's
            // state to draw the frame focused, maximized or fullscreen.
            if let Some(frame) = state.mapped_onto.filter(|_| state.framed) {
                conn.change_property32(
                    PropMode::REPLACE,
                    frame,
                    self.atoms._NET_WM_STATE,
                    AtomEnum::ATOM,
                    &new_props,
                )?;
            }
        }

        Ok(())
    }

    fn input_mode(&self) -> InputMode {
        let state = self.state.lock().unwrap();
        match (
            state.hints.as_ref().and_then(|hints| hints.input).unwrap_or(true),
            state.protocols.contains(&WMProtocol::TakeFocus),
        ) {
            (false, false) => InputMode::None,
            (true, false) => InputMode::Passive, // the default
            (true, true) => InputMode::LocallyActive,
            (false, true) => InputMode::GloballyActive,
        }
    }

    pub(super) fn update_properties(&self) -> Result<(), ConnectionError> {
        self.update_title()?;
        self.update_class()?;
        self.update_protocols()?;
        self.update_hints()?;
        self.update_normal_hints()?;
        self.update_transient_for()?;
        // NET_WM_STATE is managed by the WM, we don't need to update it unless explicitly asked to
        self.update_net_window_type()?;
        self.update_motif_hints()?;
        self.update_startup_id()?;
        self.update_pid()?;
        self.update_gtk_frame_extents()?;
        Ok(())
    }

    pub(super) fn update_property(&self, atom: Atom) -> Result<Option<WmWindowProperty>, ConnectionError> {
        match atom {
            atom if atom == self.atoms._NET_WM_NAME || atom == AtomEnum::WM_NAME.into() => {
                self.update_title()?;
                Ok(Some(WmWindowProperty::Title))
            }
            atom if atom == AtomEnum::WM_CLASS.into() => {
                self.update_class()?;
                Ok(Some(WmWindowProperty::Class))
            }
            atom if atom == self.atoms.WM_PROTOCOLS => {
                self.update_protocols()?;
                Ok(Some(WmWindowProperty::Protocols))
            }
            atom if atom == self.atoms.WM_HINTS => {
                self.update_hints()?;
                Ok(Some(WmWindowProperty::Hints))
            }
            atom if atom == AtomEnum::WM_NORMAL_HINTS.into() => {
                self.update_normal_hints()?;
                Ok(Some(WmWindowProperty::NormalHints))
            }
            atom if atom == AtomEnum::WM_TRANSIENT_FOR.into() => {
                self.update_transient_for()?;
                Ok(Some(WmWindowProperty::TransientFor))
            }
            atom if atom == self.atoms._NET_WM_WINDOW_TYPE => {
                self.update_net_window_type()?;
                Ok(Some(WmWindowProperty::WindowType))
            }
            atom if atom == self.atoms._MOTIF_WM_HINTS => {
                self.update_motif_hints()?;
                Ok(Some(WmWindowProperty::MotifHints))
            }
            atom if atom == self.atoms._NET_STARTUP_ID => {
                self.update_startup_id()?;
                Ok(Some(WmWindowProperty::StartupId))
            }
            atom if atom == self.atoms._NET_WM_PID => {
                self.update_pid()?;
                Ok(Some(WmWindowProperty::Pid))
            }

            _ => Ok(None), // unknown
        }
    }

    fn update_class(&self) -> Result<(), ConnectionError> {
        let conn = self.conn.upgrade().ok_or(ConnectionError::UnknownError)?;
        let (class, instance) = match WmClass::get(&*conn, self.window)?.reply_unchecked() {
            Ok(Some(wm_class)) => (
                WINDOWS_1252.decode(wm_class.class()).0.to_string(),
                WINDOWS_1252.decode(wm_class.instance()).0.to_string(),
            ),
            Ok(None) | Err(ConnectionError::ParseError(_)) => (Default::default(), Default::default()), // Getting the property failed
            Err(err) => return Err(err),
        };

        let mut state = self.state.lock().unwrap();
        state.class = class;
        state.instance = instance;

        Ok(())
    }

    fn update_hints(&self) -> Result<(), ConnectionError> {
        let conn = self.conn.upgrade().ok_or(ConnectionError::UnknownError)?;
        let mut state = self.state.lock().unwrap();
        state.hints = match WmHints::get(&*conn, self.window)?.reply_unchecked() {
            Ok(hints) => hints,
            Err(ConnectionError::ParseError(_)) => None,
            Err(err) => return Err(err),
        };
        Ok(())
    }

    fn update_normal_hints(&self) -> Result<(), ConnectionError> {
        let conn = self.conn.upgrade().ok_or(ConnectionError::UnknownError)?;
        let mut state = self.state.lock().unwrap();
        state.normal_hints = match WmSizeHints::get_normal_hints(&*conn, self.window)?.reply_unchecked() {
            Ok(hints) => hints,
            Err(ConnectionError::ParseError(_)) => None,
            Err(err) => return Err(err),
        };
        Ok(())
    }

    fn update_motif_hints(&self) -> Result<(), ConnectionError> {
        let conn = self.conn.upgrade().ok_or(ConnectionError::UnknownError)?;
        let Some(hints) = (match conn
            .get_property(
                false,
                self.window,
                self.atoms._MOTIF_WM_HINTS,
                AtomEnum::ANY,
                0,
                2048,
            )?
            .reply_unchecked()
        {
            Ok(Some(reply)) => reply.value32().map(|vals| vals.collect::<Vec<_>>()),
            Ok(None) | Err(ConnectionError::ParseError(_)) => return Ok(()),
            Err(err) => return Err(err),
        }) else {
            return Ok(());
        };

        if hints.len() < 5 {
            return Ok(());
        }

        let mut state = self.state.lock().unwrap();
        state.motif_hints = hints;
        Ok(())
    }

    fn update_startup_id(&self) -> Result<(), ConnectionError> {
        if let Some(startup_id) = self.read_window_property_string(self.atoms._NET_STARTUP_ID)? {
            let mut state = self.state.lock().unwrap();
            state.startup_id = Some(startup_id);
        }
        Ok(())
    }

    fn update_pid(&self) -> Result<(), ConnectionError> {
        if let Some(pid) = self.read_window_property_u32(self.atoms._NET_WM_PID)? {
            let mut state = self.state.lock().unwrap();
            state.pid = Some(pid);
        }
        Ok(())
    }

    fn update_protocols(&self) -> Result<(), ConnectionError> {
        let conn = self.conn.upgrade().ok_or(ConnectionError::UnknownError)?;
        let Some(protocols) = (match conn
            .get_property(
                false,
                self.window,
                self.atoms.WM_PROTOCOLS,
                AtomEnum::ATOM,
                0,
                2048,
            )?
            .reply_unchecked()
        {
            Ok(Some(reply)) => reply.value32().map(|vals| vals.collect::<Vec<_>>()),
            Ok(None) | Err(ConnectionError::ParseError(_)) => return Ok(()),
            Err(err) => return Err(err),
        }) else {
            return Ok(());
        };

        let mut state = self.state.lock().unwrap();
        state.protocols = protocols
            .into_iter()
            .filter_map(|atom| match atom {
                x if x == self.atoms.WM_TAKE_FOCUS => Some(WMProtocol::TakeFocus),
                x if x == self.atoms.WM_DELETE_WINDOW => Some(WMProtocol::DeleteWindow),
                _ => None,
            })
            .collect::<Vec<_>>();
        Ok(())
    }

    fn update_transient_for(&self) -> Result<(), ConnectionError> {
        let conn = self.conn.upgrade().ok_or(ConnectionError::UnknownError)?;
        let reply = match conn
            .get_property(
                false,
                self.window,
                AtomEnum::WM_TRANSIENT_FOR,
                AtomEnum::WINDOW,
                0,
                2048,
            )?
            .reply_unchecked()
        {
            Ok(Some(reply)) => reply,
            Ok(None) | Err(ConnectionError::ParseError(_)) => return Ok(()),
            Err(err) => return Err(err),
        };
        let window = reply
            .value32()
            .and_then(|mut iter| iter.next())
            .filter(|w| *w != 0);

        let mut state = self.state.lock().unwrap();
        state.transient_for = window;
        Ok(())
    }

    fn update_title(&self) -> Result<(), ConnectionError> {
        let title = self
            .read_window_property_string(self.atoms._NET_WM_NAME)?
            .or(self.read_window_property_string(AtomEnum::WM_NAME)?)
            .unwrap_or_default();

        let mut state = self.state.lock().unwrap();
        state.title = title;
        Ok(())
    }

    fn update_net_window_type(&self) -> Result<(), ConnectionError> {
        let conn = self.conn.upgrade().ok_or(ConnectionError::UnknownError)?;
        let atoms = match conn
            .get_property(
                false,
                self.window,
                self.atoms._NET_WM_WINDOW_TYPE,
                AtomEnum::ATOM,
                0,
                1024,
            )?
            .reply_unchecked()
        {
            Ok(atoms) => atoms,
            Err(ConnectionError::ParseError(_)) => return Ok(()),
            Err(err) => return Err(err),
        };

        let mut state = self.state.lock().unwrap();
        state.window_type = atoms
            .and_then(|atoms| Some(atoms.value32()?.collect::<Vec<_>>()))
            .unwrap_or_default();
        Ok(())
    }

    fn read_window_property_string(&self, atom: impl Into<Atom>) -> Result<Option<String>, ConnectionError> {
        let conn = self.conn.upgrade().ok_or(ConnectionError::UnknownError)?;
        let reply = match conn
            .get_property(false, self.window, atom, AtomEnum::ANY, 0, 2048)?
            .reply_unchecked()
        {
            Ok(Some(reply)) => reply,
            Ok(None) | Err(ConnectionError::ParseError(_)) => return Ok(None),
            Err(err) => return Err(err),
        };
        let Some(bytes) = reply.value8() else {
            return Ok(None);
        };
        let bytes = bytes.collect::<Vec<u8>>();

        match reply.type_ {
            x if x == AtomEnum::STRING.into() => Ok(Some(WINDOWS_1252.decode(&bytes).0.to_string())),
            x if x == self.atoms.UTF8_STRING => Ok(String::from_utf8(bytes).ok()),
            _ => Ok(None),
        }
    }

    fn read_window_property_u32(&self, atom: impl Into<Atom>) -> Result<Option<u32>, ConnectionError> {
        let conn = self.conn.upgrade().ok_or(ConnectionError::UnknownError)?;
        let reply = match conn
            .get_property(false, self.window, atom, AtomEnum::CARDINAL, 0, 1)?
            .reply_unchecked()
        {
            Ok(Some(reply)) => reply,
            Ok(None) | Err(ConnectionError::ParseError(_)) => return Ok(None),
            Err(err) => return Err(err),
        };

        if let Some(mut value32) = reply.value32() {
            if let Some(value) = value32.next() {
                return Ok(Some(value));
            }
        }

        Ok(None)
    }

    /// Retrieve user_data associated with this X11 window
    pub fn user_data(&self) -> &UserDataMap {
        &self.user_data
    }

    /// Send a close request to this window.
    ///
    /// Will outright destroy windows that don't support the `NET_DELETE_WINDOW` protocol.
    pub fn close(&self) -> Result<(), ConnectionError> {
        let conn = self.conn.upgrade().ok_or(ConnectionError::UnknownError)?;
        let state = self.state.lock().unwrap();
        if state.protocols.contains(&WMProtocol::DeleteWindow) {
            let event = ClientMessageEvent::new(
                32,
                self.window,
                self.atoms.WM_PROTOCOLS,
                [self.atoms.WM_DELETE_WINDOW, 0, 0, 0, 0],
            );
            conn.send_event(false, self.window, EventMask::NO_EVENT, event)?;
        } else {
            conn.destroy_window(self.window)?;
        }
        conn.flush()
    }

    /// Get the client PID associated with the X11 window.
    pub fn get_client_pid(&self) -> Result<u32, Box<dyn std::error::Error>> {
        if let Some(connection) = self.conn.upgrade() {
            let window = self.window;

            match query_client_ids(
                &connection,
                &[ClientIdSpec {
                    client: window,
                    mask: x11rb::protocol::res::ClientIdMask::LOCAL_CLIENT_PID,
                }],
            ) {
                Ok(cookie) => {
                    let reply = cookie.reply()?;

                    if let Some(id_value) = reply.ids.first() {
                        if let Some(pid) = id_value.value.first().copied() {
                            return Ok(pid);
                        } else {
                            return Err(Box::new(std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                "No matching client ID found",
                            )));
                        }
                    }
                }
                Err(_) => {
                    return Ok(0);
                }
            }
        }
        Ok(0)
    }
}

/// Trait for objects, that represent an x11 window in some shape or form
/// and can be tested for equality.
pub trait X11Relatable {
    /// Returns if this object is considered to represent the same underlying x11 window as provided
    fn is_window(&self, window: &X11Surface) -> bool;
}

impl X11Relatable for X11Surface {
    fn is_window(&self, window: &X11Surface) -> bool {
        self == window
    }
}

impl X11Relatable for WlSurface {
    fn is_window(&self, window: &X11Surface) -> bool {
        let serial = compositor::with_states(self, |states| {
            states
                .cached_state
                .get::<crate::wayland::xwayland_shell::XWaylandShellCachedState>()
                .current()
                .serial
        });

        window.wl_surface_serial() == serial
    }
}

impl IsAlive for X11Surface {
    #[inline]
    fn alive(&self) -> bool {
        self.state.lock().unwrap().alive
    }
}

impl<D: SeatHandler + 'static> KeyboardTarget<D> for X11Surface {
    fn enter(&self, seat: &Seat<D>, data: &mut D, keys: Vec<KeysymHandle<'_>>, serial: Serial) {
        let (set_input_focus, send_take_focus) = match self.input_mode() {
            InputMode::None => return,
            InputMode::Passive => (true, false),
            InputMode::LocallyActive => (true, true),
            InputMode::GloballyActive => (false, true),
        };

        if let Some(conn) = self.conn.upgrade() {
            if set_input_focus {
                if let Err(err) = conn.set_input_focus(InputFocus::NONE, self.window, x11rb::CURRENT_TIME) {
                    warn!("Unable to set focus for X11Surface ({:?}): {}", self.window, err);
                }
            }

            if send_take_focus {
                let event = ClientMessageEvent::new(
                    32,
                    self.window,
                    self.atoms.WM_PROTOCOLS,
                    [self.atoms.WM_TAKE_FOCUS, x11rb::CURRENT_TIME, 0, 0, 0],
                );
                if let Err(err) = conn.send_event(false, self.window, EventMask::NO_EVENT, event) {
                    warn!(
                        "Unable to send take focus event for X11Surface ({:?}): {}",
                        self.window, err
                    );
                }
                let _ = conn.flush();
            }

            let _ = conn.flush();
        }

        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            KeyboardTarget::enter(surface, seat, data, keys, serial);
        }
    }

    fn leave(&self, seat: &Seat<D>, data: &mut D, serial: Serial) {
        if self.input_mode() == InputMode::None {
            return;
        } else if let Some(conn) = self.conn.upgrade() {
            if let Err(err) = conn.set_input_focus(InputFocus::NONE, x11rb::NONE, x11rb::CURRENT_TIME) {
                warn!("Unable to unfocus X11Surface ({:?}): {}", self.window, err);
            }
            let _ = conn.flush();
        }

        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            KeyboardTarget::leave(surface, seat, data, serial);
        }
    }

    fn key(
        &self,
        seat: &Seat<D>,
        data: &mut D,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: u32,
    ) {
        if self.input_mode() == InputMode::None {
            return;
        }

        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            KeyboardTarget::key(surface, seat, data, key, state, serial, time)
        }
    }

    fn modifiers(&self, seat: &Seat<D>, data: &mut D, modifiers: ModifiersState, serial: Serial) {
        if self.input_mode() == InputMode::None {
            return;
        }

        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            KeyboardTarget::modifiers(surface, seat, data, modifiers, serial);
        }
    }
}

impl<D: SeatHandler + 'static> PointerTarget<D> for X11Surface {
    fn enter(&self, seat: &Seat<D>, data: &mut D, event: &MotionEvent) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::enter(surface, seat, data, event);
        }
    }

    fn motion(&self, seat: &Seat<D>, data: &mut D, event: &MotionEvent) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::motion(surface, seat, data, event);
        }
    }

    fn relative_motion(&self, seat: &Seat<D>, data: &mut D, event: &RelativeMotionEvent) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::relative_motion(surface, seat, data, event);
        }
    }

    fn button(&self, seat: &Seat<D>, data: &mut D, event: &ButtonEvent) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::button(surface, seat, data, event);
        }
    }

    fn axis(&self, seat: &Seat<D>, data: &mut D, frame: AxisFrame) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::axis(surface, seat, data, frame);
        }
    }

    fn frame(&self, seat: &Seat<D>, data: &mut D) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::frame(surface, seat, data);
        }
    }

    fn leave(&self, seat: &Seat<D>, data: &mut D, serial: Serial, time: u32) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::leave(surface, seat, data, serial, time);
        }
    }

    fn gesture_swipe_begin(&self, seat: &Seat<D>, data: &mut D, event: &GestureSwipeBeginEvent) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::gesture_swipe_begin(surface, seat, data, event);
        }
    }

    fn gesture_swipe_update(&self, seat: &Seat<D>, data: &mut D, event: &GestureSwipeUpdateEvent) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::gesture_swipe_update(surface, seat, data, event);
        }
    }

    fn gesture_swipe_end(&self, seat: &Seat<D>, data: &mut D, event: &GestureSwipeEndEvent) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::gesture_swipe_end(surface, seat, data, event);
        }
    }

    fn gesture_pinch_begin(&self, seat: &Seat<D>, data: &mut D, event: &GesturePinchBeginEvent) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::gesture_pinch_begin(surface, seat, data, event)
        }
    }

    fn gesture_pinch_update(&self, seat: &Seat<D>, data: &mut D, event: &GesturePinchUpdateEvent) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::gesture_pinch_update(surface, seat, data, event)
        }
    }

    fn gesture_pinch_end(&self, seat: &Seat<D>, data: &mut D, event: &GesturePinchEndEvent) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::gesture_pinch_end(surface, seat, data, event)
        }
    }

    fn gesture_hold_begin(&self, seat: &Seat<D>, data: &mut D, event: &GestureHoldBeginEvent) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::gesture_hold_begin(surface, seat, data, event)
        }
    }

    fn gesture_hold_end(&self, seat: &Seat<D>, data: &mut D, event: &GestureHoldEndEvent) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            PointerTarget::gesture_hold_end(surface, seat, data, event)
        }
    }
}

impl<D: SeatHandler + 'static> TouchTarget<D> for X11Surface {
    fn down(&self, seat: &Seat<D>, data: &mut D, event: &crate::input::touch::DownEvent, seq: Serial) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            TouchTarget::down(surface, seat, data, event, seq)
        }
    }

    fn up(&self, seat: &Seat<D>, data: &mut D, event: &crate::input::touch::UpEvent, seq: Serial) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            TouchTarget::up(surface, seat, data, event, seq)
        }
    }

    fn motion(&self, seat: &Seat<D>, data: &mut D, event: &crate::input::touch::MotionEvent, seq: Serial) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            TouchTarget::motion(surface, seat, data, event, seq)
        }
    }

    fn frame(&self, seat: &Seat<D>, data: &mut D, seq: Serial) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            TouchTarget::frame(surface, seat, data, seq)
        }
    }

    fn cancel(&self, seat: &Seat<D>, data: &mut D, seq: Serial) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            TouchTarget::cancel(surface, seat, data, seq)
        }
    }

    fn shape(&self, seat: &Seat<D>, data: &mut D, event: &crate::input::touch::ShapeEvent, seq: Serial) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            TouchTarget::shape(surface, seat, data, event, seq)
        }
    }

    fn orientation(
        &self,
        seat: &Seat<D>,
        data: &mut D,
        event: &crate::input::touch::OrientationEvent,
        seq: Serial,
    ) {
        if let Some(surface) = self.state.lock().unwrap().wl_surface.as_ref() {
            TouchTarget::orientation(surface, seat, data, event, seq)
        }
    }
}
