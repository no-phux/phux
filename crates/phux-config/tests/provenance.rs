//! Provenance recording for the layered merge (phux-r82.4).
//!
//! `merged_config_with_provenance` attributes every effective leaf key
//! to the layer that set it, and every array element to the layer that
//! contributed it. These tests drive a three-layer fixture stack —
//! embedded defaults <- base.toml <- distro.toml <- user config — and
//! check:
//! 1. The layer stack is reported in merge order with the right kinds.
//! 2. Scalar attribution: untouched defaults stay on layer 0; each
//!    override lands on the overriding layer; the user file wins last.
//! 3. Array attribution: `-append` elements carry their contributing
//!    layer, in order, across defaults + multiple layers.
//! 4. The table half equals `merged_config_table` (same merge).
//! 5. Every leaf of the merged table has a provenance entry.
//! 6. Non-bare key segments are quoted TOML-address style.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::fs;
use std::path::{Path, PathBuf};

use phux_config::{ConfigProvenance, KeyOrigin, LayerSource, merged_config_with_provenance};
use tempfile::TempDir;

/// Write `contents` to `dir/name`, creating parent directories.
fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(&path, contents).expect("write layer");
    path
}

/// The root (user) input of the three-layer fixture.
const USER_INPUT: &str = r#"
extends = ["distro.toml"]

[keybindings]
prefix = "C-b"

[[plugins-append]]
manifest = "/home/me/phux-plugin.toml"
"#;

/// Build the three-layer fixture: base <- distro <- user, over the
/// embedded defaults. Layer indexes: 0 defaults, 1 base, 2 distro,
/// 3 user.
fn three_layer_stack(tmp: &TempDir) -> (toml::Table, ConfigProvenance, PathBuf) {
    write(
        tmp.path(),
        "base.toml",
        r#"
[defaults]
history-limit = 1111
mouse         = false

[[plugins-append]]
manifest = "/opt/base/phux-plugin.toml"

[keybindings.prefix-table]
"b" = "new-window"
"#,
    );
    write(
        tmp.path(),
        "distro.toml",
        r#"
extends = ["base.toml"]

[defaults]
history-limit = 2222

[status]
right-append = [{ kind = "time", format = "%H:%M" }]
"#,
    );
    let config_path = tmp.path().join("config.toml");
    let (merged, provenance) =
        merged_config_with_provenance(USER_INPUT, &config_path).expect("layered parse");
    (merged, provenance, config_path)
}

fn origin<'a>(provenance: &'a ConfigProvenance, key: &str) -> &'a KeyOrigin {
    provenance
        .keys
        .get(key)
        .unwrap_or_else(|| panic!("provenance entry for `{key}`"))
}

#[test]
fn layer_stack_is_reported_in_merge_order() {
    let tmp = TempDir::new().expect("tempdir");
    let (_, provenance, config_path) = three_layer_stack(&tmp);

    assert_eq!(
        provenance.layers,
        vec![
            LayerSource::Defaults,
            LayerSource::Extended(tmp.path().join("base.toml")),
            LayerSource::Extended(tmp.path().join("distro.toml")),
            LayerSource::User(config_path),
        ]
    );
    assert_eq!(provenance.layers[0].path(), None);
    assert!(provenance.layers[1].path().is_some());
}

#[test]
fn scalars_attribute_to_the_last_layer_that_set_them() {
    let tmp = TempDir::new().expect("tempdir");
    let (_, provenance, _) = three_layer_stack(&tmp);

    // Untouched shipped default.
    assert_eq!(origin(&provenance, "defaults.term").layer, 0);
    // Set by base, never overridden above it.
    assert_eq!(origin(&provenance, "defaults.mouse").layer, 1);
    // Base sets it, distro overrides: distro owns it.
    assert_eq!(origin(&provenance, "defaults.history-limit").layer, 2);
    // User leaf wins over everything.
    assert_eq!(origin(&provenance, "keybindings.prefix").layer, 3);
    // Tables merge per key: base's rebind of one chord owns only that
    // chord; sibling shipped bindings stay on the defaults layer.
    assert_eq!(origin(&provenance, "keybindings.prefix-table.b").layer, 1);
    assert_eq!(origin(&provenance, "keybindings.prefix-table.x").layer, 0);
    // Scalars carry no element attribution.
    assert_eq!(origin(&provenance, "keybindings.prefix").elements, None);
}

