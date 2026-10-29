//! System `git` for plugin install and update.
//!
//! Every invocation puts `--` before a caller-supplied URL or commit, sets
//! a non-interactive environment, and dies with its process group if it
//! exceeds the fetch timeout. Userinfo is scrubbed from errors.

use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::source::redact_git_url;

/// Default bound for one `git` fetch or clone. Shallow (`--depth 1`).
const GIT_FETCH_TIMEOUT: Duration = Duration::from_secs(180);

/// Milliseconds override for [`git_timeout`], so a test can force the kill
/// path. A missing, unparseable, or zero value keeps [`GIT_FETCH_TIMEOUT`].
pub(super) const GIT_TIMEOUT_ENV: &str = "PHUX_PLUGIN_GIT_TIMEOUT_MS";

/// Protocols a plugin fetch may use. `file` is how tests clone a local repo.
const ALLOWED_PROTOCOLS: &str = "https:ssh:git:file";

/// Clone `url` at `rev` (branch, tag, or commit) into `dest` and return the
/// full HEAD commit. `.git` is left in place so the caller can check history.
pub(super) fn clone_at(url: &str, rev: Option<&str>, dest: &Path) -> Result<String, String> {
    match rev {
        Some(rev) if is_commit_id(rev) => fetch_commit(url, rev, dest),
        Some(rev) => clone_branch(url, Some(rev), dest),
        None => clone_branch(url, None, dest),
    }
}

