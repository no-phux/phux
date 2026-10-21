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

    /// A well-framed `COMMAND` whose body this build does not understand:
    /// an unknown command tag, or an unknown enum value inside a known
    /// command. Carries the frame's `request_id`, so a receiver answers with
    /// a correlated `Unsupported` error and keeps the connection.
    #[error("unsupported command (request {request_id}): unknown '{field}' value {value}")]
    UnsupportedCommand {
        /// The `COMMAND` frame's request id.
        request_id: u32,
        /// Logical name of the unrecognised field (`Command` for the tag).
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

    /// A native request exceeded its JSON byte bound before deserialization.
    #[error("NATIVE_AGENT payload exceeds protocol limits")]
    NativeAgentLimitExceeded,
    /// A native request was not valid JSON matching the typed DTO.
    #[error("invalid NATIVE_AGENT JSON payload")]
    InvalidNativeAgentPayload,

    /// A `CHECKOUT` request or answer exceeded its JSON byte bound.
    #[error("CHECKOUT payload exceeds protocol limits")]
    CheckoutLimitExceeded,
    /// A `CHECKOUT` request or answer was not JSON matching the typed DTO.
    #[error("invalid CHECKOUT JSON payload")]
    InvalidCheckoutPayload,

    /// A well-formed JSON payload named an operation, variant, or field this
    /// build does not know: newer, not malformed. A receiver refuses it
    /// with `Unsupported` and keeps the connection.
    #[error("unsupported {payload} payload: unknown operation or field")]
    UnsupportedPayload {
        /// The payload's DTO, for diagnostics.
        payload: &'static str,
    },

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

    /// An organization body exceeded a protocol bound: a name over 256
    /// bytes, an arrangement over 64 KiB, or a list, cursor, or restore
    /// record over its cap (`docs/spec/L1.md` §5.7).
    #[error("organization body exceeds protocol limits")]
    OrganizationLimitExceeded,

    /// An organization key was not 16 bytes, or was all zero.
    #[error("organization key must be 16 non-zero bytes")]
    InvalidOrganizationKey,

    /// An arrangement document had a split ratio outside `(0, 1)`, a bad
    /// satellite host, or two owners.
    #[error("arrangement document is malformed")]
    InvalidArrangement,
}

impl DecodeError {
    /// Whether this is a value this build does not know in an otherwise
    /// well-framed message (an unknown frame type, tag, enum value, presence
    /// byte, or JSON operation or field), which a newer 1.x peer may send:
    /// refuse it as `Unsupported` and keep the connection. Every other
    /// error is malformed input.
    #[must_use]
    pub const fn is_unsupported(&self) -> bool {
        matches!(
            self,
            Self::UnknownFrameKind { .. }
                | Self::UnknownEnumValue { .. }
                | Self::UnsupportedCommand { .. }
                | Self::UnsupportedPayload { .. }
        )
    }

    /// [`Self::UnknownEnumValue`] for `field` carrying `value`.
    pub(crate) fn unknown_enum(field: &'static str, value: impl Into<u32>) -> Self {
        Self::UnknownEnumValue {
            field,
            value: value.into(),
        }
    }

    /// Promote an unknown enum value inside a `COMMAND` body to
    /// [`Self::UnsupportedCommand`] for `request_id`; every other error is
    /// malformed input and stays as it is.
    pub(crate) const fn into_unsupported_command(self, request_id: u32) -> Self {
        match self {
            Self::UnknownEnumValue { field, value } => Self::UnsupportedCommand {
                request_id,
                field,
                value,
            },
            other => other,
        }
    }
}

/// Whether a `serde_json` error message names an unknown variant or field:
/// a well-formed payload from a newer peer, as opposed to bad JSON, a
/// missing field, or a mistyped value. `serde`'s `unknown_variant` and
/// `unknown_field` messages open with these words; a test pins them.
pub(crate) fn names_unknown_json(message: &str) -> bool {
    message.starts_with("unknown variant") || message.starts_with("unknown field")
}
