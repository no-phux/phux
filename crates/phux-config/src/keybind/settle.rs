//! Settling the merged `[keybindings]` tables (key modes included) across
//! config layers: the newest spelling of a key wins, and `"none"` unbinds it.

use std::collections::{BTreeMap, BTreeSet};

use super::{KeyChord, parse_chord_sequence};

/// The action that removes a binding: `"x" = "none"` unbinds `x`, shipped
/// defaults included.
pub const UNBIND_ACTION: &str = "none";

/// The binding table plugin manifest `keys` merge into.
const PREFIX_TABLE: &str = "prefix-table";

/// Binding tables under `[keybindings]`, as they are spelled in TOML.
const BINDING_TABLES: [&str; 2] = ["global", PREFIX_TABLE];

/// The table of key-mode binding tables (`[keybindings.modes.<name>]`).
const MODES: &str = "modes";

/// Settle the merged binding tables, returning the prefix-table keys that
/// `"none"` unbound as chord sequences ([`KeybindingsCfg::unbound`]).
///
/// Spellings of one key set in different
/// layers (`"X"` in the defaults, `"S-x"` in yours) are one binding, and the
/// later layer's wins; spellings tied in one layer stay for the resolver to
/// report. Then every `"none"` binding is removed. Provenance forgets every
/// removed entry.
///
/// [`KeybindingsCfg::unbound`]: crate::KeybindingsCfg::unbound
pub fn settle_bindings(
    merged: &mut toml::Table,
    provenance: &mut crate::ConfigProvenance,
) -> BTreeSet<Vec<KeyChord>> {
    let mut unbound_prefix_keys = BTreeSet::new();
    let Some(toml::Value::Table(keybindings)) = merged.get_mut("keybindings") else {
        return unbound_prefix_keys;
    };
    for name in BINDING_TABLES {
        let Some(toml::Value::Table(table)) = keybindings.get_mut(name) else {
            continue;
        };
        let unbound = settle_table(table, &format!("keybindings.{name}"), provenance);
        if name == PREFIX_TABLE {
            let sequences = unbound.iter().map(|b| parse_chord_sequence(b));
            unbound_prefix_keys.extend(sequences.filter_map(Result::ok));
        }
    }
    // Each key mode's table settles on its own: a mode's `"none"` unbinds
    // the key in that mode only.
    if let Some(toml::Value::Table(modes)) = keybindings.get_mut(MODES) {
        for (mode, table) in modes.iter_mut() {
            if let toml::Value::Table(table) = table {
                let table_path = crate::layer::child_path(&format!("keybindings.{MODES}"), mode);
                settle_table(table, &table_path, provenance);
            }
        }
    }
    unbound_prefix_keys
}

/// Settle one binding table: drop spellings a later layer replaced, then
/// every `"none"`. Returns the bindings `"none"` removed.
fn settle_table(
    table: &mut toml::Table,
    table_path: &str,
    provenance: &mut crate::ConfigProvenance,
) -> Vec<String> {
    let shadowed = shadowed_spellings(table, table_path, provenance);
    // A `"none"` another layer's spelling replaced unbinds nothing.
    let unbound: Vec<String> = table
        .iter()
        .filter(|(binding, value)| is_unbind(value) && !shadowed.contains(binding))
        .map(|(binding, _)| binding.clone())
        .collect();
    for binding in shadowed.iter().chain(&unbound) {
        remove_binding(table, table_path, binding, provenance);
    }
    unbound
}

/// Drop `binding` from `table` and every provenance entry under it.
fn remove_binding(
    table: &mut toml::Table,
    table_path: &str,
    binding: &str,
    provenance: &mut crate::ConfigProvenance,
) {
    table.remove(binding);
    let path = crate::layer::child_path(table_path, binding);
    let nested = format!("{path}.");
    provenance
        .keys
        .retain(|key, _| key != &path && !key.starts_with(&nested));
}

/// Bindings another spelling of the same key replaced from a later layer.
fn shadowed_spellings(
    table: &toml::Table,
    table_path: &str,
    provenance: &crate::ConfigProvenance,
) -> Vec<String> {
    let mut by_key: BTreeMap<Vec<KeyChord>, Vec<(usize, &String)>> = BTreeMap::new();
    for binding in table.keys() {
        if let Ok(sequence) = parse_chord_sequence(binding) {
            let layer = binding_layer(provenance, table_path, binding).unwrap_or(0);
            by_key.entry(sequence).or_default().push((layer, binding));
        }
    }
    let mut shadowed = Vec::new();
    for spellings in by_key.values() {
        let newest = spellings.iter().map(|(layer, _)| *layer).max().unwrap_or(0);
        shadowed.extend(
            spellings
                .iter()
                .filter(|(layer, _)| *layer < newest)
                .map(|(_, binding)| (*binding).clone()),
        );
    }
    shadowed
}

