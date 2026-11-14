use super::*;

/// `phux spawn nosuch` printed "spawn failed: spawn failed: Unable to
/// spawn nosuch because:" and then the server's whole `PATH`, several
/// kilobytes long. The refusal is one line and names no `PATH` value.
#[test]
fn a_missing_command_is_one_line_without_the_path() {
    let mut cmd = CommandBuilder::new("phux-no-such-command-for-this-test");
    cmd.env("PATH", "/nonexistent/a:/nonexistent/b");
    let pair = native_pty_system()
        .openpty(PtySize::default())
        .expect("openpty");
    let err = pair.slave.spawn_command(cmd).expect_err("no such command");
    let reason = spawn_failure_reason(&err);
    assert_eq!(
        reason,
        "Unable to spawn phux-no-such-command-for-this-test because: not found in PATH"
    );
    assert_eq!(
        TerminalActorError::Spawn(reason.clone()).to_string(),
        reason
    );
}
