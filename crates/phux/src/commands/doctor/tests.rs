use super::*;

/// An over-long socket path fails with a hint.
#[test]
fn an_over_long_socket_path_fails_with_a_hint() {
    let long = PathBuf::from(format!("/tmp/{}/phux.sock", "x".repeat(200)));
    let check = check_socket_path(&long);
    assert_eq!(check.status, Status::Fail);
    assert!(
        check.hint.is_some(),
        "a failure with no hint is not a diagnosis"
    );
}

/// A stopped server warns rather than fails.
#[test]
fn a_missing_server_warns_rather_than_fails() {
    let check = check_server(std::path::Path::new("/tmp/phux-doctor-absent-server.sock"));
    assert_eq!(check.status, Status::Warn);
}

/// Warnings alone exit 0; a warning is "could not verify", not "broken".
#[test]
fn warnings_alone_do_not_fail_the_run() {
    let checks = vec![
        Check::pass("a", "fine"),
        Check::warn("b", "unknown", "do something"),
    ];
    assert!(checks.iter().all(|c| c.status != Status::Fail));
    assert_eq!(report_human(&checks), ExitCode::SUCCESS);
}

/// Any failure fails the run, so `phux doctor` can gate a setup script.
#[test]
fn one_failure_fails_the_run() {
    let checks = vec![
        Check::pass("a", "fine"),
        Check::fail("b", "broken", "fix it"),
    ];
    assert_eq!(report_human(&checks), ExitCode::FAILURE);
}

/// A stale shim warns with both schema numbers and the reinstall command.
#[test]
fn a_stale_claude_shim_warns_and_names_the_reinstall_command() {
    let path = std::path::Path::new("/data/phux/shims/claude");
    let check = shim_check(Some(1), 3, path);
    assert_eq!(check.status, Status::Warn);
    assert!(check.detail.contains("schema 1"), "{}", check.detail);
    assert!(check.detail.contains("schema 3"), "{}", check.detail);
    assert!(check.detail.contains("/data/phux/shims/claude"));
    let hint = check
        .hint
        .expect("a warn without a hint is half a diagnosis");
    assert!(
        hint.contains("phux agent install-claude"),
        "the remedy must be the literal command: {hint}"
    );
    assert!(hint.contains("detector"), "{hint}");
    assert!(
        shim_check(Some(2), 3, path)
            .hint
            .expect("hint")
            .contains("agent wait"),
        "schema 2's consequence is the per-turn clobber, not the stand-down",
    );
}

/// A newer shim warns with the opposite remedy; absent or current passes.
#[test]
fn a_shim_newer_than_the_binary_warns_with_the_other_remedy() {
    let check = shim_check(Some(4), 3, std::path::Path::new("/data/phux/shims/claude"));
    assert_eq!(check.status, Status::Warn);
    let hint = check.hint.expect("hint");
    assert!(hint.contains("phux update"), "{hint}");

    for installed in [None, Some(3)] {
        let check = shim_check(
            installed,
            3,
            std::path::Path::new("/data/phux/shims/claude"),
        );
        assert_eq!(check.status, Status::Pass, "{installed:?}");
        assert!(check.hint.is_none());
    }
}

/// The logs line names the server log, counts client logs, names the dir.
#[test]
#[allow(clippy::unwrap_used, reason = "test code")]
fn logs_check_names_paths_and_counts_clients() {
    let dir = tempfile::tempdir().unwrap();
    let server_log = dir.path().join("server.log");
    std::fs::write(&server_log, b"started\n").unwrap();
    std::fs::write(dir.path().join("client-100.log"), b"a\n").unwrap();
    std::fs::write(dir.path().join("client-200.log"), b"b\n").unwrap();

    let check = check_logs_at(dir.path(), &server_log);
    assert_eq!(check.status, Status::Pass);
    assert!(check.detail.contains(&server_log.display().to_string()));
    assert!(check.detail.contains("2 client log(s)"));
    assert!(check.detail.contains(&dir.path().display().to_string()));
    assert!(!check.detail.contains("not created yet"));
}

