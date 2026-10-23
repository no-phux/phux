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

#[test]
fn resolve_shell_precedence() {
    let zsh = || Some("/bin/zsh".to_owned());
    assert_eq!(
        resolve_shell_from(Some("/opt/fancy/fish"), zsh()),
        "/opt/fancy/fish"
    );
    assert_eq!(resolve_shell_from(None, zsh()), "/bin/zsh");
    assert_eq!(resolve_shell_from(Some("  "), zsh()), "/bin/zsh");
    assert_eq!(resolve_shell_from(None, None), "/bin/sh");
}

fn argv_strings(cmd: &CommandBuilder) -> Vec<String> {
    cmd.get_argv()
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn login_flags_by_shell() {
    let cases: &[(&str, bool, &[&str])] = &[
        ("/bin/zsh", false, &["/bin/zsh"]),
        ("/bin/bash", true, &["/bin/bash", "-l"]),
        ("/bin/zsh", true, &["/bin/zsh", "-l"]),
        ("sh", true, &["sh", "-l"]),
        (
            "/opt/homebrew/bin/fish",
            true,
            &["/opt/homebrew/bin/fish", "--login"],
        ),
        ("/opt/exotic/rc", true, &["/opt/exotic/rc"]),
    ];
    for (shell, login, argv) in cases {
        assert_eq!(
            argv_strings(&default_shell_command(shell, *login)),
            *argv,
            "{shell}"
        );
    }
}

#[test]
fn shell_command_applies_login_flag_before_dash_c() {
    let cmd = shell_command("/bin/zsh", "htop", true);
    assert_eq!(argv_strings(&cmd), ["/bin/zsh", "-l", "-c", "htop"]);
    let cmd = shell_command("/opt/fancy/fish", "btop --utf-force", false);
    assert_eq!(
        argv_strings(&cmd),
        ["/opt/fancy/fish", "-c", "btop --utf-force"]
    );
}
