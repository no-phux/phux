//! Session lifetime, reconnect policy, and the control-plane pump.

use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use phux_dial::DialError;
use tokio::sync::watch;
use tokio::time::Instant;

use super::establish::dial;
use super::io::{Io, LinkError};
use super::{ConnectOptions, ConnectionEnd, Shared, Signals, Target, Transport, Wake, lock};
use crate::reconnect::{earned_immediate, ladder_for};

use crate::control::{ControlError, InboundDelivery, Status};

mod ping;
use ping::peel_queued_pings;

/// How long an attached session may sit with no inbound bytes before a probe.
/// A busy session never reaches it: any inbound frame pushes the deadline out.
const QUIET_PROBE: Duration = Duration::from_secs(10);

enum Decision {
    Stop,
    RetryNow,
    RetryBackoff,
}

struct Attempt {
    end: ConnectionEnd,
    was_attached: bool,
    elapsed: Duration,
}

fn attempt(end: ConnectionEnd, was_attached: bool, started: std::time::Instant) -> Attempt {
    Attempt {
        end,
        was_attached,
        elapsed: started.elapsed(),
    }
}

/// A local socket inherits the upgrade poll unless the caller chose a ladder.
fn options_for(target: &Target, options: ConnectOptions) -> ConnectOptions {
    let local_socket = matches!(target.transport, Transport::Uds(_) | Transport::Wsl { .. });
    ConnectOptions {
        ladder: ladder_for(local_socket, options.ladder),
        ..options
    }
}

/// Run one session to its end: connect, then reconnect with backoff on
/// drops, until the consumer closes it or a refusal ends it.
pub async fn run_session(
    target: Target,
    options: ConnectOptions,
    shared: Shared,
    mut signals: Signals,
    wake: Wake,
) {
    let options = options_for(&target, options);
    let mut backoff = options.ladder.floor;
    let mut attempts: u32 = 0;
    // The never-attached deadline: armed from the first dial, moot once a
    // handshake lands. It bounds how long a session may spend having never
    // attached, never how long an attached session may run.
    let ladder_deadline = tokio::time::Instant::now() + options.initial_budget;
    loop {
        attempts += 1;
        let Some(tried) = run_attempt(
            &target,
            options,
            &shared,
            &mut signals,
            &wake,
            ladder_deadline,
        )
        .await
        else {
            fail(&shared, None, &wake);
            return;
        };
        // Attached at the moment the connection ended means it was
        // healthy until the drop: the next ladder starts from the floor.
        if tried.was_attached {
            backoff = options.ladder.floor;
        }
        match finish_attempt(&shared, tried.end, attempts, options.initial_attempts) {
            Decision::Stop => {
                wake();
                return;
            }
            Decision::RetryNow => wake(),
            // A settled session redials before sleeping. The floor still
            // applies when that redial never attaches.
            Decision::RetryBackoff if earned_immediate(tried.was_attached, tried.elapsed) => {
                wake();
            }
            Decision::RetryBackoff => {
                wake();
                if wait_to_retry(backoff, &shared, &mut signals, &wake).await {
                    return;
                }
                backoff = options.ladder.next(backoff);
            }
        }
    }
}

async fn wait_to_retry(
    backoff: Duration,
    shared: &Shared,
    signals: &mut Signals,
    wake: &Wake,
) -> bool {
    let retry = tokio::time::sleep(backoff);
    tokio::pin!(retry);
    loop {
        let expiry = expiry_wait(shared);
        tokio::select! {
            () = &mut retry => return false,
            () = signal(&mut signals.nudge) => return false,
            () = signal(&mut signals.resync) => return false,
            // No socket to migrate while waiting: a new path is a reason to
            // dial now rather than finish the backoff the old one earned.
            () = signal(&mut signals.rebind) => return false,
            () = tokio::time::sleep(expiry) => expire_operations(shared, wake),
            () = closed(&mut signals.close) => {
                lock(shared).close();
                wake();
                return true;
            }
        }
    }
}

fn expiry_wait(shared: &Shared) -> Duration {
    lock(shared)
        .next_operation_deadline()
        .map_or_else(crate::control::max_expiry_wait, |deadline| {
            deadline.saturating_duration_since(std::time::Instant::now())
        })
}

fn expire_operations(shared: &Shared, wake: &Wake) {
    if lock(shared).expire_operations() {
        wake();
    }
}

