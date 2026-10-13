//! `phux upgrade` and the client-side upgrade driver `phux update` shares.
//!
//! The driver owns what a refusal means: a transient one (the tree moved, the
//! aggregate deadline passed) is asked again on a short bounded backoff; a
//! server that predates agent-session-safe handoff (phux-twft, fixed in
//! d50f8979) refuses while any `AgentSession` exists, so the driver can close
//! those sessions with consent and ask once more; anything else is final.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::AttachError;
use phux_protocol::ids::ResourceId;
use phux_server::runtime::default_socket_path;

use crate::commands::server_target::ServerTarget;
use crate::commands::{cli_runtime, report_no_server, warn_interleaved_degradation};

/// What a server said when asked to graceful-upgrade.
///
/// Re-exported from [`phux_client::upgrade`] (the wire round trip) so `phux
/// update` can drive the same primitive after it has put a new binary on
/// disk, without duplicating the "a disconnect right after the ack is
/// success" subtlety. `phux upgrade` remains the low-level verb: it re-execs
/// whatever is already on disk and discovers, downloads, and verifies
/// nothing.
pub(crate) use phux_client::upgrade::UpgradeAck;

/// The pauses between attempts after a transient refusal: four attempts
/// spread over five seconds, long enough for an exit watcher to reap a pane
/// or a tree edit to settle, short enough to stay an interactive command.
pub(crate) const TRANSIENT_BACKOFF: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_millis(1500),
    Duration::from_millis(3000),
];

/// Server refusals that ask the caller to try again: `TreeChanged` and
/// `PaneExiting` say so, and `HandoffDeadline` is the same race measured by
/// the clock.
const TRANSIENT_MARKERS: [&str; 2] = [
    "retry the upgrade",
    "did not complete within the aggregate deadline",
];

/// The refusal a server older than d50f8979 gives while it holds any
/// `AgentSession`: it asked every resource for a PTY handoff, and agent
/// sessions are built with a closed upgrade mailbox. Current servers skip a
/// closed mailbox, so no current refusal carries this text.
const LEGACY_AGENT_SESSION_MARKER: &str =
    "did not provide an upgrade handoff: actor mailbox closed";

/// What a refusal permits the driver to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefusalClass {
    /// A race the server expects to clear; asking again shortly succeeds.
    Transient,
    /// An older server that cannot hand off while agent sessions exist.
    LegacyAgentSessions,
    /// Asking again cannot help until something else changes.
    Fatal,
}

/// Classify a server's refusal message.
pub(crate) fn classify(message: &str) -> RefusalClass {
    if TRANSIENT_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
    {
        RefusalClass::Transient
    } else if message.contains(LEGACY_AGENT_SESSION_MARKER) {
        RefusalClass::LegacyAgentSessions
    } else {
        RefusalClass::Fatal
    }
}

/// How far the driver may go when an older server refuses because of agent
/// sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Consent {
    /// `--close-agent-sessions`: close them and retry without asking.
    Granted,
    /// A terminal is attached: explain and ask.
    Ask,
    /// No one to ask: explain and print the exact commands.
    Advise,
}

impl Consent {
    /// The consent this process has: the flag wins, then a terminal on both
    /// stdin and stderr, else advice only.
    pub(crate) fn detect(close_agent_sessions: bool) -> Self {
        if close_agent_sessions {
            Self::Granted
        } else if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
            Self::Ask
        } else {
            Self::Advise
        }
    }
}

/// The server and the person the driver talks to, behind one seam so the
/// policy is tested without a socket, a clock, or a terminal.
pub(crate) trait UpgradeEnv {
    /// Send one `UPGRADE` and classify the reply.
    fn send_upgrade(&mut self) -> Result<UpgradeAck, AttachError>;
    /// The server's local `AgentSession` resources.
    fn agent_sessions(&mut self) -> Result<Vec<ResourceId>, AttachError>;
    /// Close one `AgentSession`.
    fn close_agent_session(&mut self, session: &ResourceId) -> Result<(), String>;
    /// Wait out one backoff step.
    fn sleep(&mut self, delay: Duration);
    /// Tell the person one line (stderr); silent for machine output.
    fn say(&mut self, line: &str);
    /// Ask a yes/no question; `true` only on an explicit yes.
    fn confirm(&mut self, action: &str) -> bool;
}

