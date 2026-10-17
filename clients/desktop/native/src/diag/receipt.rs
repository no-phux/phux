//! The diagnostic receipt `Copy Diagnostics` puts on the clipboard.
//!
//! Identity, not content: which app and runtime, which server incarnation and
//! connection epoch, which host, and the last redacted warnings. The fields
//! are typed and closed, so no terminal text, title, path answer or
//! clipboard data has a way in.

use serde::Serialize;

/// Everything a receipt says. Build it from typed state only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    /// The installed bundle's version, or `dev <crate version>`.
    pub app_version: String,
    /// The bundle's build revision, when installed.
    pub app_revision: Option<String>,
    /// `stable` or `next`, when installed.
    pub app_channel: Option<String>,
    /// The wire `major.minor` this build links.
    pub app_protocol: String,
    /// The installed `phux` runtime, once an update check has read it.
    pub runtime_version: Option<String>,
    /// The protocol triple the connected server selected.
    pub server_protocol: Option<String>,
    /// Lowercase hex server incarnation, after `HELLO_OK`.
    pub server_id: Option<String>,
    /// The runtime's connection epoch.
    pub connection_epoch: Option<u64>,
    /// Runtime connection status.
    pub status: Option<String>,
    /// `local`, or the remote registry entry's name (never its endpoint).
    pub host: String,
    /// Last connection failure, redacted.
    pub connection_error: Option<String>,
    /// The most recent warnings and errors, redacted, oldest first.
    pub recent_errors: Vec<String>,
}

impl Receipt {
    /// The fields every receipt carries, in order.
    pub const FIELDS: &'static [&'static str] = &[
        "appVersion",
        "appRevision",
        "appChannel",
        "appProtocol",
        "runtimeVersion",
        "serverProtocol",
        "serverId",
        "connectionEpoch",
        "status",
        "host",
        "connectionError",
        "recentErrors",
    ];

    /// The clipboard form: pretty JSON. The free-text fields are redacted
    /// again here, whoever built the receipt; typed identity fields are not,
    /// so a 40-hex build revision stays readable.
    #[must_use]
    pub fn to_clipboard(&self) -> String {
        let mut guarded = self.clone();
        guarded.connection_error = guarded.connection_error.as_deref().map(super::redact);
        for line in &mut guarded.recent_errors {
            *line = super::redact(line);
        }
        serde_json::to_string_pretty(&guarded).unwrap_or_default()
    }
}

/// `dev 0.1.0` for a bare executable; the bundle's version otherwise.
#[must_use]
pub fn development_version() -> String {
    format!("dev {}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_receipt_carries_identity_and_no_terminal_text() {
        let token = "8d0f3c2b1a9e8d7c6b5a49382716f5e4d3c2b1a0f9e8d7c6b5a4938271605f4e";
        let receipt = Receipt {
            app_version: "0.1.0-alpha.5".into(),
            app_revision: Some("dec9d4550c1a2b3c4d5e6f708192a3b4c5d6e7f8".into()),
            app_protocol: "1.4".into(),
            runtime_version: Some("0.9.0".into()),
            server_protocol: Some("1.4.0".into()),
            server_id: Some("0123456789abcdef0123456789abcdef".into()),
            connection_epoch: Some(3),
            status: Some("Attached".into()),
            host: "mini".into(),
            connection_error: Some(format!("token={token}")),
            recent_errors: vec![format!("dial failed with {token}")],
            ..Receipt::default()
        };
        let text = receipt.to_clipboard();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut expected = Receipt::FIELDS.to_vec();
        expected.sort_unstable();
        let mut keys_sorted = keys.clone();
        keys_sorted.sort_unstable();
        assert_eq!(keys_sorted, expected, "a closed set of identity fields");
        assert_eq!(value["serverId"], "0123456789abcdef0123456789abcdef");
        assert_eq!(value["connectionEpoch"], 3);
        assert_eq!(value["host"], "mini");
        assert_eq!(
            value["appRevision"],
            "dec9d4550c1a2b3c4d5e6f708192a3b4c5d6e7f8"
        );
        assert!(!text.contains(token), "{text}");
        for content_key in ["screen", "text", "title", "scrollback", "clipboard", "cwd"] {
            assert!(value.get(content_key).is_none());
        }
    }
}
