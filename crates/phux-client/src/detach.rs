//! `DETACH_CLIENTS` wire primitive for `phux detach` — force-detaching
//! clients from *outside* the attach UI (distinct from `FrameKind::Detach`,
//! which only detaches the sending connection).

use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::state::Degradation;

/// What a `DETACH_CLIENTS` request answered.
#[derive(Debug, Clone, PartialEq)]
pub enum DetachOutcome {
    /// The server detached this many clients.
    Detached(u64),
    /// The reply's count was not a parseable integer — a malformed reply,
    /// not "0 clients detached". Carries the raw string for the caller's
    /// diagnostic.
    Malformed(String),
    /// The server refused.
    Refused(String),
    /// An unexpected reply shape (the contract is `OkWith(Json(count))`; a
    /// bare `Ok` or anything else cannot confirm what happened).
    Unexpected(CommandResult),
}

impl DetachOutcome {
    /// Classify a `DETACH_CLIENTS` `COMMAND_RESULT`.
    #[must_use]
    pub fn from_result(result: CommandResult) -> Self {
        match result {
            CommandResult::OkWith(CommandValue::Json(count)) => count
                .trim()
                .parse::<u64>()
                .map_or_else(|_| Self::Malformed(count.clone()), Self::Detached),
            CommandResult::Error { message, .. } => Self::Refused(message),
            other => Self::Unexpected(other),
        }
    }
}

/// Send `DETACH_CLIENTS` for `session` (every attached client with `None`)
/// over `conn` and classify the reply, with any interleaved per-satellite
/// unreachability notices as the [`Degradation`].
///
/// # Errors
///
/// Transport and decode failures from [`Connection::request`].
pub async fn detach_clients(
    conn: &mut Connection,
    request_id: u32,
    session: Option<String>,
) -> Result<(DetachOutcome, Degradation), AttachError> {
    let (result, interleaved) = conn
        .request(request_id, Command::DetachClients { session })
        .await?
        .into_parts();
    Ok((
        DetachOutcome::from_result(result),
        Degradation::from_interleaved(&interleaved),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_every_reply_shape() {
        assert_eq!(
            DetachOutcome::from_result(CommandResult::OkWith(CommandValue::Json("3".to_owned()))),
            DetachOutcome::Detached(3)
        );
        assert_eq!(
            DetachOutcome::from_result(CommandResult::OkWith(CommandValue::Json(
                "not-a-number".to_owned()
            ))),
            DetachOutcome::Malformed("not-a-number".to_owned())
        );
        assert_eq!(
            DetachOutcome::from_result(CommandResult::Error {
                code: phux_protocol::wire::frame::ErrorCode::PermissionDenied,
                message: "no".to_owned(),
            }),
            DetachOutcome::Refused("no".to_owned())
        );
        assert!(matches!(
            DetachOutcome::from_result(CommandResult::Ok),
            DetachOutcome::Unexpected(CommandResult::Ok)
        ));
    }

    /// The session rides the frame verbatim, and an interleaved degradation
    /// notice reaches the caller beside the classified count.
    #[tokio::test]
    async fn detach_clients_sends_the_frame_and_keeps_degradation_notices() {
        use crate::testkit::{ScriptSpec, ScriptedServer};
        use phux_protocol::wire::frame::FrameKind;

        const NOTICE: &str = "satellite build-box is unreachable: link is down";
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind");
        let spec = ScriptSpec::new()
            .degradation_notice(NOTICE)
            .detach_result(3);
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let mut conn = Connection::connect(&socket).await.expect("connect");
        let (outcome, degradation) = detach_clients(&mut conn, 1, Some("work".to_owned()))
            .await
            .expect("scripted server answers");
        drop(conn);
        let seen = server.await.expect("scripted server task");

        assert_eq!(outcome, DetachOutcome::Detached(3));
        assert_eq!(degradation.notices(), [NOTICE.to_owned()]);
        assert!(
            seen.iter().any(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::DetachClients { session: Some(s) },
                    ..
                } if s == "work"
            )),
            "sent {seen:?}"
        );
    }
}
