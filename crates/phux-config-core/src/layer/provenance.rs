//! Recording which layer set each leaf while the stack merges.

use std::collections::BTreeMap;

use super::{KeyOrigin, child_path};

/// Provenance recorder for one layer's merge.
pub(super) struct Recorder<'a> {
    pub(super) layer: usize,
    pub(super) keys: &'a mut BTreeMap<String, KeyOrigin>,
}

impl Recorder<'_> {
    /// A plain assignment replaced whatever lower layers set at `path`.
    pub(super) fn record_set(&mut self, path: &str, value: &toml::Value) {
        let elements = match value {
            toml::Value::Array(items) => Some(vec![self.layer; items.len()]),
            _ => None,
        };
        self.keys.insert(
            path.to_owned(),
            KeyOrigin {
                layer: self.layer,
                elements,
            },
        );
    }

    /// An `-append` added `added` elements to the array at `path`.
    pub(super) fn record_append(&mut self, path: &str, added: usize) {
        match self.keys.get_mut(path) {
            Some(origin) if origin.elements.is_some() => {
                origin.layer = self.layer;
                if let Some(elements) = origin.elements.as_mut() {
                    elements.extend(std::iter::repeat_n(self.layer, added));
                }
            }
            // The append created the array, so it owns every element.
            _ => {
                self.keys.insert(
                    path.to_owned(),
                    KeyOrigin {
                        layer: self.layer,
                        elements: Some(vec![self.layer; added]),
                    },
                );
            }
        }
    }
}

/// Project the recorded origins onto the final table's leaves, dropping
/// entries left stale by shape changes across layers.
pub(super) fn finalize_keys(
    table: &toml::Table,
    recorded: &BTreeMap<String, KeyOrigin>,
    prefix: &str,
    out: &mut BTreeMap<String, KeyOrigin>,
) {
    for (key, value) in table {
        let path = child_path(prefix, key);
        match value {
            toml::Value::Table(t) => finalize_keys(t, recorded, &path, out),
            leaf => {
                let mut origin = recorded.get(&path).cloned().unwrap_or(KeyOrigin {
                    layer: 0,
                    elements: None,
                });
                if let toml::Value::Array(items) = leaf {
                    let matches = origin
                        .elements
                        .as_ref()
                        .is_some_and(|e| e.len() == items.len());
                    if !matches {
                        origin.elements = Some(vec![origin.layer; items.len()]);
                    }
                } else {
                    origin.elements = None;
                }
                out.insert(path, origin);
            }
        }
    }
}
