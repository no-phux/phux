//! `PRIVACY_STATUS` over a one-shot owner-socket connection: the macOS
//! privacy grants the server holds (`docs/install.md#macos-permissions`).

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};

use crate::attach::connection::Connection;
use crate::org::OrgError;

/// Ask the server on `socket` which privacy grants it holds.
///
/// # Errors
/// Connection failure, a server without `PRIVACY_STATUS`, a refusal, or a
/// non-JSON answer.
pub async fn privacy_status(socket: &std::path::Path) -> Result<serde_json::Value, OrgError> {
    let mut conn = Connection::connect(socket)
        .await
        .map_err(OrgError::Attach)?;
    if !conn
        .server_features_ext()
        .contains(ServerFeatureExt::PrivacyStatus)
    {
        return Err(OrgError::Unsupported);
    }
    let reply = conn
        .request(1, Command::PrivacyStatus)
        .await
        .map_err(OrgError::Attach)?;
    match reply.into_parts().0 {
        CommandResult::OkWith(CommandValue::Json(text)) => {
            serde_json::from_str(&text).map_err(|error| OrgError::Unexpected(error.to_string()))
        }
        CommandResult::Error { code, message } => Err(OrgError::Server { code, message }),
        _ => Err(OrgError::Unexpected(
            "PRIVACY_STATUS answered without JSON".into(),
        )),
    }
}
