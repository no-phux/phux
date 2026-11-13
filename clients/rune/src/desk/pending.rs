//! [`Pending`]: the question the confirm bar holds.

use phux_protocol::ids::{ResourceId, SessionKey};

use super::resources;

/// What waits on the user's Return or Escape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    /// Explicit consent to start a supervised read-only mirror producer.
    Mirror(resources::Source, String),
    /// A line-breaking paste, and the terminal it was meant for.
    Paste(ResourceId, String),
    /// Closing a terminal that is running a command.
    Close(ResourceId),
    /// Closing every terminal of a tab.
    CloseTab(Vec<ResourceId>),
    /// Ending a session and everything in it.
    CloseSession(SessionKey, String),
    /// A program's clipboard read (OSC 52).
    ClipboardRead(ResourceId, phux_client_core::engine::ClipboardSelection),
}
