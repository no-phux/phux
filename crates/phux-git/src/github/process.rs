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
    use crate::bounded::{Run, RunError, Stop};
    Run {
        program: child.program,
        dir: child.dir,
        args: child.args,
        env: child.env,
        timeout: child.timeout,
        max_stdout: child.max_stdout,
        stdin: child.stdin,
        stop: Stop::Kill,
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
pub(crate) fn exec(child: Child<'_>) -> Result<Finished, Failure> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut command = Command::new(child.program);
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
