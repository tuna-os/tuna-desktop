//! AT-SPI focus and caret events for the screen magnifier (#349):
//! gnome-shell 51.0 `js/ui/focusCaretTracker.js` (listeners for
//! `object:state-changed:focused`, `:selected` and
//! `object:text-caret-moved`, registered only while tracking wants
//! them) and `ZoomRegion._updateFocus`, `_updateCaret` and
//! `_convertExtentsToScreenSpace` (window-relative extents, accepted
//! only from the focused widget of the active window).
//!
//! Blocking D-Bus calls run on worker threads with Atspi's 250 ms
//! timeout; the compositor reads the latest accepted extents and maps
//! them through the keyboard-focused surface's origin itself (see
//! `Runtime::magnifier_inputs`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::zvariant::{Structure, Value};

const REGISTRY: &str = "org.a11y.atspi.Registry";
const REGISTRY_PATH: &str = "/org/a11y/atspi/registry";
const FOCUS_EVENTS: [&str; 2] = [
    "object:state-changed:focused",
    "object:state-changed:selected",
];
const CARET_EVENT: &str = "object:text-caret-moved";
/// `Atspi.set_timeout(250, 250)`.
const TIMEOUT: Duration = Duration::from_millis(250);
/// Parents walked looking for the application (a deeper tree is
/// treated as unvalidated).
const MAX_DEPTH: usize = 64;

// at-spi2-core `atspi-constants.h`.
const ROLE_DIALOG: u32 = 16;
const ROLE_FRAME: u32 = 23;
const ROLE_WINDOW: u32 = 69;
const ROLE_APPLICATION: u32 = 75;
const STATE_ACTIVE: u32 = 1;
const STATE_FOCUSED: u32 = 12;
const COORD_TYPE_WINDOW: u32 = 1;

/// One accepted event: the source's extents in its window's
/// coordinates, the sender's process, and an arrival number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extents {
    pub seq: u64,
    pub pid: Option<u32>,
    pub rect: (i32, i32, i32, i32),
}

#[derive(Debug, Default)]
struct Latest {
    focus: Option<Extents>,
    caret: Option<Extents>,
    seq: u64,
}

#[derive(Debug, Default)]
struct Shared {
    latest: Mutex<Latest>,
    want_focus: AtomicBool,
    want_caret: AtomicBool,
}

/// The magnifier's AT-SPI listener. Idle until the magnifier first
/// wants focus or caret events.
#[derive(Debug, Default)]
pub struct FocusCaretTracker {
    shared: Arc<Shared>,
    control: Option<mpsc::Sender<(bool, bool)>>,
    wanted: (bool, bool),
}

impl FocusCaretTracker {
    /// Follow what the magnifier wants this frame. A change
    /// (de)registers the listeners on the worker; turning one off also
    /// drops its last event, so nothing stale applies later.
    pub fn sync(&mut self, focus: bool, caret: bool, wake: &crate::wake::Wake) {
        self.shared.want_focus.store(focus, Ordering::Release);
        self.shared.want_caret.store(caret, Ordering::Release);
        if (focus, caret) == self.wanted {
            return;
        }
        self.wanted = (focus, caret);
        if let Ok(mut latest) = self.shared.latest.lock() {
            if !focus {
                latest.focus = None;
            }
            if !caret {
                latest.caret = None;
            }
        }
        if self.control.is_none() && (focus || caret) {
            let (tx, rx) = mpsc::channel();
            let shared = self.shared.clone();
            let wake = wake.clone();
            let spawned = std::thread::Builder::new()
                .name("tuna-atspi-focus".into())
                .spawn(move || {
                    if let Err(error) = run(shared, rx, wake) {
                        eprintln!(
                            "tuna-compositor: magnifier AT-SPI tracking unavailable: {error}"
                        );
                    }
                });
            if spawned.is_ok() {
                self.control = Some(tx);
            }
        }
        if let Some(control) = &self.control {
            let _ = control.send((focus, caret));
        }
    }

    /// The last accepted focus event, if focus tracking wants it.
    pub fn focus(&self) -> Option<Extents> {
        self.shared.latest.lock().ok()?.focus
    }

    /// The last accepted caret event, if caret tracking wants it.
    pub fn caret(&self) -> Option<Extents> {
        self.shared.latest.lock().ok()?.caret
    }
}

/// The accessibility bus (`org.a11y.Bus.GetAddress` on the session bus).
fn connect() -> zbus::Result<zbus::blocking::Connection> {
    let session = zbus::blocking::Connection::session()?;
    let address: String = session
        .call_method(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            Some("org.a11y.Bus"),
            "GetAddress",
            &(),
        )?
        .body()
        .deserialize()?;
    zbus::blocking::connection::Builder::address(address.as_str())?
        .method_timeout(TIMEOUT)
        .build()
}

