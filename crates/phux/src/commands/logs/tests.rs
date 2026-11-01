#![allow(
    clippy::unwrap_used,
    reason = "test code; a failed unwrap is the failure"
)]

use std::path::Path;
use std::time::{Duration, SystemTime};

use super::{
    client_log_target, client_logs_newest_first, client_pid, inventory, json_doc, render_human,
    tail_file,
};
/// Write a client log and pin its mtime, so "newest" is a controlled
/// fact rather than a race against the filesystem clock.
fn write_client_log(dir: &Path, name: &str, age: Duration) {
    let path = dir.join(name);
    std::fs::write(&path, b"log line\n").unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_modified(SystemTime::now() - age).unwrap();
}

/// A fresh machine with no state dir still gets the full inventory, marked
/// "not created yet", plus the doctor hint.
#[test]
fn bare_inventory_reports_missing_files_as_not_created_yet() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_path_buf();
    let server_log = state.join("server.log");

    let inv = inventory(state.clone(), server_log.clone());
    let human = render_human(&inv);
    assert!(human.contains(&server_log.display().to_string()));
    assert!(human.contains("not created yet"));
    assert!(human.contains("none yet"));
    assert!(human.contains(&format!("State dir: {}", state.display())));
    assert!(human.contains("phux doctor"), "must point at doctor");

    // A state dir that does not even exist yet is the same story, not
    // an error.
    let absent = state.join("never-created");
    let inv = inventory(absent.clone(), absent.join("server.log"));
    let human = render_human(&inv);
    assert!(human.contains("not created yet"));
    assert!(human.contains("none yet"));
}

/// An existing server log reports its facts instead of the missing-file
/// line, and the client list shows newest first with the overflow
/// summarized.
#[test]
fn inventory_reports_existing_files_with_size_and_age() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("server.log"), b"started\n").unwrap();
    for (name, age) in [
        ("client-1.log", 400),
        ("client-2.log", 300),
        ("client-3.log", 200),
        ("client-4.log", 100),
    ] {
        write_client_log(dir.path(), name, Duration::from_secs(age));
    }

    let inv = inventory(dir.path().to_path_buf(), dir.path().join("server.log"));
    assert_eq!(inv.clients.len(), 4);
    assert!(
        inv.clients[0].path.ends_with("client-4.log"),
        "newest first"
    );

    let human = render_human(&inv);
    assert!(!human.contains("not created yet"));
    assert!(human.contains("8 B"));
    assert!(
        human.contains("... and 1 more"),
        "overflow past the shown few is a count, not a wall: {human}"
    );
}

/// `--client` picks the newest log by mtime; `--pid` overrides with the
/// exact per-pid file.
#[test]
fn client_selection_prefers_newest_and_honors_pid() {
    let dir = tempfile::tempdir().unwrap();
    write_client_log(dir.path(), "client-100.log", Duration::from_secs(3600));
    write_client_log(dir.path(), "client-200.log", Duration::from_secs(60));
    // Not a client log; must never be selected or listed.
    std::fs::write(dir.path().join("server.log"), b"x").unwrap();

    let newest = client_logs_newest_first(dir.path());
    assert_eq!(newest.len(), 2);
    assert!(newest[0].ends_with("client-200.log"));

    let picked = client_log_target(dir.path(), None).unwrap();
    assert!(picked.ends_with("client-200.log"));

    let picked = client_log_target(dir.path(), Some(100)).unwrap();
    assert!(picked.ends_with("client-100.log"));

    // A pid nobody ran still names the exact path, so the missing-file
    // report can say where the log would have been.
    let picked = client_log_target(dir.path(), Some(31337)).unwrap();
    assert!(picked.ends_with("client-31337.log"));
}

/// With no client logs at all, `--client` has no path to even name, so
/// the selection itself reports — with the dir and the how-one-appears
/// explanation.
#[test]
fn client_selection_with_no_logs_explains_itself() {
    let dir = tempfile::tempdir().unwrap();
    let err = client_log_target(dir.path(), None).unwrap_err();
    assert!(err.contains("no client logs"));
    assert!(err.contains(&dir.path().display().to_string()));
    assert!(err.contains("phux attach"), "must say how a log appears");
}

