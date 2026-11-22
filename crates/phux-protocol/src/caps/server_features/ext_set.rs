//! The set operations of [`ServerFeatureExtSet`].

use super::{ServerFeatureExt, ServerFeatureExtSet};

impl ServerFeatureExtSet {
    const KNOWN: u32 = Self::with(ServerFeatureExt::ALL).0;

    /// Known extended features in this set, in bit order.
    pub fn iter(self) -> impl Iterator<Item = ServerFeatureExt> {
        ServerFeatureExt::ALL
            .iter()
            .copied()
            .filter(move |feature| self.contains(*feature))
    }

    /// No extended features.
    #[must_use]
    pub const fn new() -> Self {
        Self(0)
    }

    /// Every known extended feature.
    #[must_use]
    pub const fn all() -> Self {
        Self(Self::KNOWN)
    }

    /// Construct a set from known features.
    #[must_use]
    pub const fn with(features: &[ServerFeatureExt]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < features.len() {
            bits |= features[i] as u32;
            i += 1;
        }
        Self(bits)
    }

    /// Copy with one feature cleared.
    #[must_use]
    pub const fn without(self, feature: ServerFeatureExt) -> Self {
        Self(self.0 & !(feature as u32))
    }

    /// Whether this set contains a feature.
    #[must_use]
    pub const fn contains(self, feature: ServerFeatureExt) -> bool {
        self.0 & (feature as u32) != 0
    }

    /// Whether no extended feature is present.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Known bits to write to the second word.
    #[must_use]
    pub const fn as_wire(self) -> u32 {
        self.0 & Self::KNOWN
    }

    /// Read known bits, ignoring future additions.
    #[must_use]
    pub const fn from_wire(bits: u32) -> Self {
        Self(bits & Self::KNOWN)
    }
}
