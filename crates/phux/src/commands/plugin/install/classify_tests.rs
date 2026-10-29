use std::fs;
use std::path::Path;

use super::{InstallSource, classify_source_in};

fn git(
    base: &Path,
    reference: &str,
    rev: Option<&str>,
    subdir_flag: Option<&str>,
) -> (String, Option<String>, Option<String>) {
    let classified = classify_source_in(base, reference, rev, subdir_flag)
        .unwrap_or_else(|err| panic!("classify {reference:?}: {err}"));
    assert!(classified.note.is_none(), "note: {:?}", classified.note);
    match classified.source {
        InstallSource::Git {
            url,
            branch,
            subdir,
        } => (url, branch, subdir),
        other => panic!("expected git, got {other:?}"),
    }
}

#[test]
fn shorthand_owner_repo_subdir_and_rev() {
    let base = Path::new("/tmp/phux-classify-missing");
    let (url, branch, subdir) = git(base, "owner/repo", None, None);
    assert_eq!(url, "https://github.com/owner/repo");
    assert_eq!(branch, None);
    assert_eq!(subdir, None);

    let (url, branch, subdir) = git(base, "owner/repo/sub/dir", None, None);
    assert_eq!(url, "https://github.com/owner/repo");
    assert_eq!(subdir.as_deref(), Some("sub/dir"));
    assert_eq!(branch, None);

    let (url, branch, subdir) = git(base, "owner/repo.git", Some("v1.2.0"), None);
    assert_eq!(url, "https://github.com/owner/repo");
    assert_eq!(branch.as_deref(), Some("v1.2.0"));
    assert_eq!(subdir, None);

    let (url, _, subdir) = git(base, "Phall/PhuxPlugin.git/plugins/demo", None, None);
    assert_eq!(url, "https://github.com/Phall/PhuxPlugin");
    assert_eq!(subdir.as_deref(), Some("plugins/demo"));

    let (url, branch, subdir) = git(base, "owner/repo", Some("main"), Some("plugins/demo/"));
    assert_eq!(url, "https://github.com/owner/repo");
    assert_eq!(branch.as_deref(), Some("main"));
    assert_eq!(subdir.as_deref(), Some("plugins/demo"));
}

#[test]
fn urls_are_not_rewritten() {
    let base = Path::new("/tmp/phux-classify-missing");
    for reference in [
        "https://github.com/owner/repo",
        "https://github.com/owner/repo/tree/main/plugins/demo",
        "git@github.com:owner/repo.git",
        "ssh://git@github.com/owner/repo",
        "file:///tmp/plugin.git",
    ] {
        let (url, branch, subdir) = git(base, reference, None, None);
        assert_eq!(url, reference);
        assert_eq!(branch, None);
        assert_eq!(subdir, None, "{reference}");
    }
    let (url, branch, subdir) = git(
        base,
        "https://github.com/owner/repo",
        Some("main"),
        Some("plugins/demo"),
    );
    assert_eq!(url, "https://github.com/owner/repo");
    assert_eq!(branch.as_deref(), Some("main"));
    assert_eq!(subdir.as_deref(), Some("plugins/demo"));
}

#[test]
fn existing_local_path_wins_over_shorthand() {
    let base = tempfile::tempdir().unwrap();
    let plugin = base.path().join("acme").join("tools");
    fs::create_dir_all(&plugin).unwrap();
    fs::write(plugin.join("phux-plugin.toml"), "id = \"x\"\n").unwrap();

    let classified = classify_source_in(base.path(), "acme/tools", None, None).unwrap();
    match classified.source {
        InstallSource::Dir(path) => {
            assert_eq!(path, plugin.canonicalize().unwrap());
        }
        other => panic!("expected directory, got {other:?}"),
    }
    let note = classified.note.expect("ambiguity note");
    assert!(note.contains("chosen as a local directory"), "{note}");
    assert!(note.contains("https://github.com/acme/tools"), "{note}");
    assert!(note.contains("was not used"), "{note}");

    let dotted = base.path().join("dotted");
    fs::create_dir_all(dotted.join("keep")).unwrap();
    let classified = classify_source_in(base.path(), "./dotted/keep", None, None).unwrap();
    assert!(matches!(classified.source, InstallSource::Dir(_)));
    assert!(
        classified.note.is_none(),
        "a leading ./ is a path, not shorthand: {:?}",
        classified.note
    );

    let err = classify_source_in(base.path(), "acme/tools", Some("main"), None).unwrap_err();
    assert!(err.contains("chosen as a local directory"), "{err}");
    assert!(err.contains("--rev only applies to git sources"), "{err}");

    let err = classify_source_in(base.path(), "acme/tools", None, Some("plugins")).unwrap_err();
    assert!(
        err.contains("--subdir only applies to git sources"),
        "{err}"
    );
}

#[test]
fn invalid_references_name_the_choice() {
    let base = tempfile::tempdir().unwrap();
    let missing = classify_source_in(base.path(), "./owner/repo", None, None).unwrap_err();
    assert!(missing.contains("chosen as a local path"), "{missing}");
    assert!(missing.contains("never shorthand"), "{missing}");
    assert!(
        missing.contains("neither an existing local path nor a git URL"),
        "{missing}"
    );

    let parent = classify_source_in(base.path(), "owner/repo/../x", None, None).unwrap_err();
    assert!(parent.contains("chosen as a local path"), "{parent}");

    for reference in [
        "owner",
        "-owner/repo",
        "owner/-repo",
        "owner//repo",
        "owner/repo.git.git",
    ] {
        let err = classify_source_in(base.path(), reference, None, None).unwrap_err();
        assert!(
            err.contains("neither an existing local path nor a git URL"),
            "{reference}: {err}"
        );
        assert!(err.contains("GitHub shorthand"), "{reference}: {err}");
    }

    let both = classify_source_in(base.path(), "owner/repo/sub/dir", None, Some("other/place"))
        .unwrap_err();
    assert!(both.contains("sub/dir"), "{both}");
    assert!(both.contains("other/place"), "{both}");
    assert!(both.contains("neither was chosen"), "{both}");

    let file = base.path().join("acme").join("plain");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "not a plugin").unwrap();
    let err = classify_source_in(base.path(), "acme/plain", None, None).unwrap_err();
    assert!(err.contains("not a recognized tarball"), "{err}");
    assert!(err.contains("chosen as a local file"), "{err}");
    assert!(err.contains("https://github.com/acme/plain"), "{err}");

    let tar = base.path().join("plugin.tar.gz");
    fs::write(&tar, "tar").unwrap();
    let classified = classify_source_in(base.path(), "plugin.tar.gz", None, None).unwrap();
    assert!(matches!(classified.source, InstallSource::Tarball(_)));
    assert!(classified.note.is_none());
}
