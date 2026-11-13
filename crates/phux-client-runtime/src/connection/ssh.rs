//! The protocol over `ssh HOST phux stdio-bridge`: a child-stdio lane for
//! hosts UDP cannot reach. ssh authenticates; the far server sees its own
//! user on its own socket, so no token or pin is involved.

use std::path::Path;

use super::io::{Explain, Io};

mod diagnose;
mod host_key;
mod master;

pub use diagnose::failure_hint;
pub use host_key::{HOST_KEY_REFUSED, host_key_remedy};
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

static AGENT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Name the SSH agent socket every login from this process uses.
///
/// For an app that did not inherit a shell's `SSH_AUTH_SOCK`. The first call
/// stands. An `IdentityAgent` in the user's ssh config still wins, as it
/// does over the environment.
pub fn set_agent(socket: std::path::PathBuf) {
    let _ = AGENT.set(socket);
}

/// Whether a login from this process can reach an agent at all.
fn agent_known() -> bool {
    AGENT.get().is_some() || std::env::var_os("SSH_AUTH_SOCK").is_some()
}

/// Point `command`, an ssh that logs in, at the agent from [`set_agent`].
fn use_agent(command: &mut tokio::process::Command) {
    agent_env(command, AGENT.get().map(std::path::PathBuf::as_path));
}

fn agent_env(command: &mut tokio::process::Command, socket: Option<&Path>) {
    if let Some(socket) = socket {
        command.env("SSH_AUTH_SOCK", socket);
    }
}

/// Every option a lane's ssh runs with, before `--`. `prompts` when the
/// run has an askpass program ([`askpass_env`]).
fn lane_options(prompts: bool) -> Vec<String> {
    // A password or 2FA login done once in the foreground (`phux attach
    // --ssh`, `phux host add`) left a master this run rides; without one,
    // this run leaves one for the next.
    let mut options = batch_control_options();
    // The pipes are the protocol's, and the terminal may be the TUI's: with
    // no askpass, a login that must prompt fails here and is left to
    // `ssh -t`. A dead path ends the link in about 45 s instead of hanging.
    if !prompts {
        options.extend(["-o", "BatchMode=yes"].map(str::to_owned));
    }
    options.extend(
        [
            "-T",
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

/// Make ssh ask through `askpass` and never a terminal, as phux's master
/// login does for a desktop client.
fn askpass_env(command: &mut tokio::process::Command, askpass: &Path) {
    command
        .env("SSH_ASKPASS", askpass)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env(ASKPASS_ENV, "1");
}

fn command(
    destination: &str,
    remote_phux: &str,
    askpass: Option<&Path>,
) -> std::io::Result<tokio::process::Command> {
    validate(destination, remote_phux)?;
    let mut command = tokio::process::Command::new(program());
    use_agent(&mut command);
    if let Some(askpass) = askpass {
        askpass_env(&mut command, askpass);
    }
    command
        .args(lane_options(askpass.is_some()))
        .arg("--")
        .arg(destination)
        .arg(remote_command(remote_phux));
    Ok(command)
}

/// Start the lane. Dials to one destination in this process take turns
/// logging in, so the first becomes phux's master and the rest ride it,
/// rather than all racing sshd's `MaxStartups` to be it.
///
/// When the master is full (sshd's `MaxSessions`), ssh logs this one lane
/// in on its own connection. With `askpass` that login prompts the way the
/// master's did, so a password or 2FA host still opens it.
pub(super) async fn establish(
    destination: &str,
    remote_phux: &str,
    askpass: Option<&Path>,
) -> std::io::Result<Io> {
    let mut command = command(destination, remote_phux, askpass)?;
    // Without an askpass program this never fails; the lane reports.
    let _ = master::ensure(destination, None).await;
    let owned = destination.to_owned();
    let diagnostic = diagnose::diagnostic_command(&lane_options(askpass.is_some()), destination);
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
        let command = command("me@box", "/opt/my phux/phux", None).expect("valid");
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
        let lane = command("me@box", "phux", None).expect("valid");
        let args: Vec<_> = lane.as_std().get_args().map(OsString::from).collect();
        assert!(args.contains(&OsString::from("BatchMode=yes")), "{args:?}");
    }

    /// With an askpass program the lane may prompt through it, never the
    /// terminal, under the same rules as phux's master login: so when sshd's
    /// `MaxSessions` turns it away from the full master and ssh logs it in on
    /// its own, a password or 2FA host still opens it.
    #[test]
    fn a_lane_with_askpass_prompts_through_it_and_never_the_terminal() {
        let lane = command("me@box", "phux", Some(Path::new("/app/askpass"))).expect("valid");
        let lane = lane.as_std();
        let args: Vec<_> = lane.get_args().map(OsString::from).collect();
        assert!(!args.contains(&OsString::from("BatchMode=yes")), "{args:?}");
        let env = |key: &str| {
            lane.get_envs()
                .find(|(name, _)| *name == key)
                .and_then(|(_, value)| value)
                .map(std::ffi::OsStr::to_os_string)
        };
        assert_eq!(env("SSH_ASKPASS"), Some("/app/askpass".into()));
        assert_eq!(env("SSH_ASKPASS_REQUIRE"), Some("force".into()));
        assert_eq!(env(ASKPASS_ENV), Some("1".into()));
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
            lane_options(false)
                .into_iter()
                .skip(batch_control_options().len()),
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
    fn a_named_agent_replaces_the_inherited_one_and_none_leaves_it() {
        let mut command = tokio::process::Command::new("ssh");
        agent_env(&mut command, None);
        assert_eq!(command.as_std().get_envs().count(), 0);
        agent_env(&mut command, Some(Path::new("/run/agent.sock")));
        let envs: Vec<_> = command.as_std().get_envs().collect();
        assert_eq!(
            envs,
            [(
                std::ffi::OsStr::new("SSH_AUTH_SOCK"),
                Some(std::ffi::OsStr::new("/run/agent.sock"))
            )]
        );
    }

    #[test]
    fn option_shaped_or_empty_inputs_are_refused() {
        for (destination, phux) in [("-oProxyCommand=x", "phux"), ("", "phux"), ("box", "-x")] {
            let error = command(destination, phux, None).expect_err("refused");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        }
    }
}
