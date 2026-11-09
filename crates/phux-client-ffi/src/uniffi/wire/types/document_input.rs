//! Input to a document pane: the intent a phone sends and how it ended.

use phux_protocol::wire::frame::{DocumentInput, DocumentKey};

/// A named key for [`WireDocumentIntent::Key`]: a small closed set.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireDocumentKey {
    Enter,
    Tab,
    Backspace,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
}

/// One input intent for a document's producer. Points and deltas are in the
/// published frame's own pixels: map a touch with `document_frame_point`
/// and a pan with `document_scroll`.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum WireDocumentIntent {
    /// Load a URL.
    Navigate { url: String },
    /// Click at a frame pixel.
    Click { x: u32, y: u32 },
    /// Insert text at the focus.
    Type { text: String },
    /// Press and release one key.
    Key { key: WireDocumentKey },
    /// Scroll by frame pixels; positive `dy` reveals what is below.
    Scroll { dx: i32, dy: i32 },
    /// Put `text` (at most 1 MiB) on the host clipboard and press the
    /// host's paste chord; a page inserts it.
    Paste { text: String },
    /// Read the host clipboard; the text comes back as
    /// [`WireDocumentInputOutcome::Copied`].
    CopyClipboard,
}

/// How a `send_document_input` ended.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum WireDocumentInputOutcome {
    /// The producer has the intent.
    Sent,
    /// The host clipboard's text, the answer to a `CopyClipboard`.
    Copied { text: String },
    /// This device holds no control grant for the document's class.
    NoGrant,
    /// The document's producer is not connected.
    NoProducer,
    /// The document has no control class, so it takes no pane input.
    NotControllable,
    /// A transient failure; input stays on.
    Other { message: String },
}

/// A frame pixel.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct WireFramePoint {
    pub x: u32,
    pub y: u32,
}

impl From<WireDocumentIntent> for DocumentInput {
    fn from(intent: WireDocumentIntent) -> Self {
        match intent {
            WireDocumentIntent::Navigate { url } => Self::Navigate { url },
            WireDocumentIntent::Click { x, y } => Self::Click { x, y },
            WireDocumentIntent::Type { text } => Self::Type { text },
            WireDocumentIntent::Key { key } => Self::Key {
                key: match key {
                    WireDocumentKey::Enter => DocumentKey::Enter,
                    WireDocumentKey::Tab => DocumentKey::Tab,
                    WireDocumentKey::Backspace => DocumentKey::Backspace,
                    WireDocumentKey::Escape => DocumentKey::Escape,
                    WireDocumentKey::ArrowUp => DocumentKey::ArrowUp,
                    WireDocumentKey::ArrowDown => DocumentKey::ArrowDown,
                    WireDocumentKey::ArrowLeft => DocumentKey::ArrowLeft,
                    WireDocumentKey::ArrowRight => DocumentKey::ArrowRight,
                },
            },
            WireDocumentIntent::Scroll { dx, dy } => Self::Scroll { dx, dy },
            WireDocumentIntent::Paste { text } => Self::Paste { text, chord: true },
            WireDocumentIntent::CopyClipboard => Self::CopyClipboard { reply: 0 },
        }
    }
}
