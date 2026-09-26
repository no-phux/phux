//! Session-local presentation identity, independent of terminal execution.

#[cfg(feature = "engine")]
use std::sync::atomic::{AtomicU64, Ordering};

/// Opaque identity for an independent presentation of a terminal.
///
/// Allocated identities are never reused, including across reconnects and
/// separate clients. Cloning a client does not allocate a view. Views use the
/// terminal's canonical geometry; creating one never attaches or resizes a PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ViewId(u64);

impl ViewId {
    /// Encode this opaque identity across a language-binding boundary.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Decode a binding's handle. This does not establish that the view is
    /// live or belongs to a client; the engine validates both on every request.
    #[must_use]
    pub const fn from_raw(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    #[cfg(feature = "engine")]
    pub(crate) fn allocate() -> Option<Self> {
        allocate_handle().map(Self)
    }
}

/// View, document, and gesture handles share a non-reusing process-local
/// allocator so callbacks from a replaced owner cannot alias new handles.
#[cfg(feature = "engine")]
#[allow(
    clippy::redundant_pub_crate,
    reason = "internal allocator must not be exported; unreachable_pub rejects pub in this private module"
)]
pub(crate) fn allocate_handle() -> Option<u64> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .ok()
}
