//! Unit tests for comment-preserving machine-registry edits.

use super::{tables_mut, validate_fingerprint};
use toml_edit::{DocumentMut, Table};

#[test]
fn review_competing_registry_mutation_cannot_be_lost_during_publication() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let raw =
        "[[remote]]\nname='a'\nendpoint='ssh://a'\n[[remote]]\nname='b'\nendpoint='ssh://b'\n";
    std::fs::write(&path, raw).expect("seed");
    let nested_succeeded = std::rc::Rc::new(std::cell::Cell::new(false));
    let observed = nested_succeeded.clone();
    let competing_path = path.clone();
    super::BEFORE_PUBLISH.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            observed
                .set(super::forget_machine(&competing_path, raw, "remote", "b", "ssh://b").is_ok());
        }));
    });
    super::forget_machine(&path, raw, "remote", "a", "ssh://a").expect("outer forget");
    assert!(
        !nested_succeeded.get(),
        "a cooperating writer committed inside another writer's checked publication"
    );
    let after = std::fs::read_to_string(path).expect("read");
    assert!(after.contains("name='b'"));
    assert!(!after.contains("name='a'"));
}

#[cfg(unix)]
#[test]
fn symlink_and_real_path_writers_cannot_interleave_during_publication() {
    let dir = tempfile::tempdir().expect("tempdir");
    let real = dir.path().join("real.toml");
    let alias = dir.path().join("config.toml");
    std::os::unix::fs::symlink(&real, &alias).expect("symlink");
    let raw =
        "[[remote]]\nname='a'\nendpoint='ssh://a'\n[[remote]]\nname='b'\nendpoint='ssh://b'\n";
    for (outer, competing) in [(&alias, &real), (&real, &alias)] {
        std::fs::write(&real, raw).expect("seed");
        let nested_succeeded = std::rc::Rc::new(std::cell::Cell::new(false));
        let observed = nested_succeeded.clone();
        let competing = competing.clone();
        super::BEFORE_PUBLISH.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                observed
                    .set(super::forget_machine(&competing, raw, "remote", "b", "ssh://b").is_ok());
            }));
        });
        super::forget_machine(outer, raw, "remote", "a", "ssh://a").expect("outer forget");
        assert!(
            !nested_succeeded.get(),
            "alias writer committed after the other writer's checked publication"
        );
        let after = std::fs::read_to_string(&real).expect("read");
        assert!(after.contains("name='b'"));
        assert!(!after.contains("name='a'"));
        assert!(
            std::fs::symlink_metadata(&alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        // The refused writer can refresh and retry after the owner commits.
        super::forget_machine(&alias, &after, "remote", "b", "ssh://b").expect("retry");
        assert!(!std::fs::read_to_string(&real).unwrap().contains("name='b'"));
    }
}

#[cfg(unix)]
#[test]
fn dangling_registry_symlink_is_refused_without_replacing_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let real = dir.path().join("missing.toml");
    let alias = dir.path().join("config.toml");
    std::os::unix::fs::symlink(&real, &alias).expect("symlink");
    assert!(
        super::edit_document(&alias).is_err(),
        "a dangling alias must not become a separate transaction target"
    );
    assert!(
        std::fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!real.exists());
}

#[cfg(unix)]
#[test]
fn missing_registry_through_parent_alias_retains_one_transaction_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    let real = dir.path().join("real");
    std::fs::create_dir(&real).expect("real parent");
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&real, &alias).expect("parent alias");
    let path = real.join("nested/config.toml");
    let aliased_path = alias.join("nested/config.toml");
    let mut first = super::edit_document(&aliased_path).expect("new aliased registry");
    assert!(super::edit_document(&path).is_err(), "real path contends");
    first["first"] = toml_edit::value(1);
    first.commit().expect("publish");
    let second = super::edit_document(&path).expect("released after publication");
    assert_eq!(second["first"].as_integer(), Some(1));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        std::fs::read_to_string(&aliased_path).unwrap()
    );
}

#[test]
fn transaction_lock_covers_read_modify_publish_and_releases_on_drop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let mut first = super::edit_document(&path).expect("first transaction");
    assert!(
        super::edit_document(&path).is_err(),
        "second writer cannot read under first writer's lock"
    );
    first["first"] = toml_edit::value(1);
    first.commit().expect("publish first");
    let mut second = super::edit_document(&path).expect("lock released");
    assert_eq!(second["first"].as_integer(), Some(1));
    second["second"] = toml_edit::value(2);
    second.commit().expect("publish second");
    let third = super::edit_document(&path).expect("third transaction");
    std::fs::write(&path, "# external edit\n").expect("noncooperating editor");
    assert!(
        third.commit().is_err(),
        "observed external changes are refused"
    );
    assert_eq!(
        std::fs::read_to_string(path).expect("preserved"),
        "# external edit\n"
    );
}