/// Ask for an upgrade, retrying transient refusals on [`TRANSIENT_BACKOFF`].
/// One progress line is said before the first retry; a fatal refusal or a
/// transport failure ends the attempt at once.
///
/// # Errors
///
/// The first transport failure, unchanged.
pub(crate) fn request_with_retry(env: &mut dyn UpgradeEnv) -> Result<UpgradeAck, AttachError> {
    let mut ack = env.send_upgrade()?;
    for (attempt, delay) in TRANSIENT_BACKOFF.iter().enumerate() {
        let UpgradeAck::Refused(message) = &ack else {
            return Ok(ack);
        };
        if classify(message) != RefusalClass::Transient {
            return Ok(ack);
        }
        if attempt == 0 {
            let budget: Duration = TRANSIENT_BACKOFF.iter().sum();
            env.say(&format!(
                "server not ready to hand off ({message}); retrying for up to {}s",
                budget.as_secs()
            ));
        }
        env.sleep(*delay);
        ack = env.send_upgrade()?;
    }
    Ok(ack)
}

/// The whole client-side upgrade: [`request_with_retry`], then, when an older
/// server refuses because of agent sessions, close them under `consent` and
/// ask once more.
///
/// `socket` is the explicit `--socket`, echoed into printed commands so they
/// reach the same server.
///
/// # Errors
///
/// The first transport failure, unchanged.
pub(crate) fn drive(
    env: &mut dyn UpgradeEnv,
    consent: Consent,
    socket: Option<&Path>,
) -> Result<UpgradeAck, AttachError> {
    let ack = request_with_retry(env)?;
    let UpgradeAck::Refused(message) = &ack else {
        return Ok(ack);
    };
    if classify(message) != RefusalClass::LegacyAgentSessions {
        return Ok(ack);
    }

    let sessions = match env.agent_sessions() {
        Ok(sessions) => sessions,
        Err(err) => {
            env.say(&format!(
                "could not list the server's agent sessions: {err}"
            ));
            return Ok(ack);
        }
    };
    if sessions.is_empty() {
        env.say(UNLISTED_BLOCKER);
        return Ok(ack);
    }

    let ids = sessions
        .iter()
        .map(phux_client::selector::format_terminal_id)
        .collect::<Vec<_>>();
    if consent != Consent::Granted {
        env.say(&format!(
            "this server predates agent-session-safe handoff and cannot upgrade while agent \
             sessions exist ({})",
            ids.join(" ")
        ));
        env.say(CLOSE_EFFECT);
    }
    let proceed = match consent {
        Consent::Granted => true,
        Consent::Ask => env.confirm(&format!(
            "close {} agent session{} and retry the upgrade",
            ids.len(),
            if ids.len() == 1 { "" } else { "s" }
        )),
        Consent::Advise => {
            for line in close_commands(&ids, socket) {
                env.say(&line);
            }
            false
        }
    };
    if !proceed {
        return Ok(ack);
    }

    for (session, id) in sessions.iter().zip(&ids) {
        match env.close_agent_session(session) {
            Ok(()) => env.say(&format!("closed agent session {id}")),
            Err(err) => env.say(&format!("could not close agent session {id}: {err}")),
        }
    }
    let retried = request_with_retry(env)?;
    if let UpgradeAck::Refused(message) = &retried
        && classify(message) == RefusalClass::LegacyAgentSessions
    {
        env.say(STILL_BLOCKED);
    }
    Ok(retried)
}

/// What closing an agent session does to the pane and the harness behind it,
/// as the integrations implement it: every phux call they make is a one-shot
/// CLI run, an emit to a closed session is only reported, and nothing maps
/// that failure to a reopen (`integrations/runtime/src/adapter.ts`
/// `AgentSessionEmitter`; omp disables itself on any failure).
const CLOSE_EFFECT: &str = "closing them keeps every pane and agent process running; only their \
     phux event logs end, and no harness reopens one at once (pi and OpenCode do with their next \
     agent session, omp when it restarts)";

/// The legacy refusal with no listed agent session: the old server still holds
/// a resource handle whose registry entry is gone (the other half of
/// phux-twft), which no client verb can reach.
const UNLISTED_BLOCKER: &str = "this server predates agent-session-safe handoff and is holding \
     a resource it no longer lists, so no client command can clear it; only a server restart \
     (`phux kill --server`, which ends every pane) replaces it";

