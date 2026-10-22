//! The keeper: the one process that opens a host's work store.
//!
//! The keeper takes the store lock, binds the store's socket, and answers
//! one request per connection, in arrival order. Holding the lock is what
//! makes it the only writer; clients never open the store themselves. A
//! request waiting for mail is parked, not answered, until mail for its
//! reader is sent, its deadline passes, or its client hangs up. It
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

mod waiters;
use waiters::Waiters;

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
/// `on_serving` runs once the socket is bound and the store is open, before
/// the accept loop. The keeper stays up past `idle_exit` while any objective
/// is `working` or `queued`.
///
/// # Errors
///
/// The lock, socket, or poll failed before or while serving.
pub fn run_keeper(endpoint: &Endpoint, idle_exit: Duration) -> Result<KeeperExit, Error> {
    run_keeper_with(endpoint, idle_exit, || {})
}

/// [`run_keeper`] with a hook invoked once this process is the keeper.
///
/// # Errors
///
/// The lock, socket, or poll failed before or while serving.
pub fn run_keeper_with(
    endpoint: &Endpoint,
    idle_exit: Duration,
    on_serving: impl FnOnce() + Send,
) -> Result<KeeperExit, Error> {
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
            if let Event::Connection(stream) =
                next_event(&listener, &Waiters::default(), idle_exit)?
            {
                refuse(&stream, Fault::from(err));
            }
            return Ok(KeeperExit::StoreFailed);
        }
    };
    let _pid = write_pid(endpoint);
    on_serving();
    let mut waiters = Waiters::default();
    loop {
        match next_event(&listener, &waiters, idle_exit)? {
            Event::Connection(stream) => {
                if serve(&stream, &mut store, &mut waiters) == Served::Stale {
                    return Ok(KeeperExit::Stale);
                }
            }
            Event::HungUp(gone) => waiters.forget(&gone),
            Event::Deadline => waiters.expire(Instant::now()),
            Event::Idle => {
                if store.has_open_work().unwrap_or(true) {
                    continue;
                }
                return Ok(KeeperExit::Idle);
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Served {
    Answered,
    Stale,
}

/// Answer one connection, or park it when it waits for mail. A client that
/// hangs up or times out costs only its own request.
fn serve(stream: &UnixStream, store: &mut Store, waiters: &mut Waiters) -> Served {
    let Ok(line) = read_request(stream) else {
        return Served::Answered;
    };
    let request = match decode(&line) {
        Decoded::Request(request) => request,
        Decoded::Rejected(response, served) => {
            let _ = reply(stream, &response);
            return served;
        }
    };
    match request {
        Request::WaitMail(wait) => waiters.admit(stream, wait, store),
        // Both can leave mail to take: a new message, or a released take.
        request @ (Request::SendMail(_) | Request::ReleaseMail(_)) => {
            let _ = reply(stream, &handle(store, request));
            waiters.wake(store);
        }
        other => {
            let _ = reply(stream, &handle(store, other));
        }
    }
    Served::Answered
}

fn read_request(stream: &UnixStream) -> io::Result<String> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut line = String::new();
    BufReader::new(stream.take(MAX_LINE)).read_line(&mut line)?;
    Ok(line)
}

/// Write one response line.
pub(super) fn reply(stream: &UnixStream, response: &Response) -> io::Result<()> {
    let mut line = serde_json::to_string(response).map_err(io::Error::other)?;
    line.push('\n');
    let mut writer = stream;
    writer.write_all(line.as_bytes())
}

/// Compatibility is decided from the envelope before interpreting a versioned
/// operation. An incompatible payload cannot prevent graceful retirement.
#[derive(serde::Deserialize)]
struct CompatibilityHeader {
    build: String,
}

/// A request line, read.
enum Decoded {
    Request(Request),
    /// The reply it gets instead, and whether the caller was from another build.
    Rejected(Response, Served),
}

fn decode(line: &str) -> Decoded {
    // Ignore the payload only for this header pass. Decode the original bytes
    // afterward: materializing Value first would erase duplicate-field errors.
    let envelope = match serde_json::from_str::<CompatibilityHeader>(line) {
        Ok(envelope) => envelope,
        Err(err) => return bad_request(&err),
    };
    if envelope.build != BUILD {
        return Decoded::Rejected(
            Response::Fault(Fault::Stale {
                keeper: BUILD.to_owned(),
            }),
            Served::Stale,
        );
    }
    match serde_json::from_str::<Envelope>(line) {
        Ok(envelope) => Decoded::Request(envelope.request),
        Err(err) => bad_request(&err),
    }
}

fn bad_request(err: &serde_json::Error) -> Decoded {
    Decoded::Rejected(
        Response::Fault(Fault::BadRequest {
            message: format!("unreadable request: {err}"),
            keeper: Some(BUILD.to_owned()),
        }),
        Served::Answered,
    )
}

/// Answer the first caller with the store's failure, or with the reason its
/// request cannot be read.
fn refuse(stream: &UnixStream, fault: Fault) {
    let response = match read_request(stream).map(|line| decode(&line)) {
        Ok(Decoded::Rejected(response, _)) => response,
        Ok(Decoded::Request(_)) | Err(_) => Response::Fault(fault),
    };
    let _ = reply(stream, &response);
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
        Request::OpenRuns => store.open_runs().map(Response::OpenRuns),
        Request::LaunchQueued(request) => store.launch_queued(request).map(Response::StartApplied),
        Request::ReplayLaunch { operation_id } => store
            .replay_launch(&operation_id)
            .map(Response::StartReplay),
        Request::SendMail(request) => store.send_mail(&request).map(Response::Sent),
        Request::ReadMail(request) => store.read_mail(&request).map(Response::Mail),
        Request::AckMail(request) => store.ack_mail(&request).map(Response::Acked),
        Request::WaitMail(request) => store.take_mail(&request).map(Response::Mail),
        Request::ReleaseMail(request) => store.release_mail(&request).map(Response::Released),
        Request::TakeClaim(request) => store.take_claim(&request).map(Response::ClaimOutcome),
        Request::RenewClaim(request) => store.renew_claim(&request).map(Response::Claims),
        Request::ReleaseClaim(request) => store.release_claim(&request).map(Response::Released),
        Request::Claims(filter) => store.claims(&filter).map(Response::Claims),
    };
    result.unwrap_or_else(|err| Response::Fault(err.into()))
}

/// What woke the keeper.
enum Event {
    Connection(UnixStream),
    /// Parked waiters, by index, whose clients hung up.
    HungUp(Vec<usize>),
    /// A parked waiter's deadline passed.
    Deadline,
    /// `idle` passed with nothing parked and no connection.
    Idle,
}

/// Block until a connection, a parked client hanging up, the nearest
/// waiter deadline, or, with nothing parked, `idle`.
fn next_event(listener: &UnixListener, waiters: &Waiters, idle: Duration) -> Result<Event, Error> {
    let until = waiters
        .next_deadline()
        .unwrap_or_else(|| Instant::now() + idle);
    loop {
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(if waiters.is_empty() {
                Event::Idle
            } else {
                Event::Deadline
            });
        }
        let mut fds: Vec<PollFd<'_>> = std::iter::once(PollFd::new(listener, PollFlags::IN))
            .chain(
                waiters
                    .streams()
                    .map(|stream| PollFd::new(stream, PollFlags::IN)),
            )
            .collect();
        match rustix::event::poll(&mut fds, Some(&timespec(remaining))) {
            Ok(0) | Err(rustix::io::Errno::INTR) => continue,
            Ok(_) => {}
            Err(err) => return Err(io::Error::from(err).into()),
        }
        let gone: Vec<usize> = fds[1..]
            .iter()
            .enumerate()
            .filter(|(_, fd)| !fd.revents().is_empty())
            .map(|(index, _)| index)
            .collect();
        if !gone.is_empty() {
            return Ok(Event::HungUp(gone));
        }
        match listener.accept() {
            Ok((stream, _)) => return Ok(Event::Connection(stream)),
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(err) => return Err(err.into()),
        }
    }
}