/// One attempt under the never-attached budget. `None` means the budget
/// ran out before any attach.
async fn run_attempt(
    target: &Target,
    options: ConnectOptions,
    shared: &Shared,
    signals: &mut Signals,
    wake: &Wake,
    ladder_deadline: tokio::time::Instant,
) -> Option<Attempt> {
    let connection = run_connection(target, options, shared, signals, wake);
    if lock(shared).attached_once() {
        return Some(connection.await);
    }
    tokio::pin!(connection);
    let budget = tokio::time::sleep_until(ladder_deadline);
    tokio::pin!(budget);
    let mut armed = true;
    loop {
        tokio::select! {
            end = &mut connection => return Some(end),
            () = &mut budget, if armed => {
                if lock(shared).attached_once() {
                    armed = false;
                    continue;
                }
                return None;
            }
        }
    }
}

fn fail(shared: &Shared, message: Option<String>, wake: &Wake) {
    let mut control = lock(shared);
    let message = message
        .or_else(|| control.last_error().map(str::to_owned))
        .unwrap_or_else(|| "connect: no answer within the initial budget".to_owned());
    control.fail(message);
    drop(control);
    wake();
}

fn finish_attempt(
    shared: &Shared,
    end: ConnectionEnd,
    attempts: u32,
    initial_attempts: u32,
) -> Decision {
    let mut control = lock(shared);
    match end {
        ConnectionEnd::Closed => {
            control.close();
            Decision::Stop
        }
        ConnectionEnd::Resync => {
            control.connection_lost(None);
            Decision::RetryNow
        }
        ConnectionEnd::Refused(message) => {
            control.fail(message);
            Decision::Stop
        }
        ConnectionEnd::Dropped(message) => {
            if control.attached_once() || attempts < initial_attempts {
                control.connection_lost(message);
                Decision::RetryBackoff
            } else {
                control.fail(message.unwrap_or_else(|| "server closed".to_owned()));
                Decision::Stop
            }
        }
    }
}

