//! Scripted transports, and the write queue's bounds and fan-out under a blocked writer.

use crate::hub::link::session::run_relay_session;

use super::*;

/// Scripted transport: pops the next result per connect call.
#[derive(Clone)]
pub(super) struct ScriptTransport {
    pub(super) script: Rc<RefCell<VecDeque<Result<ScriptConn, String>>>>,
    pub(super) calls: Rc<Cell<u32>>,
    pub(super) seen_tokens: Rc<RefCell<Vec<Option<String>>>>,
}

impl ScriptTransport {
    pub(super) fn new(script: Vec<Result<ScriptConn, String>>) -> Self {
        Self {
            script: Rc::new(RefCell::new(script.into())),
            calls: Rc::new(Cell::new(0)),
            seen_tokens: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

/// A scripted connection that drops with the reason sent on `closed`.
pub(super) struct ScriptConn {
    pub(super) closed: Option<tokio::sync::mpsc::Receiver<String>>,
    pub(super) keepalive_error: Option<String>,
}

pub(super) struct ScriptReader {
    pub(super) closed: Option<tokio::sync::mpsc::Receiver<String>>,
}

pub(super) struct ScriptWriter {
    pub(super) keepalive_error: Option<String>,
}

pub(super) struct BlockedWriteConn {
    pub(super) inbound: tokio::sync::mpsc::Receiver<Vec<u8>>,
    pub(super) write_started: Arc<tokio::sync::Notify>,
    pub(super) release_write: Arc<tokio::sync::Notify>,
    pub(super) writer_dropped: Arc<std::sync::atomic::AtomicBool>,
}

pub(super) struct BlockedWriteReader {
    pub(super) inbound: tokio::sync::mpsc::Receiver<Vec<u8>>,
}

pub(super) struct BlockedWriteWriter {
    pub(super) write_started: Arc<tokio::sync::Notify>,
    pub(super) release_write: Arc<tokio::sync::Notify>,
    pub(super) dropped: Arc<std::sync::atomic::AtomicBool>,
}

impl ScriptConn {
    /// A connection that stays up for the whole test.
    pub(super) const fn open_forever() -> Self {
        Self {
            closed: None,
            keepalive_error: None,
        }
    }

    /// A connection the test can drop by sending a reason.
    pub(super) fn closable() -> (tokio::sync::mpsc::Sender<String>, Self) {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        (
            tx,
            Self {
                closed: Some(rx),
                keepalive_error: None,
            },
        )
    }

    /// Never closes, but its keepalive reports the link dead.
    pub(super) fn keepalive_fails(reason: &str) -> Self {
        Self {
            closed: None,
            keepalive_error: Some(reason.to_owned()),
        }
    }
}

impl LinkTransport for ScriptTransport {
    type Conn = ScriptConn;

    #[allow(
        clippy::future_not_send,
        clippy::unused_async_trait_impl,
        reason = "test transport is Rc-scripted and driven on a LocalSet; its trait methods remain async to match production"
    )]
    async fn connect(
        &self,
        _host: &SatelliteHost,
        _spec: &DialSpec,
        token: Option<String>,
    ) -> Result<ScriptConn, String> {
        self.calls.set(self.calls.get() + 1);
        self.seen_tokens.borrow_mut().push(token);
        self.script
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Err("script exhausted".to_owned()))
    }
}

#[allow(
    clippy::unused_async_trait_impl,
    reason = "the scripted test connection implements the production async transport trait without I/O"
)]
impl LinkConn for ScriptConn {
    type Reader = ScriptReader;
    type Writer = ScriptWriter;

    fn into_parts(self) -> (Self::Reader, Self::Writer) {
        (
            ScriptReader {
                closed: self.closed,
            },
            ScriptWriter {
                keepalive_error: self.keepalive_error,
            },
        )
    }

    fn bootstrap_limits(&self) -> Result<BootstrapLimits, String> {
        Ok(BootstrapLimits::default())
    }

    fn bootstrap_profile(&self) -> Result<BootstrapProfile, String> {
        Ok(BootstrapProfile::SynthesizedVtRaw)
    }

