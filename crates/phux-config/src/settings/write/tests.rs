//! Unit tests for the settings writer.
#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use super::*;

fn temp_files_in(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.contains(".tmp-"))
                })
                .collect()
        })
        .unwrap_or_default()
}

const PATH: &str = "/nonexistent/config.toml";

fn apply(text: &str, key: &str, edit: Edit) -> Result<EditOutcome, ConfigError> {
    apply_edit(text, key, edit, Path::new(PATH))
}

fn set(text: &str, key: &str, value: impl Into<toml::Value>) -> EditOutcome {
    apply(text, key, Edit::Set(value.into())).expect("edit applies")
}

/// (a) Untouched lines survive byte-for-byte: comments, blank lines,
/// odd spacing, the trailing newline.
#[test]
fn unrelated_comments_and_blank_lines_survive_byte_for_byte() {
    let text = "# my config\n\n\n[defaults]\n# keep this\nterm   =   \"xterm-256color\"  # spaced\n\n\n[sidebar]\nwidth = 28\n";
    let out = set(text, "sidebar.width", 32);
    assert_eq!(
        out.text,
        "# my config\n\n\n[defaults]\n# keep this\nterm   =   \"xterm-256color\"  # spaced\n\n\n[sidebar]\nwidth = 32\n"
    );
    assert_eq!(out.previous, Some(toml::Value::Integer(28)));
    assert_eq!(out.line.as_deref(), Some("width = 32"));
}

/// (b) An existing key is replaced on its own line, trailing comment
/// and position intact.
#[test]
fn setting_an_existing_key_replaces_in_place_and_keeps_its_comment() {
    let text = "[sidebar]\nenabled = true\nwidth = 28  # narrow\nposition = \"left\"\n";
    let out = set(text, "sidebar.width", 32);
    assert_eq!(
        out.text,
        "[sidebar]\nenabled = true\nwidth = 32  # narrow\nposition = \"left\"\n"
    );
    assert_eq!(out.line.as_deref(), Some("width = 32  # narrow"));
}

/// (c) A missing table is appended, separated from the previous block
/// by a blank line; a missing key in an existing table is appended to
/// that table.
#[test]
fn setting_a_key_whose_table_is_absent_appends_the_table() {
    let text = "[sidebar]\nwidth = 28\n";
    let out = set(text, "theme.accent", "#ff0000");
    assert_eq!(
        out.text,
        "[sidebar]\nwidth = 28\n\n[theme]\naccent = \"#ff0000\"\n"
    );
    assert_eq!(out.previous, None);
    assert_eq!(out.line.as_deref(), Some("accent = \"#ff0000\""));

    let out = set(&out.text, "sidebar.position", "right");
    assert_eq!(
        out.text,
        "[sidebar]\nwidth = 28\nposition = \"right\"\n\n[theme]\naccent = \"#ff0000\"\n"
    );

    // An empty document gets no leading blank line.
    let out = set("", "sidebar.width", 40);
    assert_eq!(out.text, "[sidebar]\nwidth = 40\n");
}

/// The scaffold-shaped file: a sub-table exists but its parent has no
/// header of its own. Setting a parent key must still produce a file
/// that loads with the value in effect.
#[test]
fn setting_a_key_in_an_implicit_parent_table_loads_correctly() {
    let text = "[keybindings.prefix-table]\n\"x\" = \"kill-pane\"\n";
    let out = set(text, "keybindings.prefix", "C-b");
    let cfg = crate::parse_with_defaults(&out.text, Path::new(PATH)).expect("loads");
    assert_eq!(cfg.keybindings.prefix, "C-b");
    assert_eq!(
        cfg.keybindings.prefix_table.get("x"),
        Some(&crate::Action::Bare("kill-pane".to_owned()))
    );
    assert!(out.text.contains("\"x\" = \"kill-pane\""), "{}", out.text);
}

/// A key written in dotted form at the root is still found and
/// replaced in place.
#[test]
fn dotted_root_keys_are_edited_in_place() {
    let out = set("sidebar.width = 28 # d\n", "sidebar.width", 30);
    assert_eq!(out.text, "sidebar.width = 30 # d\n");
    assert_eq!(out.previous, Some(toml::Value::Integer(28)));
}

/// (d) Unset removes the leaf and reports what it held; the table stays.
#[test]
fn unset_removes_the_leaf_and_reports_previous() {
    let text = "[sidebar]\nwidth = 32  # gone\nposition = \"right\"\n";
    let out = apply(text, "sidebar.width", Edit::Unset).expect("unset");
    assert_eq!(out.text, "[sidebar]\nposition = \"right\"\n");
    assert_eq!(out.previous, Some(toml::Value::Integer(32)));
    assert_eq!(out.line, None);

    let out = apply(&out.text, "sidebar.position", Edit::Unset).expect("unset");
    assert_eq!(out.text, "[sidebar]\n");

    // Unsetting what is not there is a no-op success.
    let out = apply("[defaults]\nterm = \"a\"\n", "sidebar.width", Edit::Unset).expect("noop");
    assert_eq!(out.text, "[defaults]\nterm = \"a\"\n");
    assert_eq!(out.previous, None);
    let out = apply("", "sidebar.width", Edit::Unset).expect("noop on empty");
    assert_eq!(out.text, "");
}