/// The legacy refusal again after closing every listed session.
const STILL_BLOCKED: &str = "still refused after closing the agent sessions: a harness may have \
     opened a new one, or the server is holding a resource it no longer lists; run the upgrade \
     again, and if it keeps refusing only a server restart (`phux kill --server`, which ends \
     every pane) replaces it";

/// The exact commands that close `ids` and retry, for a caller with no one to
/// ask.
fn close_commands(ids: &[String], socket: Option<&Path>) -> Vec<String> {
    let phux = socket.map_or_else(
        || "phux".to_owned(),
        |path| {
            format!(
                "phux --socket {}",
                super::ssh_bootstrap::shell_quote(&path.to_string_lossy())
            )
        },
    );
    let mut lines = vec![
        "to close them and retry, run:".to_owned(),
        format!("  {phux} upgrade --close-agent-sessions"),
        "or, one at a time:".to_owned(),
    ];
    lines.extend(
        ids.iter()
            .map(|id| format!("  {phux} agent session close {id}")),
    );
    lines.push(format!("  {phux} upgrade"));
    lines
}

/// The live [`UpgradeEnv`]: one local server, one runtime, stderr, and the
/// terminal.
pub(crate) struct LiveUpgrade<'a> {
    socket: &'a Path,
    runtime: tokio::runtime::Runtime,
    quiet: bool,
}

impl<'a> LiveUpgrade<'a> {
    /// A driver environment for the server at `socket`. `quiet` keeps stderr
    /// clean for machine output.
    ///
    /// # Errors
    ///
    /// When no runtime can be built.
    pub(crate) fn new(socket: &'a Path, quiet: bool) -> Result<Self, AttachError> {
        let runtime = cli_runtime().map_err(|_| {
            AttachError::Io(std::io::Error::other(
                "could not build a tokio runtime for the upgrade request",
            ))
        })?;
        Ok(Self {
            socket,
            runtime,
            quiet,
        })
    }
}

impl UpgradeEnv for LiveUpgrade<'_> {
    fn send_upgrade(&mut self) -> Result<UpgradeAck, AttachError> {
        let socket = self.socket;
        self.runtime.block_on(async move {
            let mut conn = ServerTarget::local(socket).connect().await?;
            match phux_client::upgrade::upgrade(&mut conn, 0).await {
                // The pre-exec ack. A `Disconnected` immediately after is the
                // expected blink as the old image is replaced — both mean the
                // upgrade is under way.
                Ok((ack, degradation)) => {
                    warn_interleaved_degradation(&degradation);
                    Ok(ack)
                }
                Err(AttachError::Disconnected) => Ok(UpgradeAck::Upgrading),
                Err(err) => Err(err),
            }
        })
    }

    fn agent_sessions(&mut self) -> Result<Vec<ResourceId>, AttachError> {
        let socket = self.socket;
        self.runtime.block_on(async move {
            let mut conn = ServerTarget::local(socket).connect().await?;
            let (snapshot, _degradation) = phux_client::state::get_state_on(&mut conn)
                .await?
                .into_parts();
            Ok(snapshot
                .resources
                .iter()
                .filter(|info| {
                    phux_client::resource::is_agent_session(info)
                        && matches!(info.id, ResourceId::Local { .. })
                })
                .map(|info| info.id.clone())
                .collect())
        })
    }

    fn close_agent_session(&mut self, session: &ResourceId) -> Result<(), String> {
        let socket = self.socket;
        self.runtime.block_on(async move {
            let mut conn = ServerTarget::local(socket)
                .connect()
                .await
                .map_err(|err| err.to_string())?;
            phux_client::agent_session::close(&mut conn, session)
                .await
                .map_err(|err| err.to_string())
        })
    }

    fn sleep(&mut self, delay: Duration) {
        std::thread::sleep(delay);
    }

    fn say(&mut self, line: &str) {
        if !self.quiet {
            eprintln!("phux: {line}");
        }
    }

    fn confirm(&mut self, action: &str) -> bool {
        !self.quiet && super::confirm::confirmed(false, action).is_ok()
    }
}

/// Ask the server at `socket_path` to graceful-upgrade in place, once, with
/// no retry: the attach-time skew reconciliation stays best-effort.
///
/// The `Err` arm carries the transport failure unchanged so the caller can
/// render it its own way.
pub(crate) fn request_upgrade(socket_path: &Path) -> Result<UpgradeAck, AttachError> {
    LiveUpgrade::new(socket_path, true)?.send_upgrade()
}