/// `true` when `rev` is a 7 to 40 character hexadecimal commit id.
pub(super) fn is_commit_id(rev: &str) -> bool {
    (7..=40).contains(&rev.len()) && rev.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// `true` when `rev` is a full 40-character commit id.
pub(super) fn is_full_commit(rev: &str) -> bool {
    rev.len() == 40 && rev.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Whether `old` is an ancestor of `new` in `repo` (equal counts).
/// A shallow clone is deepened, then the recorded commit is fetched on its
/// own, so a rewritten branch where that object still exists is visible.
pub(super) fn is_descendant(
    repo: &Path,
    old: &str,
    new: &str,
    branch: Option<&str>,
) -> Result<bool, String> {
    if old.eq_ignore_ascii_case(new) {
        return Ok(true);
    }
    let mut deepen = vec![
        OsStr::new("fetch"),
        OsStr::new("--depth"),
        OsStr::new("100"),
        OsStr::new("origin"),
    ];
    if let Some(branch) = branch.filter(|branch| !is_commit_id(branch)) {
        deepen.push(OsStr::new(branch));
    }
    let _ = run_git(Some(repo), &deepen, "git fetch", None);
    if is_ancestor(repo, old, new)? {
        return Ok(true);
    }
    let _ = run_git(
        Some(repo),
        &[
            OsStr::new("fetch"),
            OsStr::new("--depth"),
            OsStr::new("1"),
            OsStr::new("origin"),
            OsStr::new("--"),
            OsStr::new(old),
        ],
        "git fetch",
        None,
    );
    is_ancestor(repo, old, new)
}

fn clone_branch(url: &str, branch: Option<&str>, dest: &Path) -> Result<String, String> {
    let mut args: Vec<&OsStr> = vec![OsStr::new("clone"), OsStr::new("--depth"), OsStr::new("1")];
    if let Some(branch) = branch {
        args.push(OsStr::new("--branch"));
        args.push(OsStr::new(branch));
    }
    args.push(OsStr::new("--"));
    args.push(OsStr::new(url));
    args.push(dest.as_os_str());
    run_git(None, &args, "git clone", Some(url))?;
    rev_parse(dest)
}

fn fetch_commit(url: &str, rev: &str, dest: &Path) -> Result<String, String> {
    std::fs::create_dir_all(dest)
        .map_err(|err| format!("could not create {}: {err}", dest.display()))?;
    run_git(
        Some(dest),
        &[OsStr::new("init"), OsStr::new("--quiet")],
        "git init",
        Some(url),
    )?;
    run_git(
        Some(dest),
        &[
            OsStr::new("remote"),
            OsStr::new("add"),
            OsStr::new("origin"),
            OsStr::new("--"),
            OsStr::new(url),
        ],
        "git remote add",
        Some(url),
    )?;
    if let Err(err) = run_git(
        Some(dest),
        &[
            OsStr::new("fetch"),
            OsStr::new("--depth"),
            OsStr::new("1"),
            OsStr::new("origin"),
            OsStr::new("--"),
            OsStr::new(rev),
        ],
        "git fetch",
        Some(url),
    ) {
        if !is_full_commit(rev) {
            return Err(format!(
                "could not fetch short commit {rev}; pass the full 40-character SHA ({err})"
            ));
        }
        return Err(err);
    }
    // FETCH_HEAD is ours, not a caller argument, so it stays on the option
    // side of checkout. `--` would make git treat it as a path.
    run_git(
        Some(dest),
        &[
            OsStr::new("checkout"),
            OsStr::new("--detach"),
            OsStr::new("--quiet"),
            OsStr::new("FETCH_HEAD"),
        ],
        "git checkout",
        Some(url),
    )?;
    let head = rev_parse(dest)?;
    if is_full_commit(rev) && !head.eq_ignore_ascii_case(rev) {
        return Err(format!("fetched {head} but --rev pinned {rev}"));
    }
    if !is_full_commit(rev)
        && !head
            .to_ascii_lowercase()
            .starts_with(&rev.to_ascii_lowercase())
    {
        return Err(format!("fetched {head} which does not match --rev {rev}"));
    }
    Ok(head)
}

fn rev_parse(repo: &Path) -> Result<String, String> {
    let stdout = run_git(
        Some(repo),
        &[OsStr::new("rev-parse"), OsStr::new("HEAD")],
        "git rev-parse",
        None,
    )?;
    Ok(stdout.trim().to_owned())
}

fn is_ancestor(repo: &Path, old: &str, new: &str) -> Result<bool, String> {
    let status = git_status(
        Some(repo),
        &[
            OsStr::new("merge-base"),
            OsStr::new("--is-ancestor"),
            OsStr::new(old),
            OsStr::new(new),
        ],
        "git merge-base",
    )?;
    // Exit 1 means "not an ancestor". A missing object is also not an
    // ancestor yet: the caller fetches that commit and asks again.
    Ok(status == 0)
}

fn git_status(cwd: Option<&Path>, args: &[&OsStr], what: &str) -> Result<i32, String> {
    let mut cmd = git_command();
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    cmd.args(args);
    run_bounded(cmd, what, None, git_timeout()).map(|done| done.status)
}

fn run_git(
    cwd: Option<&Path>,
    args: &[&OsStr],
    what: &str,
    secret_url: Option<&str>,
) -> Result<String, String> {
    let mut cmd = git_command();
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    cmd.args(args);
    let done = run_bounded(cmd, what, secret_url, git_timeout())?;
    if done.status == 0 {
        Ok(done.stdout)
    } else {
        Err(format!(
            "{what} failed (exit {}): {}",
            done.status, done.stderr
        ))
    }
}

fn git_command() -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_ALLOW_PROTOCOL", ALLOWED_PROTOCOLS);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_ASKPASS", "false");
    cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    cmd.stdin(Stdio::null());
    cmd
}

fn git_timeout() -> Duration {
    timeout_from(std::env::var(GIT_TIMEOUT_ENV).ok().as_deref())
}

fn timeout_from(raw: Option<&str>) -> Duration {
    raw.and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(GIT_FETCH_TIMEOUT, Duration::from_millis)
}

#[derive(Debug)]
struct ToolDone {
    status: i32,
    stdout: String,
    stderr: String,
}

