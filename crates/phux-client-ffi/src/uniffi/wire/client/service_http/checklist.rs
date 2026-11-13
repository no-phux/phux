//! What works on the connected host (`HOST_CHECKLIST`, behind
//! `features_ext.HOST_CHECKLIST`): one record per item of the `checklist`
//! document (`docs/troubleshooting.md#what-works-here`).
//! `negotiated_features` names the capability `host-checklist`.

use phux_client_runtime::control::ChecklistError;

use super::super::RemoteClient;

/// One item: a capability of the host, whether it works, why, and the fix.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireHostCheck {
    /// Stable id: `agent_status`, `notifications`, `remote_reach`,
    /// `file_transfer`, `restore`, `skill`; later servers may add more.
    pub id: String,
    /// What to show as the item's name.
    pub title: String,
    /// `ok`, `off`, `waiting` or `unknown`; treat anything else as unknown.
    pub status: String,
    /// Why, in plain words.
    pub reason: String,
    /// What to run on the host, one command per line.
    pub fix_command: Option<String>,
    /// What the client can do in one tap instead (`enable_notifications`).
    pub fix_action: Option<String>,
    /// The `notifications` item only: device ids holding a push grant
    /// (the suffix of `phux.push/v1/<device>`).
    pub push_devices: Vec<String>,
}

/// Why the checklist has no answer.
#[derive(uniffi::Error, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WireHostChecklistError {
    /// Nothing was sent: no connection is established.
    #[error("not connected")]
    NotConnected,
    /// Nothing was sent: the host's phux is too old to answer.
    #[error("{reason}")]
    Unsupported { reason: String },
    /// The connection ended before the answer.
    #[error("the connection ended before the server answered")]
    Disconnected,
    /// The server could not build it (`reason` says why), or refused.
    #[error("{reason}")]
    Refused { code: u16, reason: String },
}

impl From<ChecklistError> for WireHostChecklistError {
    fn from(error: ChecklistError) -> Self {
        let reason = error.to_string();
        match error {
            ChecklistError::NotConnected => Self::NotConnected,
            ChecklistError::Unsupported => Self::Unsupported { reason },
            ChecklistError::Disconnected => Self::Disconnected,
            ChecklistError::Refused { code, message } => Self::Refused {
                code: code.as_wire(),
                reason: message,
            },
        }
    }
}

/// The items of a `checklist` document; a row without an id is skipped.
fn checks(doc: &serde_json::Value) -> Vec<WireHostCheck> {
    let text = |row: &serde_json::Value, key: &str| row[key].as_str().map(str::to_owned);
    doc["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            Some(WireHostCheck {
                id: text(row, "id")?,
                title: text(row, "title").unwrap_or_default(),
                status: text(row, "status").unwrap_or_else(|| "unknown".to_owned()),
                reason: text(row, "reason").unwrap_or_default(),
                fix_command: text(row, "fix_command"),
                fix_action: text(row, "fix_action"),
                push_devices: row["push_devices"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|id| id.as_str().map(str::to_owned))
                    .collect(),
            })
        })
        .collect()
}

#[uniffi::export]
impl RemoteClient {
    /// What works on the connected host, in display order. Runs the host's
    /// `phux doctor`, so it can take a few seconds.
    pub async fn host_checklist(&self) -> Result<Vec<WireHostCheck>, WireHostChecklistError> {
        let client = self
            .runtime_client()
            .ok_or(WireHostChecklistError::NotConnected)?;
        Ok(checks(&client.host_checklist().await?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_become_records() {
        let doc = serde_json::json!({ "schema_version": 1, "items": [
            {
                "id": "notifications", "title": "Asks and notifications", "status": "off",
                "reason": "no phone has turned on notifications for this host",
                "fix_command": null, "fix_action": "enable_notifications",
                "push_devices": ["a1"],
            },
            { "title": "no id" },
        ]});
        assert_eq!(
            checks(&doc),
            [WireHostCheck {
                id: "notifications".into(),
                title: "Asks and notifications".into(),
                status: "off".into(),
                reason: "no phone has turned on notifications for this host".into(),
                fix_command: None,
                fix_action: Some("enable_notifications".into()),
                push_devices: vec!["a1".into()],
            }]
        );
    }
}
