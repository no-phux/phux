//! Connection-scoped stream ids and the 16-byte operation identifiers.

/// Non-zero, connection-scoped identifier for one logical terminal
/// subscription, allocated by the originating endpoint. Relays map it rather
/// than reuse it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct StreamId(core::num::NonZeroU64);

impl StreamId {
    /// Construct a stream identifier, returning `None` for the reserved zero value.
    #[must_use]
    pub const fn new(raw: u64) -> Option<Self> {
        match core::num::NonZeroU64::new(raw) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Return the non-zero wire value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl core::fmt::Display for StreamId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "StreamId({})", self.get())
    }
}

/// Non-zero identifier for one terminal replica generation. Every bootstrap
/// gets a new one; no frame may carry a tombstoned generation's id.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct BootstrapId(core::num::NonZeroU64);

impl BootstrapId {
    /// Construct a bootstrap identifier, returning `None` for the reserved zero value.
    #[must_use]
    pub const fn new(raw: u64) -> Option<Self> {
        match core::num::NonZeroU64::new(raw) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// Return the non-zero wire value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl core::fmt::Display for BootstrapId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "BootstrapId({})", self.get())
    }
}

/// Whether a 16-byte id is not the reserved all-zero value.
pub(super) const fn is_nonzero(bytes: &[u8; 16]) -> bool {
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != 0 {
            return true;
        }
        index += 1;
    }
    false
}

/// Opaque, non-zero, client-generated id for one acknowledged input
/// operation. Debug is redacted: ids correlate with sensitive input.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct InputOperationId([u8; 16]);

impl InputOperationId {
    /// Construct a non-zero operation identifier.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Option<Self> {
        if is_nonzero(&bytes) {
            Some(Self(bytes))
        } else {
            None
        }
    }

    /// Borrow the 16-byte wire representation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl core::fmt::Debug for InputOperationId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("InputOperationId(<redacted>)")
    }
}

/// Opaque, non-zero, CSPRNG-drawn key that makes one create idempotent
/// (`SPAWN_RESOURCE` field 17, ADR-0126; `EVENT.operation_id`). Debug is
/// redacted.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct IdempotencyKey([u8; 16]);

impl IdempotencyKey {
    /// Canonical lowercase text for explicit receipts and recovery documents.
    /// Ordinary debug formatting remains redacted.
    #[must_use]
    pub fn to_hex(self) -> String {
        use core::fmt::Write as _;
        let mut text = String::with_capacity(32);
        for byte in self.0 {
            // Formatting a primitive into a String cannot fail.
            let _ = write!(text, "{byte:02x}");
        }
        text
    }

    /// Construct a non-zero key.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Option<Self> {
        if is_nonzero(&bytes) {
            Some(Self(bytes))
        } else {
            None
        }
    }

    /// Borrow the 16-byte wire representation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl core::fmt::Debug for IdempotencyKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("IdempotencyKey(<redacted>)")
    }
}

/// Opaque, non-zero, client-generated id for one chunked upload. It names
/// the partial file across reconnects, so a retried chunk is idempotent.
/// Debug is redacted.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileUploadId([u8; 16]);

impl FileUploadId {
    /// Construct a non-zero upload identifier.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Option<Self> {
        if is_nonzero(&bytes) {
            Some(Self(bytes))
        } else {
            None
        }
    }

    /// Borrow the 16-byte wire representation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl core::fmt::Debug for FileUploadId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("FileUploadId(<redacted>)")
    }
}

/// Server-minted, non-zero id of one held action awaiting approval
/// (ADR-0128); text form is 32 lowercase hex digits. It names a request, not
/// an authority: a decision is authorized by the decider's grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApprovalId([u8; 16]);

impl ApprovalId {
    /// Construct a non-zero approval id.
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Option<Self> {
        if is_nonzero(&bytes) {
            Some(Self(bytes))
        } else {
            None
        }
    }

    /// Borrow the 16-byte wire representation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// The approval a canonical `phux.approval.decide/v1/<id>` key names.
    #[must_use]
    pub fn from_decide_key(key: &str) -> Option<Self> {
        key.strip_prefix(crate::wire::frame::APPROVAL_DECIDE_KEY_PREFIX)
            .and_then(Self::parse)
    }

    /// The server-owned record key, `phux.approval/v1/<id>`.
    #[must_use]
    pub fn record_key(&self) -> String {
        format!("{}{self}", crate::wire::frame::APPROVAL_KEY_PREFIX)
    }

    /// The decision key, `phux.approval.decide/v1/<id>`.
    #[must_use]
    pub fn decide_key(&self) -> String {
        format!("{}{self}", crate::wire::frame::APPROVAL_DECIDE_KEY_PREFIX)
    }

    /// Parse the text form: exactly 32 lowercase hex digits, not all zero.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.as_bytes();
        if digits.len() != 32 {
            return None;
        }
        let mut bytes = [0_u8; 16];
        for (byte, pair) in bytes.iter_mut().zip(digits.as_chunks::<2>().0) {
            *byte = (lower_hex_value(pair[0])? << 4) | lower_hex_value(pair[1])?;
        }
        Self::new(bytes)
    }
}

/// The value of one lowercase hex digit.
const fn lower_hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

impl core::fmt::Display for ApprovalId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