/// Run `cmd` in its own process group and kill that group on timeout.
fn run_bounded(
    mut cmd: Command,
    what: &str,
    secret_url: Option<&str>,
    timeout: Duration,
) -> Result<ToolDone, String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|err| format!("could not spawn {what}: {err}"))?;
    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| format!("{what} stdout was not captured"))?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| format!("{what} stderr was not captured"))?;
    let stdout_task = thread::spawn(move || read_pipe(&mut stdout_pipe));
    let stderr_task = thread::spawn(move || read_pipe(&mut stderr_pipe));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                kill_group(&mut child);
                let stderr = join_pipe(stderr_task);
                return Err(scrub(
                    &format!("{what} {}", timeout_phrase(timeout)),
                    secret_url,
                    &stderr,
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(err) => return Err(format!("could not wait for {what}: {err}")),
        }
    };
    let stdout = join_pipe(stdout_task);
    let stderr = join_pipe(stderr_task);
    let code = status.code().unwrap_or(1);
    Ok(ToolDone {
        status: code,
        stdout: scrub(&stdout, secret_url, ""),
        stderr: scrub(stderr.trim(), secret_url, ""),
    })
}

fn read_pipe(pipe: &mut impl Read) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = pipe.read_to_end(&mut buf);
    buf
}

fn join_pipe(handle: thread::JoinHandle<Vec<u8>>) -> String {
    let bytes = handle.join().unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn kill_group(child: &mut Child) {
    #[cfg(unix)]
    {
        use rustix::process::{Pid, Signal};
        if let Some(pid) = i32::try_from(child.id()).ok().and_then(Pid::from_raw) {
            let _ = rustix::process::kill_process_group(pid, Signal::KILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn timeout_phrase(timeout: Duration) -> String {
    if timeout.as_secs() == 0 {
        format!("timed out after {}ms", timeout.as_millis())
    } else {
        format!("timed out after {}s", timeout.as_secs())
    }
}

fn scrub(text: &str, secret_url: Option<&str>, extra: &str) -> String {
    let mut combined = if extra.is_empty() {
        text.to_owned()
    } else {
        format!("{text} {}", extra.trim())
    };
    if let Some(secret) = secret_url {
        let redacted = redact_git_url(secret);
        if redacted != secret {
            combined = combined.replace(secret, &redacted);
        }
    }
    combined
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::{git_command, is_commit_id, is_full_commit, run_bounded, timeout_from};
    use std::time::Duration;

    #[test]
    fn git_invocation_is_non_interactive_and_protocol_limited() {
        let cmd = git_command();
        let env = |key: &str| {
            cmd.get_envs().find_map(|(name, value)| {
                (name == key).then(|| value.unwrap_or_default().to_string_lossy().into_owned())
            })
        };
        assert_eq!(
            env("GIT_ALLOW_PROTOCOL").as_deref(),
            Some("https:ssh:git:file")
        );
        assert_eq!(env("GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
        assert_eq!(env("GIT_ASKPASS").as_deref(), Some("false"));
        assert_eq!(
            env("GIT_SSH_COMMAND").as_deref(),
            Some("ssh -o BatchMode=yes")
        );
        assert!(is_full_commit("0123456789abcdef0123456789abcdef01234567"));
        assert!(is_commit_id("abcdef0"));
        assert!(!is_commit_id("abc"));
        assert!(!is_commit_id("main"));
        assert_eq!(timeout_from(None), Duration::from_secs(180));
        assert_eq!(timeout_from(Some("250")), Duration::from_millis(250));
        assert_eq!(timeout_from(Some("0")), Duration::from_secs(180));
    }

    #[test]
    fn timeout_kills_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("child");
        let script = format!("sleep 60 &\necho $! > {}\nwait\n", pidfile.display());
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        let err = run_bounded(cmd, "sleeper", None, Duration::from_millis(400)).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        let pid = std::fs::read_to_string(&pidfile).expect("child pid was recorded");
        let alive = Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("kill -0")
            .success();
        assert!(!alive, "process group child {pid} still alive");
    }
}