/// (e) Invalid values are rejected with an error naming the problem.
#[test]
fn invalid_values_are_rejected_and_the_error_names_the_problem() {
    let err = apply("", "sidebar.width", Edit::Set("wide".into())).expect_err("wrong type");
    let text = err.to_string();
    assert!(
        text.contains("sidebar.width") && text.contains("bad value"),
        "{text}"
    );

    let err =
        apply("", "keybindings.prefix", Edit::Set("not a chord".into())).expect_err("bad chord");
    let text = err.to_string();
    assert!(
        text.contains("keybindings.prefix") && text.contains("bad chord"),
        "{text}"
    );

    let over = i64::from(crate::MAX_HISTORY_BYTES) + 1;
    let err = apply("", "defaults.history-bytes", Edit::Set(over.into())).expect_err("cap");
    let text = err.to_string();
    assert!(
        text.contains("defaults.history-bytes") && text.contains("67108864"),
        "{text}"
    );

    let err = apply("", "sidebar.position", Edit::Set("middle".into())).expect_err("variant");
    assert!(err.to_string().contains("sidebar.position"), "{err}");

    // An unknown key is a schema finding at that key.
    let err = apply("", "sidebar.widht", Edit::Set(1.into())).expect_err("typo");
    let text = err.to_string();
    assert!(
        text.contains("sidebar.widht") && text.contains("unknown key"),
        "{text}"
    );
}

#[test]
fn malformed_paths_and_shapes_are_rejected() {
    assert!(matches!(
        apply("", "width", Edit::Set(1.into())),
        Err(ConfigError::Edit { .. })
    ));
    assert!(matches!(
        apply("", "sidebar..width", Edit::Set(1.into())),
        Err(ConfigError::Edit { .. })
    ));
    assert!(matches!(
        apply("", ".width", Edit::Set(1.into())),
        Err(ConfigError::Edit { .. })
    ));
    // Path through a scalar.
    let err = apply(
        "[sidebar]\nwidth = 1\n",
        "sidebar.width.x",
        Edit::Set(1.into()),
    )
    .expect_err("through scalar");
    assert!(err.to_string().contains("not a table"), "{err}");
    // The leaf is a table.
    let err = apply(
        "[keybindings.prefix-table]\n",
        "keybindings.prefix-table",
        Edit::Unset,
    )
    .expect_err("table leaf");
    assert!(err.to_string().contains("holds a table"), "{err}");
    // Date-times are refused before the schema even sees them.
    let dt: toml::Value = toml::from_str::<toml::Table>("v = 1979-05-27T07:32:00Z")
        .unwrap()
        .remove("v")
        .unwrap();
    assert!(matches!(
        apply("", "defaults.term", Edit::Set(dt)),
        Err(ConfigError::Edit { .. })
    ));
    // Broken input surfaces as a parse error with a position.
    assert!(matches!(
        apply("[sidebar\n", "sidebar.width", Edit::Set(1.into())),
        Err(ConfigError::Parse {
            position: Some(_),
            ..
        })
    ));
}

/// An argv lands as a TOML array and reads back through the schema.
#[test]
fn argv_values_round_trip() {
    let argv = toml::Value::Array(vec!["curl".into(), "-F".into(), "file=@{path}".into()]);
    let out = set("", "voice.transcriber", argv);
    assert_eq!(
        out.text,
        "[voice]\ntranscriber = [\"curl\", \"-F\", \"file=@{path}\"]\n"
    );
    let cfg = crate::parse_with_defaults(&out.text, Path::new(PATH)).expect("loads");
    assert!(cfg.voice.is_configured());
    let out = set(&out.text, "voice.timeout-secs", 5);
    let cfg = crate::parse_with_defaults(&out.text, Path::new(PATH)).expect("loads");
    assert_eq!(cfg.voice.timeout_secs, Some(5));
    let out = set("", "experimental.predictive-echo", false);
    let cfg = crate::parse_with_defaults(&out.text, Path::new(PATH)).expect("loads");
    assert_eq!(cfg.experimental.predictive_echo, Some(false));
}

/// (f) A missing file is created along with its parent directory.
#[test]
fn write_edit_creates_a_missing_file_and_its_parent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested").join("deeper").join("config.toml");
    let out = write_edit(&path, "sidebar.width", Edit::Set(32.into())).expect("write");
    assert_eq!(out.previous, None);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[sidebar]\nwidth = 32\n"
    );
    assert!(temp_files_in(path.parent().unwrap()).is_empty());

    // A second edit reads the file back and edits it in place.
    let out = write_edit(&path, "sidebar.width", Edit::Set(40.into())).expect("write");
    assert_eq!(out.previous, Some(toml::Value::Integer(32)));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[sidebar]\nwidth = 40\n"
    );
}

