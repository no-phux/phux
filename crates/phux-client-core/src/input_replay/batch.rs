//! The `APPLY_INPUT` wire caps a batch must fit before it is journaled.

use phux_protocol::input::InputEvent;

// Tag, operation id, worst-case satellite id, and event count.
const HEADER: usize = 1 + 16 + 264 + 2;
// Event tag, length prefix, and widest fixed key/mouse body.
const PER_EVENT: usize = 32;

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
/// Delivery selected for one borrowed user paste. Raw fallback stays atomic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserPasteDelivery {
    /// Journal the original operation and await its delivery receipt.
    Acknowledged,
    /// Queue one unreceipted paste frame, never split into chunks.
    Raw,
    /// Nothing may be queued.
    Refused(&'static str),
}

/// Choose delivery without allocating or copying the clipboard payload.
/// `local` means acknowledged input can reach this execution; raw fallback
/// must not overtake any unresolved journal operation.
#[must_use]
pub fn user_paste_delivery(
    text: impl AsRef<[u8]>,
    local: bool,
    must_order_after: bool,
    delivery_fenced: bool,
) -> UserPasteDelivery {
    use phux_protocol::wire::frame::{MAX_APPLY_INPUT_COMMAND_BODY, MAX_FRAME_LEN};
    let text = text.as_ref();
    if local && text.len() <= MAX_APPLY_INPUT_COMMAND_BODY - HEADER - PER_EVENT {
        return UserPasteDelivery::Acknowledged;
    }
    if text.len() > MAX_FRAME_LEN as usize - 4096 {
        return UserPasteDelivery::Refused("the paste is larger than 16 MB");
    }
    if must_order_after {
        return UserPasteDelivery::Refused("an earlier paste to this terminal is unresolved");
    }
    if delivery_fenced {
        return UserPasteDelivery::Refused("delivery to this terminal is fenced");
    }
    UserPasteDelivery::Raw
}

/// Local submission is not server admission or delivery. Receipt correlations
/// retain the journal's original operation identity across reconnect/replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum InputSubmission {
    /// No input-ready execution was available; no work was queued.
    NotQueued,
    /// Nothing was applied; the existing receipt stream carries the reason.
    Refused {
        /// Correlation of the immediate refusal receipt.
        delivery_id: u64,
    },
    /// One raw frame was queued locally, with no delivery guarantee or receipt.
    QueuedRaw,
    /// Journaled locally; the receipt later settles Delivered/Refused/Unknown.
    AwaitingReceipt {
        /// Correlation settled by the existing delivery receipt stream.
        delivery_id: u64,
    },
}

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

#[cfg(test)]
mod paste_policy_tests {
    use super::*;
    use phux_protocol::input::paste::{PasteEvent, PasteTrust};
    use phux_protocol::wire::frame::MAX_APPLY_INPUT_COMMAND_BODY;

    #[test]
    fn borrowed_choice_matches_the_journals_exact_single_paste_bound() {
        let largest = MAX_APPLY_INPUT_COMMAND_BODY - HEADER - PER_EVENT;
        for len in [0, largest - 1, largest, largest + 1, 70 * 1024] {
            let text = "x".repeat(len);
            let admitted = validate_apply_input_batch(&[InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data: text.as_bytes().to_vec(),
            })])
            .is_ok();
            assert_eq!(
                user_paste_delivery(&text, true, false, false) == UserPasteDelivery::Acknowledged,
                admitted,
            );
        }
    }

    #[test]
    fn atomic_raw_fallback_never_overtakes_unresolved_or_unknown_input() {
        let large = "x".repeat(70 * 1024);
        for (text, local) in [(large.as_str(), true), ("satellite paste", false)] {
            assert_eq!(
                user_paste_delivery(text, local, false, false),
                UserPasteDelivery::Raw
            );
            assert!(matches!(
                user_paste_delivery(text, local, true, false),
                UserPasteDelivery::Refused(_)
            ));
            assert!(matches!(
                user_paste_delivery(text, local, false, true),
                UserPasteDelivery::Refused(_)
            ));
        }
    }
}
