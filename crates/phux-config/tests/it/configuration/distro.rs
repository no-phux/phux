//! `--distro` spec resolution, with injected search directories.

#![allow(clippy::expect_used, reason = "tests")]

use std::fs;
use std::path::{Path, PathBuf};

use phux_config::distro::{
    DistroError, current_distro, list_distros_in, resolve_distro_in, search_dirs, switch_distro,
};
use tempfile::TempDir;

/// Create `dir/<name>/<name>.toml`, returning its canonical path.
fn plant_distro(dir: &Path, name: &str) -> PathBuf {
    let package = dir.join(name);
    fs::create_dir_all(&package).expect("mkdir distro package");
    let layer = package.join(format!("{name}.toml"));
    fs::write(&layer, "[defaults]\nhistory-limit = 123\n").expect("write layer");
    layer.canonicalize().expect("canonicalize")
}

/// Bare names search the directories in order (first hit wins); file and
/// directory path specs bypass them; a renamed name falls through to its
/// alias.
#[test]
fn specs_resolve_to_absolute_layer_files() {
    let first = TempDir::new().expect("tempdir");
    let second = TempDir::new().expect("tempdir");
    let winner = plant_distro(first.path(), "herdr");
    plant_distro(second.path(), "herdr");
    let starter = plant_distro(second.path(), "starter");
    let dirs = [first.path().to_path_buf(), second.path().to_path_buf()];

    assert_eq!(resolve_distro_in("herdr", &dirs).expect("name"), winner);
    let path_spec = winner.to_str().expect("utf8");
    assert_eq!(resolve_distro_in(path_spec, &[]).expect("file"), winner);
    let dir_spec = winner.parent().and_then(Path::to_str).expect("utf8");
    assert_eq!(resolve_distro_in(dir_spec, &[]).expect("dir"), winner);
    assert_eq!(
        resolve_distro_in("herdr", &dirs[1..]).expect("stub wins"),
        second
            .path()
            .join("herdr/herdr.toml")
            .canonicalize()
            .unwrap()
    );
    fs::remove_dir_all(second.path().join("herdr")).expect("drop stub");
    assert_eq!(
        resolve_distro_in("herdr", &dirs[1..]).expect("alias"),
        starter
    );
}

#[test]
fn failures_name_the_spec_and_every_checked_path() {
    let a = TempDir::new().expect("tempdir");
    let b = TempDir::new().expect("tempdir");
    let err = resolve_distro_in("nope", &[a.path().to_path_buf(), b.path().to_path_buf()])
        .expect_err("unknown name");
    let DistroError::UnknownName { name, candidates } = &err else {
        panic!("expected UnknownName, got {err:?}");
    };
    assert_eq!(name, "nope");
    assert!(candidates[0].starts_with(a.path()) && candidates[1].starts_with(b.path()));
    assert!(err.to_string().contains("nope.toml"), "{err}");

    let missing = a.path().join("ghost.toml");
    let err = resolve_distro_in(missing.to_str().expect("utf8"), &[]).expect_err("missing");
    assert!(matches!(err, DistroError::Unreadable { .. }), "{err:?}");
}

/// The repo checkout's `distros/` is on the default search list: `starter`
/// resolves there, and `herdr` hits its compatibility stub.
#[test]
fn the_repo_checkout_serves_the_bundled_distros() {
    let checkout = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("the test runner sets CARGO_MANIFEST_DIR"),
    )
    .join("../../distros");
    let dirs = search_dirs(Some(&checkout));
    let starter = resolve_distro_in("starter", &dirs).expect("starter");
    assert!(
        starter.ends_with("distros/starter/starter.toml"),
        "{starter:?}"
    );
    let herdr = resolve_distro_in("herdr", &dirs).expect("herdr");
    assert!(herdr.ends_with("distros/herdr/herdr.toml"), "{herdr:?}");
    let tmp = TempDir::new().expect("tempdir");
    let config = tmp.path().join("config.toml");
    switch_distro(&config, Some(&starter)).expect("bundled starter switches cleanly");
    assert_eq!(current_distro(&config).expect("current"), Some(starter));
    switch_distro(&config, Some(&herdr)).expect("compatibility distro switches cleanly");
    switch_distro(&config, None).expect("off");
}

#[test]
fn discovery_is_sorted_deduplicated_and_agrees_with_resolution() {
    let first = TempDir::new().expect("tempdir");
    let second = TempDir::new().expect("tempdir");
    let winner = plant_distro(first.path(), "zebra");
    plant_distro(second.path(), "zebra");
    plant_distro(second.path(), "alpha");
    fs::create_dir(first.path().join("not-a-distro")).expect("mkdir");
    plant_distro(first.path(), "bad.name");
    let dirs = [first.path().to_path_buf(), second.path().to_path_buf()];
    let listed = list_distros_in(&dirs).expect("list");
    assert_eq!(
        listed.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ["alpha", "zebra"]
    );
    assert_eq!(listed[1].path, winner);
    for distro in listed {
        assert_eq!(
            resolve_distro_in(&distro.name, &dirs).expect("resolve"),
            distro.path
        );
    }
}

