//! One link's supervisor and its relay session: dial, negotiate, serve, reconnect.

use super::{
    AbortOnDrop, Arc, BACKOFF_BASE, BACKOFF_CAP, Backoff, CancellationToken, Cell, DialSpec,
    Duration, FramingError, HubEntry, HubLinkStatuses, LINK_KEEPALIVE_INTERVAL, LINK_SEND_TIMEOUT,
    LINK_WRITE_QUEUE, LinkConn, LinkReader, LinkStatus, LinkTransport, LinkWrite, QueuedLinkWrite,
    RELAY_DELIVERY_RETRY_INTERVAL, Rc, SatelliteHost, drive_link_writer, encode_frame_too_large,
    info, plan_link, queue_link_write, read_link_token, warn,
};

/// Supervise one satellite link: plan, dial, relay, redial, until `cancel`
/// or every relay handle is dropped. A refused link still drains its
/// mailbox with typed errors.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: hub link supervisors run on the server's LocalSet; the transport seam is generic so tests can inject !Send scripted transports"
)]
pub(crate) async fn run_link<T: LinkTransport>(
    host: SatelliteHost,
    entry: HubEntry,
    transport: T,
    statuses: HubLinkStatuses,
    mailbox: crate::hub::relay::RelayMailbox,
    cancel: CancellationToken,
) {
    let crate::hub::relay::RelayMailbox {
        requests: relay_rx,
        unsubscribes: unsub_rx,
        journal,
        operations,
        resource_kinds,
    } = mailbox;
    let mut inbox = LinkInbox {
        host: &host,
        relay_rx,
        unsub_rx,
        cancel: &cancel,
    };
    let spec = match plan_link(&entry) {
        Ok(spec) => spec,
        Err(refusal) => return refuse_link(&statuses, &refusal, &mut inbox).await,
    };

    let mut backoff = Backoff::new(BACKOFF_BASE, BACKOFF_CAP);
    loop {
        let attempt = backoff.failures().saturating_add(1);
        let inbound = matches!(spec, DialSpec::Inbound);
        statuses.set(
            &host,
            if inbound {
                LinkStatus::Awaiting
            } else {
                LinkStatus::Connecting { attempt }
            },
        );
        let connect = async {
            // Re-read the token every attempt (rotation).
            let token = read_link_token(spec.token_file())?;
            transport.connect(&host, &spec, token).await
        };
        // Fail relay requests fast while the dial is in flight.
        let Some(outcome) = inbox.drain_until(connect, "link is connecting").await else {
            return;
        };

        let (failed_attempt, last_error) = match outcome {
            Ok(conn) => {
                info!(satellite = %host, target = %spec, "hub link established");
                statuses.set(&host, LinkStatus::Connected);
                let connected_at = tokio::time::Instant::now();
                let session = run_relay_session(
                    &host,
                    conn,
                    &mut inbox.relay_rx,
                    &mut inbox.unsub_rx,
                    &cancel,
                    journal.as_ref(),
                    &operations,
                    Arc::clone(&resource_kinds),
                );
                match session.await {
                    Some(reason) => {
                        warn!(
                            satellite = %host,
                            reason = %reason,
                            "hub link lost; scheduling redial"
                        );
                        // A fast death is a failed attempt (`settle_after_loss`).
                        (backoff.settle_after_loss(connected_at.elapsed()), reason)
                    }
                    // Cancelled or every handle dropped: supervisor done.
                    None => return,
                }
            }
            Err(error) => {
                warn!(
                    satellite = %host,
                    target = %spec,
                    attempt,
                    error = %error,
                    "hub link attempt failed"
                );
                (attempt, error)
            }
        };

        // An inbound satellite redials on its own backoff; waiting here too
        // would only leave its new connection idle.
        let retry_in = if inbound {
            Duration::ZERO
        } else {
            backoff.next_delay()
        };
        statuses.set(
            &host,
            LinkStatus::Backoff {
                attempt: failed_attempt,
                retry_in,
                last_error,
            },
        );
        let backoff_sleep = tokio::time::sleep(retry_in);
        let slept = inbox
            .drain_until(backoff_sleep, "link is backing off before redial")
            .await;
        if slept.is_none() {
            return;
        }
    }
}

/// A link whose plan was refused is never dialed: publish the refusal,
/// then fail relay requests fast until the supervisor exits.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: runs on the server's LocalSet inside run_link"
)]
pub(super) async fn refuse_link(
    statuses: &HubLinkStatuses,
    refusal: &impl std::fmt::Display,
    inbox: &mut LinkInbox<'_>,
) {
    warn!(
        satellite = %inbox.host,
        refusal = %refusal,
        "hub link refused (fail closed); not dialing"
    );
    statuses.set(
        inbox.host,
        LinkStatus::Refused {
            reason: refusal.to_string(),
        },
    );
    let _never = inbox
        .drain_until(
            std::future::pending::<()>(),
            "link refused (fail closed); fix the registry entry",
        )
        .await;
}

