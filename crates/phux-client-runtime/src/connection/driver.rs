//! Session lifetime, reconnect policy, and the control-plane pump.

use std::pin::Pin;
use std::time::Duration;

use tokio::sync::watch;

use super::io::{Io, dial};
use super::{ConnectOptions, ConnectionEnd, Shared, Signals, Target, Wake, lock};
use crate::control::{ControlError, Status};

enum Decision {
    Stop,
    RetryNow,
    RetryBackoff,
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
    let mut backoff = options.ladder.floor;
    let mut attempts: u32 = 0;
    // The never-attached deadline: armed from the first dial, moot once a
    // handshake lands. It bounds how long a session may spend having never
    // attached, never how long an attached session may run.
    let ladder_deadline = tokio::time::Instant::now() + options.initial_budget;
    loop {
        attempts += 1;
        let Some((end, was_attached)) = run_attempt(
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
        if was_attached {
            backoff = options.ladder.floor;
        }
        match finish_attempt(&shared, end, attempts, options.initial_attempts) {
            Decision::Stop => {
                wake();
                return;
            }
            Decision::RetryNow => wake(),
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
    tokio::select! {
        () = tokio::time::sleep(backoff) => false,
        () = signal(&mut signals.nudge) => false,
        () = signal(&mut signals.resync) => false,
        () = closed(&mut signals.close) => {
            lock(shared).close();
            wake();
            true
        }
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
) -> Option<(ConnectionEnd, bool)> {
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
/// Returns how it ended and whether it was attached at that moment.
async fn run_connection(
    target: &Target,
    options: ConnectOptions,
    shared: &Shared,
    signals: &mut Signals,
    wake: &Wake,
) -> (ConnectionEnd, bool) {
    let name = target.name.as_str();
    let started = std::time::Instant::now();
    tracing::info!(host = name, transport = target.transport.label(), "dialing");
    let mut io = match dial(target, options).await {
        Ok(io) => io,
        Err(end) => return (end, false),
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
    (end, was_attached)
}

struct Pump<'a> {
    name: &'a str,
    io: &'a mut Io,
    options: ConnectOptions,
    shared: &'a Shared,
    signals: &'a mut Signals,
    wake: &'a Wake,
    probe_deadline: Option<Pin<Box<tokio::time::Sleep>>>,
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
        }
    }

    async fn run(&mut self) -> ConnectionEnd {
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
        Ok(())
    }

    async fn step(&mut self) -> Result<(), ConnectionEnd> {
        let expiry = self.expiry_wait();
        tokio::select! {
            () = self.signals.outbound.notified() => self.flush_outbound().await,
            () = closed(&mut self.signals.close) => Err(ConnectionEnd::Closed),
            () = signal(&mut self.signals.resync) => Err(ConnectionEnd::Resync),
            () = signal(&mut self.signals.nudge) => self.start_probe().await,
            () = wait_for_probe(&mut self.probe_deadline), if self.probe_deadline.is_some() => {
                Err(ConnectionEnd::Dropped(Some("liveness probe timed out".to_owned())))
            }
            () = tokio::time::sleep(expiry) => self.expire_inputs().await,
            inbound = self.io.read_frame(self.name) => self.accept_inbound(inbound).await,
        }
    }

    fn expiry_wait(&self) -> Duration {
        lock(self.shared)
            .next_input_deadline()
            .map_or_else(crate::control::max_expiry_wait, |deadline| {
                deadline.saturating_duration_since(std::time::Instant::now())
            })
    }

    async fn flush_outbound(&mut self) -> Result<(), ConnectionEnd> {
        let frames = lock(self.shared).take_outbound();
        self.write_all(frames).await
    }

    async fn start_probe(&mut self) -> Result<(), ConnectionEnd> {
        self.io
            .probe(self.name)
            .await
            .map_err(|error| ConnectionEnd::Dropped(Some(error)))?;
        if self.probe_deadline.is_none() {
            self.probe_deadline = Some(Box::pin(tokio::time::sleep(self.options.probe_timeout)));
        }
        Ok(())
    }

    async fn expire_inputs(&mut self) -> Result<(), ConnectionEnd> {
        let (expired, frames) = {
            let mut control = lock(self.shared);
            let expired = control.expire_inputs();
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
        inbound: Result<Option<Vec<u8>>, String>,
    ) -> Result<(), ConnectionEnd> {
        // Any inbound traffic, a pong included, proves liveness.
        self.probe_deadline = None;
        let frame = inbound
            .map_err(|error| ConnectionEnd::Dropped(Some(error)))?
            .ok_or_else(|| {
                ConnectionEnd::Dropped(Some(format!("{} closed the connection", self.name)))
            })?;
        if frame.is_empty() {
            return Ok(());
        }
        let (fed, frames) = {
            let mut control = lock(self.shared);
            let fed = control.feed_bytes(&frame);
            (fed, control.take_outbound())
        };
        self.write_all(frames).await?;
        // Edge-triggered: a frame flood costs one callback, not one per
        // frame, and the callback runs with no lock held.
        (self.wake)();
        match fed {
            Ok(()) => Ok(()),
            // A retired or mismatched generation is visible to the
            // embedder as InvalidState; it must not tear the socket down.
            Err(ControlError::InvalidState(_)) => Ok(()),
            Err(error) => Err(control_error(error)),
        }
    }

    async fn write_all(&mut self, frames: Vec<Vec<u8>>) -> Result<(), ConnectionEnd> {
        for frame in frames {
            self.io
                .write_frame(self.name, &frame)
                .await
                .map_err(|error| ConnectionEnd::Dropped(Some(error)))?;
        }
        Ok(())
    }
}

async fn wait_for_probe(deadline: &mut Option<Pin<Box<tokio::time::Sleep>>>) {
    if let Some(deadline) = deadline.as_mut() {
        deadline.await;
    }
}

fn control_error(error: ControlError) -> ConnectionEnd {
    match error {
        ControlError::Protocol(message) => ConnectionEnd::Dropped(Some(message)),
        ControlError::Refused(message) => ConnectionEnd::Refused(message),
        ControlError::InvalidState(message) => ConnectionEnd::Dropped(Some(message)),
        ControlError::Resync => ConnectionEnd::Resync,
        ControlError::Closed => ConnectionEnd::Closed,
    }
}
