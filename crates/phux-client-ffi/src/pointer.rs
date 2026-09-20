//! C marshalling for runtime-owned selection gestures.

use phux_client_runtime::engine::SelectionGestureEvent;

use crate::error::{BridgeError, check_struct, terminal_id_in};
use crate::{
    PhuxClient, PhuxClientResult, PhuxDocumentAnchor, PhuxResourceId, with_client_mut,
    with_client_ref,
};

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
    use phux_client_runtime::engine::MouseMode;
    with_client_ref(client, |client| {
        let id = unsafe { terminal_id_in(id) }?;
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("null mouse mode"))?;
        *out = match client.mouse_mode(&id)? {
            MouseMode::None => 0,
            MouseMode::X10 => 1,
            MouseMode::Normal => 2,
            MouseMode::Button => 3,
            MouseMode::Any => 4,
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
    if event.reserved != 0 {
        return Err(BridgeError::invalid("gesture reserved field must be zero"));
    }
    Ok(())
}

/// Apply a provider-owned Ghostty selection gesture on the runtime owner thread.
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
    with_client_mut(client, |client| {
        let id = unsafe { terminal_id_in(id) }?;
        let event =
            unsafe { event.as_ref() }.ok_or_else(|| BridgeError::invalid("null gesture event"))?;
        validate_event(event)?;
        let out =
            unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("null gesture result"))?;
        *out = PhuxSelectionGestureResult::default();
        let result = client.selection_gesture(
            &id,
            SelectionGestureEvent {
                phase: event.phase,
                clicks: event.clicks,
                handle: event.handle,
                column: event.column,
                rectangle: event.rectangle,
                row: event.row,
                x: event.x,
                y: event.y,
                columns: event.columns,
                cell_width: event.cell_width,
                screen_height: event.screen_height,
                padding_left: event.padding_left,
            },
        )?;
        *out = PhuxSelectionGestureResult {
            handle: result.handle,
            start: PhuxDocumentAnchor {
                opaque_id: result.start,
            },
            end: PhuxDocumentAnchor {
                opaque_id: result.end,
            },
        };
        Ok(())
    })
}
