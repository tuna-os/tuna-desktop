//! The frames protocol's pure logic: what the property bytes mean and
//! which window an X event concerns. Nothing here talks to the X server,
//! so all of it is unit tested.
//!
//! Each function names the Mutter 51 function it reproduces
//! (`src/frames/meta-frame.c`, `src/frames/meta-window-tracker.c`).

use std::collections::HashMap;

/// An X resource id (window or atom), as Xlib's `XID`.
pub type Xid = std::os::raw::c_ulong;

/// One `XGetWindowProperty` reply, copied out of Xlib's buffer.
///
/// `type_` is 0 (`None`) when the window has no such property. When the
/// property exists with another type than the one requested, the server
/// reports the real type and no items.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Property {
    pub type_: Xid,
    pub format: i32,
    /// The items of a format-32 property; Xlib hands these over as C
    /// `long`s, of which only the low 32 bits travel on the wire.
    pub longs: Vec<u32>,
    /// The bytes of a format-8 property.
    pub bytes: Vec<u8>,
}

/// `listen_set_up_frame`: a window wants a frame when its
/// `_MUTTER_NEEDS_FRAME` (read as `CARDINAL`) has a non-zero first item.
pub fn needs_frame(property: &Property) -> bool {
    property.longs.first().is_some_and(|value| *value != 0)
}

/// `frame_sync_wm_state`: whether the client's `_NET_WM_STATE` lists
/// `_NET_WM_STATE_FULLSCREEN`, in which case the frame starts fullscreen
/// and reports no title bar.
pub fn lists_atom(property: &Property, atom: Xid) -> bool {
    property.longs.iter().any(|item| Xid::from(*item) == atom)
}

/// `get_utf8_string_prop`: the value Mutter takes from `_NET_WM_NAME` or
/// `_NET_WM_VISIBLE_NAME`, requested as `UTF8_STRING`.
///
/// Mutter uses Xlib's buffer as a C string whenever Xlib returned one,
/// and Xlib returns one for every existing property. So an absent
/// property is `None` (the next name in line is used), while a property
/// of another type is an empty title, as in Mutter. The string ends at
/// the first NUL. Mutter does not validate the bytes; here invalid UTF-8
/// is replaced rather than handed to GTK.
pub fn utf8_string(property: &Property) -> Option<String> {
    if property.type_ == 0 {
        return None;
    }
    let end = property
        .bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(property.bytes.len());
    Some(String::from_utf8_lossy(&property.bytes[..end]).into_owned())
}

/// The three names a frame's title is chosen from (`update_frame_title`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Title {
    net_wm_visible_name: Option<String>,
    net_wm_name: Option<String>,
    wm_name: Option<String>,
}

impl Title {
    /// `frame_sync_net_wm_visible_name`.
    pub fn set_net_wm_visible_name(&mut self, name: Option<String>) {
        self.net_wm_visible_name = name;
    }

    /// `frame_sync_net_wm_name`. Mutter drops the visible name here: a
    /// new `_NET_WM_NAME` outdates the name the window manager derived
    /// from the old one, until the window manager writes it again.
    pub fn set_net_wm_name(&mut self, name: Option<String>) {
        self.net_wm_visible_name = None;
        self.net_wm_name = name;
    }

    /// `frame_sync_wm_name`, once `WM_NAME` was read; `None` when the
    /// text does not convert to UTF-8.
    pub fn set_wm_name(&mut self, name: Option<String>) {
        self.wm_name = name;
    }

    /// `update_frame_title`: `_NET_WM_VISIBLE_NAME`, else `_NET_WM_NAME`,
    /// else `WM_NAME`, else empty.
    pub fn text(&self) -> &str {
        self.net_wm_visible_name
            .as_deref()
            .or(self.net_wm_name.as_deref())
            .or(self.wm_name.as_deref())
            .unwrap_or("")
    }
}

const MWM_HINTS_FUNCTIONS: u32 = 1 << 0;
const MWM_FUNC_ALL: u32 = 1 << 0;
const MWM_FUNC_CLOSE: u32 = 1 << 5;

