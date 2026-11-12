//! Paste input: `PasteEvent` and the caller's `PasteTrust` claim, which the
//! server uses to gate `paste::is_safe` before `paste::encode` (SPEC §9.4,
//! ADR-0008).

#![allow(clippy::module_name_repetitions)]

/// Trust classification for a paste payload: `Untrusted` is safety-checked
/// and rejected, sanitized, or allowed per server policy.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteTrust {
    /// Caller asserts the payload is safe.
    Trusted = 0,
    /// Caller cannot vouch for safety.
    Untrusted = 1,
}

/// A paste event from a client (SPEC §9.4). Bracketing is the server's call
/// from the pane's DEC 2004 state. `Debug` never prints the payload
/// (ADR-0028): clipboards carry secrets.
#[derive(Clone, PartialEq, Eq)]
pub struct PasteEvent {
    /// Trust classification for `data`.
    pub trust: PasteTrust,
    /// Raw paste payload; not necessarily UTF-8.
    pub data: Vec<u8>,
}

/// Clipboard text a client answers a pane's OSC 52 read with
/// (`CLIPBOARD_REPLY`). `Debug` never prints it (ADR-0028).
#[derive(Clone, PartialEq, Eq)]
pub struct ClipboardData(pub String);

impl core::fmt::Debug for ClipboardData {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ClipboardData")
            .field("len", &self.0.len())
            .finish()
    }
}

impl core::fmt::Debug for PasteEvent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PasteEvent")
            .field("trust", &self.trust)
            .field("data_len", &self.data.len())
            .finish()
    }
}