#[test]
fn append_arrays_attribute_each_element_to_its_contributor() {
    let tmp = TempDir::new().expect("tempdir");
    let (merged, provenance, _) = three_layer_stack(&tmp);

    // `[[plugins]]`: absent from the defaults; base appends one entry,
    // the user appends another. Element order is stack order.
    let plugins = origin(&provenance, "plugins");
    assert_eq!(plugins.elements.as_deref(), Some(&[1, 3][..]));
    assert_eq!(plugins.layer, 3, "last contributor owns the key");

    // `status.right`: the shipped defaults carry two widgets; the
    // distro appends a clock after them.
    let shipped_len = 2;
    let right = origin(&provenance, "status.right");
    assert_eq!(
        right.elements.as_deref(),
        Some(&[vec![0; shipped_len], vec![2]].concat()[..])
    );
    let merged_right = merged["status"]["right"]
        .as_array()
        .expect("status.right is an array");
    assert_eq!(
        merged_right.len(),
        right.elements.as_ref().map(Vec::len).expect("elements"),
        "element attribution covers every merged element"
    );
}

#[test]
fn plain_array_assignment_attributes_all_elements_to_the_assigner() {
    let tmp = TempDir::new().expect("tempdir");
    write(
        tmp.path(),
        "distro.toml",
        r#"
[status]
right = ["session-name", { kind = "time", format = "%H:%M" }]
"#,
    );
    let user = r#"
extends = ["distro.toml"]

[status]
right-append = ["session-name"]
"#;

    let (_, provenance) = merged_config_with_provenance(user, &tmp.path().join("config.toml"))
        .expect("layered parse");
    // The distro's plain assignment replaces the shipped array (all
    // elements re-attributed to layer 1); the user appends one more.
    let right = origin(&provenance, "status.right");
    assert_eq!(right.elements.as_deref(), Some(&[1, 1, 2][..]));
    assert_eq!(right.layer, 2);
}

#[test]
fn merged_table_half_matches_merged_config_table() {
    let tmp = TempDir::new().expect("tempdir");
    let (merged, _, config_path) = three_layer_stack(&tmp);
    let table = phux_config::merged_config_table(USER_INPUT, &config_path).expect("merge");
    assert_eq!(merged, table);
}

/// Mirror the library's path grammar: bare segments join with `.`,
/// anything else is double-quoted with `\` / `"` escaped.
fn child_path(prefix: &str, key: &str) -> String {
    let is_bare = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let segment = if is_bare {
        key.to_owned()
    } else {
        format!("\"{}\"", key.replace('\\', "\\\\").replace('"', "\\\""))
    };
    if prefix.is_empty() {
        segment
    } else {
        format!("{prefix}.{segment}")
    }
}

/// Assert every leaf under `table` carries an in-range provenance
/// entry (arrays additionally one element attribution per element).
fn assert_leaves_attributed(table: &toml::Table, prefix: &str, provenance: &ConfigProvenance) {
    for (key, value) in table {
        let path = child_path(prefix, key);
        match value {
            toml::Value::Table(t) => assert_leaves_attributed(t, &path, provenance),
            toml::Value::Array(items) => {
                let entry = origin(provenance, &path);
                assert_eq!(
                    entry.elements.as_ref().map(Vec::len),
                    Some(items.len()),
                    "element attribution length for `{path}`"
                );
                assert!(entry.elements.iter().flatten().all(|l| *l < 4));
            }
            _ => {
                assert!(origin(provenance, &path).layer < 4);
            }
        }
    }
}

#[test]
fn every_merged_leaf_has_a_provenance_entry() {
    let tmp = TempDir::new().expect("tempdir");
    let (merged, provenance, _) = three_layer_stack(&tmp);
    assert_leaves_attributed(&merged, "", &provenance);
}

#[test]
fn non_bare_key_segments_are_quoted() {
    let tmp = TempDir::new().expect("tempdir");
    let (_, provenance, _) = three_layer_stack(&tmp);

    // The shipped prefix-table binds `%` (split-pane), a table value
    // whose leaves sit under a quoted segment.
    assert_eq!(
        origin(&provenance, "keybindings.prefix-table.\"%\".action").layer,
        0
    );

    // A user-supplied dotted key quotes as one segment.
    let user = "[theme]\n\"a.b\" = \"x\"\n";
    let (_, provenance) =
        merged_config_with_provenance(user, &tmp.path().join("config.toml")).expect("parse");
    assert_eq!(origin(&provenance, "theme.\"a.b\"").layer, 1);
}