/// Number of `long`s Mutter reads of `_MOTIF_WM_HINTS`: flags, functions
/// and decorations.
pub const MOTIF_WM_HINTS_LONGS: std::os::raw::c_long = 3;

/// `frame_sync_motif_wm_hints`: whether the frame has a close button.
///
/// Without a functions hint the window is deletable. With one,
/// `MWM_FUNC_CLOSE` allows closing, or, when `MWM_FUNC_ALL` is set,
/// forbids it (the listed functions are then the ones removed).
///
/// Mutter reads the flags and functions without checking how many items
/// the property has; a property too short to hold both leaves the window
/// deletable here.
pub fn deletable(motif_wm_hints: &Property) -> bool {
    let (Some(flags), Some(functions)) =
        (motif_wm_hints.longs.first(), motif_wm_hints.longs.get(1))
    else {
        return true;
    };
    if flags & MWM_HINTS_FUNCTIONS == 0 {
        return true;
    }
    if functions & MWM_FUNC_ALL == 0 {
        functions & MWM_FUNC_CLOSE != 0
    } else {
        functions & MWM_FUNC_CLOSE == 0
    }
}

const P_MIN_SIZE: u32 = 1 << 4;
const P_MAX_SIZE: u32 = 1 << 5;

/// `WM_SIZE_HINTS` items before ICCCM 1 added the base size and gravity
/// (Xlib's `OldNumPropSizeElements`): the least `XGetWMNormalHints`
/// accepts.
const OLD_SIZE_HINTS_LONGS: usize = 15;
/// All `WM_SIZE_HINTS` items (Xlib's `NumPropSizeElements`).
pub const SIZE_HINTS_LONGS: std::os::raw::c_long = 18;

/// `frame_sync_wm_normal_hints`: whether the frame can be resized, or
/// `None` when the hints cannot be read and the frame keeps its state.
///
/// `property` is `WM_NORMAL_HINTS` requested as `WM_SIZE_HINTS`
/// (`size_hints_type`). As `XGetWMNormalHints`, this rejects a property
/// of another type or format, or with fewer than 15 items. A window is
/// fixed-size only when it gives both a minimum and a maximum size and
/// they are equal.
pub fn resizable(property: &Property, size_hints_type: Xid) -> Option<bool> {
    if property.type_ != size_hints_type
        || property.format != 32
        || property.longs.len() < OLD_SIZE_HINTS_LONGS
    {
        return None;
    }
    let hints = &property.longs;
    let flags = hints[0];
    let (min_width, min_height, max_width, max_height) = (hints[5], hints[6], hints[7], hints[8]);
    Some(
        flags & P_MIN_SIZE == 0
            || flags & P_MAX_SIZE == 0
            || min_width != max_width
            || min_height != max_height,
    )
}

/// `client_window_has_wm_protocol`: whether `WM_PROTOCOLS` (read as
/// `ATOM`) lists `protocol`.
pub fn has_wm_protocol(wm_protocols: Option<&Property>, protocol: Xid) -> bool {
    wm_protocols.is_some_and(|property| lists_atom(property, protocol))
}

/// The visible borders a frame reports in `_MUTTER_FRAME_EXTENTS`, in
/// device pixels (Mutter keeps them in a `GtkBorder`, hence `i16`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Extents {
    pub left: i16,
    pub right: i16,
    pub top: i16,
    pub bottom: i16,
}

impl Extents {
    /// `meta_frame_new`: before the first allocation the frame reports
    /// its header's minimum height as the top border; none when the
    /// client is fullscreen (`header_height` 0).
    pub fn for_header(header_height: i32, scale: i32) -> Self {
        Self {
            top: (f64::from(header_height) * f64::from(scale)) as i16,
            ..Self::default()
        }
    }

    /// `meta_frame_size_allocate`: the content widget's origin inside
    /// the window, scaled to device pixels, is the left and top border.
    /// Right and bottom are always 0, as in Mutter ("right/bottom are
    /// broken, if they are ever other than 0").
    pub fn for_content_origin(x: f32, y: f32, scale: i32) -> Self {
        Self {
            left: (f64::from(x) * f64::from(scale)) as i16,
            top: (f64::from(y) * f64::from(scale)) as i16,
            ..Self::default()
        }
    }

