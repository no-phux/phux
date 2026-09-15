//! Named projection metadata keys (ADR-0129, `docs/spec/L3.md` §3.5).
//!
//! A projection is not a resource: it is whichever
//! `<prefix>.layout/v1/<session-id>` L3 key a consumer chooses to read and
//! write. This module owns the grammar so the CLI (`--projection`) and the
//! C FFI share one parser.

use phux_protocol::{GroupId, SessionId};

/// Group 1 is the v0.x layout metadata scope (`docs/spec/L3.md` §3.2).
pub const LAYOUT_METADATA_GROUP: GroupId = GroupId::new(1);

/// Spec-recommended per-key cap (`docs/spec/L3.md` §2), also the reference
/// server's `limits.metadata-value-bytes` default.
pub const MAX_LAYOUT_METADATA_BYTES: usize = 256 * 1024;

/// Parse the session a `<prefix>.layout/v1/<session-id>` key names.
///
/// `None` when `key` does not match the grammar. The `<session-id>` segment
/// must be the session's **canonical** decimal form — no leading zero
/// (other than a bare `"0"`), no leading `+`, no non-ASCII-digit content —
/// enforced as `suffix == id.to_string()` rather than merely "parses as
/// u32". Without this, `myapp.layout/v1/07` and `myapp.layout/v1/7` would
/// both name session 7 but compare unequal as strings: the server's reap
/// cleanup matches the literal key `*.layout/v1/7`, so a non-canonical key
/// that slipped past a looser check would never be deleted when its session
/// reaps. `<prefix>` must also not itself contain the `.layout/v1/`
/// separator, so a key with two occurrences (`a.layout/v1/b.layout/v1/7`)
/// is rejected rather than silently matched on its last one.
#[must_use]
pub fn projection_key_session(key: &str) -> Option<SessionId> {
    let (prefix, suffix) = key.rsplit_once(".layout/v1/")?;
    if prefix.is_empty() || prefix.contains(".layout/v1/") {
        return None;
    }
    let id = suffix.parse::<u32>().ok()?;
    if suffix != id.to_string() {
        return None;
    }
    Some(SessionId::new(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_key_session_accepts_the_adr_0129_grammar() {
        assert_eq!(
            projection_key_session("phux.tui.layout/v1/7"),
            Some(SessionId::new(7))
        );
        assert_eq!(
            projection_key_session("myapp.layout/v1/7"),
            Some(SessionId::new(7))
        );
        assert_eq!(
            projection_key_session("a.b.c.layout/v1/0"),
            Some(SessionId::new(0))
        );
    }

    #[test]
    fn projection_key_session_rejects_empty_prefix_and_foreign_shapes() {
        assert_eq!(projection_key_session(".layout/v1/7"), None);
        assert_eq!(projection_key_session("not-a-layout-key"), None);
        assert_eq!(projection_key_session("phux.tui.layout/v1"), None);
        assert_eq!(projection_key_session("layout/v1/7"), None);
    }

    #[test]
    fn projection_key_session_id_must_be_canonical_decimal() {
        for non_canonical in [
            "myapp.layout/v1/07",
            "myapp.layout/v1/+7",
            "myapp.layout/v1/7 ",
        ] {
            assert_eq!(
                projection_key_session(non_canonical),
                None,
                "{non_canonical:?} must not resolve to any session"
            );
        }
    }

    #[test]
    fn projection_key_prefix_must_not_contain_the_separator_itself() {
        assert_eq!(projection_key_session("a.layout/v1/b.layout/v1/7"), None);
    }
}
