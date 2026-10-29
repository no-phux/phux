#![allow(
    clippy::wildcard_imports,
    reason = "wire records intentionally share the parent UniFFI enum namespace"
)]

//! The Swift/Kotlin-facing records and their lowering from
//! [`crate::projection`]. They mirror projected values because the foreign
//! names are a published surface, and `uniffi` derives may not enter the
//! binding-neutral layer (ADR-0135); the mirrors are `From` impls only.

use super::*;

mod agent_state;
mod lowering;
mod paths;
mod wire_event;

pub use agent_state::*;
pub use paths::*;
pub use wire_event::*;

/// Connection status, polled by Swift to drive UI.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireStatus {
    Idle,
    Connecting,
    Attached,
    Closed,
    Failed,
}

impl From<status::Connection> for WireStatus {
    fn from(value: status::Connection) -> Self {
        match value {
            status::Connection::Connecting => Self::Connecting,
            status::Connection::Attached => Self::Attached,
            status::Connection::Closed => Self::Closed,
            status::Connection::Failed => Self::Failed,
        }
    }
}

/// Client-side terminal colours (ADR-0157): exactly sixteen `ansi16`
/// entries (palette 0..=15) plus the default foreground, background, and
/// cursor.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireTerminalTheme {
    pub ansi16: Vec<crate::uniffi::engine::Color>,
    pub foreground: crate::uniffi::engine::Color,
    pub background: crate::uniffi::engine::Color,
    pub cursor: crate::uniffi::engine::Color,
}

impl TryFrom<WireTerminalTheme> for phux_client_runtime::engine::TerminalTheme {
    type Error = WireError;

    fn try_from(theme: WireTerminalTheme) -> Result<Self, WireError> {
        let rgb = |c: crate::uniffi::engine::Color| [c.r, c.g, c.b];
        let ansi16: [crate::uniffi::engine::Color; 16] =
            theme
                .ansi16
                .try_into()
                .map_err(|entries: Vec<_>| WireError::Runtime {
                    reason: format!(
                        "a terminal theme needs 16 ansi16 entries, got {}",
                        entries.len()
                    ),
                })?;
        Ok(Self {
            ansi16: ansi16.map(rgb),
            foreground: rgb(theme.foreground),
            background: rgb(theme.background),
            cursor: rgb(theme.cursor),
        })
    }
}

#[derive(uniffi::Error, Debug, thiserror::Error)]
pub enum WireError {
    #[error("already connected")]
    AlreadyConnected,
    #[error("runtime error: {reason}")]
    Runtime { reason: String },
}

/// Product-facing outcome of one acknowledged input operation.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireInputDeliveryOutcome {
    Delivered,
    Refused,
    Unknown,
}

impl From<outcome::Delivery> for WireInputDeliveryOutcome {
    fn from(value: outcome::Delivery) -> Self {
        match value {
            outcome::Delivery::Delivered => Self::Delivered,
            outcome::Delivery::Refused => Self::Refused,
            outcome::Delivery::Unknown => Self::Unknown,
        }
    }
}

/// The local disposition of one `resize_terminal`. Only `Queued` sent
/// anything; the server's applied geometry arrives in frames.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireResizeOutcome {
    Queued,
    /// An axis was zero. Nothing was sent.
    InvalidSize,
    /// This connection attached as a viewer. Nothing was sent.
    Observer,
    /// No live, confirmed subscription for the pane yet. Nothing was sent.
    NotReady,
    /// Not connected, or the terminal id does not parse.
    Unavailable,
}

impl From<phux_client_runtime::control::TerminalResizeOutcome> for WireResizeOutcome {
    fn from(value: phux_client_runtime::control::TerminalResizeOutcome) -> Self {
        use phux_client_runtime::control::TerminalResizeOutcome as Outcome;
        match value {
            Outcome::Queued => Self::Queued,
            Outcome::InvalidSize => Self::InvalidSize,
            Outcome::Observer => Self::Observer,
            Outcome::NotReady => Self::NotReady,
        }
    }
}

