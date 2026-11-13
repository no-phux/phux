//! The server half of `phux doctor`'s "What works here" checklist: which
//! phones registered for push (`phux.push/v1/<device>`), and whether the
//! server serves file downloads.

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::wire::frame::{PUSH_GRANT_KEY_PREFIX, Scope};

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// What the checklist asks the server on `socket`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFacts {
    /// The device ids holding a push grant; `None` when the listing was
    /// refused.
    pub push_devices: Option<Vec<String>>,
    /// Whether the server advertises `FILE_DOWNLOAD`.
    pub file_download: bool,
}

/// Ask the server on `socket`.
///
/// # Errors
///
/// Connection and transport failures.
pub async fn host_facts(socket: &std::path::Path) -> Result<HostFacts, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    let file_download = conn
        .server_features_ext()
        .contains(ServerFeatureExt::FileDownload);
    let (keys, _interleaved) = conn
        .request_metadata_keys(1, Scope::Global)
        .await?
        .into_parts();
    let push_devices = keys.ok().map(|keys| {
        keys.iter()
            .filter_map(|key| key.strip_prefix(PUSH_GRANT_KEY_PREFIX))
            .map(str::to_owned)
            .collect()
    });
    Ok(HostFacts {
        push_devices,
        file_download,
    })
}

/// Ask the server on `socket` for its checklist (`HOST_CHECKLIST`), the
/// document a phone reads.
///
/// # Errors
///
/// Connection failure, a server without `HOST_CHECKLIST`, a refusal, or a
/// non-JSON answer.
pub async fn fetch(socket: &std::path::Path) -> Result<serde_json::Value, crate::org::OrgError> {
    use crate::org::OrgError;
    use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};

    let mut conn = Connection::connect(socket)
        .await
        .map_err(OrgError::Attach)?;
    if !conn
        .server_features_ext()
        .contains(ServerFeatureExt::HostChecklist)
    {
        return Err(OrgError::Unsupported);
    }
    let reply = conn
        .request(1, Command::HostChecklist)
        .await
        .map_err(OrgError::Attach)?;
    match reply.into_parts().0 {
        CommandResult::OkWith(CommandValue::Json(text)) => {
            serde_json::from_str(&text).map_err(|error| OrgError::Unexpected(error.to_string()))
        }
        CommandResult::Error { code, message } => Err(OrgError::Server { code, message }),
        _ => Err(OrgError::Unexpected(
            "HOST_CHECKLIST answered without JSON".into(),
        )),
    }
}
