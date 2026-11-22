//! `MIRROR` over a one-shot connection: a host's capture targets and its
//! on-demand mirrors (`docs/resource-kinds.md#standing-mirrors`).

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, MirrorOp};

use crate::attach::connection::Connection;
use crate::org::OrgError;

/// Send one `MIRROR` and read its JSON answer.
///
/// # Errors
/// A server without `MIRRORS`, the transport, a refusal, or a non-JSON
/// answer.
pub async fn mirror(
    conn: &mut Connection,
    host: Option<String>,
    op: MirrorOp,
) -> Result<serde_json::Value, OrgError> {
    send(conn, Command::Mirror { host, op }).await
}

/// Report this host's capture targets (a JSON array) as Rune's macOS
/// runner, over the owner socket at `socket`; the answer names the mirrors
/// to run.
///
/// # Errors
/// As [`mirror`], plus connection failure.
pub async fn provide(
    socket: &std::path::Path,
    targets: String,
) -> Result<serde_json::Value, OrgError> {
    let mut conn = Connection::connect(socket)
        .await
        .map_err(OrgError::Attach)?;
    send(&mut conn, Command::MirrorProvide { targets }).await
}

async fn send(conn: &mut Connection, command: Command) -> Result<serde_json::Value, OrgError> {
    if !conn
        .server_features_ext()
        .contains(ServerFeatureExt::Mirrors)
    {
        return Err(OrgError::Unsupported);
    }
    let reply = conn.request(1, command).await.map_err(OrgError::Attach)?;
    match reply.into_parts().0 {
        CommandResult::OkWith(CommandValue::Json(text)) => {
            serde_json::from_str(&text).map_err(|error| OrgError::Unexpected(error.to_string()))
        }
        CommandResult::Error { code, message } => Err(OrgError::Server { code, message }),
        _ => Err(OrgError::Unexpected("MIRROR answered without JSON".into())),
    }
}
