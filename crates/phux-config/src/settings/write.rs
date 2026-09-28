//! Set or unset one dotted key in the user's `config.toml`, keeping every
//! other byte (ADR-0023: the file is the source of truth).
//!
//! Edits go through `toml_edit`: an existing value is replaced in place
//! (trailing comment intact), a missing key or table is appended. Every edit
//! is validated (it must load, and `phux config check` must report nothing at
//! the key) before [`write_edit`] touches disk.

use std::io::Write as _;
use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, TableLike};

use crate::ConfigError;

/// What to do to one key.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// Assign this value, creating the key and its table as needed.
    Set(toml::Value),
    /// Remove the key from the user's file so the layers beneath show
    /// through. Removing an absent key is a no-op success.
    Unset,
}

/// What an edit did.
#[derive(Debug, Clone, PartialEq)]
pub struct EditOutcome {
    /// The whole file after the edit.
    pub text: String,
    /// What the *user's file* held at the key before the edit — not the
    /// merged effective value. `None` when the key was absent.
    pub previous: Option<toml::Value>,
    /// For a `Set`, the assignment line now in the file (leading indent and
    /// newline trimmed; a trailing comment kept), e.g. `width = 32`. `None`
    /// for an `Unset`.
    pub line: Option<String>,
}

/// Apply `edit` to `key` (`table.leaf`, bare segments) in `user_toml` and
/// validate the result, without writing. `path` names the file in errors and
/// anchors relative `extends` entries.
///
/// # Errors
///
/// [`ConfigError::Parse`] when the input or the edited result does not load;
/// [`ConfigError::Edit`] for a malformed key, a path through a non-table, a
/// table leaf, a date-time value, or a `phux config check` finding at the
/// key.
pub fn apply_edit(
    user_toml: &str,
    key: &str,
    edit: Edit,
    path: &Path,
) -> Result<EditOutcome, ConfigError> {
    let (table_path, leaf) = key.rsplit_once('.').ok_or_else(|| {
        edit_error(
            key,
            "a setting lives inside a table; use `<table>.<key>` (there are no top-level \
             settings)",
        )
    })?;
    if key.split('.').any(str::is_empty) {
        return Err(edit_error(key, "empty path segment"));
    }

    let mut doc = parse_document(user_toml, path)?;
    let previous = previous_value(user_toml, key, path)?;

    let line = match edit {
        Edit::Set(value) => {
            let new_value = to_edit_value(&value, key)?;
            let document_was_empty = doc.as_table().is_empty();
            let table = table_for_set(&mut doc, table_path, key, document_was_empty)?;
            set_leaf(table, leaf, key, new_value)?;
            table
                .get_key_value(leaf)
                .map(|(k, item)| render_line(k, item))
        }
        Edit::Unset => {
            unset_leaf(&mut doc, table_path, leaf, key)?;
            None
        }
    };

    let text = doc.to_string();
    validate(&text, key, path)?;
    Ok(EditOutcome {
        text,
        previous,
        line,
    })
}

