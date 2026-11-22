//! `new-worktree`: a git worktree and the session bound to it.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::attach::input_dispatch::args::str_arg;
use crate::render::overlay::PromptOverlay;

use super::sessions::reattach_named;
use super::{ActionCall, spawn_place};

/// Create a worktree for `branch` beside the focused pane's repository and
/// switch to its session, or prompt for the branch.
pub(in crate::attach) fn new_worktree(
    ActionCall {
        resolved,
        ctx,
        focused,
        panes,
        effects,
    }: ActionCall<'_, '_>,
) {
    let Some(branch) = str_arg(resolved, "branch") else {
        ctx.overlays
            .push(Box::new(PromptOverlay::new_worktree(ctx.theme)));
        return;
    };
    let place = spawn_place(resolved, focused, panes);
    // The command must reach the server this window is attached to, and its
    // repository must be on this machine: only a local socket is both.
    let socket = match ctx.control_dial {
        Some(crate::attach::Dial::Uds(path)) => Ok(Some(path.as_path())),
        Some(_) => Err("this session is on another machine; run it there".to_owned()),
        None => Ok(None),
    };
    let created = match (socket, place.host, place.directory) {
        (Err(why), _, _) => Err(why),
        (_, Some(host), _) => Err(format!("{host} is another machine; run it there")),
        (_, None, None) => Err("the focused pane reports no directory".to_owned()),
        (Ok(socket), None, Some(directory)) => {
            let program = std::env::current_exe().unwrap_or_else(|_| "phux".into());
            create(&program, socket, &branch, &directory)
        }
    };
    match created {
        Ok(session) => reattach_named(ctx, effects, session),
        Err(why) => {
            *ctx.rename_notice = Some(format!("could not create worktree {branch}: {why}"));
        }
    }
}

/// Run `phux worktree new`, which owns the path, hook, and session rules,
/// and return the session it bound.
// ponytail: blocks the UI for as long as `git worktree add` takes; move it
// to a task if a large repository makes that noticeable.
fn create(
    program: &Path,
    socket: Option<&Path>,
    branch: &str,
    directory: &str,
) -> Result<String, String> {
    let mut command = Command::new(program);
    if let Some(socket) = socket {
        command.arg("--socket").arg(socket);
    }
    let out = command
        .args(["worktree", "new", branch, "--repo", directory, "--json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|err| err.to_string())?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let line = stderr.lines().rev().find(|line| !line.trim().is_empty());
        return Err(line.unwrap_or("phux worktree new failed").trim().to_owned());
    }
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .and_then(|doc| doc["session"].as_str().map(str::to_owned))
        .ok_or_else(|| "phux worktree new named no session".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &Path, body: &str) -> std::path::PathBuf {
        let path = dir.join("phux");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn the_session_comes_from_the_reply_and_a_failure_keeps_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        let ok = script(dir.path(), r#"echo "{\"session\":\"$3-in-$5\"}""#);
        assert_eq!(
            create(&ok, None, "feat", "/repo"),
            Ok("feat-in-/repo".to_owned())
        );
        // The attached server's socket goes first, so the session is bound there.
        let at = script(dir.path(), r#"echo "{\"session\":\"$1 $2 $5\"}""#);
        assert_eq!(
            create(&at, Some(Path::new("/tmp/s.sock")), "feat", "/repo"),
            Ok("--socket /tmp/s.sock feat".to_owned())
        );
        let bad = script(dir.path(), "echo 'fatal: not a git repository' >&2; exit 1");
        assert_eq!(
            create(&bad, None, "feat", "/repo"),
            Err("fatal: not a git repository".to_owned())
        );
    }
}
