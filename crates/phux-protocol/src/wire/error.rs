//! Wire decode errors.
//!
//! Owned by phux-6yl.4. See `docs/spec/proto.md` §5 and `docs/spec/appendix-encoding.md`.

use thiserror::Error;

/// Errors that can occur while decoding a wire frame.
///
/// Every variant corresponds to a malformed-input condition that the decoder
/// MUST surface without panicking. See `docs/spec/proto.md` §5 (framing) and Appendix A
/// (encoding primitives).
/// `PartialEq` is implemented but **not** `Eq` because
/// [`Self::MalformedLayoutRatio`] carries an `f32`. Two `MalformedLayoutRatio`
/// errors compare equal iff their ratios are bitwise-`PartialEq` (NaN errors
/// never compare equal to each other — desired: tests assert exact-decoding
/// behavior on finite/out-of-range ratios separately from NaN).
#[derive(Debug, Error, PartialEq)]
#[non_exhaustive]
pub enum DecodeError {
    /// The input buffer was exhausted before the decoder finished reading the
    /// expected number of bytes for a primitive, field, or frame.
    #[error("unexpected end of input")]
    UnexpectedEof,

    /// A length-prefixed string field did not contain valid UTF-8.
    #[error("invalid UTF-8 in string field")]
    InvalidUtf8,

    /// The frame's type byte did not match any known `FrameKind` discriminant.
    #[error("unknown frame kind: 0x{tag:04x}")]
    UnknownFrameKind {
        /// The unrecognised discriminant. Stored as `u16` so future minor
        /// versions can extend the space without changing the error shape.
        tag: u16,
    },

    /// A declared length prefix would, if accepted, exceed the remaining
    /// buffer or the protocol's hard frame-size cap of 16 MiB (`docs/spec/proto.md` §5).
    #[error("declared length exceeds buffer or protocol cap")]
    LengthOverflow,

    /// A `SessionId` carried the `SATELLITE` tag from ADR-0007 §3. v0.1
    /// decoders accept only `LOCAL`; satellite routing arrives in v0.2+.
    #[error("satellite-routed session ids are not supported in this protocol version")]
    UnsupportedSatelliteRoute,

    /// An enumerated field on the wire carried a value the decoder does not
    /// recognise. Used for libghostty atoms (`Key`, `KeyAction`, `MouseAction`,
    /// `MouseButton`) where minor protocol versions MAY add values; v0.1
    /// rejects unknown discriminants so that misinterpretation can't silently
    /// corrupt downstream encoding.
    #[error("unknown enum discriminant in field '{field}': {value}")]
    UnknownEnumValue {
        /// Logical name of the field that carried the bad value, for diagnostics.
        field: &'static str,
        /// The unrecognised discriminant, widened to `u32` to cover every
        /// enumerated type the codec emits.
        value: u32,
    },

    /// An acknowledged-input operation id used the reserved all-zero value.
    #[error("input operation id must not be zero")]
    InvalidInputOperationId,

    /// An `APPLY_INPUT` command exceeded its event-count or command-body
    /// limit and was rejected before allocating the event vector.
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
    /// A native history status used zero rows where forbidden or exceeded the
    /// protocol row bound.
    #[error("history page rows violate protocol limits")]
    HistoryRowLimitExceeded,

    /// Bootstrap/history negotiation or payload exceeded protocol hard bounds.
    #[error("bootstrap or history payload exceeds protocol limits")]
    BootstrapLimitExceeded,

    /// A native/compatibility profile sub-record used an invalid combination.
    #[error("invalid bootstrap profile")]
    InvalidBootstrapProfile,

    /// A [`crate::wire::info::LayoutNode`] tree nested deeper than the
    /// decoder's recursion bound (see
    /// [`crate::wire::info::MAX_LAYOUT_DEPTH`]).
    ///
    /// The codec is recursive; without a bound, attacker-controlled bytes
    /// describing a pathologically deep split tree would overflow the stack
    /// and abort the process. Real layouts nest only a handful of levels, so
    /// the bound is far above any legitimate value. Surfacing this as a clean
    /// decode error keeps a malformed `ATTACHED` / `COMMAND_RESULT` from
    /// crashing the peer.
    #[error("layout tree nested deeper than the decoder bound")]
    LayoutTooDeep,

    /// A [`crate::wire::info::LayoutNode::Split`] carried a `ratio` outside
    /// the closed interval `[0.0, 1.0]` or one that was NaN / infinite.
    ///
    /// SPEC §13 leaves layout-tree ratios implicit; phux validates on decode
    /// to reject values that would round-trip but produce nonsense layouts.
    /// See `phux_core::window::Window::split`, which applies the same
    /// validation on the core side.
    #[error("malformed layout ratio: {ratio}")]
    MalformedLayoutRatio {
        /// The offending ratio value (NaN, infinite, or out of `[0.0, 1.0]`).
        ratio: f32,
    },
}
