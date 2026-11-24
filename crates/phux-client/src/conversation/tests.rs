#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::process::Command;

use super::*;

fn entry() -> phux_config::UplinkConfigEntry {
    phux_config::UplinkConfigEntry {
        name: "participant-a".to_owned(),
        hub: "hub.example:8788".to_owned(),
        server_name: None,
        ca_fingerprint: format!("sha256:{}", "ab".repeat(32)),
        client_cert: PathBuf::from("/credentials/participant-a/cert.pem"),
        client_key: PathBuf::from("/credentials/participant-a/key.pem"),
        enabled: true,
        mail_only: true,
    }
}

fn record() -> Conversation {
    let id = "0123456789abcdef0123456789abcdef".to_owned();
    Conversation {
        version: 1,
        thread: format!("chat-{id}"),
        id,
        welcome_id: "abcdef0123456789abcdef0123456789".to_owned(),
        local_address: "participant-a/agent".to_owned(),
        peer_address: "participant-b/agent".to_owned(),
        route: ConversationRoute::Hub {
            entry: entry(),
            credential_id: "participant-a".to_owned(),
        },
    }
}

fn root() -> (tempfile::TempDir, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let state = temporary.path().canonicalize().unwrap().join("state");
    (temporary, state)
}

fn stored(state: &Path) -> PathBuf {
    state.join("chats/buddy/conversation.json")
}

fn write_record(path: &Path, record: &Conversation) {
    fs::write(path, serde_json::to_vec(record).unwrap()).unwrap();
}

#[test]
fn record_round_trip_is_private_and_no_clobber() {
    let (_temporary, state) = root();
    let record = record();
    save_new_at(&state, "buddy", &record).unwrap();
    assert_eq!(load_at(&state, "buddy").unwrap(), record);
    assert_eq!(load_path(&stored(&state)).unwrap(), record);
    assert_eq!(
        load_at(&state, "buddy").unwrap().hub_link().unwrap().name(),
        "participant-a"
    );
    for path in [
        &state,
        &state.join("chats"),
        &state.join("chats/buddy"),
        &stored(&state),
    ] {
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o077, 0);
    }
    let mut replacement = record.clone();
    replacement.peer_address = "other/agent".to_owned();
    assert!(save_new_at(&state, "buddy", &replacement).is_err());
    assert_eq!(load_at(&state, "buddy").unwrap(), record);
    assert_eq!(fs::read_dir(state.join("chats/buddy")).unwrap().count(), 1);
}

#[test]
fn names_and_absolute_paths_reject_traversal() {
    for name in [
        "",
        ".",
        "..",
        "../escape",
        "/absolute",
        "two/names",
        "two\\names",
        "-flag",
        "a.b",
        "has space",
        "☃",
    ] {
        assert!(directory_at(Path::new("/state"), name).is_err(), "{name}");
    }
    assert!(validate_name(&"a".repeat(65)).is_err());
    assert!(validate_name("Buddy-2_a").is_ok());
    assert!(directory_at(Path::new("relative"), "buddy").is_err());
    assert!(load_path(Path::new("relative/conversation.json")).is_err());
    assert!(load_path(Path::new("/state/../conversation.json")).is_err());
}

#[test]
fn records_fail_closed_on_versions_threads_addresses_and_route() {
    let (_temporary, state) = root();
    let dir = directory_at(&state, "buddy").unwrap();
    let mut invalid = record();
    invalid.version = 2;
    assert!(
        validate_stored(&invalid, &dir)
            .unwrap_err()
            .contains("version")
    );
    invalid = record();
    invalid.id = "x".repeat(32);
    assert!(validate_stored(&invalid, &dir).is_err());
    invalid = record();
    invalid.welcome_id = "not-a-mail-id".to_owned();
    assert!(
        validate_stored(&invalid, &dir)
            .unwrap_err()
            .contains("welcome_id")
    );
    invalid = record();
    invalid.thread = "another-thread".to_owned();
    assert!(validate_stored(&invalid, &dir).is_err());
    for address in ["", "two/hosts/name", "a@not-a-pane", " agent"] {
        invalid = record();
        invalid.local_address = address.to_owned();
        assert!(validate_stored(&invalid, &dir).is_err(), "{address}");
        invalid = record();
        invalid.peer_address = address.to_owned();
        assert!(validate_stored(&invalid, &dir).is_err(), "{address}");
    }
    invalid = record();
    let ConversationRoute::Hub { entry, .. } = &mut invalid.route;
    entry.mail_only = false;
    assert!(invalid.hub_link().is_err());
    let ConversationRoute::Hub { entry, .. } = &mut invalid.route;
    entry.mail_only = true;
    entry.enabled = false;
    assert!(invalid.hub_link().is_err());
}

#[test]
fn credential_references_are_absolute_and_separate() {
    let (_temporary, state) = root();
    let dir = directory_at(&state, "buddy").unwrap();
    let mut invalid = record();
    let ConversationRoute::Hub { credential_id, .. } = &mut invalid.route;
    *credential_id = "../other".to_owned();
    assert!(validate_stored(&invalid, &dir).is_err());
    for path in [
        PathBuf::from("relative/key.pem"),
        dir.join("key.pem"),
        PathBuf::from("/c/../key.pem"),
    ] {
        let mut invalid = record();
        let ConversationRoute::Hub { entry, .. } = &mut invalid.route;
        entry.client_key = path;
        assert!(save_new_at(&state, "buddy", &invalid).is_err());
    }
    assert!(!state.exists(), "validation precedes filesystem mutation");
}