/// `Atspi.EventListener.register` / `deregister` against the registry
/// daemon: the current three-argument form, else the older one.
fn registry_call(conn: &zbus::blocking::Connection, method: &str, event: &str) {
    let full = conn.call_method(
        Some(REGISTRY),
        REGISTRY_PATH,
        Some(REGISTRY),
        method,
        &(event, Vec::<String>::new(), ""),
    );
    if full.is_err() {
        let _ = conn.call_method(
            Some(REGISTRY),
            REGISTRY_PATH,
            Some(REGISTRY),
            method,
            &(event,),
        );
    }
}

fn signals(
    conn: &zbus::blocking::Connection,
    member: &str,
) -> zbus::Result<zbus::blocking::MessageIterator> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.a11y.atspi.Event.Object")?
        .member(member)?
        .build();
    zbus::blocking::MessageIterator::for_match_rule(rule, conn, Some(256))
}

fn run(
    shared: Arc<Shared>,
    control: mpsc::Receiver<(bool, bool)>,
    wake: crate::wake::Wake,
) -> zbus::Result<()> {
    let conn = connect()?;
    for (member, caret) in [("StateChanged", false), ("TextCaretMoved", true)] {
        let events = signals(&conn, member)?;
        let (conn, shared, wake) = (conn.clone(), shared.clone(), wake.clone());
        std::thread::Builder::new()
            .name(format!("tuna-atspi-{member}"))
            .spawn(move || {
                for message in events {
                    let Ok(message) = message else {
                        break;
                    };
                    let wanted = if caret {
                        &shared.want_caret
                    } else {
                        &shared.want_focus
                    };
                    if !wanted.load(Ordering::Acquire) {
                        continue;
                    }
                    let accepted = if caret {
                        caret_extents(&conn, &message)
                    } else {
                        focus_extents(&conn, &message)
                    };
                    let Some((rect, pid)) = accepted else {
                        continue;
                    };
                    // Tracking may have stopped during the calls.
                    if !wanted.load(Ordering::Acquire) {
                        continue;
                    }
                    if let Ok(mut latest) = shared.latest.lock() {
                        latest.seq += 1;
                        let event = Some(Extents {
                            seq: latest.seq,
                            pid,
                            rect,
                        });
                        if caret {
                            latest.caret = event;
                        } else {
                            latest.focus = event;
                        }
                    }
                    wake.signal();
                }
            })
            .map_err(|error| zbus::Error::Failure(error.to_string()))?;
    }
    let mut registered = (false, false);
    for (focus, caret) in control {
        if focus != registered.0 {
            let method = if focus {
                "RegisterEvent"
            } else {
                "DeregisterEvent"
            };
            for event in FOCUS_EVENTS {
                registry_call(&conn, method, event);
            }
        }
        if caret != registered.1 {
            let method = if caret {
                "RegisterEvent"
            } else {
                "DeregisterEvent"
            };
            registry_call(&conn, method, CARET_EVENT);
        }
        registered = (focus, caret);
    }
    Ok(())
}

/// One accessible object: its bus name and path.
struct Object<'a> {
    conn: &'a zbus::blocking::Connection,
    name: String,
    path: String,
}

impl Object<'_> {
    fn call<B>(&self, iface: &str, method: &str, body: &B) -> Option<zbus::Message>
    where
        B: serde::ser::Serialize + zbus::zvariant::DynamicType,
    {
        self.conn
            .call_method(
                Some(self.name.as_str()),
                self.path.as_str(),
                Some(iface),
                method,
                body,
            )
            .ok()
    }

    fn role(&self) -> Option<u32> {
        self.call("org.a11y.atspi.Accessible", "GetRole", &())?
            .body()
            .deserialize()
            .ok()
    }

    fn has_state(&self, state: u32) -> bool {
        self.call("org.a11y.atspi.Accessible", "GetState", &())
            .and_then(|reply| reply.body().deserialize::<Vec<u32>>().ok())
            .and_then(|words| words.get((state / 32) as usize).copied())
            .is_some_and(|word| word & (1 << (state % 32)) != 0)
    }

    fn parent(&self) -> Option<Self> {
        let reply = self.call(
            "org.freedesktop.DBus.Properties",
            "Get",
            &("org.a11y.atspi.Accessible", "Parent"),
        )?;
        let body = reply.body();
        let value: zbus::zvariant::OwnedValue = body.deserialize().ok()?;
        let Value::Structure(parent) = &*value else {
            return None;
        };
        let [Value::Str(name), Value::ObjectPath(path)] = parent.fields() else {
            return None;
        };
        if path.as_str() == "/org/a11y/atspi/null" {
            return None;
        }
        Some(Object {
            conn: self.conn,
            name: name.as_str().to_owned(),
            path: path.as_str().to_owned(),
        })
    }

    /// `_convertExtentsToScreenSpace`'s check: the accessible is
    /// focused and its outermost window below the application is
    /// active. (GNOME also passes its own shell's events through; the
    /// compositor decides which surface the extents belong to.)
    fn in_active_window(&self) -> bool {
        if !self.has_state(STATE_FOCUSED) {
            return false;
        }
        let mut window = None;
        let mut current = Some(Object {
            conn: self.conn,
            name: self.name.clone(),
            path: self.path.clone(),
        });
        for _ in 0..MAX_DEPTH {
            let Some(object) = current else {
                break;
            };
            match object.role() {
                Some(ROLE_APPLICATION) => break,
                Some(ROLE_FRAME | ROLE_DIALOG | ROLE_WINDOW) => {
                    current = object.parent();
                    window = Some(object);
                }
                _ => current = object.parent(),
            }
        }
        window.is_some_and(|window| window.has_state(STATE_ACTIVE))
    }
}

