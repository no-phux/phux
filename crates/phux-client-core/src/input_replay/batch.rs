//! The `APPLY_INPUT` wire caps a batch must fit before it is journaled.

use phux_protocol::input::InputEvent;

/// Which `APPLY_INPUT` wire cap a batch exceeds (`docs/protocol.md#input`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchTooLarge {
    /// More events than [`MAX_APPLY_INPUT_EVENTS`](phux_protocol::wire::frame::MAX_APPLY_INPUT_EVENTS).
    Events {
        /// Events in the batch.
        measured: usize,
        /// The protocol cap.
        limit: usize,
    },
    /// A conservative encoded-body bound over
    /// [`MAX_APPLY_INPUT_COMMAND_BODY`](phux_protocol::wire::frame::MAX_APPLY_INPUT_COMMAND_BODY).
    Bytes {
        /// The batch's encoded-body upper bound.
        measured: usize,
        /// The protocol cap.
        limit: usize,
    },
}

impl std::fmt::Display for BatchTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (measured, limit, unit) = match *self {
            Self::Events { measured, limit } => (measured, limit, "events"),
            Self::Bytes { measured, limit } => (measured, limit, "bytes"),
        };
        write!(
            f,
            "batch is {measured} {unit}, over the {limit}-{unit} protocol cap"
        )
    }
}

impl std::error::Error for BatchTooLarge {}

/// Refuse an `APPLY_INPUT` batch over either wire cap, never splitting it.
///
/// The byte rule is a conservative upper bound on the encoded command body:
/// a fixed header (tag, operation id, worst-case satellite terminal id, event
/// count), a fixed per-event allowance, plus each key event's text and each
/// paste's data. Every client precheck uses this one rule.
///
/// # Errors
///
/// Returns the [`BatchTooLarge`] cap the batch exceeds.
pub fn validate_apply_input_batch(events: &[InputEvent]) -> Result<(), BatchTooLarge> {
    use phux_protocol::wire::frame::{MAX_APPLY_INPUT_COMMAND_BODY, MAX_APPLY_INPUT_EVENTS};

    /// Tag byte, 16-byte operation id, a worst-case satellite terminal id,
    /// and the `u16` event count.
    const HEADER: usize = 1 + 16 + 264 + 2;
    /// Per-event tag plus length prefix plus the widest fixed key/mouse body.
    const PER_EVENT: usize = 32;

    if events.len() > MAX_APPLY_INPUT_EVENTS {
        return Err(BatchTooLarge::Events {
            measured: events.len(),
            limit: MAX_APPLY_INPUT_EVENTS,
        });
    }
    let encoded_bytes = events.iter().fold(HEADER, |total, event| {
        let payload = match event {
            InputEvent::Key(key) => key.text.as_ref().map_or(0, String::len),
            InputEvent::Paste(paste) => paste.data.len(),
            _ => 0,
        };
        total.saturating_add(PER_EVENT).saturating_add(payload)
    });
    if encoded_bytes > MAX_APPLY_INPUT_COMMAND_BODY {
        return Err(BatchTooLarge::Bytes {
            measured: encoded_bytes,
            limit: MAX_APPLY_INPUT_COMMAND_BODY,
        });
    }
    Ok(())
}