/// An operator's comments survive, a symlinked config is written
/// through (link and mode intact), and no temp file is left behind.
#[cfg(unix)]
#[test]
fn commit_preserves_comments_symlinks_and_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let real = dir.path().join("dotfiles").join("phux.toml");
    std::fs::create_dir_all(real.parent().unwrap()).expect("dotfiles dir");
    std::fs::write(&real, "# my notes\n[defaults]\nshell = \"fish\"\n").expect("seed");
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).expect("mode");
    let link = dir.path().join("config.toml");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    let mut edit = super::edit_document(&link).expect("parse");
    tables_mut(&mut edit, "remote")
        .expect("array")
        .push(toml_edit::Table::new());
    edit.commit().expect("write through symlink");

    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let back = std::fs::read_to_string(&real).expect("read target");
    assert!(back.contains("# my notes") && back.contains("shell = \"fish\""));
    assert!(back.contains("[[remote]]"));
    assert_eq!(
        std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let leftovers = std::fs::read_dir(real.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
        .count();
    assert_eq!(leftovers, 0);
}

/// `upsert_machine` appends when the lookup finds nothing and rewrites in
/// place otherwise; `set_or_remove` clears a field the rewrite omits.
#[test]
fn upsert_appends_then_rewrites_in_place_clearing_omitted_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "# keep me\n[[remote]]\nname = \"a\"\n").expect("seed");
    let fill = |endpoint: &str, token: Option<&str>| {
        let endpoint = endpoint.to_owned();
        let token = token.map(str::to_owned);
        move |table: &mut Table| {
            table.insert("name", toml_edit::value("b"));
            table.insert("endpoint", toml_edit::value(endpoint));
            super::set_or_remove(table, "token-file", token);
        }
    };
    super::upsert_machine(&path, "remote", || Ok(None), fill("ssh://b", Some("/t")))
        .expect("append");
    super::upsert_machine(&path, "remote", || Ok(Some(1)), fill("ssh://c", None)).expect("rewrite");
    let doc: DocumentMut = std::fs::read_to_string(&path)
        .expect("read")
        .parse()
        .expect("parse");
    assert!(doc.to_string().starts_with("# keep me"));
    let tables = doc["remote"].as_array_of_tables().expect("array");
    assert_eq!(tables.len(), 2, "rewritten, not appended");
    assert_eq!(super::machine_indices(tables, "b", "ssh://c"), vec![1]);
    assert!(tables.get(1).expect("b").get("token-file").is_none());
    assert!(super::upsert_machine(&path, "remote", || Err("lookup".to_owned()), |_| {}).is_err());
    super::remove_machine_entry(&path, "remote", "b", "ssh://c").expect("remove");
    let after = std::fs::read_to_string(&path).expect("read");
    assert!(!after.contains("ssh://c") && after.contains("name = \"a\""));
}

#[test]
fn shared_validators_keep_each_registry_s_wording() {
    assert_eq!(
        super::reject_duplicate_names(["a", "b", "a"], "satellite"),
        Err("duplicate satellite name \"a\"".to_owned())
    );
    assert_eq!(super::reject_duplicate_names(["a", "b"], "remote"), Ok(()));
    assert_eq!(super::plain_machine_name("  mini "), Some("mini"));
    for bad in ["", "  ", "a/b", "a:b"] {
        assert_eq!(super::plain_machine_name(bad), None, "{bad:?}");
    }
    let err =
        super::validate_token_file(std::path::Path::new("rel"), "satellite").expect_err("relative");
    assert_eq!(
        err,
        "satellite token-file must be an absolute path (got rel)"
    );
    assert!(super::validate_token_file(std::path::Path::new(""), "remote").is_err());
    assert!(super::validate_token_file(std::path::Path::new("/abs"), "remote").is_ok());
}

#[test]
fn fingerprint_accepts_both_shapes_pair_prints() {
    let bare = "ab".repeat(32);
    assert_eq!(validate_fingerprint(&bare, "remote").as_deref(), Ok(&*bare));
    let colon = (0..32).map(|_| "AB").collect::<Vec<_>>().join(":");
    assert!(validate_fingerprint(&colon, "remote").is_ok());
    // Surrounding whitespace from a paste is trimmed, not rejected.
    assert!(validate_fingerprint(&format!("  {bare}\n"), "remote").is_ok());
}

#[test]
fn fingerprint_rejects_a_truncated_paste() {
    // 63 digits — the classic one-character-short copy.
    let short = "a".repeat(63);
    assert!(validate_fingerprint(&short, "remote").is_err());
    assert!(validate_fingerprint("", "remote").is_err());
    assert!(validate_fingerprint(&"z".repeat(64), "remote").is_err());
    // The message must name which registry rejected it.
    let err = validate_fingerprint("nope", "satellite").expect_err("reject");
    assert!(err.starts_with("satellite cert-fingerprint"), "got {err}");
}

/// Another holder of the registry lock (another phux, or a child that
/// inherited it while being spawned) is waited out, not reported as busy.
#[test]
fn a_briefly_held_lock_is_waited_for() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "").expect("seed");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.path().join("config.toml.registry.lock"))
        .expect("lock file");
    held.lock().expect("hold the lock");
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(held);
    });
    let edit = super::edit_document(&path).expect("waits for the holder");
    edit.commit().expect("publish");
    release.join().expect("released");
}
