//! Native gesture adaptation. Ghostty owns word, line and drag semantics;
//! snapshots become ordinary kernel document anchors before returning to C.
use libghostty_vt::selection::gesture::{
    Behavior, Behaviors, DragEvent, Geometry, Gesture, PressEvent,
};
use libghostty_vt::terminal::{Point, PointCoordinate, PointSpace};
use phux_client_core::session::ReplicaKey;
use phux_protocol::ResourceId;

use crate::error::{BridgeError, check_struct, terminal_id_in};
use crate::{
    Client, PhuxClient, PhuxClientResult, PhuxDocumentAnchor, PhuxDocumentPoint, PhuxResourceId,
};

#[allow(
    clippy::redundant_pub_crate,
    reason = "shared with private client state, not part of the C ABI"
)]
pub(crate) struct PointerGesture {
    key: ReplicaKey,
    handle: u64,
    gesture: Gesture<'static>,
}

/// Sized, versioned native input. Positions and geometry use the same surface
/// units. Phase 0 presses, 1 drags, 2 releases; clicks is 1, 2 or 3 on press.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxSelectionGestureEvent {
    pub size: usize,
    pub version: u32,
    pub phase: u32,
    pub clicks: u32,
    pub handle: u64,
    pub column: u16,
    pub rectangle: bool,
    pub reserved: u8,
    pub row: u32,
    pub x: f64,
    pub y: f64,
    pub columns: u32,
    pub cell_width: u32,
    pub screen_height: u32,
    pub padding_left: u32,
}

/// Returned anchors are owned by the caller and released through `anchor_release`.
/// Zero endpoints on press/drag mean no selection; release retains the range.
#[repr(C)]
#[derive(Default, Debug)]
pub struct PhuxSelectionGestureResult {
    pub handle: u64,
    pub start: PhuxDocumentAnchor,
    pub end: PhuxDocumentAnchor,
}

impl Client {
    pub(crate) fn reset_gesture(&mut self, id: &ResourceId) {
        let Some(mut state) = self.gestures.remove(id) else {
            return;
        };
        if self.terminal_key(id).ok().as_ref() != Some(&state.key) {
            return;
        }
        if let Ok(terminal) = self.terminal(id) {
            state.gesture.reset(terminal);
        }
    }

    pub(crate) fn reset_gestures(&mut self) {
        let ids: Vec<_> = self.gestures.keys().cloned().collect();
        for id in ids {
            self.reset_gesture(&id);
        }
    }

    fn gesture_state(
        &mut self,
        id: &ResourceId,
        event: &PhuxSelectionGestureEvent,
    ) -> Result<PointerGesture, BridgeError> {
        let key = self.terminal_key(id)?;
        if event.phase == 0 {
            self.clear_selection(id)?;
            let handle = self.next_gesture;
            self.next_gesture = handle
                .checked_add(1)
                .ok_or_else(|| BridgeError::state("gesture handles exhausted"))?;
            return Ok(PointerGesture {
                key,
                handle,
                gesture: Gesture::new().map_err(BridgeError::ghostty)?,
            });
        }
        let valid = self
            .gestures
            .get(id)
            .is_some_and(|state| state.key == key && state.handle == event.handle);
        if !valid {
            return Err(BridgeError::state("stale selection gesture"));
        }
        self.gestures
            .remove(id)
            .ok_or_else(|| BridgeError::state("missing selection gesture"))
    }

    fn gesture_event(
        &mut self,
        id: &ResourceId,
        event: &PhuxSelectionGestureEvent,
    ) -> Result<PhuxSelectionGestureResult, BridgeError> {
        validate_event(event)?;
        let mut state = self.gesture_state(id, event)?;
        let result = self.gesture_snapshot(id, event, &mut state);
        if event.phase == 2 || result.is_err() {
            if let Ok(terminal) = self.terminal(id) {
                state.gesture.reset(terminal);
            }
        } else {
            self.gestures.insert(id.clone(), state);
        }
        result
    }

