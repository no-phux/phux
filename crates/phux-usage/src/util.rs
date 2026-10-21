//! Small shared parsers: JWT claims, account fingerprints, timestamps.

use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The opaque account fingerprint: the first 12 hex digits of
/// sha256(`account_id`). Lets clients dedup one account seen on several
/// hosts without learning who it is.
#[must_use]
pub(crate) fn fingerprint(account_id: &str) -> Option<String> {
    let account_id = account_id.trim();
    if account_id.is_empty() {
        return None;
    }
    let digest = Sha256::digest(account_id.as_bytes());
    let mut hex = hex::encode(digest);
    hex.truncate(12);
    Some(hex)
}

/// A JWT's payload claims, unverified. Only for reading non-secret facts
/// (plan tier, expiry) the vendor's own client already trusts on disk.
#[must_use]
pub(crate) fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// RFC 3339 / ISO 8601 to Unix milliseconds.
#[must_use]
pub(crate) fn iso_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|t| t.timestamp_millis())
}

/// A JSON number as `f64`.
#[must_use]
pub(crate) fn number(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

/// A JSON number as whole Unix seconds.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    reason = "a timestamp sent as a float is whole seconds; the fraction is noise"
)]
pub(crate) fn seconds(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    value.as_i64().or_else(|| value.as_f64().map(|v| v as i64))
}

/// A non-empty JSON string.
#[must_use]
pub(crate) fn text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Whole minutes, rounded up, in a human form: `6m`, `2h`.
#[must_use]
pub(crate) fn human_wait(ms: i64) -> String {
    let minutes = (ms.max(0) + 59_999) / 60_000;
    if minutes >= 120 {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_twelve_hex_of_sha256() {
        // sha256("abc") = ba7816bf8f01cfea...
        assert_eq!(fingerprint("abc").as_deref(), Some("ba7816bf8f01"));
        assert_eq!(fingerprint("  "), None);
    }

    #[test]
    fn jwt_payload_decodes_without_verification() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"tier":5}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(jwt_claims(&token).unwrap()["tier"], 5);
        assert!(jwt_claims("not-a-jwt").is_none());
    }

    #[test]
    fn iso_timestamps_parse_with_offsets_and_fractions() {
        assert_eq!(iso_ms("2000-01-01T00:00:00Z"), Some(946_684_800_000));
        assert_eq!(iso_ms("2000-01-01T00:00:00.430Z"), Some(946_684_800_430));
        assert_eq!(iso_ms("2000-01-01T02:00:00+02:00"), Some(946_684_800_000));
        assert_eq!(
            iso_ms("2026-10-20T03:49:59.556817+00:00").map(|v| v % 1000),
            Some(556)
        );
        assert_eq!(iso_ms("nope"), None);
    }

    #[test]
    fn waits_round_up_to_minutes() {
        assert_eq!(human_wait(1), "1m");
        assert_eq!(human_wait(360_000), "6m");
        assert_eq!(human_wait(7_200_000), "2h");
    }
}
