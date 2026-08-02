//! `phux config init` scaffolding (phux-ijp).
//!
//! The starter config is a comment-projection of the embedded defaults:
//! it must be (a) fully inert — parsing it yields exactly the shipped
//! defaults, with no overrides — and (b) faithful — it shows each
//! option's real default value as a comment, not a placeholder.

use std::path::Path;

use phux_config::parse_with_defaults;
use phux_config::scaffold::{
    ScaffoldOutcome, distro_reference_config, reference_config, write_reference_config,
    write_scaffold,
};

mod common;
use common::path;

/// A scaffolded file with nothing uncommented must parse to the same
/// effective config as an empty user file: pure shipped defaults. This
/// is what makes the projection inert — it never freezes a default.
#[test]
fn reference_config_is_inert() {
    let from_reference =
        parse_with_defaults(&reference_config(), &path()).expect("reference parses");
    let from_empty = parse_with_defaults("", &path()).expect("empty parses");
    assert_eq!(
        from_reference, from_empty,
        "commented starter config must impose no overrides"
    );
}

/// Every non-comment, non-blank line in the projection must be a
/// comment — there must be no live assignment that could silently
/// override a default.
#[test]
fn reference_config_has_no_active_lines() {
    for line in reference_config().lines() {
        let trimmed = line.trim_start();
        assert!(
            trimmed.is_empty() || trimmed.starts_with('#'),
            "active (uncommented) line leaked into starter config: {line:?}"
        );
    }
}

/// The projection must carry the embedded defaults' real values verbatim
/// (as commented lines), so the starter file documents each option with
/// its actual default. Spot-check a scalar, a table header, and confirm
/// the embedded "ships with the binary" preamble was replaced.
#[test]
fn reference_config_shows_real_default_values() {
    let r = reference_config();
    assert!(
        r.contains("# history-limit = 50000"),
        "scalar default should appear commented with its real value"
    );
    assert!(
        r.contains("# prefix = \"C-a\""),
        "prefix default should appear commented with its real value"
    );
    assert!(
        r.contains("# [keybindings.prefix-table]"),
        "table headers should be commented, not dropped"
    );
    assert!(
        !r.contains("embedded via include_str"),
        "embedded-default preamble should be replaced by the user header"
    );
}

#[test]
fn write_creates_then_refuses_overwrite() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested").join("config.toml");

    // First write lands the file and creates the parent dir.
    let outcome = write_reference_config(&path, false).expect("first write ok");
    assert_eq!(outcome, ScaffoldOutcome::Wrote(path.clone()));
    assert!(path.exists());
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        reference_config()
    );

    // A user edit must survive a non-forced re-run untouched.
    std::fs::write(&path, "# user edits\n").expect("overwrite for test");
    let outcome = write_reference_config(&path, false).expect("second write ok");
    assert_eq!(outcome, ScaffoldOutcome::Skipped(path.clone()));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "# user edits\n",
        "skipped write must not clobber existing content"
    );

    // With force, we overwrite.
    let outcome = write_reference_config(&path, true).expect("forced write ok");
    assert_eq!(outcome, ScaffoldOutcome::Wrote(path.clone()));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        reference_config()
    );
}

/// The distro scaffold's only live statement is the `extends` line;
/// everything else stays the inert comment-projection.
#[test]
fn distro_scaffold_has_exactly_one_active_line_the_extends() {
    let scaffold = distro_reference_config(Path::new("/opt/phux/distros/herdr/herdr.toml"));
    let active: Vec<&str> = scaffold
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !(trimmed.is_empty() || trimmed.starts_with('#'))
        })
        .collect();
    assert_eq!(
        active,
        vec![r#"extends = ["/opt/phux/distros/herdr/herdr.toml"]"#],
        "distro scaffold must be inert apart from the extends line"
    );
    // And the shared body still documents the real defaults.
    assert!(scaffold.contains("# history-limit = 50000"));
}

/// The scaffolded stack must resolve: with a real layer on disk, parsing
/// the distro scaffold yields the layer's values and nothing else beyond
/// the shipped defaults.
#[test]
fn distro_scaffold_parses_to_layer_values_over_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    let layer = dir.path().join("mini.toml");
    std::fs::write(&layer, "[defaults]\nhistory-limit = 4242\n").expect("write layer");

    let scaffold = distro_reference_config(&layer);
    let cfg =
        parse_with_defaults(&scaffold, &dir.path().join("config.toml")).expect("stack parses");
    assert_eq!(cfg.defaults.history_limit, 4242, "layer value wins");
    assert_eq!(
        cfg.keybindings.prefix, "C-a",
        "untouched keys keep the shipped default"
    );
}

/// Paths needing TOML escaping (quotes, backslashes) survive the
/// scaffold round trip.
#[test]
fn distro_scaffold_escapes_awkward_paths() {
    let scaffold = distro_reference_config(Path::new(r#"/tmp/we"ird/dis\tro.toml"#));
    assert!(
        scaffold.contains(r#"extends = ["/tmp/we\"ird/dis\\tro.toml"]"#),
        "quotes and backslashes must be escaped: {scaffold}"
    );
    // The document must stay parseable TOML even with the odd path.
    let table: toml::Table = toml::from_str(&scaffold).expect("scaffold parses as TOML");
    let extends = table["extends"].as_array().expect("extends array");
    assert_eq!(
        extends[0].as_str(),
        Some(r#"/tmp/we"ird/dis\tro.toml"#),
        "path round-trips through escaping"
    );
}

#[test]
fn write_scaffold_shares_the_refuse_to_clobber_contract() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let contents = distro_reference_config(Path::new("/opt/d/d.toml"));

    let outcome = write_scaffold(&path, &contents, false).expect("first write ok");
    assert_eq!(outcome, ScaffoldOutcome::Wrote(path.clone()));
    assert_eq!(std::fs::read_to_string(&path).expect("read back"), contents);

    let outcome = write_scaffold(&path, "clobber?", false).expect("second write ok");
    assert_eq!(outcome, ScaffoldOutcome::Skipped(path.clone()));
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        contents,
        "skipped write must not clobber"
    );
}