fn timespec(duration: Duration) -> Timespec {
    Timespec {
        tv_sec: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(duration.subsec_nanos()),
    }
}

struct PidFile(PathBuf);

impl Drop for PidFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn write_pid(endpoint: &Endpoint) -> Option<PidFile> {
    let path = endpoint.store.parent()?.join("keeper.pid");
    let pid = std::process::id().to_string();
    fs::write(&path, pid).ok()?;
    Some(PidFile(path))
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

#[cfg(test)]
mod compatibility_tests {
    use super::*;

    #[test]
    fn matching_version_duplicate_fields_are_rejected_before_dispatch() {
        let line = format!(
            r#"{{"build":"{BUILD}","request":{{"op":"list","repo":null,"archived":false,"archived":true}}}}"#
        );
        let Decoded::Rejected(response, served) = decode(&line) else {
            panic!("duplicate fields reached dispatch");
        };
        assert_eq!(served, Served::Answered);
        assert!(
            matches!(response, Response::Fault(Fault::BadRequest { keeper: Some(keeper), .. }) if keeper == BUILD)
        );
    }

    #[test]
    fn matching_version_parser_errors_identify_the_keeper_and_do_not_dispatch() {
        let line =
            serde_json::json!({"build": BUILD, "request": {"op": "unknown_operation"}}).to_string();
        let Decoded::Rejected(response, served) = decode(&line) else {
            panic!("invalid payload dispatched");
        };
        assert_eq!(served, Served::Answered);
        assert!(
            matches!(response, Response::Fault(Fault::BadRequest { keeper: Some(keeper), .. }) if keeper == BUILD)
        );
    }
}
