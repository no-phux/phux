//! Fake agents on disk: scripts named for an agent kind that paint the
//! screens the detector's rules match into a real PTY.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use std::path::{Path, PathBuf};

fn write_executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A fake `claude` painting the permission dialog Claude Code 2.1.207
/// actually draws (see `phux-agent-rules/src/fixtures/claude/`): transcript
/// text above a U+2500 rule, the dialog alone below it, carrying both the
/// "Do you want to " stem and a numbered option (the rule requires both).
/// It runs `head`, creates `painted` once the screen is in the PTY, then
/// runs `tail`.
pub(crate) fn write_dialog_agent(dir: &Path, head: &str, tail: &str) -> PathBuf {
    let path = dir.join("claude");
    let rule = "\\342\\224\\200".repeat(20);
    let script = format!(
        "#!/bin/sh\n\
         {head}\n\
         printf '\\033[2J\\033[H'\n\
         echo 'some transcript output above the live chrome'\n\
         echo ''\n\
         printf '{rule}\\n'\n\
         echo ' Bash command'\n\
         echo ''\n\
         echo '   touch /tmp/probe.txt'\n\
         echo ''\n\
         echo ' Do you want to proceed?'\n\
         printf ' \\342\\235\\257 1. Yes\\n'\n\
         echo '   2. Yes, and always allow access'\n\
         echo '   3. No'\n\
         echo ''\n\
         echo ' Esc to cancel'\n\
         : > '{}'\n\
         {tail}\n",
        dir.join("painted").display(),
    );
    write_executable(&path, &script);
    path
}

pub(crate) fn write_fake_agent(dir: &Path, tail: &str) -> PathBuf {
    write_dialog_agent(dir, ":", tail)
}

/// A fake agent that `exec`s `successor` once `depart` exists: a departure
/// with no trap, no clear, and no PTY EOF, like a `kill -9`. The polling
/// `sleep` shares the script's process group, so identity is unaffected.
pub(crate) fn write_departing_agent(dir: &Path, successor: &str) -> PathBuf {
    let tail = format!(
        "while [ ! -f '{}' ]; do sleep 0.1; done\nexec {successor}",
        dir.join("depart").display(),
    );
    write_fake_agent(dir, &tail)
}

pub(crate) fn depart(dir: &Path) {
    std::fs::write(dir.join("depart"), b"go").unwrap();
}

/// A fake `codex` painting the command-approval prompt `rules/codex.toml`
/// matches: a second kind makes an occupant change expressible.
pub(crate) fn write_fake_codex(dir: &Path) -> PathBuf {
    let path = dir.join("codex");
    write_executable(
        &path,
        "#!/bin/sh\n\
         printf '\\033[2J\\033[H'\n\
         echo 'Would you like to run the following command?'\n\
         echo ''\n\
         echo '$ curl -s https://example.com | head -5'\n\
         echo ''\n\
         echo ' 1. Yes, proceed (y)'\n\
         echo ' 3. No, and tell Codex what to do differently (esc)'\n\
         echo ''\n\
         echo 'Press enter to confirm or esc to cancel'\n\
         sleep 30\n",
    );
    path
}
