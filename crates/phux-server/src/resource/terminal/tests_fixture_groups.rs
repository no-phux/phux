//! Cleanup for job-control fixtures that intentionally escape pane teardown.

use std::path::{Path, PathBuf};
use std::process::Command;

use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

/// A test job's process group, stopped on drop (not after SIGKILL). Drop
/// signals only a live leader still running this fixture's script, since the
/// pid may have been reused.
pub(super) struct FixtureGroup {
    pid_file: PathBuf,
    env: &'static str,
}

impl FixtureGroup {
    pub(super) fn new(dir: &Path, env: &'static str) -> Self {
        Self {
            pid_file: dir.join(env),
            env,
        }
    }

    pub(super) fn configure(&self, cmd: &mut portable_pty::CommandBuilder) {
        cmd.env(self.env, &self.pid_file);
    }

    pub(super) fn pid_file(&self) -> &Path {
        &self.pid_file
    }

    /// The script prelude: register, honor a stop marker, run `body`. Run it
    /// as `/bin/sh <tempdir>/<name>.sh` under `set -m` and never `exec` away,
    /// so `$$` is the group and the command line names the tempdir.
    pub(super) fn script(&self, body: &str) -> String {
        let env = self.env;
        format!(
            "printf %s \"$$\" > \"${env}.tmp\" && \
             mv \"${env}.tmp\" \"${env}\" || exit 1\n\
             [ -d \"${{{env}%/*}}\" ] && [ ! -e \"${env}.stop\" ] || exit 0\n\
             {body}"
        )
    }

    /// Whether `pid` is alive and still runs something from this fixture's
    /// tempdir, which no recycled pid can.
    fn owns_leader(&self, pid: i32) -> bool {
        let Some(dir) = self.pid_file.parent().and_then(Path::to_str) else {
            return false;
        };
        Command::new("ps")
            .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
            .output()
            .is_ok_and(|out| {
                out.status.success() && String::from_utf8_lossy(&out.stdout).contains(dir)
            })
    }
}

impl Drop for FixtureGroup {
    fn drop(&mut self) {
        let _ = std::fs::write(self.pid_file.with_extension("stop"), b"stop");
        let Ok(text) = std::fs::read_to_string(&self.pid_file) else {
            return;
        };
        let Ok(raw) = text.trim().parse::<i32>() else {
            return;
        };
        if raw > 1 && self.owns_leader(raw) {
            kill_and_reap_group(raw);
        }
    }
}

/// Own the pane's session leader as well, including when its actor task is
/// dropped during unwinding before it can run async shutdown.
pub(super) struct FixturePane(pub(super) i32);

impl FixturePane {
    pub(super) fn for_actor(actor: &super::super::TerminalActor) -> Self {
        let pty = actor.pty.as_ref().expect("test actor has PTY");
        let pid = pty.child.process_id().expect("pane child pid");
        Self(i32::try_from(pid).expect("pid fits i32"))
    }
}

impl Drop for FixturePane {
    fn drop(&mut self) {
        // Signal only a still-running leader (a reaped one's id may recycle).
        let pid = Pid::from_raw(self.0);
        if waitpid(pid, Some(WaitPidFlag::WNOHANG)) == Ok(WaitStatus::StillAlive) {
            kill_and_reap_group(self.0);
        }
    }
}

fn kill_and_reap_group(raw: i32) {
    // Never allow malformed fixture state to signal the harness's own group.
    if raw <= 1 {
        return;
    }
    let pid = Pid::from_raw(raw);
    // ESRCH means the pid leads no group (it was not run as its own job), so
    // at least take the process itself.
    if killpg(pid, Signal::SIGKILL) == Err(Errno::ESRCH) {
        let _ = kill(pid, Signal::SIGKILL);
    }
    reap_bounded(pid);
}

