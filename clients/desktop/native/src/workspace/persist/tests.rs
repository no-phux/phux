use super::*;

#[test]
fn key_scoped_paths_are_rename_stable_and_separate_from_name_scoped_paths() {
    let key = phux_protocol::ids::SessionKey::new([1; 16]).unwrap();
    let keyed = session_layout_paths("private-server.sock", key);
    assert_eq!(keyed, session_layout_paths("private-server.sock", key));
    assert_ne!(
        keyed.layout,
        layout_paths("private-server.sock", "A").layout
    );
    assert_ne!(
        keyed.layout,
        layout_paths("private-server.sock", "renamed-A").layout
    );
    assert_ne!(
        keyed.layout,
        session_layout_paths("another-server.sock", key).layout
    );
}

#[test]
fn key_store_imports_without_overwriting_old_or_conflicting_layouts() {
    let scratch = Scratch::new("layout-key");
    let old = scratch.join("name.json");
    let keyed = scratch.join("key.json");
    let old_value = serde_json::json!({"version": 2, "serverId": "old", "tabs": []});
    fs::write(&old, old_value.to_string()).unwrap();
    let store = FileLayoutStore::new(&keyed, Some(old.clone()));
    assert_eq!(store.read(), StoreValue::Present(old_value.clone()));
    assert!(
        !keyed.exists(),
        "reading the fallback does not migrate destructively"
    );
    for bytes in ["damaged", r#"{"version":99,"serverId":"other","tabs":[]}"#] {
        fs::write(&keyed, bytes).unwrap();
        assert!(load_layout(&store.read()).blocked);
        assert_eq!(fs::read_to_string(&keyed).unwrap(), bytes);
        assert_eq!(fs::read_to_string(&old).unwrap(), old_value.to_string());
    }
    let conflicting = serde_json::json!({"version": 2, "serverId": "other", "tabs": []});
    fs::write(&keyed, conflicting.to_string()).unwrap();
    assert_eq!(store.read(), StoreValue::Present(conflicting.clone()));
    assert_eq!(fs::read_to_string(&keyed).unwrap(), conflicting.to_string());
    assert_eq!(fs::read_to_string(&old).unwrap(), old_value.to_string());
}

#[test]
fn sha256_matches_the_empty_and_abc_vectors() {
    assert_eq!(
        hex(sha256(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex(sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn version_one_migrates_into_even_row_splits_and_future_versions_are_blocked() {
    let parsed = parse_layout(&serde_json::json!({
        "version": 1,
        "serverId": "s",
        "tabs": [{ "id": "t", "terminals": ["a", "b", "c"], "focusedTerminal": "b" }],
        "display": { "fontSize": 15, "optionAsAlt": true }
    }))
    .expect("v1");
    assert_eq!(parsed.version, 2);
    assert_eq!(parsed.display.font_size, 15.0);
    assert!(parsed.display.option_as_alt);
    let SavedNode::Split { ratio, second, .. } = &parsed.tabs[0].root else {
        panic!("row");
    };
    assert!((f64::from(*ratio) - 1.0 / 3.0).abs() < 1e-5);
    let SavedNode::Split { ratio, .. } = second.as_ref() else {
        panic!("inner");
    };
    assert!((ratio - 0.5).abs() < 1e-5);
    assert!(
        parse_layout(&serde_json::json!({ "version": 9, "serverId": "s", "tabs": [] })).is_none()
    );
    let damaged =
        StoreValue::Present(serde_json::json!({ "version": 99, "serverId": "s", "tabs": [] }));
    assert!(load_layout(&damaged).blocked);
    assert!(!load_layout(&StoreValue::Missing).blocked);
}

#[test]
fn sanitize_bounds_unknown_themes() {
    use crate::settings::{DisplayPrefs, SIDEBAR_MAX, sanitize_display};
    let clean = sanitize_display(&DisplayPrefs {
        font_size: 400.0,
        line_height: f64::NAN,
        sidebar_width: 9000.0,
        theme_id: "neon".into(),
        font_family: "   ".into(),
        terminal_opacity: 0.1,
        ..DisplayPrefs::default()
    });
    assert_eq!(clean.font_size, 28.0);
    assert_eq!(clean.line_height, DisplayPrefs::default().line_height);
    assert_eq!(clean.sidebar_width, SIDEBAR_MAX);
    assert_eq!(clean.theme_id, DisplayPrefs::default().theme_id);
    assert_eq!(clean.font_family, "Paper Mono");
    assert_eq!(clean.terminal_opacity, 0.55);
}

#[test]
fn recent_folders_survive_restart_reorder_and_forget_without_exceeding_limit() {
    let scratch = Scratch::new("recent");
    let store = FileLayoutStore::new(scratch.join("projects.json"), None);
    let mut history = RecentProjects::from_store(store.clone());
    for index in 0..10 {
        history
            .record(PathBuf::from(format!("/checkouts/{index}")))
            .expect("save");
    }
    assert_eq!(history.entries().len(), 8);
    assert_eq!(history.entries()[0], Path::new("/checkouts/9"));
    assert!(!history.entries().contains(&PathBuf::from("/checkouts/0")));
    history
        .record(PathBuf::from("/checkouts/4"))
        .expect("reorder");
    assert!(history.flush().is_empty());
    let mut restarted = RecentProjects::from_store(store.clone());
    assert_eq!(restarted.entries()[0], Path::new("/checkouts/4"));
    assert_eq!(restarted.entries().len(), 8);
    restarted.forget(Path::new("/checkouts/4")).expect("forget");
    assert!(restarted.flush().is_empty());
    assert!(
        !RecentProjects::from_store(store)
            .entries()
            .contains(&PathBuf::from("/checkouts/4"))
    );
}

#[test]
fn failed_rename_removes_the_staging_file_and_keeps_the_destination() {
    let scratch = Scratch::new("layout-rename");
    let path = scratch.join("layout.json");
    fs::create_dir(&path).unwrap();
    let marker = path.join("marker");
    fs::write(&marker, b"kept").unwrap();
    let store = FileLayoutStore::new(&path, None);
    assert!(store.write(&serde_json::json!(1)).is_err());
    assert_eq!(fs::read(&marker).unwrap(), b"kept");
    assert!(
        !scratch.has_staging(),
        "a failed replace must not leave a staging file"
    );
}

/// A unique temporary directory, removed (even when read-only) on drop.
pub(super) struct Scratch {
    pub(super) path: PathBuf,
}

impl Scratch {
    pub(super) fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "phux-desktop-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
    pub(super) fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
    pub(super) fn has_staging(&self) -> bool {
        fs::read_dir(&self.path).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".next")
        })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = fs::metadata(&self.path) {
                let mut perms = metadata.permissions();
                perms.set_mode(perms.mode() | 0o200);
                let _ = fs::set_permissions(&self.path, perms);
            }
        }
        let _ = fs::remove_dir_all(&self.path);
    }
}