    fn server_features(&self) -> Result<phux_protocol::caps::ServerFeatureSet, String> {
        Ok(phux_protocol::caps::ServerFeatureSet::with(&[
            phux_protocol::caps::ServerFeature::ListDirectory,
        ]))
    }
}

impl LinkConn for BlockedWriteConn {
    type Reader = BlockedWriteReader;
    type Writer = BlockedWriteWriter;

    fn into_parts(self) -> (Self::Reader, Self::Writer) {
        (
            BlockedWriteReader {
                inbound: self.inbound,
            },
            BlockedWriteWriter {
                write_started: self.write_started,
                release_write: self.release_write,
                dropped: self.writer_dropped,
            },
        )
    }

    fn bootstrap_limits(&self) -> Result<BootstrapLimits, String> {
        Ok(BootstrapLimits::default())
    }

    fn bootstrap_profile(&self) -> Result<BootstrapProfile, String> {
        Ok(BootstrapProfile::SynthesizedVtRaw)
    }

    fn server_features(&self) -> Result<phux_protocol::caps::ServerFeatureSet, String> {
        Ok(phux_protocol::caps::ServerFeatureSet::with(&[
            phux_protocol::caps::ServerFeature::ListDirectory,
        ]))
    }
}

impl LinkReader for BlockedWriteReader {
    async fn recv_frame(&mut self) -> Result<Option<Vec<u8>>, String> {
        Ok(self.inbound.recv().await)
    }
}

#[allow(
    clippy::unused_async_trait_impl,
    reason = "test writer implements the production async transport trait"
)]
impl LinkWriter for BlockedWriteWriter {
    async fn send_frame(&mut self, _frame: &[u8]) -> Result<(), String> {
        self.write_started.notify_waiters();
        self.release_write.notified().await;
        Ok(())
    }

    async fn keepalive(&mut self) -> Result<(), String> {
        Ok(())
    }
}

impl Drop for BlockedWriteWriter {
    fn drop(&mut self) {
        self.dropped
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl LinkReader for ScriptReader {
    async fn recv_frame(&mut self) -> Result<Option<Vec<u8>>, String> {
        match &mut self.closed {
            Some(rx) => rx.recv().await.map_or(Ok(None), Err),
            None => std::future::pending().await,
        }
    }
}

#[allow(
    clippy::unused_async_trait_impl,
    reason = "scripted test writer implements the production async transport trait"
)]
impl LinkWriter for ScriptWriter {
    async fn send_frame(&mut self, _frame: &[u8]) -> Result<(), String> {
        Ok(())
    }

    async fn keepalive(&mut self) -> Result<(), String> {
        self.keepalive_error.clone().map_or(Ok(()), Err)
    }
}

#[test]
fn active_write_bytes_remain_charged_against_the_aggregate_bound() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let total = Rc::new(Cell::new(0));
    let active_bytes = LINK_WRITE_QUEUE_BYTES / 2 + 1;
    queue_link_write(&tx, &total, LinkWrite::Frames(vec![vec![0; active_bytes]]))
        .expect("first write fits");
    let mut active = rx.try_recv().expect("queued write");
    let active_write = active.take_write().expect("active write");
    assert_eq!(total.get(), active_bytes, "active bytes stay reserved");

    let error = queue_link_write(&tx, &total, LinkWrite::Frames(vec![vec![0; active_bytes]]))
        .expect_err("active plus queued bytes exceed the aggregate bound");
    assert!(error.contains("exceeded"));

    drop(active_write);
    drop(active);
    assert_eq!(total.get(), 0);
}

