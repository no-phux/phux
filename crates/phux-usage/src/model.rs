//! The `phux.usage/v1` document: one snapshot of every plan this host can
//! see. Readers ignore unknown fields, so fields are only ever added.

use serde::{Deserialize, Serialize};

/// The Global L3 metadata key the snapshot is published under.
pub const USAGE_KEY: &str = "phux.usage/v1";
/// The Global key a client may set to ask for a fresh reading. A hint only:
/// it skips the minimum interval, never a rate-limit backoff.
pub const REFRESH_KEY: &str = "phux.usage.refresh/v1";
/// The `schema` field value.
pub const SCHEMA: &str = "phux.usage/v1";
/// The largest document the contract allows.
pub const MAX_DOCUMENT_BYTES: usize = 16 * 1024;

/// One host's plan-usage snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Always [`SCHEMA`].
    pub schema: String,
    /// Short host name of the publishing machine.
    pub host: String,
    /// When this document was written (Unix ms).
    pub observed_at_ms: i64,
    /// One entry per plan found on this host.
    pub plans: Vec<Plan>,
}

/// One subscription plan's limits.
#[expect(
    clippy::struct_field_names,
    reason = "the contract names the field `plan`"
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    /// Vendor id: `claude`, `codex`, `grok`.
    pub provider: String,
    /// The product the plan is used through, e.g. `claude-code`.
    pub harness: String,
    /// Vendor plan id as reported (`max`, `pro`, `supergrok_heavy`).
    pub plan: Option<String>,
    /// Human label, e.g. `Max 20x`.
    pub label: Option<String>,
    /// First 12 hex of sha256(vendor account id); never an email or token.
    pub account: Option<String>,
    /// Where the numbers came from: `oauth`, `rollout`, `wham`, `billing`.
    pub source: String,
    /// Freshness of `windows`.
    pub status: Status,
    /// Short reason when `status` is not `ok`.
    pub error: Option<String>,
    /// When `windows` were read from their source (Unix ms).
    pub fetched_at_ms: Option<i64>,
    /// Limit windows, primary first.
    pub windows: Vec<Window>,
}

/// How much to trust a plan's windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Read from the source recently.
    Ok,
    /// The last good reading, older than the source would normally give.
    Stale,
    /// The source cannot be read (signed out, token expired).
    Unavailable,
    /// The last attempt failed for another reason.
    Error,
}

/// One utilization window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// `5h`, `7d`, `7d_opus`, ...
    pub id: String,
    /// Display label.
    pub label: String,
    /// Window length in minutes.
    pub minutes: u32,
    /// 0-100, possibly above 100 when the vendor says so.
    pub used_percent: f64,
    /// When the window resets (Unix ms), when known.
    pub resets_at_ms: Option<i64>,
}

impl Window {
    /// A window keyed by its length: 300 minutes is `5h`, 10080 is `7d`,
    /// anything else is named by hours or days.
    #[must_use]
    pub fn by_length(minutes: u32, used_percent: f64, resets_at_ms: Option<i64>) -> Self {
        let id = window_id(minutes);
        Self {
            label: id.clone(),
            id,
            minutes,
            used_percent,
            resets_at_ms,
        }
    }
}

fn window_id(minutes: u32) -> String {
    match minutes {
        300 => "5h".to_owned(),
        10_080 => "7d".to_owned(),
        m if m % 1440 == 0 => format!("{}d", m / 1440),
        m if m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_named_by_length() {
        assert_eq!(window_id(300), "5h");
        assert_eq!(window_id(10_080), "7d");
        assert_eq!(window_id(43_200), "30d");
        assert_eq!(window_id(120), "2h");
        assert_eq!(window_id(45), "45m");
    }

    #[test]
    fn status_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&Status::Unavailable).unwrap_or_default(),
            "\"unavailable\""
        );
    }
}
