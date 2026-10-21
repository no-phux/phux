//! A spawn or move result this build cannot read, kept verbatim for relaying.

/// A `RESOURCE_SPAWNED` or `RESOURCE_MOVED` result this build cannot read,
/// kept as its exact wire bytes.
///
/// A newer server's result (an unknown result or error tag, a payload this
/// build does not know how to read) settles the request with the outcome not
/// stated, and a hub relaying it re-encodes these bytes verbatim, outer tag
/// and payload included, so a newer client downstream reads what the
/// satellite said. Encoding one only ever echoes the bytes it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableResult {
    bytes: Vec<u8>,
}

impl UnreadableResult {
    /// The result exactly as it arrived on the wire: its result tag, then
    /// whatever followed. Encoding writes these bytes back unchanged.
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
