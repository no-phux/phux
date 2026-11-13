//! [`HandleMap`]: resource keys to the live handles that execute them.

use std::collections::HashMap;

use phux_protocol::ResourceId;
use phux_protocol::ids::ResourceKey;

/// The translation between resource keys and the live handles that execute
/// them on the connected server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HandleMap {
    to_handle: HashMap<ResourceKey, ResourceId>,
    to_key: HashMap<ResourceId, ResourceKey>,
}

impl HandleMap {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a map from `(key, live handle)` pairs; records without a live
    /// handle are skipped.
    #[must_use]
    pub fn from_pairs<I>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (ResourceKey, Option<ResourceId>)>,
    {
        let mut map = Self::new();
        for (key, handle) in pairs {
            if let Some(handle) = handle {
                map.insert(key, handle);
            }
        }
        map
    }

    /// Record that `key` executes as `handle`, replacing any earlier pairing
    /// of either side.
    pub fn insert(&mut self, key: ResourceKey, handle: ResourceId) {
        if let Some(old) = self.to_handle.insert(key, handle.clone()) {
            self.to_key.remove(&old);
        }
        if let Some(old) = self.to_key.insert(handle, key)
            && old != key
        {
            self.to_handle.remove(&old);
        }
    }

    /// Forget `key` and its handle.
    pub fn remove_key(&mut self, key: &ResourceKey) {
        if let Some(handle) = self.to_handle.remove(key) {
            self.to_key.remove(&handle);
        }
    }

    /// The live handle `key` executes as.
    #[must_use]
    pub fn handle(&self, key: &ResourceKey) -> Option<&ResourceId> {
        self.to_handle.get(key)
    }

    /// The key `handle` executes.
    #[must_use]
    pub fn key(&self, handle: &ResourceId) -> Option<ResourceKey> {
        self.to_key.get(handle).copied()
    }

    /// How many keys have a live handle.
    #[must_use]
    pub fn len(&self) -> usize {
        self.to_handle.len()
    }

    /// Whether no key has a live handle.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.to_handle.is_empty()
    }
}