#[test]
fn unsafe_bare_names_are_rejected() {
    for name in ["", ".", "..", "a\\b", "a:b", "a b", "é", "a.b"] {
        assert!(
            matches!(
                resolve_distro_in(name, &[]),
                Err(DistroError::InvalidName(_))
            ),
            "{name}"
        );
    }
}

#[test]
fn switching_preserves_overrides_comments_and_unmanaged_layers() {
    let tmp = TempDir::new().expect("tempdir");
    let a = plant_distro(tmp.path(), "a");
    let b = plant_distro(tmp.path(), "b");
    fs::write(&b, "[defaults]\nhistory-limit = 456\n").expect("layer");
    fs::write(tmp.path().join("personal.toml"), "[sidebar]\nwidth = 30\n").expect("personal layer");
    let path = tmp.path().join("config.toml");
    let original = "# hand written\nextends = [\n  'personal.toml', # keep layer\n] # keep array\n\n[defaults]\nhistory-limit   =   789 # personal wins\n";
    fs::write(&path, original).expect("config");
    assert_eq!(current_distro(&path).expect("current"), None);
    switch_distro(&path, Some(&a)).expect("select a");
    assert_eq!(current_distro(&path).expect("current"), Some(a));
    switch_distro(&path, Some(&b)).expect("select b");
    assert_eq!(current_distro(&path).expect("current"), Some(b));
    let text = fs::read_to_string(&path).expect("read");
    let cfg = phux_config::parse_with_defaults(&text, &path).expect("load");
    assert_eq!(cfg.defaults.history_limit, 789);
    assert!(text.contains("'personal.toml', # keep layer"), "{text}");
    assert!(
        text.contains("history-limit   =   789 # personal wins"),
        "{text}"
    );
    switch_distro(&path, None).expect("off");
    assert_eq!(current_distro(&path).expect("current"), None);
    assert_eq!(fs::read_to_string(&path).expect("read"), original);
}

#[test]
fn broken_switches_leave_existing_and_missing_configs_untouched() {
    let tmp = TempDir::new().expect("tempdir");
    let bad = plant_distro(tmp.path(), "bad");
    let path = tmp.path().join("config.toml");
    for body in [
        "not = toml = nope",
        "[sidebar]\nwidht = 12",
        "extends = ['missing.toml']",
        "extends = ['bad.toml']",
        "[keybindings]\nprefix = 'not a chord'",
    ] {
        fs::write(&bad, body).expect("layer");
        assert!(switch_distro(&path, Some(&bad)).is_err(), "{body}");
        assert!(!path.exists());
    }
    let good = plant_distro(tmp.path(), "good");
    fs::write(&path, "[sidebar]\nwidth = 'wide'\n").expect("config");
    assert!(switch_distro(&path, Some(&good)).is_err());
    assert_eq!(
        fs::read_to_string(&path).expect("read"),
        "[sidebar]\nwidth = 'wide'\n"
    );
}

#[test]
fn existing_init_scaffold_is_adopted_without_removing_added_layers() {
    let tmp = TempDir::new().expect("tempdir");
    let a = plant_distro(tmp.path(), "a");
    let b = plant_distro(tmp.path(), "b");
    let path = tmp.path().join("config.toml");
    let scaffold = phux_config::scaffold::distro_reference_config(&a);
    let scaffold = scaffold.replace("]\n\n", ", 'personal.toml']\n\n");
    fs::write(tmp.path().join("personal.toml"), "[sidebar]\nwidth = 31\n").expect("layer");
    fs::write(&path, &scaffold).expect("scaffold");
    assert_eq!(current_distro(&path).expect("current"), Some(a));
    switch_distro(&path, Some(&b)).expect("migrate");
    let text = fs::read_to_string(&path).expect("read");
    assert!(text.contains("'personal.toml'"));
    let doc: toml::Table = toml::from_str(&text).expect("TOML");
    assert_eq!(doc["extends"].as_array().expect("array").len(), 2);
    assert_eq!(current_distro(&path).expect("current"), Some(b));
    switch_distro(&path, None).expect("off");
    assert!(
        fs::read_to_string(&path)
            .expect("read")
            .contains("'personal.toml'")
    );
}

#[test]
fn a_removed_selected_layer_can_be_turned_off_and_noop_off_does_not_write() {
    let tmp = TempDir::new().expect("tempdir");
    let layer = plant_distro(tmp.path(), "a");
    let path = tmp.path().join("config.toml");
    switch_distro(&path, None).expect("off missing");
    assert!(!path.exists());
    switch_distro(&path, Some(&layer)).expect("select");
    fs::remove_file(&layer).expect("remove layer");
    assert_eq!(current_distro(&path).expect("current broken"), Some(layer));
    switch_distro(&path, None).expect("off broken");
    let before = fs::metadata(&path)
        .expect("metadata")
        .modified()
        .expect("modified");
    switch_distro(&path, None).expect("off again");
    assert_eq!(
        fs::metadata(&path)
            .expect("metadata")
            .modified()
            .expect("modified"),
        before
    );
}

