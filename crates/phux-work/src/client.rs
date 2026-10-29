//! The work API every surface uses: the CLI, Lead, the board, MCP, and later
//! the desktop. A [`Client`] sends each call to the store's keeper and starts
//! one when none answers, or hands it to an [`Exchange`] that carries it to a
//! keeper on another host.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::Error;
use crate::id::Alias;
use crate::service::Endpoint;
use crate::start::{LaunchQueued, OpenRun, StartApplied, StartPlan, StartRequest};
use crate::store::{
    AddObjective, Applied, EditObjective, ListFilter, Objective, RaiseSignal, RevisionEdit, Signal,
    StateEdit,
};
use crate::wire::{BUILD, Envelope, Fault, MAX_LINE, Request, Response};

mod coord;
mod lifeline;
use lifeline::{Patience, await_reply};

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

/// Carries one request line to a keeper on another host and brings back its
/// reply line, such as a satellite's link to its hub ([`crate::federation`]).
pub trait Exchange: std::fmt::Debug + Send + Sync {
    /// Send `line` and return the one reply line, waiting at most `reply`
    /// for it. Once `lifeline` (see [`Client::wait_mail_while`]) closes, give
    /// up so the far keeper takes nothing.
    ///
    /// # Errors
    ///
    /// [`Error::Keeper`] when the exchange breaks or `lifeline` closes first.
    fn exchange(
        &self,
        line: &str,
        reply: Duration,
        lifeline: Option<std::os::fd::BorrowedFd<'_>>,
    ) -> Result<String, Error>;
}

/// A connection-per-call client of one store's keeper.
#[derive(Debug, Clone)]
pub struct Client {
    route: Route,
}

#[derive(Debug, Clone)]
enum Route {
    Local(Local),
    Remote(Arc<dyn Exchange>),
}

/// This host's keeper, at its socket.
#[derive(Debug, Clone)]
struct Local {
    endpoint: Endpoint,
    launcher: Option<Launcher>,
}

impl Client {
    /// Talk to the keeper at `endpoint`, starting one with `launcher` when
    /// none answers. Without a launcher, a missing keeper is an error.
    #[must_use]
    pub const fn new(endpoint: Endpoint, launcher: Option<Launcher>) -> Self {
        Self {
            route: Route::Local(Local { endpoint, launcher }),
        }
    }

    /// Talk to a keeper on another host through `exchange`. That keeper is
    /// never started or replaced from here.
    #[must_use]
    pub fn remote(exchange: Arc<dyn Exchange>) -> Self {
        Self {
            route: Route::Remote(exchange),
        }
    }

    /// Whether calls go to another host's keeper.
    #[must_use]
    pub const fn is_remote(&self) -> bool {
        matches!(self.route, Route::Remote(_))
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

    fn call(&self, request: Request) -> Result<Response, Error> {
        self.call_within(request, prompt())
    }

    /// One request, whose reply may take as long as `patience` allows.
    fn call_within(&self, request: Request, patience: Patience<'_>) -> Result<Response, Error> {
        let line = encode_request(request)?;
        match &self.route {
            Route::Local(local) => local.call_within(&line, patience),
            Route::Remote(remote) => {
                let reply = remote.exchange(&line, patience.reply, patience.lifeline)?;
                let response = serde_json::from_str(&reply)
                    .map_err(|err| Error::Keeper(format!("unreadable reply: {err}")))?;
                classify_reply(response, false)?
                    .ok_or_else(|| Error::Keeper("the far keeper is another build".to_owned()))
            }
        }
    }
}

impl Local {
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
        patience: Patience<'_>,
    ) -> Result<Option<Response>, Error> {
        let response = exchange(stream, line, patience)?;
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
        match exchange(&probe_stream, &probe, prompt())? {
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

    fn call_within(&self, line: &str, patience: Patience<'_>) -> Result<Response, Error> {
        let started = Instant::now();
        let mut pause = PAUSE_START;
        let mut keeper: Option<Child> = None;
        let mut spawns = 0;
        let mut replaced_stale = false;
        loop {
            match UnixStream::connect(&self.endpoint.socket) {
                Ok(stream) => {
                    match self.compatible_exchange(&stream, line, !replaced_stale, patience)? {
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

/// The patience of an ordinary call: a reply within [`REPLY_TIMEOUT`].
const fn prompt() -> Patience<'static> {
    Patience {
        reply: REPLY_TIMEOUT,
        lifeline: None,
    }
}

fn exchange(stream: &UnixStream, line: &str, patience: Patience<'_>) -> Result<Response, Error> {
    let broken = |err: std::io::Error| Error::Keeper(format!("exchange failed: {err}"));
    stream
        .set_read_timeout(Some(patience.reply))
        .map_err(broken)?;
    stream
        .set_write_timeout(Some(REPLY_TIMEOUT))
        .map_err(broken)?;
    let mut writer = stream;
    writer.write_all(line.as_bytes()).map_err(broken)?;
    await_reply(stream, patience)?;
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
mod compatibility_tests;