    /// `update_extents`: the property's four `CARDINAL`s, in the order
    /// the window manager reads them (`meta_frame_query_borders`): left,
    /// right, top, bottom.
    pub fn cardinals(&self) -> [Xid; 4] {
        [self.left, self.right, self.top, self.bottom].map(|border| border as Xid)
    }
}

/// The frames this process made: frame window → frame, and client
/// window → frame window (`MetaWindowTracker`'s `frames` and
/// `client_windows`).
#[derive(Debug)]
pub struct Registry<F> {
    frames: HashMap<Xid, F>,
    clients: HashMap<Xid, Xid>,
}

impl<F> Default for Registry<F> {
    fn default() -> Self {
        Self {
            frames: HashMap::new(),
            clients: HashMap::new(),
        }
    }
}

impl<F> Registry<F> {
    /// Whether `window` is one of this process's frame windows.
    pub fn is_frame(&self, window: Xid) -> bool {
        self.frames.contains_key(&window)
    }

    /// Whether the client `window` has a frame.
    pub fn has_frame(&self, window: Xid) -> bool {
        self.clients.contains_key(&window)
    }

    pub fn insert(&mut self, client: Xid, frame_window: Xid, frame: F) {
        self.frames.insert(frame_window, frame);
        self.clients.insert(client, frame_window);
    }

    /// Forget the client's frame and hand it back for destruction.
    pub fn remove(&mut self, client: Xid) -> Option<F> {
        let frame_window = self.clients.remove(&client)?;
        self.frames.remove(&frame_window)
    }

    /// The frame an event on `window` belongs to: `window` is either a
    /// frame window or a framed client.
    pub fn frame_for(&self, window: Xid) -> Option<&F> {
        self.frames.get(&window).or_else(|| {
            self.clients
                .get(&window)
                .and_then(|frame_window| self.frames.get(frame_window))
        })
    }
}

/// The parts of the X events the tracker reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Create {
        parent: Xid,
        window: Xid,
        override_redirect: bool,
    },
    /// `event` is the window the notification was selected on.
    Configure {
        event: Xid,
        window: Xid,
        override_redirect: bool,
    },
    Destroy {
        window: Xid,
    },
    Property {
        window: Xid,
        atom: Xid,
        deleted: bool,
    },
}

/// What the tracker does about an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Watch the window's properties and frame it if it asks for one.
    Listen(Xid),
    /// The window asks for a frame.
    SetUp(Xid),
    /// The client lost its claim to a frame, or is gone.
    Remove(Xid),
    /// A property a frame mirrors may have changed on this window.
    Sync {
        window: Xid,
        atom: Xid,
    },
    Ignore,
}