/// The newest layer that set any leaf of `binding` in the table at
/// `table_path` (`keybindings.prefix-table`), or `None` when no layer did.
#[must_use]
pub fn binding_layer(
    provenance: &crate::ConfigProvenance,
    table_path: &str,
    binding: &str,
) -> Option<usize> {
    let path = crate::layer::child_path(table_path, binding);
    let nested = format!("{path}.");
    provenance
        .keys
        .iter()
        .filter(|(key, _)| **key == path || key.starts_with(&nested))
        .map(|(_, origin)| origin.layer)
        .max()
}

/// `"none"`, bare or as `{ action = "none" }`.
fn is_unbind(value: &toml::Value) -> bool {
    let action = match value {
        toml::Value::String(name) => Some(name.as_str()),
        toml::Value::Table(table) => table.get("action").and_then(toml::Value::as_str),
        _ => None,
    };
    action == Some(UNBIND_ACTION)
}

#[cfg(test)]
mod tests {
    use super::super::{Feed, ResolvedAction, Resolver, parse_chord};

    fn resolves(resolver: &mut Resolver, chords: &[&str]) -> Option<ResolvedAction> {
        let (last, init) = chords.split_last()?;
        for chord in init {
            assert_eq!(
                resolver.feed(parse_chord(chord).expect("parses")),
                Feed::Partial
            );
        }
        match resolver.feed(parse_chord(last).expect("parses")) {
            Feed::Resolved(action) => Some(action),
            _ => None,
        }
    }

    fn layered(user: &str) -> crate::Config {
        crate::parse_with_defaults(user, std::path::Path::new("config.toml"))
            .expect("layered config parses")
    }

    #[test]
    fn none_unbinds_a_shipped_binding() {
        let cfg = layered("[keybindings.prefix-table]\nx = \"none\"\n");
        assert!(!cfg.keybindings.prefix_table.contains_key("x"));
        let (mut resolver, diags) = Resolver::new_lenient(&cfg.keybindings);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(resolves(&mut resolver, &["C-a", "x"]), None);
        assert!(resolves(&mut resolver, &["C-a", "z"]).is_some());
    }

    #[test]
    fn the_config_remembers_which_prefix_keys_none_unbound() {
        let cfg = layered(
            "[keybindings.prefix-table]\n\"S-x\" = \"none\"\n\"g s\" = { action = \"none\" }\n\
             [keybindings.global]\n\"M-Enter\" = \"none\"\n",
        );
        let unbound: std::collections::BTreeSet<_> = ["X", "g s"]
            .iter()
            .map(|keys| super::parse_chord_sequence(keys).expect("parses"))
            .collect();
        assert_eq!(cfg.keybindings.unbound, unbound);
    }

    #[test]
    fn a_none_a_later_layer_rebinds_is_not_unbound() {
        let dir = std::env::temp_dir().join(format!("phux-settle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(
            dir.join("base.toml"),
            "[keybindings.prefix-table]\nX = \"none\"\n",
        )
        .expect("writes base");
        let cfg = crate::parse_with_defaults(
            "extends = [\"base.toml\"]\n[keybindings.prefix-table]\n\"S-x\" = \"toggle-zoom\"\n",
            &dir.join("config.toml"),
        )
        .expect("layered config parses");
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            cfg.keybindings.unbound.is_empty(),
            "{:?}",
            cfg.keybindings.unbound
        );
        assert!(cfg.keybindings.prefix_table.contains_key("S-x"));
    }

    #[test]
    fn another_spelling_from_a_later_layer_replaces_the_shipped_binding() {
        let cfg = layered("[keybindings.prefix-table]\n\"S-x\" = \"toggle-zoom\"\n");
        assert!(!cfg.keybindings.prefix_table.contains_key("X"));
        let (mut resolver, diags) = Resolver::new_lenient(&cfg.keybindings);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            resolves(&mut resolver, &["C-a", "X"]).map(|a| a.action),
            Some("toggle-zoom".to_owned())
        );
    }

    #[test]
    fn two_spellings_in_one_layer_still_conflict() {
        let cfg =
            layered("[keybindings.global]\n\"M-A\" = \"detach\"\n\"M-S-a\" = \"toggle-zoom\"\n");
        let (_, diags) = Resolver::new_lenient(&cfg.keybindings);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn provenance_forgets_removed_bindings() {
        let (table, provenance) = crate::merged_config_with_provenance(
            "[keybindings.prefix-table]\nx = \"none\"\n\"S-x\" = \"toggle-zoom\"\n",
            std::path::Path::new("config.toml"),
        )
        .expect("merges");
        let prefix_table = table["keybindings"]["prefix-table"]
            .as_table()
            .expect("table");
        assert!(!prefix_table.contains_key("x") && !prefix_table.contains_key("X"));
        assert!(!provenance.keys.contains_key("keybindings.prefix-table.x"));
        assert!(!provenance.keys.contains_key("keybindings.prefix-table.X"));
        assert!(provenance.keys.contains_key("keybindings.prefix-table.S-x"));
    }
}