/// One terminal result from the acknowledged-input lane. `delivery_id` is a
/// bridge-local correlation only; the secret wire operation id is never
/// exposed across FFI.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireInputDelivery {
    pub delivery_id: u64,
    pub outcome: WireInputDeliveryOutcome,
    pub code: Option<u16>,
    pub message: String,
}

impl From<outcome::InputDelivery> for WireInputDelivery {
    fn from(value: outcome::InputDelivery) -> Self {
        Self {
            delivery_id: value.delivery_id,
            outcome: value.outcome.into(),
            code: value.code,
            message: value.message,
        }
    }
}

/// Product-facing outcome of one chunked file upload.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireFileUploadOutcome {
    Completed,
    Refused,
    Unknown,
}

impl From<outcome::Upload> for WireFileUploadOutcome {
    fn from(value: outcome::Upload) -> Self {
        match value {
            outcome::Upload::Completed => Self::Completed,
            outcome::Upload::Refused => Self::Refused,
            outcome::Upload::Unknown => Self::Unknown,
        }
    }
}

/// Terminal result for one bridge-managed file upload. `transfer_id` is local;
/// the secret, retry-stable wire upload id never crosses FFI.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireFileUploadReceipt {
    pub transfer_id: u64,
    pub outcome: WireFileUploadOutcome,
    pub path: Option<String>,
    pub code: Option<u16>,
    pub message: String,
}

impl From<outcome::UploadReceipt> for WireFileUploadReceipt {
    fn from(value: outcome::UploadReceipt) -> Self {
        Self {
            transfer_id: value.transfer_id,
            outcome: value.outcome.into(),
            path: value.path,
            code: value.code,
            message: value.message,
        }
    }
}

/// Product-facing outcome of one `TRANSCRIBE` request (phux-ctf).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireTranscribeOutcome {
    /// The server answered: `text` is what it heard, `pasted` says whether it
    /// reached the pane (false for silence).
    Completed,
    /// The server refused: `code`/`message` carry the reason and its remedy.
    Refused,
    /// The connection ended before the server answered. The clip may or may
    /// not have been pasted; read the pane before retrying.
    Unknown,
}

impl From<outcome::Transcribe> for WireTranscribeOutcome {
    fn from(value: outcome::Transcribe) -> Self {
        match value {
            outcome::Transcribe::Completed => Self::Completed,
            outcome::Transcribe::Refused => Self::Refused,
            outcome::Transcribe::Unknown => Self::Unknown,
        }
    }
}

/// Terminal result for one `TRANSCRIBE` request. `transfer_id` is the local
/// upload handle the request was made for; `request_id` the wire correlation.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireTranscribeReceipt {
    pub request_id: u32,
    pub transfer_id: u64,
    pub outcome: WireTranscribeOutcome,
    pub text: Option<String>,
    pub pasted: bool,
    pub code: Option<u16>,
    pub message: String,
}

impl From<outcome::TranscribeResult> for WireTranscribeReceipt {
    fn from(value: outcome::TranscribeResult) -> Self {
        Self {
            request_id: value.request_id,
            transfer_id: value.transfer_id,
            outcome: value.outcome.into(),
            text: value.text,
            pasted: value.pasted,
            code: value.code,
            message: value.message,
        }
    }
}

/// Touch-producible mouse actions, mirrored for FFI (ADR-0024: the wire owns
/// the atoms; these are projections).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

impl MouseAction {
    pub(super) fn wire(self) -> WireMouseAction {
        match self {
            Self::Press => WireMouseAction::Press,
            Self::Release => WireMouseAction::Release,
            Self::Motion => WireMouseAction::Motion,
        }
    }
}

/// Touch-producible mouse buttons. Wheel maps to the xterm wheel buttons so
/// a two-finger scroll can drive TUIs with mouse tracking.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    WheelUp,
    WheelDown,
}

impl MouseButton {
    pub(super) fn wire(self) -> WireMouseButton {
        match self {
            Self::Left => WireMouseButton::Left,
            Self::Right => WireMouseButton::Right,
            Self::Middle => WireMouseButton::Middle,
            Self::WheelUp => WireMouseButton::Four,
            Self::WheelDown => WireMouseButton::Five,
        }
    }
}

#[cfg(test)]
mod tests;