/// `on_xevent`'s dispatch, without the XInput part.
///
/// - A new, managed child of the root that is not one of our frames is
///   listened to.
/// - A root child's configure notify is how an override-redirect change
///   shows: a window that became managed is listened to, a framed one
///   that became override-redirect loses its frame.
/// - A destroyed window loses its frame.
/// - `_MUTTER_NEEDS_FRAME` set asks for a frame, deleted removes it.
/// - Any other property change is offered to the window's frame.
pub fn classify<F>(
    event: Event,
    root: Xid,
    needs_frame_atom: Xid,
    registry: &Registry<F>,
) -> Action {
    match event {
        Event::Create {
            parent,
            window,
            override_redirect,
        } if parent == root && !override_redirect && !registry.is_frame(window) => {
            Action::Listen(window)
        }
        Event::Create { .. } => Action::Ignore,
        Event::Configure {
            event,
            window,
            override_redirect,
        } if event == root && window != root && !registry.is_frame(window) => {
            match (override_redirect, registry.has_frame(window)) {
                (false, false) => Action::Listen(window),
                (true, true) => Action::Remove(window),
                _ => Action::Ignore,
            }
        }
        Event::Configure { .. } => Action::Ignore,
        Event::Destroy { window } => Action::Remove(window),
        Event::Property {
            window,
            atom,
            deleted,
        } if atom == needs_frame_atom => {
            if deleted {
                Action::Remove(window)
            } else {
                Action::SetUp(window)
            }
        }
        Event::Property { window, atom, .. } => Action::Sync { window, atom },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: Xid = 0x1d0;
    const NEEDS_FRAME: Xid = 400;
    const CARDINAL: Xid = 6;
    const ATOM: Xid = 4;
    const WM_SIZE_HINTS: Xid = 41;
    const UTF8_STRING: Xid = 300;

    fn longs(type_: Xid, longs: &[u32]) -> Property {
        Property {
            type_,
            format: 32,
            longs: longs.to_vec(),
            bytes: Vec::new(),
        }
    }

    fn bytes(type_: Xid, bytes: &[u8]) -> Property {
        Property {
            type_,
            format: 8,
            longs: Vec::new(),
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn a_frame_is_needed_only_for_a_non_zero_cardinal() {
        assert!(needs_frame(&longs(CARDINAL, &[1])));
        assert!(!needs_frame(&longs(CARDINAL, &[0])));
        // Absent, or of another type: the server returns no items.
        assert!(!needs_frame(&Property::default()));
        assert!(!needs_frame(&longs(ATOM, &[])));
    }

    #[test]
    fn fullscreen_is_found_anywhere_in_the_state_list() {
        let state = longs(ATOM, &[310, 311, 312]);
        assert!(lists_atom(&state, 312));
        assert!(!lists_atom(&state, 313));
        assert!(!lists_atom(&Property::default(), 312));
    }

    #[test]
    fn a_utf8_name_ends_at_the_first_nul() {
        assert_eq!(
            utf8_string(&bytes(UTF8_STRING, "Términal\0junk".as_bytes())).as_deref(),
            Some("Términal")
        );
        assert_eq!(
            utf8_string(&bytes(UTF8_STRING, b"xterm")).as_deref(),
            Some("xterm")
        );
    }

    #[test]
    fn an_absent_name_falls_through_but_a_mistyped_one_is_empty() {
        assert_eq!(utf8_string(&Property::default()), None);
        // `_NET_WM_NAME` written as STRING (31): the server reports the
        // type and withholds the value; Mutter shows an empty title.
        assert_eq!(utf8_string(&bytes(31, b"")).as_deref(), Some(""));
    }

    #[test]
    fn invalid_utf8_never_reaches_the_title() {
        let name = utf8_string(&bytes(UTF8_STRING, b"caf\xe9")).unwrap();
        assert_eq!(name, "caf\u{fffd}");
    }

    #[test]
    fn the_title_prefers_visible_name_then_net_name_then_wm_name() {
        let mut title = Title::default();
        assert_eq!(title.text(), "");
        title.set_wm_name(Some("legacy".into()));
        assert_eq!(title.text(), "legacy");
        title.set_net_wm_name(Some("modern".into()));
        assert_eq!(title.text(), "modern");
        title.set_net_wm_visible_name(Some("modern <2>".into()));
        assert_eq!(title.text(), "modern <2>");
        title.set_net_wm_visible_name(None);
        assert_eq!(title.text(), "modern");
        title.set_net_wm_name(None);
        assert_eq!(title.text(), "legacy");
    }

    #[test]
    fn a_new_net_wm_name_outdates_the_visible_name() {
        let mut title = Title::default();
        title.set_net_wm_name(Some("one".into()));
        title.set_net_wm_visible_name(Some("one (on host)".into()));
        title.set_net_wm_name(Some("two".into()));
        assert_eq!(title.text(), "two");
    }

    #[test]
    fn windows_are_deletable_unless_motif_functions_say_otherwise() {
        const ALL: u32 = 1 << 0;
        const RESIZE: u32 = 1 << 1;
        const CLOSE: u32 = 1 << 5;
        const FUNCTIONS: u32 = 1 << 0;
        const DECORATIONS: u32 = 1 << 1;
        let hints = |flags, functions| longs(500, &[flags, functions, 0]);

        // No property, or no functions hint.
        assert!(deletable(&Property::default()));
        assert!(deletable(&hints(DECORATIONS, 0)));
        // An explicit list: closing must be on it.
        assert!(deletable(&hints(FUNCTIONS, RESIZE | CLOSE)));
        assert!(!deletable(&hints(FUNCTIONS, RESIZE)));
        assert!(!deletable(&hints(FUNCTIONS, 0)));
        // "All", minus the listed ones.
        assert!(deletable(&hints(FUNCTIONS, ALL)));
        assert!(deletable(&hints(FUNCTIONS, ALL | RESIZE)));
        assert!(!deletable(&hints(FUNCTIONS, ALL | CLOSE)));
        // Too short to hold the functions.
        assert!(deletable(&longs(500, &[FUNCTIONS])));
    }

    /// `WM_NORMAL_HINTS` with the given flags and minimum/maximum sizes.
    fn size_hints(flags: u32, min: (u32, u32), max: (u32, u32), items: usize) -> Property {
        let mut hints = vec![0; items];
        hints[0] = flags;
        hints[5] = min.0;
        hints[6] = min.1;
        hints[7] = max.0;
        hints[8] = max.1;
        longs(WM_SIZE_HINTS, &hints)
    }

    #[test]
    fn equal_minimum_and_maximum_sizes_make_a_fixed_size_window() {
        let both = P_MIN_SIZE | P_MAX_SIZE;
        let resizable = |property| resizable(&property, WM_SIZE_HINTS);
        assert_eq!(
            resizable(size_hints(both, (300, 200), (300, 200), 18)),
            Some(false)
        );
        // One dimension free is enough.
        assert_eq!(
            resizable(size_hints(both, (300, 200), (300, 900), 18)),
            Some(true)
        );
        assert_eq!(
            resizable(size_hints(both, (300, 200), (800, 200), 18)),
            Some(true)
        );
        // Equal sizes count only when both hints are given.
        assert_eq!(
            resizable(size_hints(P_MIN_SIZE, (300, 200), (300, 200), 18)),
            Some(true)
        );
        assert_eq!(
            resizable(size_hints(P_MAX_SIZE, (300, 200), (300, 200), 18)),
            Some(true)
        );
        assert_eq!(resizable(size_hints(0, (0, 0), (0, 0), 18)), Some(true));
        // The pre-ICCCM layout is still read.
        assert_eq!(
            resizable(size_hints(both, (64, 64), (64, 64), 15)),
            Some(false)
        );
    }

    #[test]
    fn unreadable_size_hints_leave_the_frame_as_it_was() {
        let both = P_MIN_SIZE | P_MAX_SIZE;
        assert_eq!(resizable(&Property::default(), WM_SIZE_HINTS), None);
        assert_eq!(
            resizable(&size_hints(both, (1, 1), (1, 1), 14), WM_SIZE_HINTS),
            None
        );
        let mut mistyped = size_hints(both, (1, 1), (1, 1), 18);
        mistyped.type_ = CARDINAL;
        assert_eq!(resizable(&mistyped, WM_SIZE_HINTS), None);
        let mut bytes = size_hints(both, (1, 1), (1, 1), 18);
        bytes.format = 8;
        assert_eq!(resizable(&bytes, WM_SIZE_HINTS), None);
    }

    #[test]
    fn close_asks_the_client_only_when_it_takes_part_in_wm_delete_window() {
        let protocols = longs(ATOM, &[350, 351]);
        assert!(has_wm_protocol(Some(&protocols), 351));
        assert!(!has_wm_protocol(Some(&protocols), 352));
        assert!(!has_wm_protocol(Some(&Property::default()), 351));
        assert!(!has_wm_protocol(None, 351));
    }

    #[test]
    fn extents_are_written_left_right_top_bottom() {
        let extents = Extents {
            left: 1,
            right: 2,
            top: 3,
            bottom: 4,
        };
        assert_eq!(extents.cardinals(), [1, 2, 3, 4]);
    }

    #[test]
    fn the_header_height_is_the_top_border_in_device_pixels() {
        assert_eq!(
            Extents::for_header(37, 1),
            Extents {
                top: 37,
                ..Extents::default()
            }
        );
        assert_eq!(Extents::for_header(37, 2).cardinals(), [0, 0, 74, 0]);
        // A fullscreen client's frame has no title bar.
        assert_eq!(Extents::for_header(0, 2), Extents::default());
    }

    #[test]
    fn the_content_origin_gives_left_and_top_only() {
        assert_eq!(
            Extents::for_content_origin(0.0, 37.0, 1).cardinals(),
            [0, 0, 37, 0]
        );
        assert_eq!(
            Extents::for_content_origin(5.0, 42.0, 2).cardinals(),
            [10, 0, 84, 0]
        );
        // Fractional origins truncate, as Mutter's integer cast does.
        assert_eq!(
            Extents::for_content_origin(0.6, 36.9, 1).cardinals(),
            [0, 0, 36, 0]
        );
    }

    fn registry() -> Registry<&'static str> {
        let mut registry = Registry::default();
        registry.insert(0x40, 0x99, "frame of 0x40");
        registry
    }

    #[test]
    fn the_registry_finds_a_frame_by_either_window() {
        let mut registry = registry();
        assert!(registry.is_frame(0x99) && !registry.is_frame(0x40));
        assert!(registry.has_frame(0x40) && !registry.has_frame(0x99));
        assert_eq!(registry.frame_for(0x40), Some(&"frame of 0x40"));
        assert_eq!(registry.frame_for(0x99), Some(&"frame of 0x40"));
        assert_eq!(registry.frame_for(0x41), None);

        // Removal is by client; the frame window is not a client.
        assert_eq!(registry.remove(0x99), None);
        assert_eq!(registry.remove(0x40), Some("frame of 0x40"));
        assert!(!registry.is_frame(0x99) && !registry.has_frame(0x40));
        assert_eq!(registry.remove(0x40), None);
    }

    fn act(event: Event) -> Action {
        classify(event, ROOT, NEEDS_FRAME, &registry())
    }

    #[test]
    fn new_managed_root_children_are_listened_to() {
        let create = |parent, window, override_redirect| Event::Create {
            parent,
            window,
            override_redirect,
        };
        assert_eq!(act(create(ROOT, 0x50, false)), Action::Listen(0x50));
        // Menus and tooltips, windows inside other windows, and the
        // frame windows this process creates itself.
        assert_eq!(act(create(ROOT, 0x50, true)), Action::Ignore);
        assert_eq!(act(create(0x40, 0x50, false)), Action::Ignore);
        assert_eq!(act(create(ROOT, 0x99, false)), Action::Ignore);
    }

    #[test]
    fn an_override_redirect_change_shows_in_a_root_configure_notify() {
        let configure = |event, window, override_redirect| Event::Configure {
            event,
            window,
            override_redirect,
        };
        // Became managed.
        assert_eq!(act(configure(ROOT, 0x50, false)), Action::Listen(0x50));
        // A framed window became override-redirect.
        assert_eq!(act(configure(ROOT, 0x40, true)), Action::Remove(0x40));
        // Nothing changed for these.
        assert_eq!(act(configure(ROOT, 0x40, false)), Action::Ignore);
        assert_eq!(act(configure(ROOT, 0x50, true)), Action::Ignore);
        // The root itself, our frames, and notifications selected on
        // the window rather than on the root.
        assert_eq!(act(configure(ROOT, ROOT, false)), Action::Ignore);
        assert_eq!(act(configure(ROOT, 0x99, false)), Action::Ignore);
        assert_eq!(act(configure(0x50, 0x50, false)), Action::Ignore);
    }

    #[test]
    fn a_destroyed_window_loses_its_frame() {
        assert_eq!(act(Event::Destroy { window: 0x40 }), Action::Remove(0x40));
    }

    #[test]
    fn the_needs_frame_property_sets_up_and_removes_frames() {
        let property = |atom, deleted| Event::Property {
            window: 0x50,
            atom,
            deleted,
        };
        assert_eq!(act(property(NEEDS_FRAME, false)), Action::SetUp(0x50));
        assert_eq!(act(property(NEEDS_FRAME, true)), Action::Remove(0x50));
        assert_eq!(
            act(property(39, false)),
            Action::Sync {
                window: 0x50,
                atom: 39
            }
        );
        assert_eq!(
            act(property(39, true)),
            Action::Sync {
                window: 0x50,
                atom: 39
            }
        );
    }
}
