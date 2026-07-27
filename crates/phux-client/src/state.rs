//! Shared server-state and L3 tag lookup helpers.
//!
//! CLI and MCP consumers use these free functions instead of maintaining
//! separate `GET_STATE` and `GET_METADATA` receive loops. Selector resolution
//! remains client-side (ADR-0017), and candidates retain snapshot order.

use std::path::Path;

use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, Scope, StateScope, TERMINAL_TAGS_KEY,
};
use phux_protocol::wire::info::SessionSnapshot;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::selector::{self, Selector, TagIndex};

/// Fetch the server-wide session snapshot over a fresh connection.
///
/// # Errors
///
/// Returns a transport error when the connection cannot be opened or closes
/// during the request, a refusal when the server rejects `GET_STATE`, or a
/// protocol error when the matching response has an unexpected value.
pub async fn get_state(socket: &Path) -> Result<SessionSnapshot, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    get_state_on(&mut conn).await
}

/// Fetch the server-wide session snapshot over an existing connection.
///
/// Unrelated interleaved frames are skipped until the matching command result
/// arrives, as required by SPEC §5.
///
/// # Errors
///
/// Returns a transport error when the connection closes during the request, a
/// refusal when the server rejects `GET_STATE`, or a protocol error when the
/// matching response has an unexpected value.
pub async fn get_state_on(conn: &mut Connection) -> Result<SessionSnapshot, AttachError> {
    const REQUEST_ID: u32 = 0;
    let (result, interleaved) = conn
        .request(
            REQUEST_ID,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await?
        .into_parts();
    // A hub answers GET_STATE with a *merged* snapshot and reports each
    // unreachable satellite as an uncorrelated ERROR pushed ahead of the ack
    // (`handle_get_state_federated`: "observable degradation, not silence").
    // The snapshot is still usable — it just does not list that satellite's
    // panes — so this is a warning, not a failure.
    report_degradation(&interleaved);
    match result {
        CommandResult::OkWith(CommandValue::State(snapshot)) => Ok(snapshot),
        CommandResult::Error { message, .. } => Err(AttachError::Refused(message)),
        other => Err(AttachError::Protocol(format!(
            "unexpected GET_STATE result: {other:?}"
        ))),
    }
}

/// The uncorrelated `ERROR` messages among frames the server interleaved
/// ahead of a `COMMAND_RESULT`.
///
/// An `ERROR` with no `request_id` is not any command's answer (`proto.md`
/// §9) — on this wire it is the federation degradation notice a hub emits
/// per unreachable satellite. Extracted rather than logged here so a caller
/// that owns a user-visible channel (the CLI owns stderr) can surface it
/// instead of burying it in a `tracing` subscriber nobody has installed.
#[must_use]
pub fn degradation_notices(interleaved: &[FrameKind]) -> Vec<String> {
    interleaved
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::Error {
                request_id: None,
                message,
                ..
            } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

/// Log every degradation notice in `interleaved` at `warn`.
///
/// The library-side default for callers with nowhere better to put it. A CLI
/// should prefer [`degradation_notices`] and print.
pub fn report_degradation(interleaved: &[FrameKind]) {
    for message in degradation_notices(interleaved) {
        tracing::warn!(
            %message,
            "server reported partial state: a federated satellite contributed nothing",
        );
    }
}

/// Fetch the L3 tag index for every pane in `snapshot` over `conn`.
///
/// Requests are pipelined and matched by request id. Missing, empty, or
/// malformed `phux.tags/v1` values are omitted. This lookup is best-effort:
/// if the server disconnects, the entries received so far are returned.
pub async fn fetch_tag_index(conn: &mut Connection, snapshot: &SessionSnapshot) -> TagIndex {
    let ids: Vec<TerminalId> = snapshot.panes.iter().map(|pane| pane.id.clone()).collect();
    let mut index = TagIndex::new();

    // GET_STATE uses request id 0, so metadata requests start at 1 on shared
    // connections. A snapshot cannot practically contain u32::MAX panes; the
    // saturating conversion still keeps malformed fixtures panic-free.
    for (offset, id) in ids.iter().enumerate() {
        let request_id = u32::try_from(offset).unwrap_or(u32::MAX).saturating_add(1);
        if conn
            .send(&FrameKind::GetMetadata {
                request_id,
                scope: Scope::Terminal(id.clone()),
                key: TERMINAL_TAGS_KEY.to_owned(),
            })
            .await
            .is_err()
        {
            return index;
        }
    }

    let mut remaining = ids.len();
    while remaining > 0 {
        match conn.recv().await {
            Ok(FrameKind::MetadataValue { request_id, value }) => {
                let Some(position) = usize::try_from(request_id)
                    .ok()
                    .and_then(|id| id.checked_sub(1))
                else {
                    continue;
                };
                let Some(terminal_id) = ids.get(position) else {
                    continue;
                };
                remaining -= 1;
                if let Some(bytes) = value
                    && let Ok(tags) = serde_json::from_slice::<Vec<String>>(&bytes)
                    && !tags.is_empty()
                {
                    index.insert(terminal_id.clone(), tags);
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    index
}

/// Resolve a selector against a snapshot, fetching L3 tags only for `#tag`.
///
/// Non-tag selectors are resolved synchronously without an extra connection.
/// A tag lookup failure degrades to an empty index, preserving the established
/// CLI behavior that reports the result as a selector miss.
pub async fn resolve_targets(
    socket: &Path,
    selector: &Selector,
    snapshot: &SessionSnapshot,
) -> Vec<TerminalId> {
    if !matches!(selector, Selector::Tag(_)) {
        return selector::resolve(selector, snapshot);
    }

    let tags = match Connection::connect(socket).await {
        Ok(mut conn) => fetch_tag_index(&mut conn, snapshot).await,
        Err(_) => TagIndex::new(),
    };
    selector::resolve_with_tags(selector, snapshot, &tags)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use phux_protocol::ids::{SessionId, TerminalId, WindowId};
    use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, FrameKind};
    use phux_protocol::wire::info::SessionSnapshot;
    use tokio::net::UnixListener;

    use super::get_state;
    use crate::testkit::{ScriptSpec, ScriptedServer};

    #[tokio::test]
    async fn get_state_skips_unrelated_command_results() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("state.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let expected =
            SessionSnapshot::new(SessionId::new(7), WindowId::new(8), TerminalId::local(9));
        // A COMMAND_RESULT for request 99 belongs to some other pipelined
        // request; the shared harness always emits it AHEAD of this one's
        // ack, because that is the only ordering in which it is a hazard.
        let spec = ScriptSpec::new().foreign_ack(99).state(expected.clone());
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let actual = get_state(&socket).await.unwrap();
        assert_eq!(actual, expected);
        let seen = server.await.unwrap();
        assert!(
            matches!(
                seen.first(),
                Some(FrameKind::Command {
                    request_id: 0,
                    command: Command::GetState { .. }
                })
            ),
            "GET_STATE is the only frame this path sends, on request id 0; got {:?}",
            seen.first()
        );
    }

    #[tokio::test]
    async fn hub_satellite_degradation_is_not_lost_before_the_get_state_ack() {
        // `handle_get_state_federated` pushes one uncorrelated ERROR per
        // unreachable satellite AHEAD of the merged snapshot's ack, on
        // purpose: "observable degradation, not silence". Every hand-rolled
        // wait loop in the workspace dropped it, so `phux ls`/`kill`/`spatial`
        // against a hub with a dead satellite reported a silently partial
        // fleet as though it were the whole truth. The snapshot must still
        // come back (degradation is not failure) and the notice must be
        // extractable rather than consumed.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("degraded.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let expected =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(2), TerminalId::local(3));
        let spec = ScriptSpec::new()
            .degradation_notice("no satellite route to build-box")
            .state(expected.clone());
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let mut conn = crate::attach::connection::Connection::connect(&socket)
            .await
            .unwrap();
        let (result, interleaved) = conn
            .request(
                0,
                Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            )
            .await
            .unwrap()
            .into_parts();
        assert!(
            matches!(result, CommandResult::OkWith(CommandValue::State(snap)) if snap == expected),
            "a degraded hub still answers with the merged snapshot"
        );
        assert_eq!(
            super::degradation_notices(&interleaved),
            vec!["no satellite route to build-box".to_owned()],
            "the satellite's failure must reach the caller, not the floor"
        );
        // The harness serves until the client hangs up, so the connection has
        // to go before its task can be joined.
        drop(conn);
        server.await.unwrap();
    }

    #[test]
    fn degradation_notices_ignores_correlated_errors() {
        // A correlated ERROR is some command's answer (proto.md §9), not a
        // degradation notice — `Connection::request` already resolves the
        // caller's own; anything else belongs to another pipelined request.
        let frames = vec![
            FrameKind::Error {
                request_id: Some(4),
                code: phux_protocol::wire::frame::ErrorCode::TerminalNotFound,
                message: "someone else's refusal".to_owned(),
            },
            FrameKind::Error {
                request_id: None,
                code: phux_protocol::wire::frame::ErrorCode::UnsupportedSatelliteRoute,
                message: "satellite unreachable".to_owned(),
            },
        ];
        assert_eq!(
            super::degradation_notices(&frames),
            vec!["satellite unreachable".to_owned()]
        );
    }
}
