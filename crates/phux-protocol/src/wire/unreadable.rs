//! A spawn or move result this build cannot read, kept for diagnostics.

/// A `RESOURCE_SPAWNED` or `RESOURCE_MOVED` result this build cannot read,
/// with the exact wire bytes it arrived as.
///
/// A newer server's result (an unknown result or error tag, a payload this
/// build does not know how to read) settles the request with the outcome not
/// stated. The bytes are for diagnostics only and are never sent on: an
/// unreadable result encodes as the reserved result tag `255` ("outcome not
/// stated"), so a hub relaying one can never forward a satellite's
/// satellite-local ids to a client that would read them as the hub's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableResult {
    bytes: Vec<u8>,
}

impl UnreadableResult {
    /// The result exactly as it arrived on the wire: its result tag, then
    /// whatever followed. Encoding writes the reserved tag `255`, never
    /// these bytes.
    #[must_use]
    pub fn from_wire(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into(),
        }
    }

    /// The raw result bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The tag this build could not place, for diagnostics: the error tag
    /// beside an `Err` (`1`) result tag, otherwise the result tag.
    #[must_use]
    pub const fn tag(&self) -> u8 {
        match self.bytes.as_slice() {
            [1, error, ..] => *error,
            [tag, ..] => *tag,
            [] => 0,
        }
    }
}