#[test]
fn malformed_unknown_and_oversized_records_do_not_load() {
    let (_temporary, state) = root();
    assert!(load_at(&state, "missing").is_err());
    save_new_at(&state, "buddy", &record()).unwrap();
    let path = stored(&state);
    for bytes in [
        b"{".to_vec(),
        vec![b' '; usize::try_from(MAX_RECORD_BYTES).unwrap() + 1],
        br#"{"version":1,"route":{"kind":"local"}}"#.to_vec(),
    ] {
        fs::write(&path, bytes).unwrap();
        assert!(load_path(&path).is_err());
    }
    let mut invalid = record();
    invalid.version = 255;
    write_record(&path, &invalid);
    assert!(load_path(&path).unwrap_err().contains("version"));
    let mut unknown = serde_json::to_value(record()).unwrap();
    unknown["surprise"] = serde_json::json!(true);
    fs::write(&path, serde_json::to_vec(&unknown).unwrap()).unwrap();
    assert!(load_path(&path).is_err());
}

#[test]
fn rejects_symlinks_and_unsafe_permissions_without_following() {
    let (_temporary, state) = root();
    save_new_at(&state, "buddy", &record()).unwrap();
    let path = stored(&state);
    let original = path.with_extension("original");
    fs::rename(&path, &original).unwrap();
    symlink(&original, &path).unwrap();
    assert!(load_path(&path).is_err());
    assert!(save_new_at(&state, "buddy", &record()).is_err());
    fs::remove_file(&path).unwrap();
    fs::rename(&original, &path).unwrap();
    for target in [
        &path,
        &state.join("chats/buddy"),
        &state.join("chats"),
        &state,
    ] {
        let permissions = fs::metadata(target).unwrap().permissions();
        fs::set_permissions(target, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(load_at(&state, "buddy").is_err(), "{}", target.display());
        fs::set_permissions(target, permissions).unwrap();
    }
    let chats = state.join("chats");
    let moved = state.join("real-chats");
    fs::rename(&chats, &moved).unwrap();
    symlink(&moved, &chats).unwrap();
    assert!(load_at(&state, "buddy").is_err());
    assert!(save_new_at(&state, "other", &record()).is_err());
    assert!(!moved.join("other").exists());
}

#[test]
fn interrupted_publication_is_loadable_but_non_files_are_rejected() {
    let (_temporary, state) = root();
    save_new_at(&state, "buddy", &record()).unwrap();
    let path = stored(&state);
    fs::hard_link(&path, path.with_extension("link")).unwrap();
    assert_eq!(
        load_path(&path).unwrap(),
        record(),
        "a crash before unlinking the publication's temporary link leaves a complete record"
    );
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(load_path(&path).is_err());
}

#[test]
fn concurrent_save_has_one_complete_winner() {
    let (_temporary, state) = root();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let workers: [_; 8] = std::array::from_fn(|_| {
        let state = state.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            save_new_at(&state, "buddy", &record())
        })
    });
    let successes = workers
        .into_iter()
        .flat_map(|worker| worker.join().unwrap())
        .count();
    assert_eq!(successes, 1);
    assert!(load_at(&state, "buddy").is_ok());
}

#[test]
fn reservation_refuses_incomplete_aliases_and_allows_identity_subdirectory() {
    let (_temporary, state) = root();
    let dir = reserve_at(&state, "buddy").unwrap();
    assert!(reserve_at(&state, "buddy").is_err());
    assert!(save_new_at(&state, "buddy", &record()).is_err());
    assert!(load_at(&state, "buddy").is_err());
    let mut record = record();
    let ConversationRoute::Hub { entry, .. } = &mut record.route;
    entry.client_cert = dir.join("identity/cert.pem");
    entry.client_key = dir.join("identity/key.pem");
    let path = save_reserved(&dir, &record).unwrap();
    assert_eq!(path, stored(&state));
    assert_eq!(load_path(&path).unwrap(), record);
    assert!(save_reserved(&dir, &record).is_err());
    assert_eq!(fs::read_dir(dir).unwrap().count(), 1);
}

/// Subprocess-only environment changes cannot race other test threads.
#[test]
fn exact_record_and_global_route_are_environment_independent() {
    const CHILD: &str = "PHUX_CONVERSATION_TEST_RECORD";
    if let Some(path) = std::env::var_os(CHILD) {
        assert_eq!(load_path(Path::new(&path)).unwrap(), record());
        match std::env::var("PHUX_CONVERSATION_TEST_CONFIG")
            .unwrap()
            .as_str()
        {
            "invalid" => assert!(
                crate::hub_mail::hub_link()
                    .unwrap_err()
                    .contains("load hub config")
            ),
            "missing" => assert!(crate::hub_mail::hub_link().unwrap().is_none()),
            _ => unreachable!("parent controls the test case"),
        }
        return;
    }
    let (_temporary, state) = root();
    save_new_at(&state, "buddy", &record()).unwrap();
    let config_home = state.join("config");
    fs::create_dir_all(config_home.join("phux")).unwrap();
    fs::write(config_home.join("phux/config.toml"), "not valid = [").unwrap();
    run_with_wrong_environment(CHILD, &state, &config_home, "invalid");
    fs::remove_file(config_home.join("phux/config.toml")).unwrap();
    run_with_wrong_environment(CHILD, &state, &config_home, "missing");
}

fn run_with_wrong_environment(child: &str, state: &Path, config_home: &Path, case: &str) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "conversation::tests::exact_record_and_global_route_are_environment_independent",
            "--nocapture",
        ])
        .env_clear()
        .env(child, stored(state))
        .env("PHUX_CONVERSATION_TEST_CONFIG", case)
        .env("HOME", state.join("wrong-home"))
        .env("XDG_STATE_HOME", state.join("wrong-state"))
        .env("PHUX_PROFILE", "wrong-profile")
        .env(CONTEXT_ENV, "/nonexistent/selected-record")
        .env("XDG_CONFIG_HOME", config_home)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{case}: stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
