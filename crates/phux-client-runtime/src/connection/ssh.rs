//! The protocol over `ssh HOST phux stdio-bridge`: a child-stdio lane for
//! hosts UDP cannot reach. ssh authenticates; the far server sees its own
//! user on its own socket, so no token or pin is involved.

use std::path::Path;

use super::io::{Explain, Io};

mod diagnose;
mod master;

pub use diagnose::failure_hint;
pub use master::{pre_auth_drop, pre_auth_retry_delay, retry_login};

/// Refuse a destination or program ssh would read as an option.
fn validate(destination: &str, remote_phux: &str) -> std::io::Result<()> {
    let bad = |what: &str| what.is_empty() || what.starts_with('-') || what.contains('\0');
    if bad(destination) || bad(remote_phux) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "an ssh destination and remote phux must be nonempty and not start with `-`",
        ));
    }
    Ok(())
}

/// Single-quote `word` for the far shell, which ssh hands one joined string.
fn shell_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// Start the server if it is not running, then bridge to its socket.
fn remote_command(remote_phux: &str) -> String {
    let phux = shell_quote(remote_phux);
    format!("{phux} server --ensure >/dev/null 2>&1; exec {phux} stdio-bridge")
}

/// `-o` options that make an ssh run a client of phux's own master
/// connection for its destination.
///
/// None when phux has no control directory
/// ([`phux_config_core::instance::ssh_control_dir`]). With no master up, ssh
/// connects as it would have. `ControlMaster=no` keeps the run
/// from becoming a master that dies with it, whatever the user's config says.
/// `ClearAllForwardings=yes` matters as much: a client whose config forwards
/// a port the master already holds abandons the master and logs in again.
#[must_use]
pub fn control_options() -> Vec<String> {
    client_options(
        phux_config_core::instance::ssh_control_dir(false).as_deref(),
        false,
    )
}

/// [`control_options`] for a `BatchMode` run, which never prompts.
///
/// With no master up, the run logs in with its key and keeps that connection
/// as phux's master for five minutes, so the runs after it skip
/// the ssh handshake, those without a terminal (scripts, Rune) included.
#[must_use]
pub fn batch_control_options() -> Vec<String> {
    client_options(
        phux_config_core::instance::ssh_control_dir(true).as_deref(),
        true,
    )
}

fn client_options(dir: Option<&Path>, start_master: bool) -> Vec<String> {
    dir.map_or_else(Vec::new, |dir| {
        // `auto` also takes over a dead master's stale socket.
        let master = if start_master {
            vec![
                "ControlMaster=auto".to_owned(),
                format!("ControlPersist={CONTROL_PERSIST_SECS}"),
            ]
        } else {
            vec!["ControlMaster=no".to_owned()]
        };
        std::iter::once(format!("ControlPath={}/%C", dir.display()))
            .chain(master)
            .chain(["ClearAllForwardings=yes".to_owned()])
            .flat_map(|option| ["-o".to_owned(), option])
            .collect()
    })
}

/// How long phux's ssh master outlives its last client: enough for the rest
/// of an enrollment or attach and a quick reconnect, short enough that a
/// password or 2FA login does not stay usable all day.
const CONTROL_PERSIST_SECS: u32 = 300;

/// `-o` options for the login that becomes phux's ssh master in `dir`.
#[must_use]
pub fn master_options(dir: &Path) -> Vec<String> {
    [
        format!("ControlPath={}/%C", dir.display()),
        // `auto`, not `yes`: over a dead master's stale socket `yes` gives
        // up multiplexing and leaves the `-N` login running for good, while
        // `auto` removes the socket and takes over.
        "ControlMaster=auto".to_owned(),
        format!("ControlPersist={CONTROL_PERSIST_SECS}"),
        // The master carries the keepalive for every client riding it.
        "ServerAliveInterval=15".to_owned(),
        "ServerAliveCountMax=3".to_owned(),
        // A network that drops packets fails in seconds, not the system's
        // TCP timeout, so a reconnect keeps its count.
        "ConnectTimeout=10".to_owned(),
        "ClearAllForwardings=yes".to_owned(),
    ]
    .into_iter()
    .flat_map(|option| ["-o".to_owned(), option])
    .collect()
}

