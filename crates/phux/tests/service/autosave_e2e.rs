//! `phux server --autosave` (ADR-0150): the archive tracks the workspace
//! while the server runs, so a `SIGKILL` (the stand-in for a crash, an
//! abort, or power loss) leaves the latest layout on disk, and both a fresh
//! autosaving server and `phux workspace restore` bring it back.

#![allow(clippy::expect_used, clippy::panic, reason = "tests")]

#[path = "../common/mod.rs"]
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

/// Generous against a loaded box; the server saves a few seconds after a
/// change (probe 1s, quiet 2s).
const SAVE_BUDGET: Duration = Duration::from_secs(30);

fn start_autosaving(prefix: &str, archive: &Path) -> common::ServerGuard {
    common::ServerGuard::builder(prefix)
        .session("seed")
        .start_with(|cmd| {
            cmd.arg("--autosave").arg(archive);
        })
}

/// Session names (with the first pane's cwd) in an archive document, sorted.
fn sessions(doc: &serde_json::Value) -> Vec<(String, Option<String>)> {
    let mut sessions: Vec<(String, Option<String>)> = doc["sessions"]
        .as_array()
        .expect("archive has a sessions array")
        .iter()
        .map(|session| {
            (
                session["name"].as_str().expect("session name").to_owned(),
                session["windows"][0]["panes"][0]["cwd"]
                    .as_str()
                    .map(str::to_owned),
            )
        })
        .collect();
    sessions.sort();
    sessions
}

fn names(doc: &serde_json::Value) -> Vec<String> {
    sessions(doc).into_iter().map(|(name, _)| name).collect()
}

/// Poll `archive` until it parses and `want` holds.
fn wait_for_archive(
    archive: &Path,
    want: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = Instant::now() + SAVE_BUDGET;
    let mut last = None;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(archive) {
            let doc: serde_json::Value =
                serde_json::from_str(&text).expect("an autosaved archive is never torn");
            if want(&doc) {
                return doc;
            }
            last = Some(doc);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("archive never reached the expected state within {SAVE_BUDGET:?}; last: {last:?}");
}

/// The live workspace of `server`, as `workspace save` renders it.
fn live(server: &common::ServerGuard) -> serde_json::Value {
    serde_json::from_str(&server.success(&["workspace", "save"])).expect("workspace save JSON")
}

/// Poll `server` until its live workspace satisfies `want`.
fn wait_for_live(
    server: &common::ServerGuard,
    want: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = Instant::now() + SAVE_BUDGET;
    loop {
        let doc = live(server);
        if want(&doc) {
            return doc;
        }
        assert!(
            Instant::now() < deadline,
            "workspace never restored within {SAVE_BUDGET:?}; live: {doc}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_sigkilled_server_leaves_the_latest_workspace_and_restore_brings_it_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let archive = dir.path().join("state").join("workspace.json");
    let work = dir.path().join("work");
    std::fs::create_dir(&work).expect("work dir");
    let work = work.canonicalize().expect("canonical work dir");
    let work_text = work.to_string_lossy().into_owned();

    let mut server = start_autosaving("as", &archive);
    server.success(&["new", "--json", "-s", "alpha", "--cwd", &work_text]);
    wait_for_archive(&archive, |doc| names(doc).contains(&"alpha".to_owned()));

    // The latest state, not the first save: a rename and another session.
    server.success(&["rename", "alpha", "beta"]);
    server.success(&["new", "--json", "-s", "gamma", "--cwd", &work_text]);
    let saved = wait_for_archive(&archive, |doc| names(doc) == ["beta", "gamma", "seed"]);
    assert!(
        sessions(&saved)
            .iter()
            .any(|(name, cwd)| name == "beta" && cwd.as_deref() == Some(work_text.as_str())),
        "the renamed session keeps its cwd: {saved}"
    );

    server.sigkill();
    let text = std::fs::read_to_string(&archive).expect("archive survives SIGKILL");
    let after_kill: serde_json::Value = serde_json::from_str(&text).expect("archive is whole");
    assert_eq!(names(&after_kill), ["beta", "gamma", "seed"]);
    assert!(
        !archive
            .with_file_name("workspace.json.autosave.tmp")
            .exists(),
        "no temp file is left behind"
    );

    // `workspace restore` into a plain server brings the sessions back.
    let plain = common::ServerGuard::builder("as-plain")
        .session("seed")
        .start();
    let archive_text = archive.to_string_lossy().into_owned();
    plain.success(&["workspace", "restore", &archive_text]);
    let restored = live(&plain);
    assert_eq!(names(&restored), ["beta", "gamma", "seed"]);
    assert!(
        sessions(&restored)
            .iter()
            .any(|(name, cwd)| name == "beta" && cwd.as_deref() == Some(work_text.as_str())),
        "restore brings back the cwd: {restored}"
    );

    // A supervised restart (a fresh autosaving server) restores it itself.
    let restarted = start_autosaving("as-restart", &archive);
    wait_for_live(&restarted, |doc| names(doc) == ["beta", "gamma", "seed"]);
}

#[test]
fn a_development_build_refuses_a_production_autosave_path() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let production = Path::new(&home).join(".local/state/phux/workspace.json");
    // Only a dev build is refused, and a sandbox whose HOME sits in the temp
    // dir has no production state; either way there is nothing to assert.
    if phux_config::instance::build_kind() != phux_config::instance::BuildKind::Dev
        || !phux_config::production::is_production_state(&production)
    {
        return;
    }
    let output = common::phux_cmd(crate::runner::phux_bin())
        // The idle limit is a backstop: a refusal never gets that far.
        .args(["server", "--exit-after-idle", "5", "--socket"])
        .arg(format!("/tmp/phux-as-refused-{}.sock", std::process::id()))
        .arg("--autosave")
        .arg(&production)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run phux server");
    assert!(
        !output.status.success(),
        "a dev build must not autosave into production"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--autosave"),
        "names the refused flag: {stderr}"
    );
}
