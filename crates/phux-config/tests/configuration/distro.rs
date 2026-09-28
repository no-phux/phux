//! `--distro` spec resolution, with injected search directories.

#![allow(clippy::expect_used, reason = "tests")]

use std::fs;
use std::path::{Path, PathBuf};

use phux_config::distro::{DistroError, resolve_distro_in, search_dirs};
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
    let dirs = search_dirs();
    let starter = resolve_distro_in("starter", &dirs).expect("starter");
    assert!(
        starter.ends_with("distros/starter/starter.toml"),
        "{starter:?}"
    );
    let herdr = resolve_distro_in("herdr", &dirs).expect("herdr");
    assert!(herdr.ends_with("distros/herdr/herdr.toml"), "{herdr:?}");
}