/// Reap a direct child without blocking teardown (a killed macOS session
/// leader cannot exit while its output is undrained).
fn reap_bounded(pid: Pid) {
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(2);
    let deadline = std::time::Instant::now() + BUDGET;
    while waitpid(pid, Some(WaitPidFlag::WNOHANG)) == Ok(WaitStatus::StillAlive) {
        if std::time::Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::Child;

    use super::*;

    /// A shell naming `dir` that blocks in `read`.
    fn fixture_shell(dir: &Path) -> Command {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "read _"])
            .arg(dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        cmd
    }

    fn assert_reaped_by_guard(mut child: Child) {
        assert_eq!(
            child
                .wait()
                .expect_err("guard must reap the child")
                .raw_os_error(),
            Some(nix::libc::ECHILD)
        );
    }

    fn check_group_cleanup(unwind: bool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let group = FixtureGroup::new(dir.path(), "PHUX_TEST_GROUP");
        let leader = fixture_shell(dir.path())
            .process_group(0)
            .spawn()
            .expect("leader");
        let pid = leader.id();
        std::fs::write(group.pid_file(), pid.to_string()).expect("register leader");
        let mut member = Command::new("/bin/sleep")
            .arg("60")
            .process_group(i32::try_from(pid).expect("pid fits i32"))
            .spawn()
            .expect("group member");
        let result = std::panic::catch_unwind(move || {
            let _group = group;
            assert!(!unwind, "exercise fixture unwind");
        });
        assert_eq!(result.is_err(), unwind);
        assert_eq!(member.wait().expect("reap member").signal(), Some(9));
        assert_reaped_by_guard(leader);
    }

    #[test]
    fn normal_cleanup_kills_the_whole_group_and_reaps_the_child() {
        check_group_cleanup(false);
    }

    #[test]
    fn panic_cleanup_kills_the_whole_group_and_reaps_the_child() {
        check_group_cleanup(true);
    }

    /// A registered pid that now belongs to someone else, the recycled-pid
    /// shape, must survive the guard.
    #[test]
    fn cleanup_leaves_a_group_the_fixture_does_not_own_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let group = FixtureGroup::new(dir.path(), "PHUX_TEST_GROUP");
        let mut stranger = Command::new("/bin/sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .expect("unrelated group leader");
        std::fs::write(group.pid_file(), stranger.id().to_string()).expect("register");
        drop(group);
        let survived = stranger.try_wait().expect("poll stranger").is_none();
        let _ = stranger.kill();
        let _ = stranger.wait();
        assert!(
            survived,
            "guard signalled a group it could not prove it owns"
        );
    }

    /// A job not run under `set -m` leads no group: `killpg` fails with ESRCH
    /// and the guard falls back to the process itself.
    #[test]
    fn cleanup_falls_back_to_the_process_when_it_leads_no_group() {
        let dir = tempfile::tempdir().expect("tempdir");
        let group = FixtureGroup::new(dir.path(), "PHUX_TEST_GROUP");
        let job = fixture_shell(dir.path()).spawn().expect("non-leader job");
        std::fs::write(group.pid_file(), job.id().to_string()).expect("register");
        drop(group);
        assert_reaped_by_guard(job);
    }

    #[test]
    fn pane_cleanup_kills_and_reaps_a_leader_the_actor_never_reached() {
        let leader = Command::new("/bin/sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .expect("leader");
        let pid = i32::try_from(leader.id()).expect("pid fits i32");
        drop(FixturePane(pid));
        assert_reaped_by_guard(leader);
    }

    #[test]
    fn a_late_start_observes_cleanup_before_doing_work() {
        let dir = tempfile::tempdir().expect("tempdir");
        let group = FixtureGroup::new(dir.path(), "PHUX_TEST_GROUP");
        let script = group.script("exit 42");
        let pid_file = group.pid_file().to_owned();
        drop(group);
        let status = Command::new("/bin/sh")
            .args(["-c", &script])
            .env("PHUX_TEST_GROUP", pid_file)
            .status()
            .expect("late shell");
        assert!(status.success(), "stopped fixture must not run its body");
    }
}
