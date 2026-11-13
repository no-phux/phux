//! The login that becomes phux's ssh master, one at a time per destination,
//! retried when sshd drops it before it starts.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::AsyncReadExt as _;

use super::{HOST_KEY_REFUSED, host_key_remedy, master_options, program};

/// Whether ssh's stderr says sshd closed the connection before the login
/// began: what sshd's `MaxStartups` does to connections over its limit,
/// so trying again shortly can work and nothing ran on the host.
///
/// A refused password or key, or a refused host key, is never one: asking
/// again would bring a dialog straight back, or wear down a host-key check.
#[must_use]
pub fn pre_auth_drop(stderr: &str) -> bool {
    let refused = [
        HOST_KEY_REFUSED,
        "Permission denied",
        "Too many authentication failures",
        "IDENTIFICATION HAS CHANGED",
    ]
    .iter()
    .any(|marker| stderr.contains(marker));
    !refused
        && stderr.lines().map(str::trim).any(|line| {
            line.contains("kex_exchange_identification")
                || line.contains("ssh_exchange_identification")
                || ((line.starts_with("Connection closed by ")
                    || line.starts_with("Connection reset by "))
                    && line.contains(" port "))
        })
}

/// Whether a failed ssh login is worth trying again.
///
/// That is [`pre_auth_drop`], or ssh saying nothing at all, which is how that drop looks under the
/// `LogLevel ERROR` generated configs often set. Only for runs whose remote
/// command never exits 255 itself, as phux's never do.
#[must_use]
pub fn retry_login(stderr: &str) -> bool {
    pre_auth_drop(stderr) || stderr.trim().is_empty()
}

/// How long to wait before retry number `retry` (from 0) of a login that
/// [`pre_auth_drop`] says never started, or `None` once out of retries.
/// The jitter spreads out many dials that were dropped together.
#[must_use]
pub fn pre_auth_retry_delay(retry: u32) -> Option<Duration> {
    use std::hash::BuildHasher as _;
    const RETRIES: u32 = 3;
    (retry < RETRIES).then(|| {
        let base = 250_u64 << retry;
        let jitter = std::collections::hash_map::RandomState::new().hash_one(retry) % base;
        Duration::from_millis(base + jitter)
    })
}

/// The lock dials to `destination` in this process share, so the first
/// logs in and the rest ride its master instead of racing to be it.
fn login_lock(destination: &str) -> Arc<tokio::sync::Mutex<()>> {
    // ponytail: one entry per destination for the process's life; fine for
    // a handful of hosts.
    static LOCKS: Mutex<Option<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = Mutex::new(None);
    let mut locks = LOCKS.lock().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(
        locks
            .get_or_insert_with(HashMap::new)
            .entry(destination.to_owned())
            .or_default(),
    )
}

/// Log in to `destination` and keep the login as phux's ssh master, unless
/// one is up or phux has no control directory. With `askpass`, ssh asks
/// through it, as the CLI does from a terminal; without, the login never
/// prompts (`BatchMode`), so it works for a key that needs no one.
///
/// `Err` is final, worded for the person, and only comes with `askpass`:
/// the sign-in was cancelled or refused, or ssh refused the host key.
/// Asking again on the next attempt would bring the dialog straight back.
/// Any other failure, such as not reaching the host, is left to the dial,
/// so a network that is down is waited out without a prompt.
pub(in super::super) async fn ensure(
    destination: &str,
    askpass: Option<&Path>,
) -> Result<(), String> {
    let Some(dir) = phux_config_core::instance::ssh_control_dir(true) else {
        return Ok(());
    };
    let lock = login_lock(destination);
    let _one_at_a_time = lock.lock().await;
    if alive(&dir, destination).await {
        return Ok(());
    }
    let mut retry = 0;
    loop {
        let Some(said) = login(&dir, destination, askpass).await else {
            return Ok(());
        };
        match pre_auth_retry_delay(retry).filter(|_| retry_login(&said)) {
            Some(delay) => tokio::time::sleep(delay).await,
            None if askpass.is_some() => {
                return login_refusal(&said, destination).map_or(Ok(()), Err);
            }
            None => return Ok(()),
        }
        retry += 1;
    }
}

