//! A git worktree and its session, made from the session palette.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_comes_from_the_clis_reply() {
        let reply = br#"{"schema_version":1,"branch":"feat","session":"repo-feat"}"#;
        assert_eq!(session_of(reply), Ok("repo-feat".to_owned()));
        assert!(session_of(b"not json").is_err());
    }
}
