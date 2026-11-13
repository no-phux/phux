//! systemd linger: without it the user manager, and every unit it runs, stops
//! at logout and starts only at the next login. With it the manager starts at
//! boot and outlives logout, which is what keeps the phux unit up.

/// Whether `loginctl show-user UID --property=Linger --value` said yes. A
/// failed probe (no logind, unknown user) reads as off.
pub(crate) fn linger_on(probe: std::io::Result<std::process::Output>) -> bool {
    probe.is_ok_and(|out| {
        out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "yes"
    })
}

/// Ask logind whether this user lingers.
pub(crate) fn probe_linger() -> bool {
    linger_on(
        std::process::Command::new("loginctl")
            .args([
                "show-user",
                &rustix::process::getuid().as_raw().to_string(),
                "--property=Linger",
                "--value",
            ])
            .output(),
    )
}

/// Turn linger on for this user when it is off. `--no-ask-password` keeps an
/// install from stopping at a polkit prompt; most distributions let a user
/// set their own linger without one. `Err` carries the command to run when
/// the policy wants an administrator.
pub(crate) fn ensure_linger() -> Result<(), String> {
    if probe_linger() {
        return Ok(());
    }
    super::run_tool(
        "loginctl",
        &["--no-ask-password".to_owned(), "enable-linger".to_owned()],
    )
    .map_err(|err| format!("{err}\n  run: sudo loginctl enable-linger {}", user_name()))
}

fn user_name() -> String {
    std::env::var("USER").unwrap_or_else(|_| rustix::process::getuid().as_raw().to_string())
}

/// The one line `install` prints about linger, after trying to enable it.
pub(crate) fn report_linger(result: Result<(), String>) {
    match result {
        Ok(()) => outln!("  linger  on (starts at boot, keeps running after logout)"),
        Err(err) => warn!(
            "service: linger is off, so the server stops at logout and starts only at\n\
             the next login, not at boot. {err}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::linger_on;
    use std::os::unix::process::ExitStatusExt;

    fn output(raw: i32, stdout: &str) -> std::process::Output {
        std::process::Output {
            status: std::process::ExitStatus::from_raw(raw),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn only_a_successful_yes_counts_as_lingering() {
        assert!(linger_on(Ok(output(0, "yes\n"))));
        assert!(!linger_on(Ok(output(0, "no\n"))));
        assert!(
            !linger_on(Ok(output(1 << 8, "yes\n"))),
            "a failed probe is off"
        );
        assert!(!linger_on(Err(std::io::Error::other("no loginctl"))));
    }
}
