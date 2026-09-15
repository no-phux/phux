//! `snake_case` names for [`ServerFeature`] bits, shared by
//! `phux status --json` (the negotiated `features` list) and
//! `phux --capabilities --json` (the kind catalog's gates).
//!
//! Names come from [`ServerFeature::snake_name`]: the `docs/spec/proto.md`
//! §6.2 constant, lower-cased. There is no second table here.

use phux_protocol::caps::{ServerFeature, ServerFeatureSet};

/// The name of `feature` — [`ServerFeature::snake_name`].
pub(crate) fn feature_name(feature: ServerFeature) -> &'static str {
    feature.snake_name()
}

/// Every feature in `features` by name, in bit order. A bit this binary
/// does not know is not named.
pub(crate) fn feature_names(features: ServerFeatureSet) -> Vec<&'static str> {
    features.iter().map(ServerFeature::snake_name).collect()
}

#[cfg(test)]
mod tests {
    use super::feature_names;
    use phux_protocol::caps::{ServerFeature, ServerFeatureSet};

    #[test]
    fn every_known_bit_is_named_exactly_once() {
        let all = ServerFeatureSet::all();
        let names = feature_names(all);
        assert_eq!(names.len(), ServerFeature::ALL.len());
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            names.len(),
            "duplicate feature names: {names:?}"
        );
        assert_eq!(feature_names(ServerFeatureSet::from_wire(u32::MAX)), names);
        assert!(feature_names(ServerFeatureSet::new()).is_empty());
    }
}