/// Read `path` (missing is empty), apply `edit`, and replace the file.
///
/// The write goes through an fsynced sibling temp file and a rename. A
/// symlink is followed so the link survives, the target's permissions are
/// kept, and a no-op edit does not touch the file.
///
/// # Errors
///
/// Everything [`apply_edit`] returns (the file is untouched);
/// [`ConfigError::Io`] when the file cannot be read; [`ConfigError::Write`]
/// when the temp file or rename fails.
pub fn write_edit(path: &Path, key: &str, edit: Edit) -> Result<EditOutcome, ConfigError> {
    let current = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(ConfigError::Io(err)),
    };
    let outcome = apply_edit(&current, key, edit, path)?;
    if outcome.text != current {
        replace_atomically(path, &outcome.text, || Ok(())).map_err(|source| {
            ConfigError::Write {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(outcome)
}

/// A [`ConfigError::Edit`] for `key`.
fn edit_error(key: &str, message: impl Into<String>) -> ConfigError {
    ConfigError::Edit {
        key: key.to_owned(),
        message: message.into(),
    }
}

/// Parse the user's text as a mutable, formatting-preserving document.
fn parse_document(user_toml: &str, path: &Path) -> Result<DocumentMut, ConfigError> {
    user_toml
        .parse::<DocumentMut>()
        .map_err(|err| ConfigError::parse(path, user_toml, err.span(), err.message()))
}

/// What the user's file holds at `key` before the edit, read through the
/// plain `toml` parser (the two crates' date-time types do not convert).
fn previous_value(
    user_toml: &str,
    key: &str,
    path: &Path,
) -> Result<Option<toml::Value>, ConfigError> {
    let table = crate::layer::parse_table(user_toml, path)?;
    Ok(super::value_at(&table, key).cloned())
}

/// Convert a `toml::Value` into a `toml_edit::Value`; date-times are refused
/// (no setting takes one).
fn to_edit_value(value: &toml::Value, key: &str) -> Result<toml_edit::Value, ConfigError> {
    Ok(match value {
        toml::Value::String(s) => toml_edit::Value::from(s.as_str()),
        toml::Value::Integer(i) => toml_edit::Value::from(*i),
        toml::Value::Float(f) => toml_edit::Value::from(*f),
        toml::Value::Boolean(b) => toml_edit::Value::from(*b),
        toml::Value::Datetime(_) => {
            return Err(edit_error(key, "no setting takes a date-time value"));
        }
        toml::Value::Array(items) => {
            let mut array = toml_edit::Array::new();
            for item in items {
                array.push(to_edit_value(item, key)?);
            }
            toml_edit::Value::Array(array)
        }
        toml::Value::Table(table) => {
            let mut inline = toml_edit::InlineTable::new();
            for (k, v) in table {
                inline.insert(k.as_str(), to_edit_value(v, key)?);
            }
            toml_edit::Value::InlineTable(inline)
        }
    })
}

/// Walk `table_path`, creating tables as needed; a new top-level table in a
/// non-empty document gets a blank line before its header.
fn table_for_set<'d>(
    doc: &'d mut DocumentMut,
    table_path: &str,
    key: &str,
    document_was_empty: bool,
) -> Result<&'d mut dyn TableLike, ConfigError> {
    // A comment-only document (a `phux config init` scaffold) holds its text
    // as trailing decor, emitted after any appended table: move it onto the
    // new table's prefix so the scaffold stays above the first assignment.
    let mut carried_prefix = if document_was_empty {
        let trailing = doc.trailing().as_str().unwrap_or("").to_owned();
        if trailing.trim().is_empty() {
            None
        } else {
            doc.set_trailing("");
            Some(if trailing.ends_with('\n') {
                trailing
            } else {
                format!("{trailing}\n")
            })
        }
    } else {
        None
    };
    let mut current: &'d mut dyn TableLike = doc.as_table_mut();
    for (depth, segment) in table_path.split('.').enumerate() {
        let item = current.entry(segment).or_insert_with(|| {
            let mut table = Table::new();
            if depth == 0 {
                if let Some(prefix) = carried_prefix.take() {
                    table.decor_mut().set_prefix(prefix);
                } else if !document_was_empty {
                    table.decor_mut().set_prefix("\n");
                }
            }
            Item::Table(table)
        });
        current = item.as_table_like_mut().ok_or_else(|| {
            edit_error(
                key,
                format!("`{segment}` is not a table, so nothing can be set beneath it"),
            )
        })?;
    }
    Ok(current)
}

/// The assignment line as the file now shows it, trimmed. Missing decor is
/// filled the way the table encoder fills it (one space each side of `=`).
fn render_line(key: &toml_edit::Key, item: &Item) -> String {
    let key_suffix = key
        .leaf_decor()
        .suffix()
        .and_then(toml_edit::RawString::as_str)
        .unwrap_or(" ");
    let value_has_prefix = item
        .as_value()
        .and_then(|value| value.decor().prefix())
        .and_then(toml_edit::RawString::as_str)
        .is_some();
    let value_prefix = if value_has_prefix { "" } else { " " };
    format!("{}{key_suffix}={value_prefix}{item}", key.display_repr())
        .trim()
        .to_owned()
}

/// Assign `value` to `leaf`, in place (keeping decor) when the key exists.
fn set_leaf(
    table: &mut dyn TableLike,
    leaf: &str,
    key: &str,
    mut value: toml_edit::Value,
) -> Result<(), ConfigError> {
    match table.get_mut(leaf) {
        None => {
            table.insert(leaf, Item::Value(value));
        }
        Some(existing) => {
            let slot = existing.as_value_mut().ok_or_else(|| {
                edit_error(key, "holds a table, not a value; edit the file directly")
            })?;
            *value.decor_mut() = slot.decor().clone();
            *slot = value;
        }
    }
    Ok(())
}

/// Remove `leaf` from the table at `table_path`, if both exist. An emptied
/// table is left in place with its comments.
fn unset_leaf(
    doc: &mut DocumentMut,
    table_path: &str,
    leaf: &str,
    key: &str,
) -> Result<(), ConfigError> {
    let mut current: &mut dyn TableLike = doc.as_table_mut();
    for segment in table_path.split('.') {
        let Some(item) = current.get_mut(segment) else {
            return Ok(());
        };
        current = item.as_table_like_mut().ok_or_else(|| {
            edit_error(
                key,
                format!("`{segment}` is not a table, so nothing can be unset beneath it"),
            )
        })?;
    }
    if current.get(leaf).is_some_and(|item| !item.is_value()) {
        return Err(edit_error(
            key,
            "holds a table, not a value; edit the file directly",
        ));
    }
    current.remove(leaf);
    Ok(())
}

/// The gate every edit passes: a `phux config check` finding at the key
/// blocks it (findings elsewhere do not), and the whole file must still
/// load.
fn validate(text: &str, key: &str, path: &Path) -> Result<(), ConfigError> {
    let report = crate::check::check(text, path)?;
    if let Some(finding) = report.findings.iter().find(|f| f.path == key) {
        return Err(edit_error(
            key,
            format!("{}: {}", finding.fault.label(), finding.message),
        ));
    }
    crate::parse_with_defaults(text, path)?;
    Ok(())
}

/// Replace `path` with `text` via an exclusive, fsynced sibling temp file
/// and a rename, so an interrupted write never truncates the config.
///
/// A symlink is followed (the link survives) and the target's permissions
/// are copied. `before_rename` runs after the fsync and may veto the
/// publication; the temp file is removed on any failure. Shared with the
/// machine registries.
#[allow(
    clippy::redundant_pub_crate,
    reason = "private module helper; pub would trip unreachable_pub"
)]
pub(crate) fn replace_atomically(
    path: &Path,
    text: &str,
    before_rename: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| Path::new("."), Path::new);
    std::fs::create_dir_all(parent)?;
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name")
        })?;
    let tmp = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        temp_nonce()
    ));
    let publish = || -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        if let Ok(metadata) = std::fs::metadata(&target) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        before_rename()?;
        std::fs::rename(&tmp, &target)
    };
    publish().inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// A temp-path suffix so concurrent writers do not collide.
fn temp_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(test)]
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

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

        let err = apply("", "keybindings.prefix", Edit::Set("not a chord".into()))
            .expect_err("bad chord");
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
}
