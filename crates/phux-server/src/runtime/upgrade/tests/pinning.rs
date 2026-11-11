//! Pinning the replacement image: the snapshot validation probes, and the
//! installed path the re-exec uses while it still matches.

use super::super::validate::*;
use super::support::*;

/// Re-exec lands on the installed (allowlistable) path while it still
/// matches the validated snapshot, and aborts once the install is
/// swapped; validation always sees the snapshot.
#[test]
fn upgrade_execs_the_installed_path_only_while_it_matches_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("phux");
    install("/usr/bin/true", &path);
    let pinned = PinnedExecutable::open(&path).unwrap();
    assert_ne!(pinned.path, pinned.source_path);
    assert_eq!(pinned.exec_path().unwrap(), pinned.source_path.as_path());

    let replacement = dir.path().join("replacement");
    install("/usr/bin/false", &replacement);
    std::fs::rename(&replacement, &path).unwrap();
    validate_binary(&pinned.path).expect("the pinned image remains the validated one");
    assert!(validate_binary(&path).is_err());
    assert!(
        pinned.exec_path().is_err(),
        "a swapped install path must abort rather than re-exec the tempfile"
    );
}

/// A bundle executable is snapshotted inside a copy of its bundle: its code
/// signature seals the Info.plist and resources, and copied out alone macOS
/// SIGKILLs it, failing every upgrade of a server run from `phux Server.app`.
#[test]
fn a_bundle_executable_is_pinned_inside_its_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("phux Server.app");
    let path = app.join("Contents/MacOS/phux");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(app.join("Contents/_CodeSignature")).unwrap();
    std::fs::write(app.join("Contents/Info.plist"), "plist").unwrap();
    std::fs::write(app.join("Contents/_CodeSignature/CodeResources"), "seal").unwrap();
    install("/usr/bin/true", &path);

    let pinned = PinnedExecutable::open(&path).unwrap();
    let copy = pinned.dir.path().join("phux Server.app");
    assert_eq!(pinned.path, copy.join("Contents/MacOS/phux"));
    assert_eq!(
        std::fs::read(copy.join("Contents/Info.plist")).unwrap(),
        b"plist"
    );
    assert!(copy.join("Contents/_CodeSignature/CodeResources").is_file());
    validate_binary(&pinned.path).expect("the bundled snapshot runs");
    assert_eq!(pinned.exec_path().unwrap(), path.as_path());
}

/// A resumed image whose predecessor handed down `<path> (deleted)`
/// still pins the installed `<path>`.
#[test]
fn an_inherited_deleted_source_pins_the_installed_path() {
    let dir = tempfile::tempdir().unwrap();
    let installed = write_stub_phux(dir.path(), "#!/bin/sh\nexit 0\n");
    let inherited = dir.path().join("phux (deleted)");
    let pinned = pin_validated_executable(Some(&inherited)).unwrap();
    assert_eq!(pinned.source_path, installed);
}
