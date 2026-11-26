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
    assert_eq!(exit_code(&checks), ExitCode::SUCCESS);
}

/// Any failure fails the run, so `phux doctor` can gate a setup script.
#[test]
fn one_failure_fails_the_run() {
    let checks = vec![
        Check::pass("a", "fine"),
        Check::fail("b", "broken", "fix it"),
    ];
    assert_eq!(exit_code(&checks), ExitCode::FAILURE);
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
    assert!(check.detail.contains("2 client logs"));
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
    assert!(check.detail.contains("0 client logs"));
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
/// its detail up with every other row; a hint sits under its row's detail
/// without repeating the check's name.
#[test]
fn human_report_aligns_details_past_the_longest_name() {
    let checks = [
        Check::pass("config", "fine"),
        Check::warn("workload-authority", "missing", "init it\n  then retry"),
    ];
    let rendered = render_human(&checks, false);
    let lines: Vec<&str> = rendered.lines().collect();
    // In characters: the marks are not all one byte wide.
    let column =
        |line: &str, needle: &str| line[..line.find(needle).expect(needle)].chars().count();
    assert_eq!(column(lines[0], "fine"), column(lines[1], "missing"));
    assert_eq!(column(lines[1], "missing"), column(lines[2], "hint: "));
    assert_eq!(lines[0], "✓ config              fine");
    assert_eq!(lines[1], "! workload-authority  missing");
    assert_eq!(lines[2].trim_start(), "hint: init it");
    assert_eq!(lines[3].trim_start(), "hint: then retry", "{rendered}");
    assert!(!rendered.contains("phux:"), "{rendered}");
}

/// The summary counts in words, failures first, and the full report ends
/// with it, after the checklist.
#[test]
fn the_summary_closes_the_report_in_plain_counts() {
    let pass = || Check::pass("a", "fine");
    let warn = || Check::warn("b", "unknown", "do something");
    let fail = || Check::fail("c", "broken", "fix it");
    assert_eq!(summary(&[pass()]), "all checks passed");
    assert_eq!(summary(&[pass(), warn()]), "1 warning, no failures");
    assert_eq!(summary(&[warn(), warn()]), "2 warnings, no failures");
    assert_eq!(summary(&[fail()]), "1 failure");
    assert_eq!(summary(&[fail(), fail(), warn()]), "2 failures, 1 warning");

    let report = render_report(&[pass(), warn()], &[], false);
    assert!(
        report.ends_with("\nWhat works here\n\n1 warning, no failures\n"),
        "{report}"
    );
}

/// A development build names its profile without nested parentheses.
#[test]
fn the_instance_reason_reads_without_nested_parentheses() {
    let check = check_instance();
    let open = check.detail.matches('(').count();
    assert!(open <= 1, "{}", check.detail);
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

#[test]
fn a_server_outside_its_systemd_unit_warns_with_the_adopt_fix() {
    use super::supervision::supervision_check;
    let scope = "0::/user.slice/user-1000.slice/session-3.scope\n";
    let check = supervision_check(Some(42), Some(scope), "phux.service", || {
        unreachable!("linger is moot for an unsupervised server")
    });
    assert_eq!(check.status, Status::Warn);
    assert!(check.detail.contains("pid 42"), "{}", check.detail);
    assert!(check.hint.unwrap().contains("phux service install --adopt"));

    // Another profile's unit is not this server's supervisor.
    let dev = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/phux-dev.service\n";
    let check = supervision_check(None, Some(dev), "phux.service", || true);
    assert_eq!(check.status, Status::Warn);
}

#[test]
fn a_supervised_server_passes_only_with_linger() {
    use super::supervision::supervision_check;
    let unit = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/phux.service\n";
    let check = supervision_check(Some(7), Some(unit), "phux.service", || true);
    assert_eq!(check.status, Status::Pass, "{}", check.detail);

    let check = supervision_check(Some(7), Some(unit), "phux.service", || false);
    assert_eq!(check.status, Status::Warn);
    assert!(check.hint.unwrap().contains("loginctl enable-linger"));

    // cgroup v1 names the unit on the systemd hierarchy's line.
    let v1 = "12:pids:/user.slice\n1:name=systemd:/user.slice/user-1000.slice/user@1000.service/phux.service\n";
    let check = supervision_check(Some(7), Some(v1), "phux.service", || true);
    assert_eq!(check.status, Status::Pass);

    let check = supervision_check(Some(7), None, "phux.service", || true);
    assert_eq!(
        check.status,
        Status::Warn,
        "unverifiable is a warning, never a pass"
    );
}
