//! Wire decode errors (`docs/spec/proto.md` §5, `appendix-encoding.md`).

use thiserror::Error;

/// A malformed-input condition the decoder surfaces instead of panicking.
#[derive(Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum DecodeError {
    /// Input ended mid-primitive, mid-field, or before a required field.
    #[error("unexpected end of input")]
    UnexpectedEof,

    /// A length-prefixed string field did not contain valid UTF-8.
    #[error("invalid UTF-8 in string field")]
    InvalidUtf8,

    /// A `FRAME_COMPRESSED` envelope was not valid DEFLATE, did not inflate to
    /// exactly its declared length, or was nested.
    #[error("compressed frame did not inflate to its declared length")]
    CompressedFrameInvalid,

    /// The frame's type byte did not match any known `FrameKind` discriminant.
    #[error("unknown frame kind: 0x{tag:04x}")]
    UnknownFrameKind {
        /// The unrecognised discriminant.
        tag: u16,
    },

    /// A declared length exceeds the buffer or the 16 MiB frame cap
    /// (`docs/spec/proto.md` §5).
    #[error("declared length exceeds buffer or protocol cap")]
    LengthOverflow,

    /// An enumerated field carried a value this decoder does not recognise.
    #[error("unknown enum discriminant in field '{field}': {value}")]
    UnknownEnumValue {
        /// Logical name of the field, for diagnostics.
        field: &'static str,
        /// The unrecognised discriminant, widened to `u32`.
        value: u32,
    },

    /// An acknowledged-input operation id used the reserved all-zero value.
    #[error("input operation id must not be zero")]
    InvalidInputOperationId,

    /// An idempotency key was not 16 non-zero bytes.
    #[error("idempotency key must be 16 non-zero bytes")]
    InvalidIdempotencyKey,

    /// An `APPLY_INPUT` command exceeded its event-count or body limit.
    #[error("APPLY_INPUT batch exceeds protocol limits")]
    ApplyInputLimitExceeded,

    /// An `INPUT_TERMINAL_REPLY` payload was empty or exceeded its byte limit.
    #[error("INPUT_TERMINAL_REPLY payload exceeds protocol limits")]
    InputTerminalReplyLimitExceeded,

    /// A file-upload id used the reserved all-zero value.
    #[error("file upload id must not be zero")]
    InvalidFileUploadId,

    /// A `PUT_FILE` command exceeded its per-chunk or whole-file limit.
    #[error("PUT_FILE chunk exceeds protocol limits")]
    FileUploadLimitExceeded,
    /// A stream id used the reserved all-zero value.
    #[error("stream id must not be zero")]
    InvalidStreamId,

    /// A bootstrap id used the reserved all-zero value.
    #[error("bootstrap id must not be zero")]
    InvalidBootstrapId,
    /// A history page used the reserved all-zero sequence value.
    #[error("history page sequence must not be zero")]
    InvalidHistoryPageSequence,
    /// A history status used zero rows where forbidden or exceeded the bound.
    #[error("history page rows violate protocol limits")]
    HistoryRowLimitExceeded,

    /// Bootstrap/history negotiation or payload exceeded protocol hard bounds.
    #[error("bootstrap or history payload exceeds protocol limits")]
    BootstrapLimitExceeded,

    /// A native/compatibility profile sub-record used an invalid combination.
    #[error("invalid bootstrap profile")]
    InvalidBootstrapProfile,

    /// An `APPEND_RESOURCE_OUTPUT` payload was empty or exceeded
    /// [`MAX_APPEND_BYTES`](crate::wire::frame::MAX_APPEND_BYTES).
    #[error("APPEND_RESOURCE_OUTPUT payload exceeds protocol limits")]
    AppendResourceOutputLimitExceeded,

    /// A `SPAWN_RESOURCE` body broke its kind's field rules
    /// (`docs/spec/L1.md` §3.1).
    #[error(
        "SPAWN_RESOURCE field {field} violates the field rules for kind {kind} (required: {required})"
    )]
    InvalidSpawnForKind {
        /// Wire tag of the spawn's `kind`.
        kind: u8,
        /// The field id that was missing (`required`) or present (forbidden).
        field: u32,
        /// `true` when required and absent; `false` when present but forbidden.
        required: bool,
    },

    /// A `SPAWN_RESOURCE` `provider` or `native_id` was empty or too long.
    #[error("SPAWN_RESOURCE agent facet string exceeds protocol limits")]
    AgentFacetLimitExceeded,

    /// A `DIRECTORY_LISTING` declared more than
    /// [`MAX_DIRECTORY_ENTRIES`](crate::wire::frame::MAX_DIRECTORY_ENTRIES).
    #[error("DIRECTORY_LISTING entry count exceeds protocol limits")]
    DirectoryEntryLimitExceeded,

    /// A `PATH_RESULTS` reply declared more than `MAX_PATH_RESULTS` rows.
    #[error("PATH_RESULTS row count exceeds protocol limits")]
    PathResultLimitExceeded,

    /// A tree in the retired `WindowInfo` layout slot nested deeper than
    /// [`MAX_LAYOUT_DEPTH`](crate::wire::info::MAX_LAYOUT_DEPTH), which would
    /// otherwise overflow the stack.
    #[error("layout tree nested deeper than the decoder bound")]
    LayoutTooDeep,
}

impl DecodeError {
    /// [`Self::UnknownEnumValue`] for `field` carrying `value`.
    pub(crate) fn unknown_enum(field: &'static str, value: impl Into<u32>) -> Self {
        Self::UnknownEnumValue {
            field,
            value: value.into(),
        }
    }
}
