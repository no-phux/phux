//! `restore-sessions`: relaunch what a server restart ended.

use std::path::Path;
use std::process::{Command, Stdio};

use super::ActionCall;

/// Run `phux restore` against the attached server and show its summary.
/// For a server that does not restore on its own (`[restore] on-start`).
pub(in crate::attach) fn restore_sessions(ActionCall { ctx, .. }: ActionCall<'_, '_>) {
    let said = match ctx.control_dial {
        Some(crate::attach::Dial::Uds(socket)) => {
            let program = std::env::current_exe().unwrap_or_else(|_| "phux".into());
            restore(&program, socket)
        }
        // The saved panes are the server's, so its own `phux` restores them.
        _ => "this session is on another machine; run `phux restore` there".to_owned(),
    };
    *ctx.rename_notice = Some(said);
}

/// The last line `phux restore` printed: its summary, or why it failed.
// ponytail: blocks the UI while panes start; move it to a task if a large
// restore makes that noticeable.
fn restore(program: &Path, socket: &Path) -> String {
    let out = Command::new(program)
        .arg("--socket")
        .arg(socket)
        .arg("restore")
        .stdin(Stdio::null())
        .output();
    let out = match out {
        Ok(out) => out,
        Err(err) => return format!("could not run phux restore: {err}"),
    };
    let text = if out.status.success() {
        out.stdout
    } else {
        out.stderr
    };
    let text = String::from_utf8_lossy(&text);
    let line = text.lines().rev().find(|line| !line.trim().is_empty());
    line.unwrap_or("phux restore said nothing")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_summary_or_the_failure_is_what_is_shown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("phux");
        let script = |body: &str| {
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        script(r#"echo "restored 2 panes on $2 ($3)""#);
        assert_eq!(
            restore(&path, Path::new("/tmp/s.sock")),
            "restored 2 panes on /tmp/s.sock (restore)"
        );
        script("echo 'phux: error: no server' >&2; exit 1");
        assert_eq!(
            restore(&path, Path::new("/tmp/s.sock")),
            "phux: error: no server"
        );
    }
}