/// Set, to `1`, in the environment of the askpass program ssh runs for
/// [`Target::ssh_askpass`](super::Target::ssh_askpass): how a desktop
/// client's own executable knows to answer one prompt and exit.
pub const ASKPASS_ENV: &str = "PHUX_ASKPASS";

/// Before each attempt on an ssh target with an askpass program, sign in
/// through it ([`master::ensure`]). `Some` ends the session: a refusal, or
/// the consumer closing it meanwhile.
pub(super) async fn sign_in(
    target: &super::Target,
    close: &mut tokio::sync::watch::Receiver<bool>,
) -> Option<super::ConnectionEnd> {
    let (super::Transport::Ssh { destination, .. }, Some(askpass)) =
        (&target.transport, &target.ssh_askpass)
    else {
        return None;
    };
    tokio::select! {
        signed_in = master::ensure(destination, Some(askpass)) => {
            signed_in.err().map(super::ConnectionEnd::Refused)
        }
        _ = close.wait_for(|closed| *closed) => Some(super::ConnectionEnd::Closed),
    }
}

/// The ssh program: `$PHUX_SSH` when set, otherwise `ssh`.
fn program() -> std::ffi::OsString {
    std::env::var_os("PHUX_SSH").unwrap_or_else(|| "ssh".into())
}

/// What ssh prints when it refuses a host key: a changed or revoked one, or
/// under `BatchMode` one it has never seen.
pub const HOST_KEY_REFUSED: &str = "Host key verification failed";

/// The fix for ssh refusing `destination`'s host key, read from ssh's
/// stderr, or `None` when that is not why it failed.
///
/// phux never removes or accepts a key itself: a changed key is what a
/// reinstall looks like, and also what an interception looks like.
#[must_use]
pub fn host_key_remedy(stderr: &str, destination: &str) -> Option<String> {
    if !stderr.contains(HOST_KEY_REFUSED) {
        return None;
    }
    let check =
        format!("connect once with `ssh {destination}` to check and accept the key it shows");
    if stderr.contains("REVOKED HOST KEY") {
        return Some(format!(
            "{destination}'s host key has been revoked; ask whoever runs the host before connecting"
        ));
    }
    if !(stderr.contains("IDENTIFICATION HAS CHANGED") || stderr.contains("DNS SPOOFING")) {
        return Some(format!(
            "ssh does not know {destination}'s host key yet, and phux never accepts one on its \
             own: {check}"
        ));
    }
    Some(format!(
        "{destination} presented a different host key than the one you trusted. A reinstall \
         does that, and so does someone intercepting the connection, so confirm the new key \
         with whoever runs the host; then remove the old key with {} and {check}",
        remove_command(stderr)
    ))
}

/// The `ssh-keygen -R` that removes the key ssh refused, quoted in
/// backticks. Some builds print it under "remove with:"; every build names
/// the `known_hosts` file (`Offending ... key in FILE:LINE`) and the entry
/// (`[host]:port`, a `HostKeyAlias`) whose key changed.
fn remove_command(stderr: &str) -> String {
    let lines = || stderr.lines().map(str::trim);
    if let Some(line) = lines().find(|line| line.starts_with("ssh-keygen ")) {
        return format!("`{line}`");
    }
    let entry = lines().find_map(|line| {
        line.strip_prefix("Host key for ")?
            .split_once(" has changed")
            .map(|(entry, _)| entry)
    });
    let file = lines().find_map(|line| {
        let (_, rest) = line.strip_prefix("Offending ")?.split_once(" key in ")?;
        rest.rsplit_once(':').map(|(file, _)| file)
    });
    match (file, entry) {
        (Some(file), Some(entry)) => format!(
            "`ssh-keygen -f {} -R {}`",
            shell_quote(file),
            shell_quote(entry)
        ),
        (None, Some(entry)) => format!("`ssh-keygen -R {}`", shell_quote(entry)),
        _ => "`ssh-keygen -R` and the host's name".to_owned(),
    }
}

/// Every option a lane's ssh runs with, before `--`.
fn lane_options() -> Vec<String> {
    // A password or 2FA login done once in the foreground (`phux attach
    // --ssh`, `phux host add`) left a master this run rides; without one,
    // this run leaves one for the next.
    let mut options = batch_control_options();
    // The pipes are the protocol's, and the terminal may be the TUI's: a
    // login that must prompt fails here and is left to `ssh -t`. A dead
    // path ends the link in about 45 s instead of hanging it.
    options.extend(
        [
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
        ]
        .map(str::to_owned),
    );
    options
}

