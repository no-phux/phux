//! Portable settlement evidence, independent of transport queue acceptance.
use phux_protocol::ResourceId;
/// Terminal-creation settlement, distinct from local submission acceptance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalCreationOutcome {
    /// The host returned a live target; this is not proof of child progress.
    LiveTarget { terminal_id: ResourceId },
    /// The host or local preflight proved the request was not admitted.
    Refused { message: String },
    /// Receipt or process evidence is missing. Never retry as a fresh create.
    Unknown { message: String },
}

/// How one acknowledged input operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The server acknowledged the write.
    Delivered,
    /// Nothing was written; retyping is safe.
    Refused,
    /// Some, all, or none of the bytes may have landed; read the pane
    /// before retyping.
    Unknown,
}
