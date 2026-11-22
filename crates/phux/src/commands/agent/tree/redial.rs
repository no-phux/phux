//! A mirror outlives its server's upgrade: a hot swap closes every
//! connection, and the resumed image keeps the mirror's document under the
//! same id with no producer. The mirror redials and publishes again, which
//! makes it the document's producer once more.

use std::path::Path;
use std::time::{Duration, Instant};

use phux_client::attach::connection::Connection;
use phux_client::tree::{self, Intent, TreeError};
use phux_protocol::ids::ResourceId;

/// The first redial's delay; each failure doubles it up to [`MAX_DELAY`].
const FIRST_DELAY: Duration = Duration::from_millis(250);
const MAX_DELAY: Duration = Duration::from_secs(5);
/// How long the server may stay away before the mirror gives up.
const GIVE_UP_AFTER: Duration = Duration::from_secs(60);

/// Whether `err` lost the connection, so a redial may cure it.
pub(super) const fn lost(err: &TreeError) -> bool {
    matches!(err, TreeError::Protocol(attach) if attach.is_transport_loss())
}

/// Dial `socket` until the server answers, backing off; `None` after
/// [`GIVE_UP_AFTER`] or on Ctrl-C.
#[allow(clippy::future_not_send, reason = "the CLI runtime is single-threaded")]
pub(super) async fn redial(socket: &Path) -> Option<Connection> {
    eprintln!("the server closed the connection; redialing");
    let give_up = Instant::now() + GIVE_UP_AFTER;
    let mut delay = FIRST_DELAY;
    while Instant::now() < give_up {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return None,
            () = tokio::time::sleep(delay) => {}
        }
        if let Ok(conn) = super::connect_socket(socket).await {
            return Some(conn);
        }
        delay = (delay * 2).min(MAX_DELAY);
    }
    None
}

/// Publish `bytes`, redialing and publishing again whenever the connection
/// is lost; the first publish on a new connection re-adopts the document.
///
/// # Errors
///
/// A refusal, or the loss when the server does not come back.
#[allow(clippy::future_not_send, reason = "the CLI runtime is single-threaded")]
pub(super) async fn publish(
    conn: &mut Connection,
    socket: &Path,
    resource: &ResourceId,
    bytes: &[u8],
    intents: &mut Vec<Intent>,
) -> Result<(), TreeError> {
    loop {
        match tree::publish_collecting(conn, resource, bytes.to_vec(), intents).await {
            Err(err) if lost(&err) => *conn = redial(socket).await.ok_or(err)?,
            other => return other.map(drop),
        }
    }
}

#[cfg(test)]
mod tests {
    use phux_client::attach::AttachError;

    use super::*;

    #[test]
    fn only_a_lost_connection_is_redialed() {
        assert!(lost(&TreeError::Protocol(AttachError::Disconnected)));
        assert!(!lost(&TreeError::Protocol(AttachError::Protocol(
            "bad".into()
        ))));
        assert!(!lost(&TreeError::NotProducer("no".into())));
    }
}
