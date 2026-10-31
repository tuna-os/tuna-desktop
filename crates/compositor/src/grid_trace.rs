//! Opt-in #965 attribution, not a presentation or client-receive receipt.
//!
//! `commit_arrival` is the server pre-commit hook, after wl_surface.frame
//! requests have populated pending state; it is not their socket arrival time.
//! Applied records describe current state, which can include older callbacks.
//! `callback_drained` compares actual callback identities around Smithay's
//! synchronous sender. It means done was queued, not flushed or received.
//! All timestamps use the presentation CLOCK_MONOTONIC (also GLib's clock).

use std::{fmt, fmt::Write as _, io, sync::LazyLock, time::Duration};

use smithay::{
    desktop::utils::send_frames_surface_tree,
    output::Output,
    reexports::wayland_server::{backend::ObjectId, protocol::wl_surface::WlSurface, Resource},
    wayland::compositor::{
        get_parent, with_states, with_surface_tree_downward, SurfaceAttributes, TraversalAction,
    },
};

use crate::State;

const NAMESPACE: &str = "tuna-shell-appgrid";
// Below POSIX's minimum PIPE_BUF, including the terminating newline. Never
// issue fragmented formatting writes or emit a truncated/ambiguous record.
const RECORD_BYTES: usize = 512;
static ENABLED: LazyLock<bool> =
    LazyLock::new(|| requested(std::env::var_os("TUNA_GRID_TRACE").as_deref()));

fn requested(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some()
}

pub(crate) fn enabled() -> bool {
    *ENABLED
}

struct Record {
    bytes: [u8; RECORD_BYTES],
    len: usize,
}

