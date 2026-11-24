//! The real binary's answer to a server about to re-exec into it: a released
//! blob passes, and one it could not resume is refused before anything
//! irreversible. A stale durable store must also leave a real server serving.
//! The server's side is in `runtime/upgrade/`.

#![allow(clippy::expect_used, reason = "tests")]

use std::process::{Command, Output, Stdio};

use crate::common::ServerGuard;

/// Run the candidate as the upgrading server does, with `blob` on stdin.
fn preflight(blob: Stdio) -> Output {
    let phux = std::env::var("NEXTEST_BIN_EXE_phux")
        .or_else(|_| std::env::var("CARGO_BIN_EXE_phux"))
        .expect("the test runner exports the phux binary");
    Command::new(phux)
        .arg("--version")
        .env("PHUX_PROBE_UPGRADE_BLOB", "1")
        .stdin(blob)
        .output()
        .expect("run phux")
}

fn stdin_of(bytes: &[u8]) -> Stdio {
    use std::io::{Seek as _, Write as _};

    let mut file = tempfile::tempfile().expect("temp file");
    file.write_all(bytes).expect("write");
    file.rewind().expect("rewind");
    file.into()
}

#[test]
fn a_released_blob_is_answered_as_readable() {
    let fixture = std::path::Path::new(
        &std::env::var("CARGO_MANIFEST_DIR").expect("the test runner exports the manifest dir"),
    )
    .join("../phux-server/tests/fixtures/upgrade-blob/0001.json");
    let output = preflight(std::fs::File::open(fixture).expect("fixture").into());
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout.trim_ascii(), b"upgrade-blob-readable");
}

#[test]
fn a_blob_from_a_newer_format_is_refused_with_the_reason() {
    let output = preflight(stdin_of(br#"{"version": 4294967295}"#));
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported upgrade blob version"),
        "{stderr}"
    );
}

/// Resolve the real socket-specific store from the fixture CLI's state dir,
/// not the test process's (possibly production) HOME/profile.
fn fixture_org_db(server: &ServerGuard) -> rusqlite::Connection {
    use std::fmt::Write as _;

    let status = server.json(&["status", "--json"]);
    assert_eq!(status["pid"], server.pid());
    let state = std::path::Path::new(
        status["logs"]["client_dir"]
            .as_str()
            .expect("status reports its state directory"),
    );
    let private_state = server.xdg.state.canonicalize().expect("private XDG state");
    assert!(
        state
            .canonicalize()
            .expect("fixture state directory")
            .starts_with(&private_state),
        "refuse to open a database outside the fixture"
    );
    // OrgStoreLocation::for_socket percent-escapes every non-name byte.
    let mut escaped = String::new();
    for &byte in server.socket.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
            escaped.push(char::from(byte));
        } else {
            write!(escaped, "%{byte:02X}").expect("escape fixture socket");
        }
    }
    let path = state
        .join("org/sockets")
        .join(format!("{escaped}.v1.sqlite3"));
    assert!(
        path.canonicalize()
            .expect("server already created its store")
            .starts_with(&private_state),
        "refuse a store symlink outside the fixture"
    );
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
        .expect("open only the existing private fixture database")
}

/// #2855: blob readability cannot prove that the durable organization can
/// resume. An external write must refuse the upgrade before cutting engines.
#[test]
#[ignore = "spawns a real phux server; run via `just e2e`."]
fn stale_stored_org_refuses_upgrade_without_stranding_the_live_pane() {
    let server = ServerGuard::builder("upgstore")
        .isolated_home()
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/sh")
        .seed_command(
            "printf 'PREFLIGHT_READY\\n'; \
             while IFS= read -r line; do \
             printf 'PREFLIGHT_REPLIED:%s:%s\\n' \"$$\" \"$line\"; done",
        )
        .start();
    let server_pid = server.pid();
    server.success(&[
        "wait",
        "work",
        "--until",
        "PREFLIGHT_READY",
        "--timeout",
        "10",
    ]);
    let listing = server.json(&["ls", "--json"]);
    let pane = listing["terminals"][0]
        .as_str()
        .expect("the original seed pane handle");
    let before = server.json(&["resource", "show", pane, "--json"]);
    let child_pid = u32::try_from(
        before["process"]["child"]["pid"]
            .as_u64()
            .expect("the seed shell's PID"),
    )
    .expect("child PID fits u32");
    assert!(crate::common::process_exists(child_pid));

    // This connection is opened only after startup. Removing the member makes
    // the stored arrangement invalid and fences the server's cached writer.
    let db = fixture_org_db(&server);
    db.busy_timeout(std::time::Duration::from_secs(5))
        .expect("bound fixture database lock waits");
    let session: String = db
        .query_row(
            "SELECT record FROM sessions WHERE name = 'work'",
            [],
            |row| row.get(0),
        )
        .expect("the private store contains the live fixture session");
    assert_eq!(
        db.execute("DELETE FROM resources", [])
            .expect("invalidate only the private fixture's stored membership"),
        1,
        "the fixture has exactly its original seed resource"
    );
    let (code, stdout, stderr) = server.run(&["upgrade"]);
    assert_eq!(
        code, 2,
        "upgrade must report a server-side refusal; stdout={stdout}; stderr={stderr}"
    );
    assert!(stderr.contains("upgrade refused"), "{stderr}");
    assert!(stderr.contains("org store"), "{stderr}");
    assert!(
        stderr.contains("changed through another connection"),
        "{stderr}"
    );
    assert!(stderr.contains("refusing stale writes"), "{stderr}");

    assert!(crate::common::process_exists(server_pid));
    assert_eq!(server.json(&["status", "--json"])["pid"], server_pid);
    assert_eq!(
        server.json(&["ls", "--json"])["terminals"],
        listing["terminals"],
        "the original pane handle remains authoritative"
    );
    let after = server.json(&["resource", "show", pane, "--json"]);
    assert_eq!(after["process"]["child"]["pid"], child_pid);
    assert!(crate::common::process_exists(child_pid));
    assert!(
        server
            .success(&["snapshot", pane])
            .contains("PREFLIGHT_READY")
    );

    // The shell's formatted response cannot be satisfied by PTY input echo.
    let token = format!("AFTER_REFUSAL_{}", std::process::id());
    let reply = format!("PREFLIGHT_REPLIED:{child_pid}:{token}");
    server.success(&["send-keys", pane, &token, "Enter"]);
    server.success(&["wait", pane, "--until", &reply, "--timeout", "10"]);
    assert!(server.success(&["snapshot", pane]).contains(&reply));
    assert_eq!(
        db.query_row("SELECT count(*) FROM resources", [], |row| row
            .get::<_, i64>(0))
            .expect("read externally invalidated membership"),
        0,
        "upgrade refusal and continued input must not silently repair the store"
    );
    assert_eq!(
        db.query_row(
            "SELECT record FROM sessions WHERE name = 'work'",
            [],
            |row| { row.get::<_, String>(0) }
        )
        .expect("read original stored session"),
        session,
        "upgrade refusal must leave durable session records untouched"
    );
}
