//! What Rune asks the CLI to do for it: a git worktree with its session,
//! and restoring what a server restart ended.

use std::time::Duration;

use super::{Desk, NoticeKind};
use crate::launch::run_for;

/// `git worktree add` on a large repository can take a while.
const TIMEOUT: Duration = Duration::from_secs(60);

/// The session `phux worktree new --json` bound.
fn session_of(reply: &[u8]) -> Result<String, String> {
    serde_json::from_slice::<serde_json::Value>(reply)
        .ok()
        .and_then(|doc| doc["session"].as_str().map(str::to_owned))
        .ok_or_else(|| "phux named no session.".to_owned())
}

impl Desk {
    /// Make a worktree for `branch` beside the focused terminal's
    /// repository and show its session. The CLI owns the path, hook, and
    /// session rules.
    pub(super) fn new_worktree(&mut self, branch: &str) {
        // Rune's CLI is only this computer's.
        let local = self.machines.current.is_none();
        let (Some(phux), Some(cwd), true) = (self.launch.phux.clone(), self.focused_cwd(), local)
        else {
            return self.say(
                NoticeKind::Info,
                "A worktree needs a terminal open in a repository on this computer.",
            );
        };
        let socket = self.launch.socket.as_ref().ok();
        let socket = socket.map(|socket| socket.display().to_string());
        let mut args = Vec::new();
        if let Some(socket) = &socket {
            args.extend(["--socket", socket]);
        }
        args.extend(["worktree", "new", branch, "--repo", &cwd, "--json"]);
        // ponytail: waits for git on the UI thread; move it to a worker, as
        // onboarding's `ask` does, if large repositories make that felt.
        match run_for(&phux, &args, TIMEOUT).and_then(|reply| session_of(&reply)) {
            // The session exists now, so this attaches and shows it.
            Ok(session) => self.create_session(session),
            Err(why) => self.say(NoticeKind::Error, why),
        }
    }
}

/// What `phux restore` said last: its summary line.
fn summary(reply: &[u8]) -> String {
    let text = String::from_utf8_lossy(reply);
    let line = text.lines().rev().find(|line| !line.trim().is_empty());
    line.unwrap_or("Nothing to restore.").trim().to_owned()
}

impl Desk {
    /// Relaunch the panes a server restart ended, for a server that does
    /// not restore by itself. The CLI owns what is restored and how.
    pub(super) fn restore_sessions(&mut self) {
        // Rune's CLI is only this computer's; another machine's own restores it.
        let local = self.machines.current.is_none();
        let (Some(phux), true) = (self.launch.phux.clone(), local) else {
            return self.say(
                NoticeKind::Info,
                "Run `phux restore` on that machine to restore its sessions.",
            );
        };
        let socket = self.launch.socket.as_ref().ok();
        let socket = socket.map(|socket| socket.display().to_string());
        let mut args = Vec::new();
        if let Some(socket) = &socket {
            args.extend(["--socket", socket]);
        }
        args.push("restore");
        // ponytail: waits on the UI thread while panes start, as above.
        match run_for(&phux, &args, TIMEOUT) {
            Ok(reply) => self.say(NoticeKind::Info, summary(&reply)),
            Err(why) => self.say(NoticeKind::Error, why),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_comes_from_the_clis_reply() {
        let reply = br#"{"schema_version":1,"branch":"feat","session":"repo-feat"}"#;
        assert_eq!(session_of(reply), Ok("repo-feat".to_owned()));
        assert!(session_of(b"not json").is_err());
        assert_eq!(summary(b"relaunched 2 panes\n\n"), "relaunched 2 panes");
        assert_eq!(summary(b""), "Nothing to restore.");
    }
}