fn command(destination: &str, remote_phux: &str) -> std::io::Result<tokio::process::Command> {
    validate(destination, remote_phux)?;
    let mut command = tokio::process::Command::new(program());
    command
        .args(lane_options())
        .arg("--")
        .arg(destination)
        .arg(remote_command(remote_phux));
    Ok(command)
}

/// Start the lane. Dials to one destination in this process take turns
/// logging in, so the first becomes phux's master and the rest ride it,
/// rather than all racing sshd's `MaxStartups` to be it.
pub(super) async fn establish(destination: &str, remote_phux: &str) -> std::io::Result<Io> {
    let mut command = command(destination, remote_phux)?;
    // Without an askpass program this never fails; the lane reports.
    let _ = master::ensure(destination, None).await;
    let owned = destination.to_owned();
    let diagnostic = diagnose::diagnostic_command(&lane_options(), destination);
    Io::child_explained(
        &mut command,
        Explain(Box::new(move |stderr| {
            diagnose::explain(stderr, &owned, &diagnostic)
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn the_destination_is_an_argument_and_the_program_is_quoted_for_the_far_shell() {
        let command = command("me@box", "/opt/my phux/phux").expect("valid");
        let args: Vec<_> = command.as_std().get_args().map(OsString::from).collect();
        let tail: Vec<_> = args.iter().rev().take(3).rev().cloned().collect();
        assert_eq!(
            tail,
            [
                "--",
                "me@box",
                "'/opt/my phux/phux' server --ensure >/dev/null 2>&1; \
                 exec '/opt/my phux/phux' stdio-bridge"
            ]
            .map(OsString::from)
        );
    }

    /// Command-line options beat the user's ssh config, so these hold
    /// whatever it says: its own `ControlPath` and `ControlMaster` never meet
    /// phux's master, and its `LocalForward`s never make a run log in again.
    #[test]
    fn every_run_is_a_client_of_phux_master_and_forwards_nothing() {
        let dir = Some(std::path::Path::new("/tmp/phux-ssh-501"));
        assert_eq!(
            client_options(dir, false),
            [
                "-o",
                "ControlPath=/tmp/phux-ssh-501/%C",
                "-o",
                "ControlMaster=no",
                "-o",
                "ClearAllForwardings=yes"
            ]
        );
        assert!(
            client_options(None, false).is_empty(),
            "no master, plain ssh"
        );
    }

    /// A `BatchMode` run with no master up becomes one that outlives it, so
    /// the next lane or one-shot run skips the handshake. Without
    /// `ControlPersist` it would be a master that dies with its own run.
    #[test]
    fn a_batch_run_leaves_phux_master_up_for_the_next() {
        let dir = Some(std::path::Path::new("/tmp/phux-ssh-501"));
        assert_eq!(
            client_options(dir, true),
            [
                "-o",
                "ControlPath=/tmp/phux-ssh-501/%C",
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPersist=300",
                "-o",
                "ClearAllForwardings=yes"
            ]
        );
        assert!(
            client_options(None, true).is_empty(),
            "no master, plain ssh"
        );
        let lane = command("me@box", "phux").expect("valid");
        let args: Vec<_> = lane.as_std().get_args().map(OsString::from).collect();
        assert!(args.contains(&OsString::from("BatchMode=yes")), "{args:?}");
    }

    #[test]
    fn a_refused_host_key_gets_a_fix_and_never_an_automatic_one() {
        const CHANGED: &str = "\
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@
@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@
Offending ED25519 key in /home/me/.ssh/known_hosts:3
  remove with:
  ssh-keygen -f '/home/me/.ssh/known_hosts' -R '[box]:2222'
Host key for [box]:2222 has changed and you have requested strict checking.
Host key verification failed.";
        // macOS's ssh prints no "remove with:"; the command is built from
        // the file and entry it does name.
        const CHANGED_MACOS: &str = "\
@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @
Offending ED25519 key in /Users/me/.ssh/known_hosts:1
Host key for [127.0.0.1]:22422 has changed and you have requested strict checking.
Host key verification failed.";
        let fix = host_key_remedy(CHANGED, "me@box").expect("changed");
        assert!(
            fix.contains("`ssh-keygen -f '/home/me/.ssh/known_hosts' -R '[box]:2222'`"),
            "ssh's own entry is quoted exactly: {fix}"
        );
        assert!(
            fix.contains("confirm the new key") && fix.contains("`ssh me@box`"),
            "{fix}"
        );

        let fix = host_key_remedy(CHANGED_MACOS, "box").expect("changed");
        assert!(
            fix.contains("`ssh-keygen -f '/Users/me/.ssh/known_hosts' -R '[127.0.0.1]:22422'`"),
            "{fix}"
        );

        let fix = host_key_remedy("Host key verification failed.\r\n", "box").expect("unknown");
        assert!(
            fix.contains("does not know") && fix.contains("`ssh box`"),
            "{fix}"
        );
        assert!(!fix.contains("ssh-keygen"), "nothing to remove: {fix}");

        let revoked = "WARNING: REVOKED HOST KEY DETECTED!\nHost key verification failed.";
        let fix = host_key_remedy(revoked, "box").expect("revoked");
        assert!(
            fix.contains("revoked") && !fix.contains("ssh-keygen"),
            "{fix}"
        );

        assert_eq!(
            host_key_remedy("Permission denied (publickey).", "box"),
            None
        );
    }

    /// A generated per-host block, as a VM manager writes one, brings its
    /// own master, `BatchMode` and host-key settings. Real ssh resolves the
    /// options (`ssh -G`, no network): phux's master settings win, the
    /// block's address and host-key choices stand, and its `BatchMode yes`
    /// reaches phux's own login, which is why a refused password says so.
    #[test]
    fn a_generated_host_block_keeps_its_address_and_loses_its_master() {
        let root = tempfile::tempdir().expect("tempdir");
        let config = root.path().join("config");
        std::fs::write(
            &config,
            "Host vm\n  HostName 127.0.0.1\n  Port 2222\n  User me\n  ControlMaster auto\n  \
             ControlPath ~/.vm/ssh/%r@%h:%p\n  ControlPersist yes\n  BatchMode yes\n  \
             UserKnownHostsFile /dev/null\n  StrictHostKeyChecking no\n",
        )
        .expect("write config");
        let resolve = |options: &[String]| -> Option<String> {
            let output = std::process::Command::new("ssh")
                .arg("-G")
                .arg("-F")
                .arg(&config)
                .args(options)
                .arg("vm")
                .stdin(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|output| output.status.success())?;
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        };
        let dir = Path::new("/tmp/phux-ssh-test");
        let Some(master) = resolve(&master_options(dir)) else {
            return; // No ssh to resolve options with.
        };
        let lines =
            |resolved: &str| -> Vec<String> { resolved.lines().map(str::to_owned).collect() };
        let has = |resolved: &[String], line: &str| resolved.iter().any(|seen| seen == line);
        let master = lines(&master);
        for line in [
            "hostname 127.0.0.1",
            "port 2222",
            "controlmaster auto",
            "controlpersist 300",
            "clearallforwardings yes",
            "userknownhostsfile /dev/null",
            "batchmode yes",
        ] {
            assert!(has(&master, line), "{line}: {master:?}");
        }
        assert!(
            master
                .iter()
                .any(|line| line.starts_with("controlpath /tmp/phux-ssh-test/")),
            "{master:?}"
        );

        let mut lane = client_options(Some(dir), true);
        lane.extend(
            lane_options()
                .into_iter()
                .skip_while(|option| option != "-T"),
        );
        let lane = lines(&resolve(&lane).expect("lane options resolve"));
        for line in ["controlmaster auto", "controlpersist 300", "batchmode yes"] {
            assert!(has(&lane, line), "{line}: {lane:?}");
        }
        assert!(
            lane.iter()
                .any(|line| line.starts_with("controlpath /tmp/phux-ssh-test/")),
            "{lane:?}"
        );

        let client = lines(&resolve(&client_options(Some(dir), false)).expect("resolves"));
        assert!(has(&client, "controlmaster false"), "{client:?}");
    }

    #[test]
    fn option_shaped_or_empty_inputs_are_refused() {
        for (destination, phux) in [("-oProxyCommand=x", "phux"), ("", "phux"), ("box", "-x")] {
            let error = command(destination, phux).expect_err("refused");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
    }
}