/// A link supervisor's relay mailbox and cancel token: what it serves
/// between connections, and hands to [`run_relay_session`] during one.
pub(super) struct LinkInbox<'a> {
    pub(super) host: &'a SatelliteHost,
    pub(super) relay_rx: tokio::sync::mpsc::Receiver<crate::hub::relay::RelayRequest>,
    pub(super) unsub_rx: tokio::sync::mpsc::UnboundedReceiver<crate::hub::relay::Unsubscribe>,
    pub(super) cancel: &'a CancellationToken,
}

impl LinkInbox<'_> {
    /// Await `until` while no link is up to carry relay traffic: every
    /// relay request fails fast with `why`, and unsubscribes are dropped
    /// (there is no registry to withdraw from). `None` means the supervisor
    /// should exit: `cancel` fired, or every relay or unsubscribe handle was
    /// dropped.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: runs on the server's LocalSet inside run_link"
    )]
    pub(super) async fn drain_until<F: std::future::Future>(
        &mut self,
        until: F,
        why: &str,
    ) -> Option<F::Output> {
        tokio::pin!(until);
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => return None,
                request = self.relay_rx.recv() => {
                    crate::hub::relay::fail_fast(request?, self.host, why);
                }
                unsubscribe = self.unsub_rx.recv() => {
                    let _dropped: crate::hub::relay::Unsubscribe = unsubscribe?;
                }
                output = &mut until => return Some(output),
            }
        }
    }
}

/// Drive one established connection's relay session. `Some(reason)` means
/// the connection was lost (redial); `None` means exit. Session teardown
/// runs on every path.
#[allow(
    clippy::future_not_send,
    clippy::too_many_arguments,
    reason = "ADR-0014: one LocalSet session receives its connection, queues, and incarnation-scoped shared facts"
)]
pub(super) async fn run_relay_session<C: LinkConn>(
    host: &SatelliteHost,
    conn: C,
    relay_rx: &mut tokio::sync::mpsc::Receiver<crate::hub::relay::RelayRequest>,
    unsub_rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::hub::relay::Unsubscribe>,
    cancel: &CancellationToken,
    journal: Option<&crate::state::SharedState>,
    operations: &crate::hub::operation_fence::OperationFence,
    resource_kinds: crate::hub::relay::ResourceKindCatalog,
) -> Option<String> {
    let mut session = match negotiated_relay_session(host, &conn) {
        Ok(session) => session,
        Err(error) => return Some(error),
    };
    session.set_resource_kinds(resource_kinds);
    session.set_journal(journal.cloned());
    // ADR-0127: publish what this satellite advertised for dispatch.
    if let Some(journal) = journal {
        let features = session.satellite_features();
        journal.with_mut(|s| s.set_satellite_features(host.clone(), features));
    }
    session.set_operation_fence(operations.clone());
    let (mut reader, writer) = conn.into_parts();
    let (write_tx, write_rx) = tokio::sync::mpsc::channel(LINK_WRITE_QUEUE);
    let queued_write_bytes = Rc::new(Cell::new(0usize));
    let (write_error_tx, mut write_error_rx) = tokio::sync::mpsc::channel(1);
    let _writer_task = AbortOnDrop(tokio::task::spawn_local(drive_link_writer(
        writer,
        write_rx,
        write_error_tx,
    )));
    // First housekeeping tick one interval out.
    let mut keepalive = tokio::time::interval_at(
        tokio::time::Instant::now() + LINK_KEEPALIVE_INTERVAL,
        LINK_KEEPALIVE_INTERVAL,
    );
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut delivery_retry = tokio::time::interval(RELAY_DELIVERY_RETRY_INTERVAL);
    delivery_retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let (lost, reason) = loop {
        tokio::select! {
            () = cancel.cancelled() => break (false, "hub is shutting down".to_owned()),
            error = write_error_rx.recv() => {
                break (true, error.unwrap_or_else(|| "satellite writer stopped".to_owned()));
            }
            request = relay_rx.recv() => {
                let Some(request) = request else {
                    break (false, "relay handles dropped".to_owned());
                };
                let frames = prepare_relay_request(&mut session, request);
                if let Err(error) = queue_link_write(&write_tx, &queued_write_bytes, LinkWrite::Frames(frames)) {
                    break (true, error);
                }
            }
            unsubscribe = unsub_rx.recv() => {
                // Apply the withdrawal and detach upstream where the last
                // proxy left.
                let Some(unsubscribe) = unsubscribe else {
                    break (false, "relay handles dropped".to_owned());
                };
                let frames = session.handle_unsubscribe(unsubscribe);
                if let Err(error) = queue_link_write(&write_tx, &queued_write_bytes, LinkWrite::Frames(frames)) {
                    break (true, error);
                }
            }
            inbound = reader.recv_frame() => {
                if let Err(error) = handle_relay_inbound(
                    &mut reader,
                    inbound,
                    &mut session,
                    &write_tx,
                    &queued_write_bytes,
                ).await {
                    break (true, error);
                }
            },
            _ = keepalive.tick() => {
                if let Err(error) = maintain_relay_session(&mut session) {
                    break (true, error);
                }
                if let Err(error) = queue_link_write(&write_tx, &queued_write_bytes, LinkWrite::Keepalive) {
                    break (true, error);
                }
            }
            _ = delivery_retry.tick() => {
                session.flush_pending_snapshots();
                let detaches = session.take_delivery_detaches();
                if let Err(error) = queue_link_write(&write_tx, &queued_write_bytes, LinkWrite::Frames(detaches)) {
                    break (true, error);
                }
            }
        }
    };
    drop(write_tx);
    session.teardown(&reason);
    lost.then_some(reason)
}