impl fmt::Write for Record {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.len.checked_add(text.len()).ok_or(fmt::Error)?;
        if end > self.bytes.len() {
            return Err(fmt::Error);
        }
        self.bytes[self.len..end].copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

fn emit_to(
    writer: &mut impl io::Write,
    monotonic_us: u64,
    phase: &str,
    details: fmt::Arguments<'_>,
) {
    let mut record = Record {
        bytes: [0; RECORD_BYTES],
        len: 0,
    };
    if writeln!(
        record,
        "tuna-grid-compositor: phase={phase} monotonic_us={monotonic_us} {details}"
    )
    .is_ok()
    {
        // One short write_all per record; stderr's lock also serializes other
        // Rust stderr users in this process. Failed writes lose evidence only.
        let _ = writer.write_all(&record.bytes[..record.len]);
    }
}

fn emit(monotonic_us: u64, phase: &str, details: fmt::Arguments<'_>) {
    emit_to(&mut io::stderr().lock(), monotonic_us, phase, details);
}

pub(crate) fn event(state: &State, phase: &str, details: fmt::Arguments<'_>) {
    if enabled() {
        emit(state.presentation_now().as_micros(), phase, details);
    }
}

// Called only behind the cached gate: disabled tracing never walks roles or
// parent trees, reads the clock, formats a record, allocates or performs I/O.
fn grid_surface(state: &State, surface: &WlSurface) -> bool {
    let mut root = surface.clone();
    while let Some(parent) = get_parent(&root) {
        root = parent;
    }
    state
        .panel_surfaces
        .iter()
        .any(|record| record.surface == root && record.namespace == NAMESPACE)
}

pub(crate) fn commit(state: &State, surface: &WlSurface, pending: bool) {
    if !enabled() || !grid_surface(state, surface) {
        return;
    }
    let at = state.presentation_now().as_micros();
    let phase = if pending {
        "commit_arrival"
    } else {
        "commit_applied"
    };
    let client = surface.client().map(|client| client.id());
    with_states(surface, |states| {
        let mut cached = states.cached_state.get::<SurfaceAttributes>();
        let attributes = if pending {
            cached.pending()
        } else {
            cached.current()
        };
        emit(
            at,
            phase,
            format_args!(
            "client={client:?} surface={} surface_object={:?} namespace={NAMESPACE} callbacks={}",
            surface.id().protocol_id(), surface.id(), attributes.frame_callbacks.len()
        ),
        );
        for callback in &attributes.frame_callbacks {
            emit(at, phase, format_args!(
                "client={client:?} surface={} surface_object={:?} namespace={NAMESPACE} callback={} callback_object={:?}",
                surface.id().protocol_id(), surface.id(), callback.id().protocol_id(), callback.id()
            ));
        }
    });
}

fn snapshot(root: &WlSurface) -> Vec<(WlSurface, Vec<ObjectId>)> {
    let mut callbacks = Vec::new();
    with_surface_tree_downward(
        root,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |surface, states, _| {
            let mut cached = states.cached_state.get::<SurfaceAttributes>();
            let current = &cached.current().frame_callbacks;
            if !current.is_empty() {
                callbacks.push((surface.clone(), current.iter().map(Resource::id).collect()));
            }
        },
        |_, _, _| true,
    );
    callbacks
}

fn drained(before: Vec<(WlSurface, Vec<ObjectId>)>, mut observe: impl FnMut(&WlSurface, ObjectId)) {
    for (surface, ids) in before {
        with_states(&surface, |states| {
            let mut cached = states.cached_state.get::<SurfaceAttributes>();
            let current = &cached.current().frame_callbacks;
            for id in ids {
                if !current.iter().any(|callback| callback.id() == id) {
                    observe(&surface, id);
                }
            }
        });
    }
}

/// Preserve the existing Smithay sender and its selection/throttling behavior.
/// Only the enabled grid path snapshots IDs, then confirms which disappeared.
pub(crate) fn send_layer_frames(
    state: &State,
    surface: &WlSurface,
    output: &Output,
    time: Duration,
) {
    let before = if enabled() && grid_surface(state, surface) {
        Some((snapshot(surface), state.presentation_now().as_micros()))
    } else {
        None
    };
    send_frames_surface_tree(surface, output, time, Some(Duration::ZERO), |_, _| {
        Some(output.clone())
    });
    if let Some((before, before_send_us)) = before {
        let after_send_us = state.presentation_now().as_micros();
        drained(before, |surface, callback| {
            let client = surface.client().map(|client| client.id());
            emit(after_send_us, "callback_drained", format_args!(
                "client={client:?} surface={} surface_object={:?} namespace={NAMESPACE} callback={} callback_object={callback:?} before_send_us={before_send_us} after_send_us={after_send_us}",
                surface.id().protocol_id(), surface.id(), callback.protocol_id()
            ));
        });
    }
}

/// Main-thread runtime/runqueue/timeslice counters (CONFIG_SCHED_INFO), or
/// `None` when unreadable, malformed or unpopulated: never invented zeros.
fn parse_schedstat(bytes: &[u8]) -> Option<(u64, u64, u64)> {
    let mut fields = std::str::from_utf8(bytes).ok()?.split_ascii_whitespace();
    let run = fields.next()?.parse().ok()?;
    let wait = fields.next()?.parse().ok()?;
    let slices = fields.next()?.parse().ok()?;
    (fields.next().is_none() && run > 0 && slices > 0).then_some((run, wait, slices))
}

thread_local! {
    // Opened on the first traced stage only; reads reuse a stack buffer.
    static SCHED: std::cell::RefCell<Option<std::fs::File>> =
        std::cell::RefCell::new(std::fs::File::open("/proc/thread-self/schedstat").ok());
}

fn thread_sched() -> Option<(u64, u64, u64)> {
    use std::io::{Read, Seek};
    SCHED.with(|file| {
        let mut file = file.borrow_mut();
        let file = file.as_mut()?;
        file.rewind().ok()?;
        let mut bytes = [0_u8; 128];
        let count = file.read(&mut bytes).ok()?;
        (count < bytes.len()).then(|| parse_schedstat(&bytes[..count]))?
    })
}

/// Bisects the compositor loop between client flush and repaint. Consecutive
/// stage deltas separate compositor CPU, runnable wait and off-CPU time.
pub(crate) fn stage(state: &State, stage: &str) {
    if !enabled() {
        return;
    }
    let at = state.presentation_now().as_micros();
    match thread_sched() {
        Some((run, wait, slices)) => emit(
            at,
            "tick_stage",
            format_args!("stage={stage} run_ns={run} wait_ns={wait} timeslices={slices}"),
        ),
        None => emit(
            at,
            "tick_stage",
            format_args!("stage={stage} schedstat=unavailable"),
        ),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::unix::net::UnixStream,
    };

    #[derive(Default)]
    struct Writes {
        bytes: Vec<u8>,
        calls: usize,
    }
    impl io::Write for Writes {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn default_disabled_and_records_are_bounded_single_writes() {
        assert!(!requested(None));
        assert!(requested(Some(std::ffi::OsStr::new(""))));
        assert!(requested(Some(std::ffi::OsStr::new("0"))));
        assert!(requested(Some(std::ffi::OsStr::new("1"))));
        let mut writer = Writes::default();
        emit_to(
            &mut writer,
            123,
            "callback_drained",
            format_args!("surface=4 callback=5"),
        );
        assert_eq!(writer.calls, 1);
        assert_eq!(
            String::from_utf8(writer.bytes.clone()).unwrap(),
            "tuna-grid-compositor: phase=callback_drained monotonic_us=123 surface=4 callback=5\n"
        );
        emit_to(
            &mut writer,
            124,
            "oversized",
            format_args!("{}", "x".repeat(RECORD_BYTES)),
        );
        assert_eq!(
            writer.calls, 1,
            "oversized records must not emit a partial line"
        );
    }

    #[test]
    fn tick_stage_scheduler_counters_are_populated_or_unavailable() {
        assert_eq!(parse_schedstat(b"10 0 2\n"), Some((10, 0, 2)));
        for bytes in [
            &b""[..],
            b"0 0 0\n",
            b"0 1 2\n",
            b"1 2 0\n",
            b"1 2",
            b"1 x 3",
            b"1 2 3 4",
        ] {
            assert_eq!(parse_schedstat(bytes), None);
        }
        // The real compositor thread exposes populated counters on Linux.
        assert!(thread_sched().is_some_and(|(run, _, slices)| run > 0 && slices > 0));
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect()
    }

    /// A real wire client with surface 4 holding frame callbacks 5 and 6
    /// after one commit; the registry/bind replies are already drained.
    pub(crate) fn frame_client() -> (crate::TestCompositor, UnixStream, WlSurface) {
        let mut compositor = crate::TestCompositor::new();
        let (server, mut wire) = UnixStream::pair().unwrap();
        wire.set_nonblocking(true).unwrap();
        let client = compositor.add_client(server);
        wire.write_all(&words(&[1, (12 << 16) | 1, 2])).unwrap(); // get_registry
        compositor.pump();
        let mut globals = Vec::new();
        loop {
            let mut bytes = [0; 4096];
            match wire.read(&mut bytes) {
                Ok(0) => panic!("registry client disconnected"),
                Ok(count) => globals.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("reading registry: {error}"),
            }
        }
        let mut cursor = 0;
        let mut compositor_name = None;
        while cursor < globals.len() {
            let size =
                u32::from_ne_bytes(globals[cursor + 4..cursor + 8].try_into().unwrap()) >> 16;
            let event = &globals[cursor..cursor + size as usize];
            let length = u32::from_ne_bytes(event[12..16].try_into().unwrap()) as usize;
            if &event[16..16 + length] == b"wl_compositor\0" {
                compositor_name = Some(u32::from_ne_bytes(event[8..12].try_into().unwrap()));
            }
            cursor += size as usize;
        }
        let interface = b"wl_compositor\0";
        let mut bind = words(&[
            2,
            40 << 16,
            compositor_name.unwrap(),
            interface.len() as u32,
        ]);
        bind.extend_from_slice(interface);
        bind.resize(32, 0);
        bind.extend(words(&[1, 3]));
        bind.extend(words(&[3, 12 << 16, 4])); // create_surface
        bind.extend(words(&[4, (12 << 16) | 3, 5])); // frame
        bind.extend(words(&[4, (12 << 16) | 3, 6])); // frame
        bind.extend(words(&[4, (8 << 16) | 6])); // commit
        wire.write_all(&bind).unwrap();
        compositor.pump();
        let mut stale = [0; 4096];
        while wire.read(&mut stale).is_ok_and(|count| count > 0) {}
        let surface: WlSurface = client
            .object_from_protocol_id(&compositor.display.handle(), 4)
            .unwrap();
        (compositor, wire, surface)
    }

    #[test]
    fn observes_only_actual_callback_ids_removed_by_smithay() {
        let (_compositor, _wire, surface) = frame_client();
        let before = snapshot(&surface);
        assert_eq!(
            before[0]
                .1
                .iter()
                .map(ObjectId::protocol_id)
                .collect::<Vec<_>>(),
            [5, 6]
        );
        let mut observed = Vec::new();
        drained(before, |_, id| observed.push(id.protocol_id()));
        assert!(
            observed.is_empty(),
            "sender invocation alone is not evidence of a drain"
        );
        let before = snapshot(&surface);
        let output = Output::new(
            "trace-test".into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        send_frames_surface_tree(
            &surface,
            &output,
            Duration::ZERO,
            Some(Duration::ZERO),
            |_, _| Some(output.clone()),
        );
        drained(before, |seen_surface, id| {
            assert_eq!(seen_surface, &surface);
            observed.push(id.protocol_id());
        });
        assert_eq!(observed, [5, 6]);
        assert!(snapshot(&surface).is_empty());
    }
}