/// The `--json` schema is a contract: `schema_version` 2, with the
/// pinned keys. A rename or removal must fail here first.
#[test]
fn json_inventory_schema_is_pinned_at_version_two() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("server.log"), b"started\n").unwrap();
    write_client_log(dir.path(), "client-42.log", Duration::from_secs(10));

    let doc = json_doc(&inventory(
        dir.path().to_path_buf(),
        dir.path().join("server.log"),
    ));
    assert_eq!(doc["schema_version"], 2);
    // Object layout is not part of the schema; pin the complete key set.
    let mut keys: Vec<_> = doc
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["client_logs", "schema_version", "server_log", "state_dir"]
    );
    assert_eq!(doc["state_dir"], dir.path().display().to_string().as_str());
    assert_eq!(doc["server_log"]["exists"], true);
    assert_eq!(doc["server_log"]["size_bytes"], 8);
    assert!(doc["server_log"]["modified_unix"].is_u64());
    assert_eq!(doc["client_logs"][0]["pid"], 42);
    assert_eq!(doc["client_logs"][0]["exists"], true);

    // A missing server log is exists=false with null facts, never an
    // absent key.
    let doc = json_doc(&inventory(
        dir.path().to_path_buf(),
        dir.path().join("no-such.log"),
    ));
    assert_eq!(doc["server_log"]["exists"], false);
    assert!(doc["server_log"]["size_bytes"].is_null());
    assert!(doc["server_log"]["modified_unix"].is_null());
}

/// The pid parse takes exactly the shape the client writer produces.
#[test]
fn client_pid_parses_only_the_writers_naming() {
    assert_eq!(client_pid(Path::new("/s/client-4242.log")), Some(4242));
    assert_eq!(client_pid(Path::new("/s/client-x.log")), None);
    assert_eq!(client_pid(Path::new("/s/server.log")), None);
}

/// A missing file fails with the caller's explanatory message instead
/// of running `tail` into an error nobody composed.
#[test]
fn tail_of_a_missing_file_fails() {
    let dir = tempfile::tempdir().unwrap();
    let code = tail_file(&dir.path().join("absent.log"), false, 10, "explain");
    assert_eq!(
        format!("{code:?}"),
        format!("{:?}", std::process::ExitCode::FAILURE)
    );
}

/// The clap surface: `--json` is the inventory for machines, so it
/// conflicts with the tail flags; `--server` and `--client` are one
/// choice; the tail modifiers require a tail target.
#[test]
fn logs_flag_grammar_rejects_contradictions() {
    assert!(crate::parse_cli(["phux", "logs"]).is_ok());
    assert!(crate::parse_cli(["phux", "logs", "--json"]).is_ok());
    assert!(crate::parse_cli(["phux", "logs", "--server", "-f", "-n", "50"]).is_ok());
    assert!(crate::parse_cli(["phux", "logs", "--client", "--pid", "42"]).is_ok());
    assert!(crate::parse_cli(["phux", "logs", "--client", "-f"]).is_ok());
    assert!(crate::parse_cli(["phux", "logs", "--cockpit"]).is_err());

    assert!(
        crate::parse_cli(["phux", "logs", "--json", "-f"]).is_err(),
        "--json is the inventory; it cannot follow"
    );
    assert!(
        crate::parse_cli(["phux", "logs", "--server", "--client"]).is_err(),
        "one tail target at a time"
    );
    // `-f` and `-n` without a tail target parse (the parser's `requires`
    // cannot express "one of two") and are refused by `run_logs`.
    assert!(crate::parse_cli(["phux", "logs", "-f"]).is_ok());
    assert_eq!(
        format!(
            "{:?}",
            super::run_logs(false, false, None, true, None, false)
        ),
        format!(
            "{:?}",
            std::process::ExitCode::from(crate::exit_codes::EXIT_USAGE)
        ),
        "-f without a tail target has nothing to follow"
    );
    assert_eq!(
        format!(
            "{:?}",
            super::run_logs(false, false, None, false, Some(5), false)
        ),
        format!(
            "{:?}",
            std::process::ExitCode::from(crate::exit_codes::EXIT_USAGE)
        ),
        "-n without a tail target has nothing to size"
    );
    assert!(
        crate::parse_cli(["phux", "logs", "--pid", "42"]).is_err(),
        "--pid only selects a client log"
    );
}
