//! The seat's touch grabs (P-DS-04, #345).
//!
//! Mutter gives every touch sequence its own focus: the surface under that
//! point when it began (`meta_wayland_touch_update` on the sprite's enter),
//! kept until it ends — an implicit grab per sequence. Smithay's default
//! grab instead sends every later finger to the first finger's surface, so
//! the seat installs this pair: the default grab opens an implicit grab on
//! the first down, as Smithay's does, but each down keeps its own focus and
//! every live sequence's serial can start a drag (`start_drag`, through the
//! vendored Smithay's `TouchGrab::sequence_start_data`).

use smithay::input::touch::{
    DownEvent, GrabStartData, MotionEvent, OrientationEvent, ShapeEvent, TouchGrab,
    TouchInnerHandle, UpEvent,
};
use smithay::utils::{Logical, Point, Serial};

use crate::State;

type Focus = Option<(
    smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    Point<f64, Logical>,
)>;

/// No finger down: the next down opens the implicit grab.
pub struct SequenceDefaultGrab;

impl TouchGrab<State> for SequenceDefaultGrab {
    fn down(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        focus: Focus,
        event: &DownEvent,
        seq: Serial,
    ) {
        handle.down(data, focus.clone(), event, seq);
        let start_data = GrabStartData {
            focus,
            slot: event.slot,
            location: event.location,
        };
        handle.set_grab(
            self,
            data,
            event.serial,
            SequenceGrab {
                sequences: vec![(event.serial, start_data.clone())],
                start_data,
                points: 1,
            },
        );
    }

    fn up(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &UpEvent,
        seq: Serial,
    ) {
        handle.up(data, event, seq);
    }

    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        focus: Focus,
        event: &MotionEvent,
        seq: Serial,
    ) {
        handle.motion(data, focus, event, seq);
    }

    fn frame(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>, seq: Serial) {
        handle.frame(data, seq);
    }

    fn cancel(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>, seq: Serial) {
        handle.cancel(data, seq);
    }

    fn shape(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &ShapeEvent,
        seq: Serial,
    ) {
        handle.shape(data, event, seq);
    }

    fn orientation(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &OrientationEvent,
        seq: Serial,
    ) {
        handle.orientation(data, event, seq);
    }

    fn start_data(&self) -> &GrabStartData<State> {
        unreachable!("the default grab has no start data")
    }

    fn unset(&mut self, _data: &mut State) {}
}

/// Fingers down: each keeps the focus it began on; the grab ends with
/// the last finger or a cancel. Every live sequence's down serial stays
/// a grab serial (`meta_wayland_seat_get_grab_info` looks through all
/// touch sequences), so any finger can start a drag-and-drop.
pub struct SequenceGrab {
    start_data: GrabStartData<State>,
    /// Each finger still down: its down serial and where it began.
    sequences: Vec<(Serial, GrabStartData<State>)>,
    points: usize,
}

impl TouchGrab<State> for SequenceGrab {
    fn down(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        focus: Focus,
        event: &DownEvent,
        seq: Serial,
    ) {
        handle.down(data, focus.clone(), event, seq);
        self.points += 1;
        self.sequences.push((
            event.serial,
            GrabStartData {
                focus,
                slot: event.slot,
                location: event.location,
            },
        ));
    }

    fn up(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &UpEvent,
        seq: Serial,
    ) {
        handle.up(data, event, seq);
        self.points = self.points.saturating_sub(1);
        self.sequences.retain(|(_, start)| start.slot != event.slot);
        if self.points == 0 {
            handle.unset_grab(self, data);
        }
    }

    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        focus: Focus,
        event: &MotionEvent,
        seq: Serial,
    ) {
        handle.motion(data, focus, event, seq);
    }

    fn frame(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>, seq: Serial) {
        handle.frame(data, seq);
    }

    fn cancel(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>, seq: Serial) {
        handle.cancel(data, seq);
        handle.unset_grab(self, data);
    }

    fn shape(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &ShapeEvent,
        seq: Serial,
    ) {
        handle.shape(data, event, seq);
    }

    fn orientation(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &OrientationEvent,
        seq: Serial,
    ) {
        handle.orientation(data, event, seq);
    }

    fn start_data(&self) -> &GrabStartData<State> {
        &self.start_data
    }

    fn sequence_start_data(&self, serial: Serial) -> Option<GrabStartData<State>> {
        self.sequences
            .iter()
            .find(|(down, _)| *down == serial)
            .map(|(_, start)| start.clone())
    }

    fn unset(&mut self, _data: &mut State) {}
}