async fn alive(dir: &Path, destination: &str) -> bool {
    tokio::process::Command::new(program())
        .args(["-o", &format!("ControlPath={}/%C", dir.display())])
        .args(["-O", "check", "--", destination])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// One master login. `None` when it succeeded or ssh could not run (the
/// dial reports that); otherwise what ssh said.
async fn login(dir: &Path, destination: &str, askpass: Option<&Path>) -> Option<String> {
    let mut command = tokio::process::Command::new(program());
    super::use_agent(&mut command);
    command.args(master_options(dir));
    match askpass {
        Some(askpass) => super::askpass_env(&mut command, askpass),
        None => {
            command.args(["-o", "BatchMode=yes"]);
        }
    }
    let mut child = command
        // `-f` backgrounds once authenticated; `-N` runs nothing remotely.
        .args(["-f", "-N", "--", destination])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let stderr = child.stderr.take();
    if child.wait().await.is_ok_and(|status| status.success()) {
        // The master forked off holding stderr; nothing to read.
        return None;
    }
    let mut said = String::new();
    if let Some(mut stderr) = stderr {
        let _ = stderr.read_to_string(&mut said).await;
    }
    Some(said)
}

/// The final reason in a failed login's stderr, if it has one.
fn login_refusal(stderr: &str, destination: &str) -> Option<String> {
    if let Some(remedy) = host_key_remedy(stderr, destination) {
        return Some(format!("ssh refused {destination}'s host key: {remedy}"));
    }
    stderr.contains("Permission denied").then(|| {
        super::failure_hint(stderr, destination).unwrap_or_else(|| {
            format!(
                "the ssh sign-in to {destination} was cancelled or refused; connect again to retry"
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_cancelled_or_refused_sign_in_ends_the_session() {
        let denied = "me@box: Permission denied (publickey,keyboard-interactive).";
        assert!(login_refusal(denied, "box").is_some_and(|why| why.contains("cancelled")));
        let host_key = "Host key verification failed.";
        assert!(login_refusal(host_key, "box").is_some_and(|why| why.contains("host key")));
        // An unreachable host walks the ladder, so a down network heals.
        let unreachable = "ssh: connect to host box port 22: No route to host";
        assert_eq!(login_refusal(unreachable, "box"), None);
        let dropped = "kex_exchange_identification: read: Connection reset by peer";
        assert_eq!(login_refusal(dropped, "box"), None);
    }

    /// What sshd prints when `MaxStartups` drops a connection is retried;
    /// a refused login or host key, an unreachable host, and a session
    /// ending are not.
    #[test]
    fn only_a_login_sshd_dropped_before_it_began_is_retried() {
        for dropped in [
            "kex_exchange_identification: read: Connection reset by peer",
            "kex_exchange_identification: Connection closed by remote host",
            "Connection closed by 10.0.0.5 port 22",
            "Connection reset by 10.0.0.5 port 22\r\n",
            "ssh_exchange_identification: Connection closed by remote host",
        ] {
            assert!(pre_auth_drop(dropped), "{dropped}");
        }
        for final_or_other in [
            "me@box: Permission denied (publickey,password).",
            "Received disconnect from 10.0.0.5 port 22:2: Too many authentication failures\n\
             Disconnected from 10.0.0.5 port 22",
            "Host key verification failed.",
            "@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @\n\
             Host key verification failed.\nConnection closed by 10.0.0.5 port 22",
            "Connection closed by 10.0.0.5 port 22\nme@box: Permission denied (publickey).",
            "ssh: connect to host box port 22: Connection refused",
            "ssh: connect to host box port 22: Operation timed out",
            "Connection to box closed.",
            "",
        ] {
            assert!(!pre_auth_drop(final_or_other), "{final_or_other}");
            assert_eq!(retry_login(final_or_other), final_or_other.is_empty());
        }
    }

    #[test]
    fn retries_back_off_with_jitter_and_stop() {
        for (retry, floor) in [(0, 250), (1, 500), (2, 1000)] {
            let delay = pre_auth_retry_delay(retry).expect("a retry").as_millis();
            assert!((floor..2 * floor).contains(&delay), "{retry}: {delay}");
        }
        assert_eq!(pre_auth_retry_delay(3), None);
    }
}
