//! The canonical string form of a wire [`ResourceId`].
//!
//! A language that cannot carry a Rust enum across the boundary needs one
//! spelling of a terminal's identity, and it must round-trip: the string a
//! binding hands out is the key it accepts back. `local:7` and
//! `satellite:<host>:7` are that spelling.

use phux_protocol::ResourceId;

/// The string a binding hands to a foreign caller for `id`.
#[must_use]
pub fn encode(id: &ResourceId) -> String {
    match id {
        ResourceId::Local { id } => format!("local:{id}"),
        ResourceId::Satellite { host, id } => format!("satellite:{}:{id}", host.as_str()),
    }
}

/// The terminal a foreign caller named, or `None` when the string is not one
/// [`encode`] produces.
///
/// A satellite host may itself contain `:`, so the numeric suffix is split
/// from the right.
#[must_use]
pub fn parse(value: &str) -> Option<ResourceId> {
    if let Some(raw) = value.strip_prefix("local:") {
        return raw.parse::<u32>().ok().map(ResourceId::local);
    }
    let rest = value.strip_prefix("satellite:")?;
    let (host, raw) = rest.rsplit_once(':')?;
    if host.is_empty() {
        return None;
    }
    raw.parse::<u32>()
        .ok()
        .map(|id| ResourceId::satellite(host, id))
}

#[cfg(test)]
mod tests {
    use super::{encode, parse};
    use phux_protocol::ResourceId;

    /// Local and satellite ids round-trip; a satellite host may contain a
    /// colon (the id is the last field).
    #[test]
    fn ids_round_trip_through_their_wire_spelling() {
        for (id, wire) in [
            (ResourceId::local(7), "local:7"),
            (ResourceId::satellite("box", 3), "satellite:box:3"),
            (ResourceId::satellite("box:2222", 3), "satellite:box:2222:3"),
        ] {
            assert_eq!(encode(&id), wire);
            assert_eq!(parse(wire), Some(id), "{wire}");
        }
    }

    #[test]
    fn malformed_ids_are_refused() {
        for value in ["", "local:", "local:x", "satellite:3", "satellite::3", "7"] {
            assert_eq!(parse(value), None, "{value:?} should not parse");
        }
    }
}
