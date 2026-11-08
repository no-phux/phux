//! `--parent-pid PID` for the mirrors: exit once the process that launched
//! the mirror is gone, so a crashed parent leaves no producer behind
//! (`docs/resource-kinds.md#supervision`).

use std::time::Duration;

use rustix::io::Errno;
use rustix::process::{Pid, test_kill_process};

/// How often the parent is probed; the mirror exits within this after it dies.
const POLL: Duration = Duration::from_millis(250);

/// Exit this process once `pid` is gone; nothing when `None`.
pub(super) fn exit_with(pid: Option<u32>) {
    if let Some(pid) = pid {
        watch(pid, || std::process::exit(0));
    }
}

/// Run `gone` on a background thread once `kill(pid, 0)` reports no such
/// process. `EPERM` means it exists under another user, so it counts as alive;
/// a pid that cannot name a process counts as gone at once.
pub(super) fn watch(pid: u32, gone: impl FnOnce() + Send + 'static) {
    let pid = i32::try_from(pid).ok().and_then(Pid::from_raw);
    std::thread::spawn(move || {
        if let Some(pid) = pid {
            while test_kill_process(pid) != Err(Errno::SRCH) {
                std::thread::sleep(POLL);
            }
        }
        gone();
    });
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    #[test]
    fn fires_once_the_watched_process_exits() {
        let mut child = std::process::Command::new("sleep")
            .arg("0.3")
            .spawn()
            .expect("spawn sleep");
        let (tx, rx) = mpsc::channel();
        super::watch(child.id(), move || tx.send(()).expect("send"));
        assert!(
            rx.recv_timeout(super::POLL * 2).is_err(),
            "fired while alive"
        );
        child.wait().expect("reap");
        rx.recv_timeout(super::POLL * 4)
            .expect("fires after the exit");
    }
}
