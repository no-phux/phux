//! The protocol over `ssh HOST phux stdio-bridge`: a child-stdio lane for
//! hosts UDP cannot reach. ssh authenticates; the far server sees its own
//! user on its own socket, so no token or pin is involved.

use super::io::Io;

/// Refuse a destination or program ssh would read as an option.
fn validate(destination: &str, remote_phux: &str) -> std::io::Result<()> {
    let bad = |what: &str| what.is_empty() || what.starts_with('-') || what.contains('\0');
    if bad(destination) || bad(remote_phux) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "an ssh destination and remote phux must be nonempty and not start with `-`",
        ));
    }
    Ok(())
}

/// Single-quote `word` for the far shell, which ssh hands one joined string.
fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// Start the server if it is not running, then bridge to its socket.
fn remote_command(remote_phux: &str) -> String {
    let phux = shell_quote(remote_phux);
    format!("{phux} server --ensure >/dev/null 2>&1; exec {phux} stdio-bridge")
}

fn command(destination: &str, remote_phux: &str) -> std::io::Result<tokio::process::Command> {
    validate(destination, remote_phux)?;
    let program = std::env::var_os("PHUX_SSH").unwrap_or_else(|| "ssh".into());
    let mut command = tokio::process::Command::new(program);
    command
        .args(["-T", "-o", "ClearAllForwardings=yes"])
        // A dead path ends the link in about 45 s instead of hanging it.
        .args(["-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"])
        .arg("--")
        .arg(destination)
        .arg(remote_command(remote_phux));
    Ok(command)
}

pub(super) fn establish(destination: &str, remote_phux: &str) -> std::io::Result<Io> {
    Io::child(&mut command(destination, remote_phux)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn the_destination_is_an_argument_and_the_program_is_quoted_for_the_far_shell() {
        let command = command("me@box", "/opt/my phux/phux").expect("valid");
        let args: Vec<_> = command.as_std().get_args().map(OsString::from).collect();
        let tail: Vec<_> = args.iter().rev().take(3).rev().cloned().collect();
        assert_eq!(
            tail,
            [
                "--",
                "me@box",
                "'/opt/my phux/phux' server --ensure >/dev/null 2>&1; \
                 exec '/opt/my phux/phux' stdio-bridge"
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn option_shaped_or_empty_inputs_are_refused() {
        for (destination, phux) in [("-oProxyCommand=x", "phux"), ("", "phux"), ("box", "-x")] {
            let error = command(destination, phux).expect_err("refused");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
    }
}
