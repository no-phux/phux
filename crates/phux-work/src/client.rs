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

use crate::error::Error;
use crate::id::Alias;
use crate::service::Endpoint;
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

    fn call(&self, request: Request) -> Result<Response, Error> {
        let mut line = serde_json::to_string(&Envelope {
            build: BUILD.to_owned(),
            request,
        })?;
        line.push('\n');
        let started = Instant::now();
        let mut pause = PAUSE_START;
        let mut keeper: Option<Child> = None;
        let mut spawns = 0;
        let mut replaced_stale = false;
        loop {
            match UnixStream::connect(&self.endpoint.socket) {
                Ok(stream) => match exchange(&stream, &line)? {
                    // That keeper exits after saying so; start this build's.
                    Response::Fault(Fault::Stale { .. }) if !replaced_stale => {
                        replaced_stale = true;
                        keeper = None;
                    }
                    Response::Fault(fault) => return Err(fault.into()),
                    response => return Ok(response),
                },
                Err(err)
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    let Some(launcher) = &self.launcher else {
                        return Err(Error::Keeper(format!(
                            "no keeper at {}",
                            self.endpoint.socket.display()
                        )));
                    };
                    // Start one unless the one we started is still coming up.
                    let starting = keeper
                        .as_mut()
                        .is_some_and(|child| matches!(child.try_wait(), Ok(None)));
                    if !starting && spawns < MAX_SPAWNS {
                        keeper = Some(launcher.spawn()?);
                        spawns += 1;
                    }
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

fn exchange(stream: &UnixStream, line: &str) -> Result<Response, Error> {
    let broken = |err: std::io::Error| Error::Keeper(format!("exchange failed: {err}"));
    stream
        .set_read_timeout(Some(REPLY_TIMEOUT))
        .map_err(broken)?;
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