fn pid_of(conn: &zbus::blocking::Connection, sender: &str) -> Option<u32> {
    let name = zbus::names::BusName::try_from(sender).ok()?;
    zbus::blocking::fdo::DBusProxy::new(conn)
        .ok()?
        .get_connection_unix_process_id(name)
        .ok()
}

/// The event's kind string and `detail1`, whatever the body's
/// trailing fields (`siiva{sv}` now, `siiv(so)` before).
fn kind_and_detail(message: &zbus::Message) -> Option<(String, i32)> {
    let body = message.body();
    let event: Structure<'_> = body.deserialize().ok()?;
    match event.fields() {
        [Value::Str(kind), Value::I32(detail1), ..] => Some((kind.as_str().to_owned(), *detail1)),
        _ => None,
    }
}

fn source<'a>(conn: &'a zbus::blocking::Connection, message: &zbus::Message) -> Option<Object<'a>> {
    let header = message.header();
    Some(Object {
        conn,
        name: header.sender()?.to_string(),
        path: header.path()?.to_string(),
    })
}

/// Four integers, as one `(iiii)` struct or as four out arguments.
fn rect_of(reply: &zbus::Message) -> Option<(i32, i32, i32, i32)> {
    let body = reply.body();
    if let Ok(rect) = body.deserialize::<(i32, i32, i32, i32)>() {
        return Some(rect);
    }
    let fields: Structure<'_> = body.deserialize().ok()?;
    match fields.fields() {
        [Value::I32(x), Value::I32(y), Value::I32(w), Value::I32(h)] => Some((*x, *y, *w, *h)),
        [Value::Structure(inner)] => match inner.fields() {
            [Value::I32(x), Value::I32(y), Value::I32(w), Value::I32(h)] => Some((*x, *y, *w, *h)),
            _ => None,
        },
        _ => None,
    }
}

/// Window extents of an AT-SPI event's source, and the sender's pid.
type Located = ((i32, i32, i32, i32), Option<u32>);

/// `_updateFocus`: a focused (or selected) component's window extents.
fn focus_extents(conn: &zbus::blocking::Connection, message: &zbus::Message) -> Option<Located> {
    let (kind, detail1) = kind_and_detail(message)?;
    if !matches!(kind.as_str(), "focused" | "selected") || detail1 != 1 {
        return None;
    }
    let object = source(conn, message)?;
    let reply = object.call(
        "org.a11y.atspi.Component",
        "GetExtents",
        &(COORD_TYPE_WINDOW,),
    )?;
    let rect = rect_of(&reply)?;
    object
        .in_active_window()
        .then(|| (rect, pid_of(conn, &object.name)))
}

/// `_updateCaret`: the caret character's window extents; a 0x0 box
/// (offset not translatable) is ignored, as in GNOME.
fn caret_extents(conn: &zbus::blocking::Connection, message: &zbus::Message) -> Option<Located> {
    let (_, offset) = kind_and_detail(message)?;
    let object = source(conn, message)?;
    let reply = object.call(
        "org.a11y.atspi.Text",
        "GetCharacterExtents",
        &(offset, COORD_TYPE_WINDOW),
    )?;
    let rect = rect_of(&reply)?;
    if rect.2 == 0 && rect.3 == 0 {
        return None;
    }
    object
        .in_active_window()
        .then(|| (rect, pid_of(conn, &object.name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_stays_idle_until_tracking_wants_events() {
        let mut tracker = FocusCaretTracker::default();
        let wake = crate::wake::Wake::default();
        tracker.sync(false, false, &wake);
        assert!(tracker.control.is_none(), "no worker without a listener");
        assert_eq!(tracker.focus(), None);
        assert_eq!(tracker.caret(), None);
    }

    #[test]
    fn turning_tracking_off_drops_the_last_event() {
        // A stand-in worker: the sync below must not start a real one.
        let (control, _worker) = mpsc::channel();
        let mut tracker = FocusCaretTracker {
            wanted: (true, true),
            control: Some(control),
            ..FocusCaretTracker::default()
        };
        {
            let mut latest = tracker.shared.latest.lock().unwrap();
            latest.focus = Some(Extents {
                seq: 1,
                pid: Some(7),
                rect: (1, 2, 3, 4),
            });
            latest.caret = Some(Extents {
                seq: 2,
                pid: Some(7),
                rect: (5, 6, 0, 9),
            });
        }
        let wake = crate::wake::Wake::default();
        tracker.sync(true, false, &wake);
        assert!(tracker.focus().is_some());
        assert_eq!(tracker.caret(), None);
        tracker.sync(false, false, &wake);
        assert_eq!(tracker.focus(), None);
    }
}
