//! The single error vocabulary for the recording pipeline, split by what the
//! user should blame.

/// Anything that can go wrong reading, replaying, or exporting a recording.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    /// The output file, the temp cast, or the sink.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// The asciicast input: a bad header, a rejected version, or a malformed
    /// event line.
    #[error("malformed asciicast: {0}")]
    Cast(String),

    /// The offline emulator refused construction, resize, or inspection.
    #[error("terminal replay: {0}")]
    Replay(String),

    /// The GIF or APNG container rejected a frame.
    #[error("encode: {0}")]
    Encode(String),
}
