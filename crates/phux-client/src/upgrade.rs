//! Wire primitive for `phux upgrade` (ADR-0032): ask the server to
//! graceful-upgrade itself in place.

use phux_protocol::wire::frame::{Command, CommandResult};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::state::Degradation;

/// What a server said when asked to graceful-upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeAck {
    /// The server acked (or blinked) and is re-execing; panes survive.
    Upgrading,
    /// The server refused, with its reason.
    Refused(String),
    /// The server answered with a frame `upgrade` does not expect.
    Unexpected(String),
}

/// Send `UPGRADE` and classify the reply. The re-exec blink surfaces as
/// `Err(AttachError::Disconnected)`, which callers treat as
/// [`UpgradeAck::Upgrading`].
///
/// # Errors
///
/// Transport and decode failures from [`Connection::request`], including
/// [`AttachError::Disconnected`] for the expected re-exec blink.
pub async fn upgrade(
    conn: &mut Connection,
    request_id: u32,
) -> Result<(UpgradeAck, Degradation), AttachError> {
    let (result, interleaved) = conn
        .request(request_id, Command::Upgrade)
        .await?
        .into_parts();
    let degradation = Degradation::from_interleaved(&interleaved);
    let ack = match result {
        CommandResult::Ok => UpgradeAck::Upgrading,
        CommandResult::Error { message, .. } => UpgradeAck::Refused(message),
        other => UpgradeAck::Unexpected(crate::explain::explain_unexpected("upgrade", &other)),
    };
    Ok((ack, degradation))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn upgrade_reports_the_ack_and_surfaces_interleaved_degradation() {
        use crate::testkit::{ScriptSpec, ScriptedServer};

        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind");
        let spec = ScriptSpec::new().degradation_notice("satellite edge is unreachable: timed out");
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let (ack, degradation) = upgrade(&mut conn, 1)
            .await
            .expect("scripted server answers");
        drop(conn);
        let _ = server.await.expect("scripted server task");

        assert_eq!(ack, UpgradeAck::Upgrading);
        assert!(!degradation.is_complete());
        assert_eq!(
            degradation.notices(),
            ["satellite edge is unreachable: timed out"]
        );
    }
}
