//! The work API every surface uses: the CLI, Lead, the board, MCP, and later
//! the desktop. A [`Client`] sends each call to the store's keeper and starts
//! one when none answers.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::coord::{
    AckMail, Claim, ClaimFilter, ClaimOutcome, Mail, ReadMail, ReleaseClaim, RenewClaim, SendMail,
    Sent, TakeClaim, WaitMail,
};
use crate::error::Error;
use crate::id::Alias;
use crate::service::Endpoint;
use crate::start::{LaunchQueued, OpenRun, StartApplied, StartPlan, StartRequest};
use crate::store::{
    AddObjective, Applied, EditObjective, ListFilter, Objective, RaiseSignal, RevisionEdit, Signal,
    StateEdit,
};
use crate::wire::{BUILD, Envelope, Fault, MAX_LINE, Request, Response};

/// How long a call waits for a keeper to start answering.
const CONNECT_BUDGET: Duration = Duration::from_secs(2);
const PAUSE_START: Duration = Duration::from_millis(5);
const PAUSE_CAP: Duration = Duration::from_millis(100);
/// Keepers one call may start: the first, one more if that one lost a race
/// with a keeper still releasing the store, and one to replace a stale build.
const MAX_SPAWNS: u32 = 3;
/// How long a call waits for the keeper's reply once connected.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// How to start a keeper when none answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launcher {
    /// Program to run.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// Where the keeper's stderr goes. `None` discards it.
    pub log: Option<PathBuf>,
}

impl Launcher {
    fn spawn(&self) -> Result<Child, Error> {
        let stderr = match &self.log {
            Some(path) => {
                let mut options = OpenOptions::new();
                options.create(true).append(true);
                {
                    use std::os::unix::fs::OpenOptionsExt as _;
                    options.mode(0o600);
                }
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Stdio::from(options.open(path)?)
            }
            None => Stdio::null(),
        };
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr);
        {
            // Its own process group, so the caller's Ctrl-C does not stop it.
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        command.spawn().map_err(|err| {
            Error::Keeper(format!("could not start {}: {err}", self.program.display()))
        })
    }
}

/// A connection-per-call client of one store's keeper.
#[derive(Debug, Clone)]
pub struct Client {
    endpoint: Endpoint,
    launcher: Option<Launcher>,
}

impl Client {
    /// Talk to the keeper at `endpoint`, starting one with `launcher` when
    /// none answers. Without a launcher, a missing keeper is an error.
    #[must_use]
    pub const fn new(endpoint: Endpoint, launcher: Option<Launcher>) -> Self {
        Self { endpoint, launcher }
    }

    /// Insert an objective. See [`crate::Store::add`].
    ///
    /// # Errors
    ///
    /// The store's errors, [`Error::Busy`] when no keeper answers in time,
    /// and [`Error::Keeper`] when the exchange breaks.
    pub fn add(&self, request: AddObjective) -> Result<Applied, Error> {
        applied(self.call(Request::Add(request))?)
    }

    /// Objectives matching `filter`.
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn list(&self, filter: ListFilter) -> Result<Vec<Objective>, Error> {
        match self.call(Request::List(filter))? {
            Response::Objectives(objectives) => Ok(objectives),
            other => Err(unexpected(&other)),
        }
    }

    /// One objective.
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn show(&self, alias: Alias) -> Result<Objective, Error> {
        match self.call(Request::Show { alias })? {
            Response::Objective(objective) => Ok(objective),
            other => Err(unexpected(&other)),
        }
    }

    /// Change a title or description.
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn edit(&self, request: EditObjective) -> Result<Applied, Error> {
        applied(self.call(Request::Edit(request))?)
    }

    /// Archive an objective.
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn archive(&self, request: RevisionEdit) -> Result<Applied, Error> {
        applied(self.call(Request::Archive(request))?)
    }

    /// Restore an archived objective.
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn restore(&self, request: RevisionEdit) -> Result<Applied, Error> {
        applied(self.call(Request::Restore(request))?)
    }

    /// Write an objective's state.
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn record_state(&self, request: StateEdit) -> Result<Applied, Error> {
        applied(self.call(Request::RecordState(request))?)
    }

