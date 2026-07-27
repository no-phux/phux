//! The single error vocabulary for the recording pipeline.
//!
//! Five variants, one per boundary the pipeline actually crosses. The split
//! is by *who the user should blame*, not by which function failed: an
//! [`RecordError::Io`] is the disk or the socket, a [`RecordError::Cast`] is
//! the input file, a [`RecordError::Replay`] is the emulator, an
//! [`RecordError::Encode`] is the image container, and a
//! [`RecordError::Unsupported`] is phux telling the user it will not do the
//! thing they asked for.
//!
//! [`RecordError::Unsupported`] is a **permanent** variant, not scaffolding:
//! asciicast v1 input, a format the build's feature flags excluded, and any
//! other deliberate refusal land there for good.

/// Anything that can go wrong reading, replaying, or exporting a recording.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    /// Underlying I/O failure: the output file, the temp cast, the sink.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// The asciicast input is not one we can read: a bad header, a version
    /// we reject, or a malformed event line.
    #[error("malformed asciicast: {0}")]
    Cast(String),

    /// The offline terminal emulator refused an operation. libghostty's
    /// `vt_write` is infallible by contract, so this is construction,
    /// resize, or snapshot inspection — never "your bytes were bad".
    #[error("terminal replay: {0}")]
    Replay(String),

    /// The GIF or APNG container rejected a frame, or the palette could not
    /// be built.
    #[error("encode: {0}")]
    Encode(String),

    /// A deliberate refusal: asciicast v1, a disabled feature, an output
    /// format this build cannot produce.
    #[error("unsupported: {0}")]
    Unsupported(String),
}