/// Wait for a watch signal; a dropped sender parks forever rather than
/// busy-spinning on the closed-channel error.
async fn signal(rx: &mut watch::Receiver<u64>) {
    if rx.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

async fn closed(rx: &mut watch::Receiver<bool>) {
    if *rx.borrow() {
        return;
    }
    if rx.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// One connection: dial, handshake, then pump frames until it ends.
/// Returns how it ended, whether it was attached, and how long it lasted.
async fn run_connection(
    target: &Target,
    options: ConnectOptions,
    shared: &Shared,
    signals: &mut Signals,
    wake: &Wake,
) -> Attempt {
    let name = target.name.as_str();
    let started = std::time::Instant::now();
    tracing::info!(host = name, transport = target.transport.label(), "dialing");
    // A dial runs to `dial_timeout` against a host that accepts and then says
    // nothing. Racing the close signal is what keeps teardown bounded by the
    // consumer rather than by that timeout: a binding that joins this thread
    // on drop would otherwise block a UI for the whole dial.
    let mut io = tokio::select! {
        dialed = dial(target, options) => match dialed {
            Ok(io) => io,
            Err(end) => return attempt(end, false, started),
        },
        () = closed(&mut signals.close) => {
            lock(shared).close();
            return attempt(ConnectionEnd::Closed, false, started);
        }
    };
    tracing::info!(
        host = name,
        transport = target.transport.label(),
        elapsed_ms = started.elapsed().as_millis(),
        "connected"
    );
    let end = Pump::new(name, &mut io, options, shared, signals, wake)
        .run()
        .await;
    let was_attached = lock(shared).status() == Status::Attached;
    io.close();
    attempt(end, was_attached, started)
}

struct Pump<'a> {
    name: &'a str,
    io: &'a mut Io,
    options: ConnectOptions,
    shared: &'a Shared,
    signals: &'a mut Signals,
    wake: &'a Wake,
    probe_deadline: Option<Pin<Box<tokio::time::Sleep>>>,
    /// When a quiet attached session should send its next liveness probe.
    quiet_probe_at: Instant,
}

impl<'a> Pump<'a> {
    fn new(
        name: &'a str,
        io: &'a mut Io,
        options: ConnectOptions,
        shared: &'a Shared,
        signals: &'a mut Signals,
        wake: &'a Wake,
    ) -> Self {
        Self {
            name,
            io,
            options,
            shared,
            signals,
            wake,
            probe_deadline: None,
            quiet_probe_at: Instant::now() + QUIET_PROBE,
        }
    }

    async fn run(&mut self) -> ConnectionEnd {
        // Writes may wait indefinitely when a peer stops reading. Observe
        // close around the whole pump, including the opening HELLO and the
        // writes inside step's selected branches, so joining the driver on
        // teardown never waits for that peer. Cancellation is safe here:
        // this connection is discarded, never resumed after a partial write.
        let mut close = self.signals.close.clone();
        tokio::select! {
            () = closed(&mut close) => ConnectionEnd::Closed,
            end = self.pump() => end,
        }
    }

    async fn pump(&mut self) -> ConnectionEnd {
        if let Err(end) = self.open().await {
            return end;
        }
        loop {
            if let Err(end) = self.step().await {
                return end;
            }
        }
    }

    async fn open(&mut self) -> Result<(), ConnectionEnd> {
        let opening = {
            let mut control = lock(self.shared);
            control.connection_opened();
            control.take_outbound()
        };
        self.write_all(opening).await?;
        // Signals raised while opening are satisfied by this dial and the
        // snapshots it is about to replay.
        self.signals.resync.mark_unchanged();
        self.signals.nudge.mark_unchanged();
        self.signals.rebind.mark_unchanged();
        Ok(())
    }

    async fn step(&mut self) -> Result<(), ConnectionEnd> {
        let expiry = self.expiry_wait();
        let reading = self.has_inbound_room();
        // Soft residual: an in-flight probe deadline while paused cannot
        // observe its answer (reads are gated), and the backlog already
        // proves the peer is alive. Clear it; do not only skip start_probe.
        if !reading {
            self.probe_deadline = None;
            // The backlog is proof of life. Don't probe the moment reads resume.
            self.note_inbound();
        }
        tokio::select! {
            () = self.signals.outbound.notified() => self.flush_outbound().await,
            () = signal(&mut self.signals.resync) => Err(ConnectionEnd::Resync),
            // A paused read has a backlog the peer just sent: it is alive,
            // and a probe answer could not be read before its deadline.
            () = signal(&mut self.signals.nudge) => if reading { self.start_probe().await } else { Ok(()) },
            () = signal(&mut self.signals.rebind) => self.rebind(reading).await,
            () = wait_for_probe(&mut self.probe_deadline), if self.probe_deadline.is_some() => {
                Err(ConnectionEnd::Dropped(Some("liveness probe timed out".to_owned())))
            }
            () = tokio::time::sleep(expiry) => self.expire_inputs().await,
            inbound = self.io.read_frames(), if reading => self.accept_inbound(inbound).await,
            // Re-checked every loop, so a session that attaches while this
            // select is parked starts its quiet clock on the next wake.
            () = tokio::time::sleep_until(self.quiet_probe_at), if self.quiet_probe_due(reading) => {
                self.arm_quiet_probe().await
            }
        }
    }

    /// An attached session with room to read, and no probe already in flight.
    fn quiet_probe_due(&self, reading: bool) -> bool {
        reading && self.probe_deadline.is_none() && self.session_attached()
    }

    fn session_attached(&self) -> bool {
        lock(self.shared).status() == Status::Attached
    }

    fn note_inbound(&mut self) {
        self.quiet_probe_at = Instant::now() + QUIET_PROBE;
    }

    async fn arm_quiet_probe(&mut self) -> Result<(), ConnectionEnd> {
        self.quiet_probe_at = Instant::now() + QUIET_PROBE;
        self.start_probe().await
    }

    /// Backpressure: a queued-delivery consumer that has fallen behind
    /// pauses reads until it drains, leaving the backlog in the socket.
    fn has_inbound_room(&self) -> bool {
        let control = lock(self.shared);
        control.options().deliver_inbound != InboundDelivery::Queued || control.has_inbound_room()
    }

    fn expiry_wait(&self) -> Duration {
        expiry_wait(self.shared)
    }

    async fn flush_outbound(&mut self) -> Result<(), ConnectionEnd> {
        let frames = lock(self.shared).take_outbound();
        self.write_all(frames).await
    }

    async fn start_probe(&mut self) -> Result<(), ConnectionEnd> {
        self.io
            .probe()
            .await
            .map_err(|error| self.dropped(&error))?;
        if self.probe_deadline.is_none() {
            self.probe_deadline = Some(Box::pin(tokio::time::sleep(self.options.probe_timeout)));
        }
        Ok(())
    }

    /// The path changed: migrate a QUIC connection onto a fresh socket,
    /// then probe, so a path the peer cannot validate drops into the
    /// ladder instead of hanging on a silent socket. A failed rebind is
    /// a drop too: the redial binds afresh, which is the fallback.
    async fn rebind(&mut self, reading: bool) -> Result<(), ConnectionEnd> {
        let migrated = self
            .io
            .rebind(self.name)
            .map_err(|error| ConnectionEnd::Dropped(Some(error)))?;
        if migrated {
            tracing::info!(host = self.name, "migrated to a fresh socket");
        }
        if reading {
            self.start_probe().await
        } else {
            Ok(())
        }
    }

    async fn expire_inputs(&mut self) -> Result<(), ConnectionEnd> {
        let (expired, frames) = {
            let mut control = lock(self.shared);
            let expired = control.expire_operations();
            (expired, control.take_outbound())
        };
        self.write_all(frames).await?;
        if expired {
            (self.wake)();
        }
        Ok(())
    }

    async fn accept_inbound(
        &mut self,
        inbound: Result<Option<Vec<Bytes>>, LinkError>,
    ) -> Result<(), ConnectionEnd> {
        // Any inbound traffic, a pong included, proves liveness.
        self.probe_deadline = None;
        self.note_inbound();
        let mut frames = inbound
            .map_err(|error| self.dropped(&error))?
            .ok_or_else(|| {
                ConnectionEnd::Dropped(Some(format!("{} closed the connection", self.name)))
            })?;
        frames.retain(|frame| !frame.is_empty());
        if frames.is_empty() {
            return Ok(());
        }
        let (fed, outbound, pongs) = {
            let mut control = lock(self.shared);
            // A binding that keeps its own per-frame state on one owning
            // thread takes delivery itself; the socket is still ours.
            let (fed, pongs) = if control.options().deliver_inbound == InboundDelivery::Queued {
                // Answer Ping on the driver before enqueue so a stalled
                // consumer cannot strand liveness replies behind UI drain.
                let (to_queue, pongs) = peel_queued_pings(frames);
                let fed = if to_queue.is_empty() {
                    Ok(())
                } else {
                    control.queue_inbound_batch(to_queue)
                };
                (fed, pongs)
            } else {
                (control.feed_shared_batch(&frames), Vec::new())
            };
            (fed, control.take_outbound(), pongs)
        };
        // Pongs first: the peer's probe must not wait on consumer-queued
        // outbound that may still be draining.
        self.write_all(pongs).await?;
        self.write_all(outbound).await?;
        // Edge-triggered: a frame flood costs one callback, not one per
        // frame, and the callback runs with no lock held.
        (self.wake)();
        // A retired or mismatched generation is visible to the
        // embedder as InvalidState; it must not tear the socket down.
        fed.or_else(|error| match error {
            ControlError::InvalidState(_) => Ok(()),
            error => Err(error),
        })
        .map_err(control_error)
    }

    async fn write_all(&mut self, frames: Vec<Vec<u8>>) -> Result<(), ConnectionEnd> {
        self.io
            .write_frames(&frames)
            .await
            .map_err(|error| self.dropped(&error))
    }

    /// A link failure ends this connection; the ladder decides what next,
    /// except for a refused token, which no redial can satisfy.
    fn dropped(&self, error: &LinkError) -> ConnectionEnd {
        link_end(self.name, error)
    }
}

async fn wait_for_probe(deadline: &mut Option<Pin<Box<tokio::time::Sleep>>>) {
    if let Some(deadline) = deadline.as_mut() {
        deadline.await;
    }
}

/// How a link failure ends the connection: a QUIC close refusing the
/// bearer token is final, everything else walks the ladder.
fn link_end(name: &str, error: &LinkError) -> ConnectionEnd {
    let message = error.describe(name);
    match error {
        LinkError::Closed(DialError::AuthRefused(_)) => ConnectionEnd::Refused(message),
        _ => ConnectionEnd::Dropped(Some(message)),
    }
}

fn control_error(error: ControlError) -> ConnectionEnd {
    match error {
        ControlError::Protocol(message) | ControlError::InvalidState(message) => {
            ConnectionEnd::Dropped(Some(message))
        }
        ControlError::Refused(message) => ConnectionEnd::Refused(message),
        ControlError::Resync => ConnectionEnd::Resync,
        ControlError::Closed => ConnectionEnd::Closed,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests;
