//! Running commands on the far host over ssh, and reading its address.

use std::ffi::OsString;
use std::process::{Command, Stdio};

/// ssh's exit status when ssh itself failed (resolution, connection,
/// authentication), as opposed to the remote command.
pub(super) const SSH_FAILED: i32 = 255;

/// A POSIX shell's "command not found".
pub(super) const COMMAND_NOT_FOUND: i32 = 127;

/// What one ssh-run command produced.
pub(crate) struct SshOutput {
    pub(crate) status: std::process::ExitStatus,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl SshOutput {
    /// The far end's stderr, trimmed, or the exit status when it said
    /// nothing.
    pub(super) fn failure_detail(&self) -> String {
        let detail = self.stderr.trim();
        if detail.is_empty() {
            format!("exit {}", self.status)
        } else {
            detail.to_owned()
        }
    }

    /// The failure worded with the command that failed, for the error path.
    pub(super) fn command_failure(&self, ssh_host: &str, argv: &[&str]) -> String {
        format!(
            "`ssh {ssh_host} {}` failed: {}",
            argv.join(" "),
            self.failure_detail()
        )
    }
}

/// Run a command on the remote host over ssh (honoring `$PHUX_SSH`), with
/// `BatchMode=yes` so a missing key errors instead of prompting. `Err` means ssh
/// could not run at all.
pub(crate) fn ssh_run(ssh_host: &str, argv: &[&str]) -> Result<SshOutput, String> {
    let program = ssh_program();
    let output = ssh_command(&program, ssh_host, argv)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("could not run {}: {err}", program.to_string_lossy()))?;
    Ok(SshOutput::from(output))
}

/// [`ssh_run`] with `stdin` piped to the remote command: how public
/// enrollment material (a CSR) reaches the far host without entering argv.
pub(crate) fn ssh_run_with_stdin(
    ssh_host: &str,
    argv: &[&str],
    stdin: &[u8],
) -> Result<SshOutput, String> {
    use std::io::Write as _;

    let program = ssh_program();
    let mut child = ssh_command(&program, ssh_host, argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("could not run {}: {err}", program.to_string_lossy()))?;
    // The input is a few hundred bytes, well under a pipe buffer, so the
    // write cannot block on output the child has not drained. A remote that
    // exits without reading is reported by its status, not the broken pipe.
    if let Some(mut pipe) = child.stdin.take() {
        let _ = pipe.write_all(stdin);
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("could not run {}: {err}", program.to_string_lossy()))?;
    Ok(SshOutput::from(output))
}

/// `ssh -o BatchMode=yes HOST ARGV...`, so a missing key errors instead of
/// prompting.
pub(super) fn ssh_command(program: &OsString, ssh_host: &str, argv: &[&str]) -> Command {
    let mut command = Command::new(program);
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(ssh_host)
        .args(argv);
    command
}

impl From<std::process::Output> for SshOutput {
    fn from(output: std::process::Output) -> Self {
        Self {
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

/// The ssh program: `$PHUX_SSH` when set, otherwise `ssh`.
pub(crate) fn ssh_program() -> OsString {
    std::env::var_os("PHUX_SSH").unwrap_or_else(|| "ssh".into())
}

/// The host ssh would connect to for `destination`, from `ssh -G`, so an
/// alias with a `HostName` resolves to the machine it names. Falls back to
/// the host spelled in the destination when `ssh -G` has no answer.
pub(crate) fn ssh_hostname(destination: &str) -> String {
    Command::new(ssh_program())
        .arg("-G")
        .arg("--")
        .arg(destination)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| parse_ssh_g_hostname(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_else(|| destination_host(destination))
}

/// The `hostname` line of `ssh -G` output.
pub(super) fn parse_ssh_g_hostname(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.strip_prefix("hostname "))
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_owned)
}

/// The host part of an ssh destination: `[user@]host`, `[user@]host:port`,
/// or `ssh://[user@]host[:port]`, with IPv6 brackets removed.
pub(crate) fn destination_host(destination: &str) -> String {
    let rest = destination.strip_prefix("ssh://").unwrap_or(destination);
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if let Some(inner) = host.strip_prefix('[') {
        return inner
            .split_once(']')
            .map_or(inner, |(host, _)| host)
            .to_owned();
    }
    if host.matches(':').count() == 1 {
        return host
            .split_once(':')
            .map_or(host, |(host, _)| host)
            .to_owned();
    }
    host.to_owned()
}

/// `HOST:PORT`, bracketing an IPv6 literal.
pub(crate) fn authority(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}