/// The socket to dial, and the explicit `--socket` (if any) that printed
/// commands must repeat to reach the same server.
pub(crate) fn resolve_socket(socket: Option<PathBuf>) -> (PathBuf, Option<PathBuf>) {
    (socket.clone().unwrap_or_else(default_socket_path), socket)
}

/// `phux upgrade` — ask the running server to graceful-upgrade itself in place
/// (ADR-0032).
///
/// The server snapshots every pane, re-execs the on-disk binary, and re-adopts
/// the live PTYs, so the shells / editors / agents in every session survive
/// the binary update. Attached clients (including this one's siblings) see a
/// brief disconnect and reconnect. Transient refusals are retried briefly;
/// an older server blocked by agent sessions gets them closed with consent.
/// Exit codes: 0 on a clean ack or the expected re-exec disconnect, 1 on no
/// server, 2 on a server-side refusal.
///
/// This verb does not fetch anything. `phux update` is the user-facing command
/// that discovers, downloads, verifies, and installs a release and then calls
/// this one.
pub(crate) fn run_upgrade(socket: Option<PathBuf>, close_agent_sessions: bool) -> ExitCode {
    let (socket_path, explicit) = resolve_socket(socket);
    let outcome = LiveUpgrade::new(&socket_path, false).and_then(|mut env| {
        drive(
            &mut env,
            Consent::detect(close_agent_sessions),
            explicit.as_deref(),
        )
    });

    match outcome {
        Ok(UpgradeAck::Upgrading) => {
            eprintln!("phux: server upgrading in place; sessions preserved");
            ExitCode::SUCCESS
        }
        Ok(UpgradeAck::Refused(message)) => {
            eprintln!("phux: upgrade refused: {message}");
            ExitCode::from(2)
        }
        Ok(UpgradeAck::Unexpected(message)) => {
            eprintln!("phux: {message}");
            ExitCode::from(2)
        }
        Err(err) => report_no_server(&err, &socket_path, "upgrade"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    const TREE_CHANGED: &str = "upgrade failed: server state changed while collecting upgrade \
                                handoffs; retry the upgrade";
    const PANE_EXITING: &str =
        "upgrade failed: pane ResourceId(3v1) is exiting and not yet reaped; retry the upgrade";
    const DEADLINE: &str =
        "upgrade failed: pane upgrade handoffs did not complete within the aggregate deadline";
    const LEGACY: &str = "upgrade failed: pane ResourceId(20v11) did not provide an upgrade \
                          handoff: actor mailbox closed";
    const VALIDATION: &str = "upgrade failed: new binary failed validation: config check failed";

    #[allow(
        clippy::unnecessary_wraps,
        reason = "a scripted reply, beside the transport failures it is mixed with"
    )]
    fn refused(message: &str) -> Result<UpgradeAck, AttachError> {
        Ok(UpgradeAck::Refused(message.to_owned()))
    }

    fn local(id: u32) -> ResourceId {
        ResourceId::local(id)
    }

    /// A scripted server and person; records every side effect.
    struct Fake {
        replies: VecDeque<Result<UpgradeAck, AttachError>>,
        sessions: Result<Vec<ResourceId>, ()>,
        answer: bool,
        requests: usize,
        sleeps: Vec<Duration>,
        closed: Vec<ResourceId>,
        asked: usize,
        said: usize,
    }

    impl Fake {
        fn new(replies: impl IntoIterator<Item = Result<UpgradeAck, AttachError>>) -> Self {
            Self {
                replies: replies.into_iter().collect(),
                sessions: Ok(Vec::new()),
                answer: false,
                requests: 0,
                sleeps: Vec::new(),
                closed: Vec::new(),
                asked: 0,
                said: 0,
            }
        }

        fn with_sessions(mut self, sessions: Vec<ResourceId>) -> Self {
            self.sessions = Ok(sessions);
            self
        }
    }

    impl UpgradeEnv for Fake {
        fn send_upgrade(&mut self) -> Result<UpgradeAck, AttachError> {
            self.requests += 1;
            self.replies
                .pop_front()
                .expect("the driver asked more often than the script allows")
        }

        fn agent_sessions(&mut self) -> Result<Vec<ResourceId>, AttachError> {
            self.sessions
                .clone()
                .map_err(|()| AttachError::Disconnected)
        }

        fn close_agent_session(&mut self, session: &ResourceId) -> Result<(), String> {
            self.closed.push(session.clone());
            Ok(())
        }

        fn sleep(&mut self, delay: Duration) {
            self.sleeps.push(delay);
        }

        fn say(&mut self, _line: &str) {
            self.said += 1;
        }

        fn confirm(&mut self, _action: &str) -> bool {
            self.asked += 1;
            self.answer
        }
    }

    #[test]
    fn refusals_classify_by_what_a_retry_can_change() {
        assert_eq!(classify(TREE_CHANGED), RefusalClass::Transient);
        assert_eq!(classify(PANE_EXITING), RefusalClass::Transient);
        assert_eq!(classify(DEADLINE), RefusalClass::Transient);
        assert_eq!(classify(LEGACY), RefusalClass::LegacyAgentSessions);
        assert_eq!(classify(VALIDATION), RefusalClass::Fatal);
        // Current servers' remaining pane failures are not the legacy one.
        assert_eq!(
            classify(
                "upgrade failed: pane ResourceId(2v1) did not provide an upgrade handoff: \
                 actor dropped its reply"
            ),
            RefusalClass::Fatal
        );
        assert_eq!(
            classify("upgrade failed: server not ready for upgrade (no listener context)"),
            RefusalClass::Fatal
        );
    }

    #[test]
    fn a_transient_refusal_is_retried_on_the_bounded_backoff_until_it_clears() {
        let mut env = Fake::new([
            refused(TREE_CHANGED),
            refused(PANE_EXITING),
            Ok(UpgradeAck::Upgrading),
        ]);
        let ack = request_with_retry(&mut env).unwrap();
        assert_eq!(ack, UpgradeAck::Upgrading);
        assert_eq!(env.requests, 3);
        assert_eq!(env.sleeps, TRANSIENT_BACKOFF[..2]);
        assert_eq!(env.said, 1, "one progress line, not one per attempt");
    }

    #[test]
    fn transient_retries_stop_after_four_attempts_within_five_seconds() {
        let mut env = Fake::new((0..4).map(|_| refused(DEADLINE)));
        let ack = request_with_retry(&mut env).unwrap();
        assert_eq!(ack, UpgradeAck::Refused(DEADLINE.to_owned()));
        assert_eq!(env.requests, 4);
        assert_eq!(env.sleeps, TRANSIENT_BACKOFF);
        assert!(env.sleeps.iter().sum::<Duration>() <= Duration::from_secs(5));
    }

    #[test]
    fn fatal_and_legacy_refusals_and_transport_failures_are_never_retried() {
        for reply in [refused(VALIDATION), refused(LEGACY)] {
            let mut env = Fake::new([reply]);
            request_with_retry(&mut env).unwrap();
            assert_eq!(env.requests, 1);
            assert!(env.sleeps.is_empty());
        }

        let mut env = Fake::new([refused(TREE_CHANGED), Err(AttachError::Disconnected)]);
        assert!(request_with_retry(&mut env).is_err());
        assert_eq!(env.requests, 2);
    }

    #[test]
    fn granted_consent_closes_every_agent_session_and_retries_once() {
        let mut env = Fake::new([
            refused(LEGACY),
            refused(TREE_CHANGED),
            Ok(UpgradeAck::Upgrading),
        ])
        .with_sessions(vec![local(3), local(7)]);
        let ack = drive(&mut env, Consent::Granted, None).unwrap();
        assert_eq!(ack, UpgradeAck::Upgrading);
        assert_eq!(env.closed, [local(3), local(7)]);
        assert_eq!(env.asked, 0, "the flag never prompts");
        assert_eq!(
            env.sleeps,
            TRANSIENT_BACKOFF[..1],
            "the retry keeps the transient policy"
        );
    }

    #[test]
    fn a_declined_prompt_closes_nothing_and_keeps_the_refusal() {
        let mut env = Fake::new([refused(LEGACY)]).with_sessions(vec![local(3)]);
        let ack = drive(&mut env, Consent::Ask, None).unwrap();
        assert_eq!(ack, UpgradeAck::Refused(LEGACY.to_owned()));
        assert_eq!(env.asked, 1);
        assert!(env.closed.is_empty());
        assert_eq!(env.requests, 1);
    }

    #[test]
    fn an_accepted_prompt_closes_and_retries() {
        let mut env =
            Fake::new([refused(LEGACY), Ok(UpgradeAck::Upgrading)]).with_sessions(vec![local(3)]);
        env.answer = true;
        let ack = drive(&mut env, Consent::Ask, None).unwrap();
        assert_eq!(ack, UpgradeAck::Upgrading);
        assert_eq!(env.closed, [local(3)]);
        assert_eq!(env.requests, 2);
    }

    #[test]
    fn without_a_terminal_the_driver_only_advises() {
        let mut env = Fake::new([refused(LEGACY)]).with_sessions(vec![local(3)]);
        let ack = drive(&mut env, Consent::Advise, None).unwrap();
        assert_eq!(ack, UpgradeAck::Refused(LEGACY.to_owned()));
        assert_eq!(env.asked, 0);
        assert!(env.closed.is_empty());
    }

    #[test]
    fn a_legacy_refusal_with_no_listed_session_or_no_listing_closes_nothing() {
        let mut env = Fake::new([refused(LEGACY)]);
        let ack = drive(&mut env, Consent::Granted, None).unwrap();
        assert_eq!(ack, UpgradeAck::Refused(LEGACY.to_owned()));
        assert!(env.closed.is_empty());
        assert_eq!(env.requests, 1);

        let mut env = Fake::new([refused(LEGACY)]);
        env.sessions = Err(());
        drive(&mut env, Consent::Granted, None).unwrap();
        assert!(env.closed.is_empty());
        assert_eq!(env.requests, 1);
    }

    #[test]
    fn a_legacy_refusal_after_closing_is_not_retried_again() {
        let mut env = Fake::new([refused(LEGACY), refused(LEGACY)]).with_sessions(vec![local(3)]);
        let ack = drive(&mut env, Consent::Granted, None).unwrap();
        assert_eq!(ack, UpgradeAck::Refused(LEGACY.to_owned()));
        assert_eq!(env.requests, 2);
        assert_eq!(env.closed, [local(3)]);
    }

    #[test]
    fn a_fatal_refusal_never_lists_or_closes_agent_sessions() {
        let mut env = Fake::new([refused(VALIDATION)]).with_sessions(vec![local(3)]);
        drive(&mut env, Consent::Granted, None).unwrap();
        assert!(env.closed.is_empty());
        assert_eq!(env.requests, 1);
    }

    #[test]
    fn printed_commands_target_the_same_socket() {
        let ids = ["@3".to_owned(), "@7".to_owned()];
        let lines = close_commands(&ids, Some(Path::new("/tmp/x y/phux.sock")));
        let commands: Vec<&str> = lines
            .iter()
            .filter_map(|line| line.strip_prefix("  "))
            .collect();
        assert_eq!(
            commands,
            [
                "phux --socket '/tmp/x y/phux.sock' upgrade --close-agent-sessions",
                "phux --socket '/tmp/x y/phux.sock' agent session close @3",
                "phux --socket '/tmp/x y/phux.sock' agent session close @7",
                "phux --socket '/tmp/x y/phux.sock' upgrade",
            ]
        );
        assert!(
            close_commands(&ids, None)
                .iter()
                .any(|line| line == "  phux agent session close @3")
        );
    }

    /// The live environment against a scripted server, with the clock
    /// recorded instead of waited on.
    struct NoWait<'a> {
        live: LiveUpgrade<'a>,
        sleeps: Vec<Duration>,
    }

    impl UpgradeEnv for NoWait<'_> {
        fn send_upgrade(&mut self) -> Result<UpgradeAck, AttachError> {
            self.live.send_upgrade()
        }

        fn agent_sessions(&mut self) -> Result<Vec<ResourceId>, AttachError> {
            self.live.agent_sessions()
        }

        fn close_agent_session(&mut self, session: &ResourceId) -> Result<(), String> {
            self.live.close_agent_session(session)
        }

        fn sleep(&mut self, delay: Duration) {
            self.sleeps.push(delay);
        }

        fn say(&mut self, line: &str) {
            self.live.say(line);
        }

        fn confirm(&mut self, _action: &str) -> bool {
            panic!("a granted or advised run never prompts");
        }
    }

    /// Serve `phux.sock` in `dir` from its own thread, one connection at a
    /// time; connection `n` is answered from `spec_for(n)`. Returns the
    /// socket and a channel carrying each finished connection's client
    /// frames.
    fn scripted_server(
        dir: &Path,
        spec_for: impl Fn(usize) -> phux_client::testkit::ScriptSpec + Send + 'static,
    ) -> (
        PathBuf,
        std::sync::mpsc::Receiver<Vec<phux_protocol::wire::frame::FrameKind>>,
    ) {
        let socket = dir.join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (record, seen) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::UnixListener::from_std(listener).unwrap();
                for connection in 0.. {
                    let frames = phux_client::testkit::ScriptedServer::accept(
                        &listener,
                        spec_for(connection),
                    )
                    .await;
                    if record.send(frames).is_err() {
                        return;
                    }
                }
            });
        });
        (socket, seen)
    }

    /// Every `KILL_RESOURCE` target the client sent on a finished connection.
    /// Connections are served one at a time, so each one before the last
    /// request has already been recorded.
    fn kills(
        seen: &std::sync::mpsc::Receiver<Vec<phux_protocol::wire::frame::FrameKind>>,
    ) -> Vec<ResourceId> {
        use phux_protocol::wire::frame::{Command, FrameKind};
        seen.try_iter()
            .flatten()
            .filter_map(|frame| match frame {
                FrameKind::Command {
                    command: Command::KillResource { terminal_id, .. },
                    ..
                } => Some(terminal_id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_scripted_transient_refusal_clears_on_the_wire() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, _seen) = scripted_server(dir.path(), |connection| {
            let spec = phux_client::testkit::ScriptSpec::new();
            if connection < 2 {
                spec.refuse_upgrade(TREE_CHANGED)
            } else {
                spec
            }
        });
        let mut env = NoWait {
            live: LiveUpgrade::new(&socket, true).unwrap(),
            sleeps: Vec::new(),
        };

        let ack = drive(&mut env, Consent::Advise, None).unwrap();

        assert_eq!(ack, UpgradeAck::Upgrading);
        assert_eq!(env.sleeps, TRANSIENT_BACKOFF[..2]);
    }

    #[test]
    fn a_legacy_server_is_upgraded_after_its_agent_sessions_close() {
        use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
        use phux_protocol::ids::{ResourceKind, SessionId, WindowId};
        use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

        let dir = tempfile::tempdir().unwrap();
        let (socket, seen) = scripted_server(dir.path(), |connection| {
            let mut agent = ResourceInfo::resource(local(3), ResourceKind::AgentSession);
            agent.parent = Some(local(1));
            let snapshot =
                SessionSnapshot::new(SessionId::new(1), WindowId::new(1), local(1)).with_resources(
                    vec![ResourceInfo::new(local(1), WindowId::new(1), 80, 24), agent],
                );
            let spec = phux_client::testkit::ScriptSpec::new()
                .server_features(ServerFeatureSet::with(&[ServerFeature::ResourceKinds]))
                .state(snapshot);
            // Only the first ask meets the old refusal; the retry is clear.
            if connection == 0 {
                spec.refuse_upgrade(LEGACY)
            } else {
                spec
            }
        });
        let mut env = NoWait {
            live: LiveUpgrade::new(&socket, true).unwrap(),
            sleeps: Vec::new(),
        };

        let ack = drive(&mut env, Consent::Granted, None).unwrap();

        assert_eq!(ack, UpgradeAck::Upgrading);
        assert_eq!(kills(&seen), [local(3)], "only the agent session closes");
        assert!(env.sleeps.is_empty());
    }

    #[test]
    fn a_legacy_server_without_consent_keeps_every_agent_session() {
        use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
        use phux_protocol::ids::{ResourceKind, SessionId, WindowId};
        use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

        let dir = tempfile::tempdir().unwrap();
        let (socket, seen) = scripted_server(dir.path(), |_| {
            let mut agent = ResourceInfo::resource(local(3), ResourceKind::AgentSession);
            agent.parent = Some(local(1));
            phux_client::testkit::ScriptSpec::new()
                .server_features(ServerFeatureSet::with(&[ServerFeature::ResourceKinds]))
                .state(
                    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), local(1))
                        .with_resources(vec![agent]),
                )
                .refuse_upgrade(LEGACY)
        });
        let mut env = NoWait {
            live: LiveUpgrade::new(&socket, true).unwrap(),
            sleeps: Vec::new(),
        };

        let ack = drive(&mut env, Consent::Advise, None).unwrap();

        assert_eq!(ack, UpgradeAck::Refused(LEGACY.to_owned()));
        assert!(kills(&seen).is_empty());
    }
}