    fn gesture_snapshot(
        &mut self,
        id: &ResourceId,
        event: &PhuxSelectionGestureEvent,
        state: &mut PointerGesture,
    ) -> Result<PhuxSelectionGestureResult, BridgeError> {
        let mut result = PhuxSelectionGestureResult {
            handle: state.handle,
            ..Default::default()
        };
        if event.phase == 2 {
            return Ok(result);
        }
        let terminal = self.terminal(id)?;
        let grid_ref = terminal
            .grid_ref(Point::Viewport(PointCoordinate {
                x: event.column,
                y: event.row,
            }))
            .map_err(BridgeError::ghostty)?;
        let selected = if event.phase == 0 {
            let behavior = match event.clicks {
                2 => Behavior::Word,
                3 => Behavior::Line,
                _ => Behavior::Cell,
            };
            let mut press = PressEvent::new().map_err(BridgeError::ghostty)?;
            press
                .set_behaviors(&Behaviors::new().with_single_click_behavior(behavior))
                .map_err(BridgeError::ghostty)?;
            press
                .set_position(event.x, event.y)
                .map_err(BridgeError::ghostty)?;
            press
                .apply(&mut state.gesture, terminal, grid_ref)
                .map_err(BridgeError::ghostty)?
        } else {
            let mut drag = DragEvent::new().map_err(BridgeError::ghostty)?;
            drag.set_position(event.x, event.y)
                .map_err(BridgeError::ghostty)?;
            drag.set_rectangle(event.rectangle)
                .map_err(BridgeError::ghostty)?;
            drag.apply(
                &mut state.gesture,
                terminal,
                grid_ref,
                Geometry {
                    columns: event.columns,
                    cell_width: event.cell_width,
                    padding_left: event.padding_left,
                    screen_height: event.screen_height,
                },
            )
            .map_err(BridgeError::ghostty)?
        };
        let Some(selected) = selected else {
            // The gesture is temporarily removed from the map while applying
            // an event, so this clears the range without ending the gesture.
            self.clear_selection(id)?;
            return Ok(result);
        };
        let start = terminal
            .point_from_grid_ref(&selected.start(), PointSpace::History)
            .map_err(BridgeError::ghostty)?
            .ok_or_else(|| BridgeError::state("lost gesture start"))?;
        let end = terminal
            .point_from_grid_ref(&selected.end(), PointSpace::History)
            .map_err(BridgeError::ghostty)?
            .ok_or_else(|| BridgeError::state("lost gesture end"))?;
        result.start = self.track_anchor(id, history_point(start))?;
        result.end = match self.track_anchor(id, history_point(end)) {
            Ok(anchor) => anchor,
            Err(error) => {
                let _ = self.release_anchor(id, result.start);
                return Err(error);
            }
        };
        if let Err(error) = self.set_selection(id, result.start, result.end, event.rectangle) {
            let _ = self.release_anchor(id, result.start);
            let _ = self.release_anchor(id, result.end);
            return Err(error);
        }
        Ok(result)
    }
}

const fn history_point(point: PointCoordinate) -> PhuxDocumentPoint {
    PhuxDocumentPoint {
        space: 0,
        row: point.y,
        column: point.x,
        reserved: 0,
    }
}

/// Read the published terminal's full DEC mouse mode without invalidating borrows.
///
/// # Safety
/// Client and terminal ID must be readable for the call on the owning thread;
/// out must point to writable u32 storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_terminal_mouse_mode(
    client: *const PhuxClient,
    id: *const PhuxResourceId,
    out: *mut u32,
) -> PhuxClientResult {
    use libghostty_vt::mouse::{EncoderOptions, TrackingMode};
    crate::with_client_ref(client, |client| {
        let id = unsafe { terminal_id_in(id) }?;
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("null mouse mode"))?;
        let terminal = client.terminal(&id)?;
        *out = match EncoderOptions::from_terminal(terminal)
            .map_err(BridgeError::ghostty)?
            .tracking_mode
        {
            TrackingMode::None => 0,
            TrackingMode::X10 => 1,
            TrackingMode::Normal => 2,
            TrackingMode::Button => 3,
            TrackingMode::Any => 4,
            _ => return Err(BridgeError::state("unsupported mouse tracking mode")),
        };
        Ok(())
    })
}

fn validate_event(event: &PhuxSelectionGestureEvent) -> Result<(), BridgeError> {
    check_struct(
        event.size,
        std::mem::size_of::<PhuxSelectionGestureEvent>(),
        event.version,
    )?;
    if event.phase > 2 || !event.x.is_finite() || !event.y.is_finite() {
        return Err(BridgeError::invalid("invalid gesture event"));
    }
    if event.phase < 2 && (event.columns == 0 || event.cell_width == 0 || event.screen_height == 0)
    {
        return Err(BridgeError::invalid("invalid gesture geometry"));
    }
    if event.phase == 0 && !(1..=3).contains(&event.clicks) {
        return Err(BridgeError::invalid("invalid gesture clicks"));
    }
    Ok(())
}

/// Apply a provider-owned Ghostty selection gesture.
///
/// # Safety
/// All pointers must be valid for their declared types for the call; client
/// requires exclusive access on its owning thread. Terminal ID spans must live
/// through the call. Output owns any nonzero anchors on success only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_selection_gesture(
    client: *mut PhuxClient,
    id: *const PhuxResourceId,
    event: *const PhuxSelectionGestureEvent,
    out: *mut PhuxSelectionGestureResult,
) -> PhuxClientResult {
    crate::with_client_mut(client, |client| {
        let id = unsafe { terminal_id_in(id) }?;
        let event =
            unsafe { event.as_ref() }.ok_or_else(|| BridgeError::invalid("null gesture event"))?;
        let out =
            unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("null gesture result"))?;
        *out = PhuxSelectionGestureResult::default();
        *out = client.gesture_event(&id, event)?;
        Ok(())
    })
}