    /// Record a signal on an objective.
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn raise_signal(&self, request: RaiseSignal) -> Result<Applied, Error> {
        applied(self.call(Request::RaiseSignal(request))?)
    }

    /// An objective's signals.
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn signals(&self, alias: Alias) -> Result<Vec<Signal>, Error> {
        match self.call(Request::Signals { alias })? {
            Response::Signals(signals) => Ok(signals),
            other => Err(unexpected(&other)),
        }
    }

    /// Whether `request` would launch or queue. Does not write.
    ///
    /// # Errors
    ///
    /// As [`Client::add`], plus the start refusals.
    pub fn plan_start(&self, request: &StartRequest) -> Result<StartPlan, Error> {
        match self.call(Request::PlanStart(request.clone()))? {
            Response::StartPlan(plan) => Ok(plan),
            other => Err(unexpected(&other)),
        }
    }

    /// The stored start when `request` repeats an admitted operation.
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn replay_start(&self, request: &StartRequest) -> Result<Option<StartApplied>, Error> {
        match self.call(Request::ReplayStart(request.clone()))? {
            Response::StartReplay(applied) => Ok(applied),
            other => Err(unexpected(&other)),
        }
    }

    /// Insert the run and, when `request.launch` is set, its binding.
    ///
    /// # Errors
    ///
    /// As [`Client::add`], plus [`Error::AtCapacity`].
    pub fn commit_start(&self, request: StartRequest) -> Result<StartApplied, Error> {
        match self.call(Request::CommitStart(request))? {
            Response::StartApplied(applied) => Ok(applied),
            other => Err(unexpected(&other)),
        }
    }

    /// Live runs, oldest first.
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn open_runs(&self) -> Result<Vec<OpenRun>, Error> {
        match self.call(Request::OpenRuns)? {
            Response::OpenRuns(runs) => Ok(runs),
            other => Err(unexpected(&other)),
        }
    }

    /// Admit a queued run. See [`crate::Store::launch_queued`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`], plus [`Error::AtCapacity`].
    pub fn launch_queued(&self, request: LaunchQueued) -> Result<StartApplied, Error> {
        match self.call(Request::LaunchQueued(request))? {
            Response::StartApplied(applied) => Ok(applied),
            other => Err(unexpected(&other)),
        }
    }

    /// The stored promotion when `operation_id` was already admitted.
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn replay_launch(
        &self,
        operation_id: crate::id::OperationId,
    ) -> Result<Option<StartApplied>, Error> {
        match self.call(Request::ReplayLaunch { operation_id })? {
            Response::StartReplay(applied) => Ok(applied),
            other => Err(unexpected(&other)),
        }
    }

