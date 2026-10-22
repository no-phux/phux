//! Settings catalogue and snapshot tests.

use std::collections::BTreeSet;
use std::path::Path;

use super::*;

/// Catalogue keys whose shipped state is unset.
const OPTIONAL_KEYS: &[&str] = &[
    "defaults.shell",
    "defaults.spawn-on-attach",
    "experimental.predictive-echo",
    "voice.transcriber",
    "voice.timeout-secs",
    "organize.jev-key-command",
];

/// Composite subtrees under the section tables: composition, not knobs.
const COMPOSITE_KEYS: &[&str] = &[
    "status.left",
    "status.center",
    "status.right",
    "keybindings.prefix-table",
    "keybindings.global",
    "keybindings.modes",
    "keybindings.mode-options",
    "theme",
];

fn scalar_leaves(value: &toml::Value, path: &str, out: &mut BTreeSet<String>) {
    if COMPOSITE_KEYS.contains(&path) {
        return;
    }
    match value {
        toml::Value::Table(table) => {
            for (key, child) in table {
                scalar_leaves(child, &format!("{path}.{key}"), out);
            }
        }
        toml::Value::Array(_) => {}
        _ => {
            out.insert(path.to_owned());
        }
    }
}

/// The coverage gate, both directions: every scalar leaf under the
/// section tables has exactly one row, and every row names such a leaf
/// or a documented unset key with an optional kind.
#[test]
fn catalog_covers_every_scalar_leaf_of_the_schema() {
    let cfg = crate::parse_with_defaults("", Path::new("test.toml")).expect("defaults parse");
    let defaults = toml::Table::try_from(cfg).expect("Config serializes");
    let mut expected = BTreeSet::new();
    for section in SettingSection::ALL {
        if let Some(table) = defaults.get(section.table()) {
            scalar_leaves(table, section.table(), &mut expected);
        }
    }
    for key in OPTIONAL_KEYS {
        assert!(
            expected.insert((*key).to_owned()),
            "{key} now has a default"
        );
    }

    let mut catalog_keys = BTreeSet::new();
    let mut last_section = 0;
    for spec in CATALOG {
        assert!(
            catalog_keys.insert(spec.key.to_owned()),
            "duplicate {}",
            spec.key
        );
        let optional = matches!(
            spec.kind,
            SettingKind::OptionalBool
                | SettingKind::OptionalText
                | SettingKind::OptionalInteger { .. }
                | SettingKind::Argv
        );
        assert_eq!(optional, OPTIONAL_KEYS.contains(&spec.key), "{}", spec.key);
        let index = SettingSection::ALL
            .iter()
            .position(|s| *s == spec.section)
            .expect("section is in ALL");
        assert!(
            index >= last_section,
            "{} is out of section order",
            spec.key
        );
        last_section = index;
        assert_eq!(spec.table(), spec.section.table(), "{}", spec.key);
        assert!(spec.summary.chars().count() <= 72, "{}", spec.key);
        assert!(
            !spec.summary.ends_with('.') && spec.detail.ends_with('.'),
            "{}",
            spec.key
        );
    }
    assert_eq!(catalog_keys, expected, "CATALOG drifted from the schema");
}

/// Every `Choice` variant is one the schema accepts, and a bogus one is
/// rejected.
#[test]
fn choice_variants_match_the_schema_enums() {
    let mut seen = 0;
    for spec in CATALOG {
        let SettingKind::Choice(variants) = spec.kind else {
            continue;
        };
        seen += 1;
        let parse = |variant: &str| {
            let text = format!("[{}]\n{} = \"{variant}\"\n", spec.table(), spec.leaf());
            crate::parse_with_defaults(&text, Path::new("t.toml"))
        };
        for variant in variants {
            assert!(parse(variant).is_ok(), "{}: `{variant}` rejected", spec.key);
        }
        assert!(parse("not-a-real-variant").is_err(), "{}", spec.key);
    }
    assert_eq!(seen, 7);
}

#[test]
fn every_integer_default_lies_within_its_bounds() {
    let snapshot = SettingsSnapshot::load("", Path::new("t.toml")).expect("load");
    for spec in CATALOG {
        let SettingKind::Integer { min, max } = spec.kind else {
            continue;
        };
        let default = snapshot
            .default_at(spec.key)
            .and_then(toml::Value::as_integer)
            .expect("integer default");
        assert!((min..=max).contains(&default), "{}: {default}", spec.key);
    }
}

#[test]
fn origins_attribute_user_and_extends_layers() {
    let snapshot = SettingsSnapshot::load("", Path::new("t.toml")).expect("load");
    for spec in CATALOG {
        assert!(snapshot.origin_at(spec.key).is_none(), "{}", spec.key);
        assert_eq!(snapshot.value_at(spec.key), snapshot.default_at(spec.key));
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let layer_path = dir.path().join("layers").join("team.toml");
    std::fs::create_dir_all(layer_path.parent().unwrap()).unwrap();
    std::fs::write(&layer_path, "[chrome]\ncompact-cols = 80\n").unwrap();
    let user_path = dir.path().join("config.toml");
    let user = "extends = [\"team\"]\n\n[sidebar]\nwidth = 32\n[theme]\naccent = \"#f00\"\n";
    let snapshot = SettingsSnapshot::load(user, &user_path).expect("load");
    assert_eq!(
        snapshot.origin_at("chrome.compact-cols"),
        Some(&LayerSource::Extended(layer_path))
    );
    assert_eq!(snapshot.value_at("chrome.compact-cols"), Some(&80.into()));
    assert_eq!(
        snapshot.origin_at("sidebar.width"),
        Some(&LayerSource::User(user_path))
    );
    assert_eq!(snapshot.value_at("sidebar.width"), Some(&32.into()));
    assert_eq!(snapshot.default_at("sidebar.width"), Some(&20.into()));
    assert!(snapshot.origin_at("sidebar.enabled").is_none());
    assert_eq!(snapshot.value_at("theme.accent"), Some(&"#f00".into()));
    assert!(snapshot.default_at("theme.accent").is_none());
    assert!(snapshot.value_at("theme.nonexistent").is_none());
    assert!(snapshot.value_at("").is_none());
}

#[test]
fn render_value_shapes() {
    assert_eq!(render_value(None), "unset");
    assert_eq!(
        render_value(Some(&"xterm-256color".into())),
        "xterm-256color"
    );
    assert_eq!(render_value(Some(&false.into())), "false");
    assert_eq!(render_value(Some(&400.into())), "400");
    let argv = vec!["curl", "-sf", "-F", "file=@{path}", "a b", "it's"].into();
    assert_eq!(
        render_value(Some(&argv)),
        "curl -sf -F file=@{path} 'a b' 'it'\\''s'"
    );
}
