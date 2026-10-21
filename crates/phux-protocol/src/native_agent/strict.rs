//! The strict side of a native request: what the server decodes must be exactly what this build understands.
//!
//! The DTOs stay tolerant, because the same types also travel server to
//! client (a stored launch recipe, a relayed publication) and a newer
//! server's additions must not break an older client there. Only the
//! server's request decode is strict: a request that does not survive a
//! round trip through this build's types named a field or value this build
//! does not know, which it refuses instead of silently dropping.

use serde_json::Value;

/// The message a refused request carries; it opens with the words
/// [`crate::wire::error::names_unknown_json`] reads as "newer, not malformed".
pub(super) const UNKNOWN_FIELD_OR_VALUE: &str =
    "unknown field or value in a native request this build does not define";

/// Whether `decoded`, re-encoded by this build, still says everything
/// `original` said. An absent key and `null` count as the same, and keys
/// this build filled with defaults are fine; a key it dropped, or a value a
/// tolerant enum folded to `unknown`, does not survive.
pub(super) fn survives_round_trip(original: &Value, decoded: &Value) -> bool {
    match (original, decoded) {
        (Value::Object(original), Value::Object(decoded)) => original.iter().all(|(key, value)| {
            value.is_null()
                || decoded
                    .get(key)
                    .is_some_and(|d| survives_round_trip(value, d))
        }),
        (Value::Array(original), Value::Array(decoded)) => {
            original.len() == decoded.len()
                && original
                    .iter()
                    .zip(decoded)
                    .all(|(o, d)| survives_round_trip(o, d))
        }
        (original, decoded) => original == decoded,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::survives_round_trip;

    #[test]
    fn extra_keys_and_folded_values_do_not_survive_but_nulls_do() {
        let decoded = json!({"a": 1, "b": null, "c": {"d": "x"}});
        assert!(survives_round_trip(
            &json!({"a": 1, "c": {"d": "x"}}),
            &decoded
        ));
        assert!(survives_round_trip(
            &json!({"a": 1, "b": null, "c": {"d": "x"}, "e": null}),
            &decoded
        ));
        assert!(!survives_round_trip(
            &json!({"a": 1, "c": {"d": "x"}, "new": true}),
            &decoded
        ));
        assert!(!survives_round_trip(
            &json!({"a": 1, "c": {"d": "newer"}}),
            &decoded
        ));
    }
}