    /// Send a message. See [`crate::Store::send_mail`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn send_mail(&self, request: SendMail) -> Result<Sent, Error> {
        match self.call(Request::SendMail(request))? {
            Response::Sent(sent) => Ok(sent),
            other => Err(unexpected(&other)),
        }
    }

    /// A reader's mail. See [`crate::Store::read_mail`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn read_mail(&self, request: ReadMail) -> Result<Vec<Mail>, Error> {
        match self.call(Request::ReadMail(request))? {
            Response::Mail(mail) => Ok(mail),
            other => Err(unexpected(&other)),
        }
    }

    /// Acknowledge a message. See [`crate::Store::ack_mail`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`], plus [`Error::NoMail`].
    pub fn ack_mail(&self, request: AckMail) -> Result<Mail, Error> {
        match self.call(Request::AckMail(request))? {
            Response::Acked(mail) => Ok(mail),
            other => Err(unexpected(&other)),
        }
    }

    /// Take the reader's undelivered mail, waiting up to the request's
    /// timeout for some to arrive. The keeper holds the request open; an
    /// empty result means the deadline passed. See [`crate::Store::take_mail`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn wait_mail(&self, request: WaitMail) -> Result<Vec<Mail>, Error> {
        let reply = Duration::from_millis(request.timeout_ms).saturating_add(REPLY_TIMEOUT);
        match self.call_within(Request::WaitMail(request), reply)? {
            Response::Mail(mail) => Ok(mail),
            other => Err(unexpected(&other)),
        }
    }

    /// Acquire a new lease. See [`crate::Store::take_claim`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn take_claim(&self, request: TakeClaim) -> Result<ClaimOutcome, Error> {
        match self.call(Request::TakeClaim(request))? {
            Response::ClaimOutcome(outcome) => Ok(outcome),
            other => Err(unexpected(&other)),
        }
    }

    /// Renew a live lease. See [`crate::Store::renew_claim`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`], or [`Error::NoClaim`] for a stale receipt.
    pub fn renew_claim(&self, request: RenewClaim) -> Result<Vec<Claim>, Error> {
        match self.call(Request::RenewClaim(request))? {
            Response::Claims(claims) => Ok(claims),
            other => Err(unexpected(&other)),
        }
    }

    /// Release claims. See [`crate::Store::release_claim`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn release_claim(&self, request: ReleaseClaim) -> Result<u64, Error> {
        match self.call(Request::ReleaseClaim(request))? {
            Response::Released(count) => Ok(count),
            other => Err(unexpected(&other)),
        }
    }

    /// Live claims. See [`crate::Store::claims`].
    ///
    /// # Errors
    ///
    /// As [`Client::add`].
    pub fn claims(&self, filter: ClaimFilter) -> Result<Vec<Claim>, Error> {
        match self.call(Request::Claims(filter))? {
            Response::Claims(claims) => Ok(claims),
            other => Err(unexpected(&other)),
        }
    }

    /// A legacy parser can reject a new operation before checking compatibility.
    /// Probe it once with the stable, read-only v1 List shape. With a different
    /// compatibility stamp it retires before dispatching even that operation.
    /// Only an explicit Stale response permits replacement; compatible parser
    /// errors are returned unchanged and never retried as mutations.
    fn compatible_exchange(
        &self,
        stream: &UnixStream,
        line: &str,
        allow_upgrade: bool,
        reply: Duration,
    ) -> Result<Option<Response>, Error> {
        let response = exchange(stream, line, reply)?;
        let legacy_rejection = matches!(
            &response,
            Response::Fault(Fault::BadRequest { keeper: None, .. })
        );
        if !allow_upgrade || !legacy_rejection {
            return classify_reply(response, allow_upgrade);
        }
        let probe = encode_request(Request::List(ListFilter {
            repo: None,
            archived: false,
        }))?;
        let probe_stream = UnixStream::connect(&self.endpoint.socket)
            .map_err(|err| Error::Keeper(format!("legacy compatibility probe: {err}")))?;
        match exchange(&probe_stream, &probe, REPLY_TIMEOUT)? {
            stale @ Response::Fault(Fault::Stale { .. }) => classify_reply(stale, allow_upgrade),
            _ => classify_reply(response, allow_upgrade),
        }
    }

    fn start_if_needed(&self, keeper: &mut Option<Child>, spawns: &mut u32) -> Result<(), Error> {
        let launcher = self.launcher.as_ref().ok_or_else(|| {
            Error::Keeper(format!("no keeper at {}", self.endpoint.socket.display()))
        })?;
        // Only the exact Child we started is considered in-flight. This is not
        // an ownership check or a reason to kill another process.
        let starting = keeper
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)));
        if !starting && *spawns < MAX_SPAWNS {
            *keeper = Some(launcher.spawn()?);
            *spawns += 1;
        }
        Ok(())
    }

    fn call(&self, request: Request) -> Result<Response, Error> {
        self.call_within(request, REPLY_TIMEOUT)
    }

    /// One request, whose reply may take up to `reply` once connected.
    fn call_within(&self, request: Request, reply: Duration) -> Result<Response, Error> {
        let line = encode_request(request)?;
        let started = Instant::now();
        let mut pause = PAUSE_START;
        let mut keeper: Option<Child> = None;
        let mut spawns = 0;
        let mut replaced_stale = false;
        loop {
            match UnixStream::connect(&self.endpoint.socket) {
                Ok(stream) => {
                    match self.compatible_exchange(&stream, &line, !replaced_stale, reply)? {
                        // That keeper exits after saying so; start this build's.
                        None => {
                            replaced_stale = true;
                            keeper = None;
                        }
                        Some(response) => return Ok(response),
                    }
                }
                Err(err) if socket_unavailable(&err) => {
                    self.start_if_needed(&mut keeper, &mut spawns)?;
                }
                Err(err) => {
                    return Err(Error::Keeper(format!(
                        "connect {}: {err}",
                        self.endpoint.socket.display()
                    )));
                }
            }
            let elapsed = started.elapsed();
            if elapsed >= CONNECT_BUDGET {
                return Err(Error::Busy);
            }
            std::thread::sleep(pause.min(CONNECT_BUDGET.saturating_sub(elapsed)));
            pause = (pause * 2).min(PAUSE_CAP);
        }
    }
}

