//! `--distro` spec resolution (phux-r82.9).
//!
//! Covers: bundled-name lookup across search directories (first hit
//! wins), path specs (file and directory forms), and the unknown-name
//! error listing every candidate that was checked. Uses
//! [`resolve_distro_in`] with injected directories so tests never
//! mutate process environment.

#![allow(clippy::expect_used, reason = "tests")]

use std::fs;
use std::path::PathBuf;

use phux_config::distro::{DistroError, resolve_distro_in};
use tempfile::TempDir;

/// Create `dir/<name>/<name>.toml` with placeholder layer contents.
fn plant_distro(dir: &std::path::Path, name: &str) -> PathBuf {
    let package = dir.join(name);
    fs::create_dir_all(&package).expect("mkdir distro package");
    let layer = package.join(format!("{name}.toml"));
    fs::write(&layer, "[defaults]\nhistory-limit = 123\n").expect("write layer");
    layer
}

#[test]
fn bare_name_resolves_to_name_slash_name_toml() {
    let tmp = TempDir::new().expect("tempdir");
    let layer = plant_distro(tmp.path(), "herdr");

    let resolved =
        resolve_distro_in("herdr", &[tmp.path().to_path_buf()]).expect("bundled name resolves");
    assert_eq!(resolved, layer.canonicalize().expect("canonicalize"));
    assert!(resolved.is_absolute());
}

#[test]
fn earlier_search_directory_wins() {
    let first = TempDir::new().expect("tempdir");
    let second = TempDir::new().expect("tempdir");
    let winner = plant_distro(first.path(), "herdr");
    plant_distro(second.path(), "herdr");

    let resolved = resolve_distro_in(
        "herdr",
        &[first.path().to_path_buf(), second.path().to_path_buf()],
    )
    .expect("resolves");
    assert_eq!(resolved, winner.canonicalize().expect("canonicalize"));
}

#[test]
fn unknown_name_error_lists_every_candidate() {
    let a = TempDir::new().expect("tempdir");
    let b = TempDir::new().expect("tempdir");

    let err = resolve_distro_in("nope", &[a.path().to_path_buf(), b.path().to_path_buf()])
        .expect_err("unknown name must fail");
    match &err {
        DistroError::UnknownName { name, candidates } => {
            assert_eq!(name, "nope");
            assert_eq!(candidates.len(), 2);
            assert!(candidates[0].starts_with(a.path()));
            assert!(candidates[1].starts_with(b.path()));
        }
        other => panic!("expected UnknownName, got: {other:?}"),
    }
    let msg = err.to_string();
    assert!(msg.contains("nope"), "error names the spec: {msg}");
    assert!(
        msg.contains("nope.toml"),
        "error lists checked paths: {msg}"
    );
}

#[test]
fn toml_path_spec_bypasses_the_search_directories() {
    let tmp = TempDir::new().expect("tempdir");
    let layer = plant_distro(tmp.path(), "herdr");

    // No search dirs at all: a path spec must not need them.
    let resolved =
        resolve_distro_in(layer.to_str().expect("utf8 path"), &[]).expect("path spec resolves");
    assert_eq!(resolved, layer.canonicalize().expect("canonicalize"));
}

#[test]
fn directory_path_spec_means_dir_slash_dirname_toml() {
    let tmp = TempDir::new().expect("tempdir");
    let layer = plant_distro(tmp.path(), "herdr");
    let package_dir = layer.parent().expect("package dir");

    let resolved = resolve_distro_in(package_dir.to_str().expect("utf8 path"), &[])
        .expect("directory spec resolves");
    assert_eq!(resolved, layer.canonicalize().expect("canonicalize"));
}

#[test]
fn missing_path_spec_is_unreadable_not_unknown() {
    let tmp = TempDir::new().expect("tempdir");
    let missing = tmp.path().join("ghost.toml");

    let err = resolve_distro_in(missing.to_str().expect("utf8 path"), &[])
        .expect_err("missing path must fail");
    assert!(
        matches!(err, DistroError::Unreadable { .. }),
        "expected Unreadable, got: {err:?}"
    );
    assert!(err.to_string().contains("ghost.toml"), "{err}");
}

#[test]
fn repo_checkout_fallback_finds_the_bundled_herdr() {
    // The public `resolve_distro` search list ends with the repo
    // checkout's distros/ directory; the in-tree herdr package must be
    // reachable through `search_dirs` even with no environment set up.
    let dirs = phux_config::distro::search_dirs();
    let resolved = resolve_distro_in("herdr", &dirs).expect("bundled herdr resolves in-repo");
    assert!(
        resolved.ends_with("distros/herdr/herdr.toml"),
        "{resolved:?}"
    );
}