#[tokio::test]
async fn blocked_outbound_write_does_not_stall_inbound_fanout() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let host = host();
            let (relay, mailbox) = relay_pair(&host);
            let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(1);
            let write_started = Arc::new(tokio::sync::Notify::new());
            let release_write = Arc::new(tokio::sync::Notify::new());
            let writer_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let conn = BlockedWriteConn {
                inbound: inbound_rx,
                write_started: Arc::clone(&write_started),
                release_write: Arc::clone(&release_write),
                writer_dropped,
            };
            let cancel = CancellationToken::new();
            let session_host = host.clone();
            let session_cancel = cancel.clone();
            let session = tokio::task::spawn_local(async move {
                let crate::hub::relay::RelayMailbox {
                    mut requests,
                    mut unsubscribes,
                    resource_kinds,
                    ..
                } = mailbox;
                run_relay_session(
                    &session_host,
                    conn,
                    &mut requests,
                    &mut unsubscribes,
                    &session_cancel,
                    None,
                    &crate::hub::operation_fence::OperationFence::default(),
                    resource_kinds,
                )
                .await
            });
            let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(4);
            let started = write_started.notified();
            tokio::pin!(started);
            relay.subscribe(
                crate::hub::relay::ProxySubscription {
                    resource_kind: None,
                    terminal: 9,
                    client: crate::state::ClientId(7),
                    out_tx,
                    consumer_cancel: CancellationToken::new(),
                    seq: 0,
                    awaits_snapshot: false,
                    bootstrap_profile: None,
                    bootstrap_limits: None,
                },
                FrameKind::SubscribeEvents {
                    terminal: Some(phux_protocol::ResourceId::local(9)),
                    after_seq: None,
                },
            );
            tokio::time::timeout(Duration::from_secs(1), &mut started)
                .await
                .expect("the ordered writer entered its blocked send");

            let mut encoded = bytes::BytesMut::new();
            FrameKind::Event {
                terminal: Some(phux_protocol::ResourceId::local(9)),
                event: phux_protocol::wire::frame::AgentEvent::CommandStarted,
                stamp: None,
            }
            .encode(&mut encoded);
            inbound_tx
                .send(encoded.to_vec())
                .await
                .expect("inject inbound event");
            assert!(matches!(
                tokio::time::timeout(Duration::from_millis(250), out_rx.recv())
                    .await
                    .expect("inbound fanout must not wait for the write")
                    .expect("subscriber remains live"),
                crate::mailbox::Outbound::Frame(FrameKind::Event { .. })
            ));

            cancel.cancel();
            release_write.notify_waiters();
            let _ = session.await;
        })
        .await;
}

#[tokio::test]
async fn aborting_session_cancels_a_blocked_transport_writer() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let host = host();
            let (relay, mailbox) = relay_pair(&host);
            let (_inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(1);
            let write_started = Arc::new(tokio::sync::Notify::new());
            let writer_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let conn = BlockedWriteConn {
                inbound: inbound_rx,
                write_started: Arc::clone(&write_started),
                release_write: Arc::new(tokio::sync::Notify::new()),
                writer_dropped: Arc::clone(&writer_dropped),
            };
            let session_host = host.clone();
            let session = tokio::task::spawn_local(async move {
                let crate::hub::relay::RelayMailbox {
                    mut requests,
                    mut unsubscribes,
                    resource_kinds,
                    ..
                } = mailbox;
                run_relay_session(
                    &session_host,
                    conn,
                    &mut requests,
                    &mut unsubscribes,
                    &CancellationToken::new(),
                    None,
                    &crate::hub::operation_fence::OperationFence::default(),
                    resource_kinds,
                )
                .await
            });
            let (out_tx, _out_rx) = tokio::sync::mpsc::channel(1);
            let started = write_started.notified();
            tokio::pin!(started);
            relay.subscribe(
                crate::hub::relay::ProxySubscription {
                    resource_kind: None,
                    terminal: 9,
                    client: crate::state::ClientId(7),
                    out_tx,
                    consumer_cancel: CancellationToken::new(),
                    seq: 0,
                    awaits_snapshot: false,
                    bootstrap_profile: None,
                    bootstrap_limits: None,
                },
                FrameKind::SubscribeEvents {
                    terminal: Some(phux_protocol::ResourceId::local(9)),
                    after_seq: None,
                },
            );
            tokio::time::timeout(Duration::from_secs(1), &mut started)
                .await
                .expect("writer entered blocked send");
            session.abort();
            for _ in 0..10 {
                tokio::task::yield_now().await;
                if writer_dropped.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
            }
            panic!("blocked transport writer survived session cancellation");
        })
        .await;
}
