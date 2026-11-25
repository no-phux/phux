//! Opening a pane's link on the client's machine (`features_ext.OPEN_URL`,
//! `docs/ssh.md#opening-links`).
//!
//! A program in a pane sends `OPEN_URL`. The server forwards it as an
//! `open_url` event to one client only: the attached one that last typed
//! into the pane, if it offered `OPEN_URL` at `HELLO`. That client asks its
//! user and answers with `OPEN_URL_REPLY`. The server then answers the
//! request: `OK` when opened, `UNAVAILABLE` when declined or unanswered for
//! [`OPEN_URL_SECS`], `CLIENT_NOT_FOUND` with no such client,
//! `RESOURCE_EXHAUSTED` when the pane already has a request waiting or the
//! server holds too many, and `INVALID_COMMAND` for anything but an absolute
//! `http` or `https` link with a host, no user name or password, and at most
//! [`MAX_OPEN_URL_BYTES`].

/// The longest link, as sent and as normalized.
pub const MAX_OPEN_URL_BYTES: usize = 8 * 1024;
/// How long the server holds a request for the client's answer. No answer is
/// a decline.
pub const OPEN_URL_SECS: u64 = 60;

/// One step of opening a link ([`Command::OpenUrl`](crate::wire::frame::Command::OpenUrl)); each has its own
/// command tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenUrlOp {
    /// `OPEN_URL`: ask the person typing into the Terminal.
    Request {
        /// The link.
        url: String,
    },
    /// `OPEN_URL_REPLY`: the answer to one `open_url` event.
    Reply {
        /// The request the event named.
        id: u64,
        /// The user agreed and the link was handed to the client's opener.
        opened: bool,
    },
}
