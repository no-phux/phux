//! Opaque checkpoint capabilities, limits and failures; no checkpoint codec.

pub const TOKEN_LEN: usize = 32;
pub type OpaqueHistoryCursor = [u8; TOKEN_LEN];
pub const MAX_NATIVE_PREFIX_CHUNKS: usize = 4_096;
pub const MAX_NATIVE_PREFIX_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_NATIVE_BOOTSTRAP_FRAMES: usize = MAX_NATIVE_PREFIX_CHUNKS + 2;

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum NativeStateError {
    #[error("native checkpoint feature unsupported")]
    UnsupportedFeature,
    #[error("unknown checkpoint version")]
    UnknownVersion,
    #[error("checkpoint corrupted")]
    Corruption,
    #[error("checkpoint truncated")]
    Truncated,
    #[error("native limit exceeded")]
    LimitExceeded,
    #[error("history cursor is stale")]
    Stale,
    #[error("requested history was pruned")]
    Pruned,
    #[error("history generation mismatch")]
    WrongGeneration,
    #[error("operation used the wrong terminal")]
    WrongTerminal,
    #[error("invalid native handle")]
    InvalidHandle,
    #[error("history import already active")]
    ImportBusy,
    #[error("out of memory")]
    OutOfMemory,
    #[error("out of space: {required_bytes} bytes, {required_rows} rows")]
    OutOfSpace {
        required_bytes: usize,
        required_rows: usize,
    },
    #[error("invalid native state")]
    InvalidState,
    #[error("continuation unavailable")]
    ContinuationUnavailable,
    #[error("terminal reset invalidated the history generation")]
    Reset,
    #[error("terminal resize invalidated the history generation")]
    Resize,
}