/// None is an explicit incompatibility acknowledgement, never a missing reply.
/// Transport failures remain errors; no extra allocation or owner state is needed.
fn classify_reply(response: Response, allow_upgrade: bool) -> Result<Option<Response>, Error> {
    match response {
        Response::Fault(Fault::Stale { .. }) if allow_upgrade => Ok(None),
        Response::Fault(fault) => Err(fault.into()),
        response => Ok(Some(response)),
    }
}

/// A transport condition only; the store lock still decides writer ownership.
fn socket_unavailable(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

fn encode_request(request: Request) -> Result<String, Error> {
    let mut line = serde_json::to_string(&Envelope {
        build: BUILD.to_owned(),
        request,
    })?;
    line.push('\n');
    Ok(line)
}

fn exchange(stream: &UnixStream, line: &str, reply: Duration) -> Result<Response, Error> {
    let broken = |err: std::io::Error| Error::Keeper(format!("exchange failed: {err}"));
    stream.set_read_timeout(Some(reply)).map_err(broken)?;
    stream
        .set_write_timeout(Some(REPLY_TIMEOUT))
        .map_err(broken)?;
    let mut writer = stream;
    writer.write_all(line.as_bytes()).map_err(broken)?;
    let mut reply = String::new();
    BufReader::new(stream.take(MAX_LINE))
        .read_line(&mut reply)
        .map_err(broken)?;
    if reply.is_empty() {
        return Err(Error::Keeper("keeper closed the connection".to_owned()));
    }
    serde_json::from_str(&reply).map_err(|err| Error::Keeper(format!("unreadable reply: {err}")))
}

fn applied(response: Response) -> Result<Applied, Error> {
    match response {
        Response::Applied(applied) => Ok(applied),
        other => Err(unexpected(&other)),
    }
}

fn unexpected(response: &Response) -> Error {
    Error::Keeper(format!("unexpected reply: {response:?}"))
}

#[cfg(test)]
mod compatibility_tests {
    #![allow(clippy::unwrap_used, reason = "test assertions")]
    use super::*;

    #[test]
    fn versioned_errors_and_a_used_upgrade_budget_never_trigger_a_probe() {
        let dir = tempfile::tempdir().unwrap();
        let client = Client::new(Endpoint::new(dir.path(), dir.path()), None);
        for (keeper, allow_upgrade) in [(Some(BUILD.to_owned()), true), (None, false)] {
            let (stream, mut peer) = UnixStream::pair().unwrap();
            let fault = Response::Fault(Fault::BadRequest {
                message: "invalid payload".into(),
                keeper,
            });
            let reply = serde_json::to_string(&fault).unwrap() + "\n";
            std::io::Write::write_all(&mut peer, reply.as_bytes()).unwrap();
            let line = encode_request(Request::List(ListFilter {
                repo: None,
                archived: false,
            }))
            .unwrap();
            // The endpoint has no listener. Any compatibility probe would fail,
            // rather than returning this unchanged parser error.
            let response = client.compatible_exchange(&stream, &line, allow_upgrade, REPLY_TIMEOUT);
            assert!(
                matches!(response, Err(Error::Keeper(message)) if message == "invalid payload")
            );
        }
    }
}