pub(super) fn negotiated_relay_session<C: LinkConn>(
    host: &SatelliteHost,
    conn: &C,
) -> Result<crate::hub::relay::RelaySession, String> {
    let profile = conn.bootstrap_profile()?;
    let limits = conn.bootstrap_limits()?;
    let features = conn.server_features()?;
    let features_ext = conn.server_features_ext()?;
    info!(satellite = %host, ?profile, "hub relay using negotiated bootstrap profile");
    let mut session =
        crate::hub::relay::RelaySession::new_negotiated(host.clone(), limits, profile, features);
    session.set_satellite_features_ext(features_ext);
    session.set_incarnation(conn.satellite_incarnation());
    Ok(session)
}

#[allow(
    clippy::future_not_send,
    reason = "called only by the LocalSet-bound hub session with its Rc-local bounded writer"
)]
pub(super) async fn handle_relay_inbound<R: LinkReader>(
    reader: &mut R,
    inbound: Result<Option<Vec<u8>>, String>,
    session: &mut crate::hub::relay::RelaySession,
    write_tx: &tokio::sync::mpsc::Sender<QueuedLinkWrite>,
    queued_write_bytes: &Rc<Cell<usize>>,
) -> Result<(), String> {
    match inbound {
        Ok(Some(frame)) => {
            session.handle_inbound(&frame)?;
            queue_link_write(
                write_tx,
                queued_write_bytes,
                LinkWrite::Frames(session.take_delivery_detaches()),
            )
        }
        Ok(None) => Err("connection closed by satellite".to_owned()),
        Err(error) => {
            if let Some(violation) = reader.take_framing_violation() {
                send_framing_goodbye(write_tx, queued_write_bytes, violation).await;
            }
            Err(error)
        }
    }
}

#[allow(
    clippy::future_not_send,
    reason = "called only by the LocalSet-bound hub session with its Rc-local bounded writer"
)]
pub(super) async fn send_framing_goodbye(
    write_tx: &tokio::sync::mpsc::Sender<QueuedLinkWrite>,
    queued_write_bytes: &Rc<Cell<usize>>,
    violation: FramingError,
) {
    let (sent, received) = tokio::sync::oneshot::channel();
    if queue_link_write(
        write_tx,
        queued_write_bytes,
        LinkWrite::Final(encode_frame_too_large(violation), sent),
    )
    .is_ok()
    {
        let _ = tokio::time::timeout(LINK_SEND_TIMEOUT, received).await;
    }
}

/// Bound reply retention and retry slow snapshots before probing the transport.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: runs on the server's LocalSet inside run_relay_session"
)]
pub(super) fn maintain_relay_session(
    session: &mut crate::hub::relay::RelaySession,
) -> Result<(), String> {
    session.check_detach_deadlines()?;
    session.prune_abandoned();
    Ok(())
}

pub(super) fn prepare_relay_request(
    session: &mut crate::hub::relay::RelaySession,
    request: crate::hub::relay::RelayRequest,
) -> Vec<Vec<u8>> {
    let mut frames = session.prepare_request(&request);
    if let Some(frame) = session.handle_request_checked(request) {
        frames.push(frame);
    }
    frames
}
