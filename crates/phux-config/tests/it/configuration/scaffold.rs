//! `phux config init` scaffolding: an inert comment-projection of the
//! embedded defaults, optionally over a distro `extends`.

use std::path::Path;

use phux_config::parse_with_defaults;
use phux_config::scaffold::{
    ScaffoldOutcome, distro_reference_config, reference_config, write_scaffold,
};

use crate::common;
use common::path;

fn active_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !(trimmed.is_empty() || trimmed.starts_with('#'))
        })
        .collect()
}

/// The scaffold is inert (parses to the pure shipped defaults, so it never
/// freezes a default) yet shows each option's real default, commented.
#[test]
fn reference_config_is_inert_and_documents_real_defaults() {
    let r = reference_config();
    assert!(active_lines(&r).is_empty(), "active line leaked");
    assert_eq!(
        parse_with_defaults(&r, &path()).expect("reference parses"),
        parse_with_defaults("", &path()).expect("empty parses")
    );
    for needle in [
        "# history-limit = 50000",
        "# prefix = \"C-a\"",
        "# [keybindings.prefix-table]",
    ] {
        assert!(r.contains(needle), "{needle}");
    }
    assert!(!r.contains("embedded via include_str"), "preamble replaced");
}

/// The distro scaffold's only live line is `extends`; with the layer on
/// disk it parses to the layer's values over the defaults, and awkward
/// paths are escaped.
#[test]
fn distro_scaffold_extends_the_layer_and_nothing_else() {
    let scaffold = distro_reference_config(Path::new("/opt/phux/distros/starter/starter.toml"));
    assert_eq!(
        active_lines(&scaffold),
        [r#"extends = ["/opt/phux/distros/starter/starter.toml"]"#]
    );
    assert!(scaffold.contains("# history-limit = 50000"));

    let dir = tempfile::tempdir().expect("tempdir");
    let layer = dir.path().join("mini.toml");
    std::fs::write(&layer, "[defaults]\nhistory-limit = 4242\n").expect("write layer");
    let cfg = parse_with_defaults(
        &distro_reference_config(&layer),
        &dir.path().join("config.toml"),
    )
    .expect("stack parses");
    assert_eq!(cfg.defaults.history_limit, 4242);
    assert_eq!(cfg.keybindings.prefix, "C-a");

    let odd = r#"/tmp/we"ird/dis\tro.toml"#;
    let table: toml::Table =
        toml::from_str(&distro_reference_config(Path::new(odd))).expect("parses as TOML");
    assert_eq!(table["extends"][0].as_str(), Some(odd));
}

/// A write creates parent directories, refuses to clobber, and overwrites
/// only with `force`.
#[test]
fn write_scaffold_refuses_to_clobber_without_force() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested").join("config.toml");
    let read = || std::fs::read_to_string(&path).expect("read back");

    let outcome = write_scaffold(&path, &reference_config(), false).expect("first write");
    assert_eq!(outcome, ScaffoldOutcome::Wrote(path.clone()));
    assert_eq!(read(), reference_config());

    std::fs::write(&path, "# user edits\n").expect("user edit");
    let outcome = write_scaffold(&path, &reference_config(), false).expect("second write");
    assert_eq!(outcome, ScaffoldOutcome::Skipped(path.clone()));
    assert_eq!(read(), "# user edits\n");

    let outcome = write_scaffold(&path, &reference_config(), true).expect("forced write");
    assert_eq!(outcome, ScaffoldOutcome::Wrote(path.clone()));
    assert_eq!(read(), reference_config());
}