/// A fresh machine's absent logs are normal.
#[test]
#[allow(clippy::unwrap_used, reason = "test code")]
fn logs_check_reports_absent_logs_as_normal() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("never-created");
    let server_log = state.join("server.log");

    let check = check_logs_at(&state, &server_log);
    assert_eq!(check.status, Status::Pass);
    assert!(check.detail.contains("not created yet"));
    assert!(check.detail.contains("0 client log(s)"));
    assert!(check.detail.contains(&server_log.display().to_string()));
}

/// An unwritable state dir warns with a hint.
#[cfg(unix)]
#[test]
#[allow(clippy::unwrap_used, reason = "test code")]
fn an_unwritable_state_dir_warns_with_a_hint() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

    let check = check_logs_at(dir.path(), &dir.path().join("server.log"));

    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(check.status, Status::Warn);
    assert!(check.detail.contains("not writable"));
    let hint = check
        .hint
        .expect("a warn without a hint is half a diagnosis");
    assert!(hint.contains(&dir.path().display().to_string()));
}

/// A check name longer than the old fixed 12-column field still lines
/// its detail up with every other row; hints identify their check.
#[test]
fn human_report_aligns_details_past_the_longest_name() {
    let checks = [
        Check::pass("config", "fine"),
        Check::warn("workload-authority", "missing", "init it"),
    ];
    let rendered = render_human(&checks, false);
    let lines: Vec<&str> = rendered.lines().collect();
    let detail_column = |line: &str, needle: &str| line.find(needle).expect(needle);
    assert_eq!(
        detail_column(lines[0], "fine"),
        detail_column(lines[1], "missing"),
        "{rendered}"
    );
    assert_eq!(lines[2], "  hint: workload-authority: init it");
    assert!(lines[0].starts_with("phux: ok: "));
    assert!(lines[1].starts_with("phux: warn: "));
}

/// A socket file with nothing listening is still called stale.
#[test]
fn a_dead_socket_is_still_called_stale() {
    for kind in [
        std::io::ErrorKind::ConnectionRefused,
        std::io::ErrorKind::NotFound,
    ] {
        let err = phux_client::attach::AttachError::Io(std::io::Error::from(kind));
        let check = server_probe_failure(std::path::Path::new("/tmp/dead.sock"), &err, None);
        assert_eq!(check.status, Status::Fail);
        let hint = check.hint.expect("a dead socket names its remedy");
        assert!(hint.contains("stale"), "{hint}");
        assert!(
            hint.contains("remove it and start a fresh server"),
            "{hint}"
        );
    }
}

/// A socket that failed for some other reason is not called stale and is
/// not told to remove the file.
#[test]
fn an_unanswered_socket_is_not_called_stale() {
    let err = phux_client::attach::AttachError::Io(std::io::Error::from(
        std::io::ErrorKind::PermissionDenied,
    ));
    let check = server_probe_failure(std::path::Path::new("/tmp/denied.sock"), &err, None);
    assert_eq!(check.status, Status::Fail);
    let hint = check.hint.expect("hint");
    assert!(!hint.contains("stale"), "{hint}");
    assert!(!hint.contains("remove"), "{hint}");
    assert_eq!(hint, crate::commands::diagnostics::UNANSWERED_SOCKET_HINT);
}

/// Doctor's old "remove it and start a fresh server" advice is for a dead
/// socket. A live incompatible server gets the stop-and-start recovery.
#[test]
fn a_live_incompatible_socket_is_not_told_to_remove_it() {
    let err = phux_client::attach::AttachError::IncompatibleProtocol(
        "incompatible protocol: client offered 1.4.0, server requires 0.9.x; \
         update the phux server so protocol major.minor match"
            .to_owned(),
    );
    let check = server_probe_failure(std::path::Path::new("/tmp/old.sock"), &err, Some(4242));
    assert_eq!(check.status, Status::Fail);
    let text = format!("{}\n{}", check.detail, check.hint.expect("hint"));
    assert!(text.contains("kill 4242"), "{text}");
    assert!(text.contains("pkill -f 'phux server'"), "{text}");
    assert!(text.contains("`phux server --ensure`"), "{text}");
    assert!(text.contains("closes the panes"), "{text}");
    for forbidden in ["stale", "phux upgrade", "survive", "remove"] {
        assert!(!text.contains(forbidden), "{forbidden} in {text}");
    }
}