#[cfg(unix)]
#[test]
fn switching_follows_config_symlinks_and_preserves_permissions() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let tmp = TempDir::new().expect("tempdir");
    let layer = plant_distro(tmp.path(), "a");
    let target = tmp.path().join("dotfile.toml");
    fs::write(&target, "# precious\n[sidebar]\nwidth = 32\n").expect("target");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("mode");
    let path = tmp.path().join("config.toml");
    symlink(&target, &path).expect("symlink");
    switch_distro(&path, Some(&layer)).expect("select");
    assert!(
        fs::symlink_metadata(&path)
            .expect("stat")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::metadata(&target).expect("stat").permissions().mode() & 0o777,
        0o600
    );
    assert!(
        fs::read_to_string(&target)
            .expect("read")
            .contains("# precious")
    );
}

#[test]
fn invalid_or_ambiguous_extends_are_never_rewritten() {
    let tmp = TempDir::new().expect("tempdir");
    let layer = plant_distro(tmp.path(), "a");
    let path = tmp.path().join("config.toml");
    for original in [
        "extends = 'a.toml'\n",
        "extends = [3]\n",
        "extends = [\n# phux-managed-distro\n'a',\n# phux-managed-distro\n'b']\n",
    ] {
        fs::write(&path, original).expect("config");
        assert!(switch_distro(&path, Some(&layer)).is_err());
        assert!(switch_distro(&path, None).is_err());
        assert_eq!(fs::read_to_string(&path).expect("read"), original);
    }
}

#[test]
fn selecting_the_same_distro_is_idempotent_and_off_keeps_entry_notes() {
    let tmp = TempDir::new().expect("tempdir");
    let layer = plant_distro(tmp.path(), "a");
    let path = tmp.path().join("config.toml");
    switch_distro(&path, Some(&layer)).expect("select");
    let before = fs::metadata(&path)
        .expect("metadata")
        .modified()
        .expect("modified");
    switch_distro(&path, Some(&layer)).expect("select again");
    assert_eq!(
        fs::metadata(&path)
            .expect("metadata")
            .modified()
            .expect("modified"),
        before
    );
    let text = fs::read_to_string(&path).expect("read").replace(
        "# phux-managed-distro",
        "# my distro note\n  # phux-managed-distro",
    );
    fs::write(&path, text).expect("note");
    switch_distro(&path, None).expect("off");
    let text = fs::read_to_string(&path).expect("read");
    assert!(text.contains("# my distro note"), "{text}");
    assert!(!text.contains("# phux-managed-distro"), "{text}");
    assert_eq!(current_distro(&path).expect("current"), None);
}

#[test]
fn off_preserves_suffix_comments_with_and_without_a_following_entry() {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("config.toml");
    fs::write(tmp.path().join("personal.toml"), "[sidebar]\nwidth = 31\n").expect("personal layer");
    for original in [
        "extends = [\n# phux-managed-distro\n'selected.toml' # keep this operator note\n]\n",
        "extends = [\n# phux-managed-distro\n'selected.toml' # keep this operator note\n, 'personal.toml']\n",
        "extends = ['personal.toml',\n# phux-managed-distro\n'selected.toml' # keep this operator note\n]\n",
    ] {
        fs::write(&path, original).expect("config");
        switch_distro(&path, None).expect("off");
        let text = fs::read_to_string(&path).expect("read");
        assert_eq!(
            text.matches("# keep this operator note").count(),
            1,
            "{text}"
        );
        assert!(!text.contains("# phux-managed-distro"), "{text}");
        assert!(!text.contains("selected.toml"), "{text}");
        assert_eq!(current_distro(&path).expect("current"), None);
        phux_config::loader::load_from(&path).expect("remaining config loads");
        if original.contains("personal.toml") {
            assert!(text.contains("personal.toml"), "{text}");
        }
    }
}

#[test]
fn missing_enabled_distro_plugins_are_rejected_but_disabled_plugins_are_allowed() {
    let tmp = TempDir::new().expect("tempdir");
    let layer = plant_distro(tmp.path(), "a");
    let path = tmp.path().join("config.toml");
    fs::write(
        &layer,
        "[[plugins]]\nmanifest = 'missing/phux-plugin.toml'\n",
    )
    .expect("layer");
    assert!(
        switch_distro(&path, Some(&layer))
            .expect_err("missing plugin")
            .contains("missing/phux-plugin.toml")
    );
    assert!(!path.exists());
    fs::write(
        &layer,
        "[[plugins]]\nmanifest = 'missing/phux-plugin.toml'\nenabled = false\n",
    )
    .expect("layer");
    switch_distro(&path, Some(&layer)).expect("disabled plugin is inert");
}

#[cfg(unix)]
#[test]
fn a_broken_higher_precedence_package_does_not_silently_fall_through() {
    let tmp = TempDir::new().expect("tempdir");
    let first = tmp.path().join("first");
    let second = tmp.path().join("second");
    fs::create_dir_all(first.join("a")).expect("mkdir");
    std::os::unix::fs::symlink("missing.toml", first.join("a/a.toml")).expect("broken link");
    plant_distro(&second, "a");
    assert!(matches!(
        resolve_distro_in("a", &[first, second]),
        Err(DistroError::Unreadable { .. })
    ));
}