/// (g) When the edit is rejected the file is byte-for-byte untouched.
#[test]
fn the_file_is_untouched_when_the_edit_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let original = "# precious\n[sidebar]\nwidth = 28\n";
    std::fs::write(&path, original).unwrap();
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();

    let err = write_edit(&path, "sidebar.width", Edit::Set("wide".into())).expect_err("bad");
    assert!(matches!(err, ConfigError::Edit { .. }), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        before
    );
    assert!(temp_files_in(dir.path()).is_empty());

    // A no-op unset does not rewrite the file either.
    write_edit(&path, "chrome.compact-cols", Edit::Unset).expect("noop");
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        before
    );
}

/// A user with a pre-existing problem elsewhere still cannot write,
/// because the result would not load; the error names the real fault.
#[test]
fn a_file_that_does_not_load_blocks_unrelated_edits() {
    let err = apply(
        "[sidebar]\nwidht = 1\n",
        "chrome.compact-cols",
        Edit::Set(70.into()),
    )
    .expect_err("unknown key elsewhere");
    assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
    assert!(err.to_string().contains("widht"), "{err}");
}

#[cfg(unix)]
#[test]
fn a_symlinked_config_is_edited_through_the_link_and_keeps_its_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let real = dir.path().join("dotfiles").join("phux.toml");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(&real, "[sidebar]\nwidth = 28\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = dir.path().join("config.toml");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    write_edit(&link, "sidebar.width", Edit::Set(32.into())).expect("write");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        "[sidebar]\nwidth = 32\n"
    );
    assert_eq!(
        std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

/// Another phux holding the config lock past [`LOCK_WAIT`] makes the
/// edit report busy instead of waiting longer, and the file is untouched.
///
/// [`LOCK_WAIT`]: crate::document::LOCK_WAIT
#[test]
fn a_settings_edit_reports_busy_while_another_writer_holds_the_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let original = "[sidebar]\nwidth = 28\n";
    std::fs::write(&path, original).unwrap();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.path().join("config.toml.registry.lock"))
        .unwrap();
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        held.lock().expect("hold the lock");
        locked_tx.send(()).unwrap();
        std::thread::sleep(crate::document::LOCK_WAIT * 3);
        drop(held);
    });
    locked_rx.recv().unwrap();

    let err = write_edit(&path, "sidebar.width", Edit::Set(32.into())).expect_err("busy");
    assert!(
        matches!(err, ConfigError::Write { .. }) && err.to_string().contains("busy"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    holder.join().unwrap();
}

/// A change another editor makes between the locked read and the
/// publication is refused rather than overwritten.
#[test]
fn a_settings_edit_refuses_a_file_changed_after_it_was_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[sidebar]\nwidth = 28\n").unwrap();
    let mut doc = open_settings(&path).expect("open");
    apply_edit_doc(&mut doc, "sidebar.width", Edit::Set(32.into()), &path).expect("edit");
    std::fs::write(&path, "# external edit\n").unwrap();

    let err = publish_if_changed(doc, &path).expect_err("stale");
    assert!(
        matches!(err, ConfigError::Write { .. }) && err.to_string().contains("changed"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# external edit\n");
    assert!(temp_files_in(dir.path()).is_empty());
}

/// Several edits land as one transaction, validated only as a whole.
#[test]
fn write_edits_publishes_every_edit_at_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[sidebar]\nwidth = 28\n").unwrap();
    write_edits(
        &path,
        vec![
            ("sidebar.width", Edit::Unset),
            ("sidebar.position", Edit::Set("right".into())),
        ],
    )
    .expect("write");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[sidebar]\nposition = \"right\"\n"
    );

    let err = write_edits(
        &path,
        vec![
            ("sidebar.position", Edit::Unset),
            ("sidebar.width", Edit::Set("wide".into())),
        ],
    )
    .expect_err("second edit is invalid");
    assert!(err.to_string().contains("sidebar.width"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[sidebar]\nposition = \"right\"\n",
        "nothing lands when any edit is refused"
    );
}

#[cfg(unix)]
#[test]
fn a_dangling_symlinked_config_is_refused_not_replaced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let link = dir.path().join("config.toml");
    std::os::unix::fs::symlink(dir.path().join("missing.toml"), &link).unwrap();
    let err = write_edit(&link, "sidebar.width", Edit::Set(32.into())).expect_err("dangling");
    assert!(matches!(err, ConfigError::Write { .. }), "{err}");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_comment_only_scaffold_keeps_its_comments_above_the_first_table() {
    // `phux config init` writes a file where every line is a comment;
    // the first edit must not push that whole scaffold beneath itself.
    let scaffold = "# phux config\n# [sidebar]\n# width = 28\n";
    let out = apply_edit(
        scaffold,
        "sidebar.width",
        Edit::Set(toml::Value::Integer(32)),
        Path::new("config.toml"),
    )
    .expect("edit applies");
    assert_eq!(
        out.text,
        "# phux config\n# [sidebar]\n# width = 28\n[sidebar]\nwidth = 32\n"
    );
}
