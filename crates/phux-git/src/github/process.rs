//! One child run for `gh` and `git`. On Unix it is bounded (a deadline, an
//! output cap, the process group killed); elsewhere it waits for the child.

use std::path::Path;
use std::time::Duration;

/// What a finished child produced.
pub(crate) struct Finished {
    pub success: bool,
    pub stdout: Vec<u8>,
    /// `stdout` reached the cap and the child was stopped.
    pub truncated: bool,
    pub stderr: String,
}

/// Why a child produced nothing.
pub(crate) enum Failure {
    /// The program is not installed.
    NotFound,
    Spawn(String),
    #[cfg_attr(
        not(unix),
        allow(dead_code, reason = "only the bounded Unix run has a deadline")
    )]
    Timeout,
    Wait(String),
}

/// The bounds and inputs of one run.
pub(crate) struct Child<'a> {
    pub program: &'a str,
    pub dir: &'a Path,
    pub args: &'a [&'a str],
    pub env: &'a [(&'a str, &'a str)],
    pub stdin: Option<Vec<u8>>,
    #[cfg_attr(
        not(unix),
        allow(dead_code, reason = "only the bounded Unix run has a deadline")
    )]
    pub timeout: Duration,
    pub max_stdout: usize,
}

#[cfg(unix)]
pub(crate) fn exec(child: Child<'_>) -> Result<Finished, Failure> {
    exec_with_stop(child, crate::bounded::Stop::Kill)
}

/// A Git write gets time to remove its lock files before being killed.
#[cfg(unix)]
pub(crate) fn exec_write(child: Child<'_>) -> Result<Finished, Failure> {
    exec_with_stop(child, crate::bounded::Stop::Terminate)
}

#[cfg(unix)]
fn exec_with_stop(child: Child<'_>, stop: crate::bounded::Stop) -> Result<Finished, Failure> {
    use crate::bounded::{Run, RunError};
    Run {
        program: child.program,
        dir: child.dir,
        args: child.args,
        env: child.env,
        timeout: child.timeout,
        max_stdout: child.max_stdout,
        stdin: child.stdin,
        stop,
    }
    .exec()
    .map(|out| Finished {
        success: out.status.success(),
        stdout: out.stdout,
        truncated: out.truncated,
        stderr: out.stderr,
    })
    .map_err(|error| match error {
        RunError::Spawn(error) if error.kind() == std::io::ErrorKind::NotFound => Failure::NotFound,
        RunError::Spawn(error) => Failure::Spawn(error.to_string()),
        RunError::Timeout => Failure::Timeout,
        RunError::Wait(message) => Failure::Wait(message),
    })
}

#[cfg(not(unix))]
pub(crate) fn exec_write(child: Child<'_>) -> Result<Finished, Failure> {
    exec(child)
}

#[cfg(not(unix))]
pub(crate) fn exec(child: Child<'_>) -> Result<Finished, Failure> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut command = Command::new(child.program);
    // As on Unix: a repository the app runs in must not steer `git`.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(child.dir)
        .args(child.args)
        .envs(child.env.iter().copied())
        .stdin(if child.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut spawned = command.spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => Failure::NotFound,
        _ => Failure::Spawn(error.to_string()),
    })?;
    if let (Some(bytes), Some(mut pipe)) = (child.stdin, spawned.stdin.take()) {
        std::thread::spawn(move || pipe.write_all(&bytes));
    }
    let out = spawned
        .wait_with_output()
        .map_err(|error| Failure::Wait(error.to_string()))?;
    let truncated = out.stdout.len() > child.max_stdout;
    let mut stdout = out.stdout;
    stdout.truncate(child.max_stdout);
    Ok(Finished {
        success: out.status.success(),
        stdout,
        truncated,
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_timed_out_write_can_clean_up_its_lock() {
        let dir =
            std::env::temp_dir().join(format!("phux-github-write-stop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock = dir.join("test.lock");
        let _ = std::fs::remove_file(&lock);
        let result = exec_write(Child {
            program: "sh",
            dir: &dir,
            args: &[
                "-c",
                "touch test.lock; trap 'rm -f test.lock; exit 1' TERM; sleep 30 & wait",
            ],
            env: &[],
            stdin: None,
            timeout: Duration::from_millis(500),
            max_stdout: 1024,
        });
        assert!(matches!(result, Err(Failure::Timeout)));
        assert!(!lock.exists(), "SIGTERM allowed the write to clean up");
        std::fs::remove_dir(&dir).unwrap();
    }
}
