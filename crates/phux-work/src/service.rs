//! The keeper: the one process that opens a host's work store.
//!
//! The keeper takes the store lock, binds the store's socket, and answers
//! one request per connection, in arrival order. Holding the lock is what
//! makes it the only writer; clients never open the store themselves. It
//! exits after `idle_exit` without a request, when a request comes from a
//! different build, and after reporting a store that would not open.

use std::fs;
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec};

use crate::error::Error;
use crate::lock;
use crate::store::{Store, lock_path, store_path};
use crate::wire::{BUILD, Envelope, Fault, MAX_LINE, Request, Response};

/// How long one connection may take to send its request or read its reply.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Where a host's store and its keeper's socket live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The SQLite file.
    pub store: PathBuf,
    /// The keeper's Unix socket.
    pub socket: PathBuf,
}

impl Endpoint {
    /// The store under `state_dir` and a socket under `runtime_dir` named
    /// for that store, so two state directories never share a keeper.
    #[must_use]
    pub fn new(state_dir: &Path, runtime_dir: &Path) -> Self {
        let store = store_path(state_dir);
        let socket = runtime_dir.join(format!(
            "work-{:016x}.sock",
            fnv1a(store.as_os_str().as_encoded_bytes())
        ));
        Self { store, socket }
    }
}

/// Why [`run_keeper`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeeperExit {
    /// No request arrived for the idle period.
    Idle,
    /// Another keeper holds the store.
    AlreadyRunning,
    /// A request came from another build; that build starts its own keeper.
    Stale,
    /// The store would not open. The first caller got the error.
    StoreFailed,
}

/// Serve `endpoint` until one of the [`KeeperExit`] conditions.
///
/// # Errors
///
/// The lock, socket, or poll failed before or while serving.
pub fn run_keeper(endpoint: &Endpoint, idle_exit: Duration) -> Result<KeeperExit, Error> {
    if let Some(parent) = nonempty_parent(&endpoint.store) {
        fs::create_dir_all(parent)?;
        lock::set_private_dir(parent)?;
    }
    let _lock = match lock::acquire(&lock_path(&endpoint.store), false) {
        Ok(lock) => lock,
        Err(Error::Busy) => return Ok(KeeperExit::AlreadyRunning),
        Err(err) => return Err(err),
    };
    if let Some(parent) = nonempty_parent(&endpoint.socket) {
        fs::create_dir_all(parent)?;
    }
    // The lock means no other keeper serves this store, so a socket here is
    // left over from one that died.
    match fs::remove_file(&endpoint.socket) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    let listener = UnixListener::bind(&endpoint.socket)?;
    let _socket = RemoveOnDrop(&endpoint.socket);
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&endpoint.socket, fs::Permissions::from_mode(0o600))?;
    }
    let mut store = match Store::open_held(&endpoint.store) {
        Ok(store) => store,
        Err(err) => {
            if let Some(stream) = next_connection(&listener, idle_exit)? {
                let _ = answer(&stream, |_| Response::Fault(Fault::from(err)));
            }
            return Ok(KeeperExit::StoreFailed);
        }
    };
    loop {
        let Some(stream) = next_connection(&listener, idle_exit)? else {
            return Ok(KeeperExit::Idle);
        };
        // A client that hangs up or times out costs only its own request.
        if matches!(
            answer(&stream, |request| handle(&mut store, request)),
            Ok(Served::Stale)
        ) {
            return Ok(KeeperExit::Stale);
        }
    }
}

enum Served {
    Answered,
    Stale,
}

/// Read one request, reply with `respond(request)`, and report whether the
/// caller was from another build.
fn answer(stream: &UnixStream, respond: impl FnOnce(Request) -> Response) -> io::Result<Served> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut line = String::new();
    BufReader::new(stream.take(MAX_LINE)).read_line(&mut line)?;
    let (response, served) = match serde_json::from_str::<Envelope>(&line) {
        Ok(envelope) if envelope.build != BUILD => (
            Response::Fault(Fault::Stale {
                keeper: BUILD.to_owned(),
            }),
            Served::Stale,
        ),
        Ok(envelope) => (respond(envelope.request), Served::Answered),
        Err(err) => (
            Response::Fault(Fault::BadRequest {
                message: format!("unreadable request: {err}"),
            }),
            Served::Answered,
        ),
    };
    let mut reply = serde_json::to_string(&response).map_err(io::Error::other)?;
    reply.push('\n');
    let mut writer = stream;
    writer.write_all(reply.as_bytes())?;
    Ok(served)
}

fn handle(store: &mut Store, request: Request) -> Response {
    let result = match request {
        Request::Add(request) => store.add(request).map(Response::Applied),
        Request::List(filter) => store.list(&filter).map(Response::Objectives),
        Request::Show { alias } => store.show(&alias).map(Response::Objective),
        Request::Edit(request) => store.edit(request).map(Response::Applied),
        Request::Archive(request) => store.archive(request).map(Response::Applied),
        Request::Restore(request) => store.restore(request).map(Response::Applied),
        Request::RecordState(request) => store.record_state(request).map(Response::Applied),
        Request::RaiseSignal(request) => store.raise_signal(request).map(Response::Applied),
        Request::Signals { alias } => store.signals(&alias).map(Response::Signals),
        Request::PlanStart(request) => store.plan_start(&request).map(Response::StartPlan),
        Request::ReplayStart(request) => store.replay_start(&request).map(Response::StartReplay),
        Request::CommitStart(request) => store.commit_start(request).map(Response::StartApplied),
        Request::SendMail(request) => store.send_mail(&request).map(Response::Sent),
        Request::ReadMail(request) => store.read_mail(&request).map(Response::Mail),
        Request::AckMail(request) => store.ack_mail(&request).map(Response::Acked),
        Request::TakeClaim(request) => store.take_claim(&request).map(Response::ClaimOutcome),
        Request::ReleaseClaim(request) => store.release_claim(&request).map(Response::Released),
        Request::Claims(filter) => store.claims(&filter).map(Response::Claims),
    };
    result.unwrap_or_else(|err| Response::Fault(err.into()))
}

/// The next connection, or `None` once `idle` passes without one.
fn next_connection(listener: &UnixListener, idle: Duration) -> Result<Option<UnixStream>, Error> {
    let started = Instant::now();
    loop {
        let remaining = idle.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Ok(None);
        }
        let timeout = Timespec {
            tv_sec: i64::try_from(remaining.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(remaining.subsec_nanos()),
        };
        let mut fds = [PollFd::new(listener, PollFlags::IN)];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) | Err(rustix::io::Errno::INTR) => continue,
            Ok(_) => {}
            Err(err) => return Err(io::Error::from(err).into()),
        }
        match listener.accept() {
            Ok((stream, _)) => return Ok(Some(stream)),
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(err) => return Err(err.into()),
        }
    }
}

struct RemoveOnDrop<'a>(&'a Path);

impl Drop for RemoveOnDrop<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
    }
}

fn nonempty_parent(path: &Path) -> Option<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

/// FNV-1a, 64-bit. Stable across builds, unlike the std hasher, so every
/// build names the same socket for the same store.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
