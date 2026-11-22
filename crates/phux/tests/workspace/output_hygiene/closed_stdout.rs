/// Run `phux <args...>` with the read end of its real stdout pipe already
/// closed; returns `(exit code, None if killed by a signal; stderr)`.
fn run_with_closed_stdout(args: &[&str]) -> (Option<i32>, String) {
    let mut child = phux()
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn phux binary");

    drop(child.stdout.take().expect("child stdout is piped"));

    let out = child.wait_with_output().expect("wait for phux");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Assert the whole contract for one verb: exit 0, no panic, and no line
/// blaming a server that is not even running.
fn assert_survives_closed_stdout(args: &[&str]) {
    let (code, stderr) = run_with_closed_stdout(args);
    assert_eq!(
        code,
        Some(0),
        "`phux {}` with a closed stdout must exit 0 (None = died from a signal); stderr={stderr}",
        args.join(" ")
    );
    assert!(
        !stderr.contains("panicked"),
        "`phux {}` panicked on a closed stdout; stderr={stderr}",
        args.join(" ")
    );
    assert!(
        !stderr.contains("server panic"),
        "`phux {}` blamed the server for its own death; stderr={stderr}",
        args.join(" ")
    );
    assert!(
        stderr.is_empty(),
        "`phux {}` must hang up in silence; stderr={stderr}",
        args.join(" ")
    );
}

/// A hung-up reader must end each shape of stdout writer silently: a
/// ~160 KB completion script (guaranteed to hit the closed pipe), an `out!`
/// fragment, a `--json` document, a verb that dials first, and the parser's
/// own `--version` / `--help` / `--skill` output.
#[test]
fn every_writer_survives_a_closed_stdout() {
    let sock = dead_socket();
    for args in [
        vec!["completion", "bash"],
        vec!["config", "show", "--default"],
        vec!["config", "plugins", "--json"],
        vec!["doctor", "--socket", &sock],
        vec!["--version"],
        vec!["--help"],
        vec!["--skill"],
    ] {
        assert_survives_closed_stdout(&args);
    }
}
