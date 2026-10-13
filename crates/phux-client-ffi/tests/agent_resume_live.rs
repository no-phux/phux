//! Connected C ABI proof that agent publications survive resume, loss, eviction,
//! and a new server incarnation.
//!
//! `run_smoke` drives an isolated [`ServerRuntime`](phux_server::ServerRuntime),
//! real `AgentSession` producer commands, and the public C ABI through a private
//! Unix proxy. The proxy gates packets; it does not rewrite them or stand in
//! for the server.
//!
//! Loss is taken after the prefix is committed and a later poll has fed terminal
//! output, and before [`phux_client_effect_clear`]. That clear is the only ACK.
//! The following attach's `agent_after_seq` is the committed cursor, so an empty
//! wire resume cannot be what restores the prefix.
//!
//! A throwaway runner includes this file and calls [`run_smoke`]. The `#[test]`
//! does the same. No production example or CLI.

#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::unwrap_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]
#![allow(
    clippy::future_not_send,
    reason = "the C client is owning-thread-only; this future stays on the LocalSet thread"
)]
#![allow(
    clippy::print_stdout,
    reason = "the standalone smoke process reports its observed record sequences"
)]

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use phux_client_ffi::{
    ABI_VERSION, AGENT_RECORDS_CLOSED, AGENT_RECORDS_LIVE, AGENT_RECORDS_RETAINED,
    EFFECT_AGENT_RECORDS, PhuxArrangementInfo, PhuxAttachOptions, PhuxBytes, PhuxClient,
    PhuxClientEffect, PhuxClientOptions, PhuxClientResult, PhuxClientState, PhuxConnectOptions,
    PhuxResourceInfo, PhuxTerminalGridView, RESOURCE_KIND_AGENT_SESSION,
    phux_client_arrangement_info, phux_client_arrangement_refresh, phux_client_connect,
    phux_client_connection_epoch, phux_client_connection_error, phux_client_effect_clear,
    phux_client_effect_count, phux_client_effect_get, phux_client_free, phux_client_nudge,
    phux_client_poll, phux_client_poll_pending, phux_client_queue_attach,
    phux_client_resource_count, phux_client_resource_get, phux_client_state,
    phux_client_terminal_grid,
};
use phux_protocol::ids::{ResourceId, ResourceKind};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    Command, CommandResult, FrameKind, InputMode, SpawnResource, SpawnResult,
};
use phux_server::ServerError;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, Spawn, attach_by_name, command, join_after_shutdown, recv_until,
    run_local, seed_pty, send_frame, spawn_resource, spawn_server_with, wait_for_socket,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

const SESSION: &str = "agent-smoke";
const DEADLINE: Duration = Duration::from_secs(20);
/// Large enough for the prefix, resume suffix, and one live record; small
/// enough that a later bulk append evicts the committed cursor.
const RING_BYTES: u32 = 2_048;
const EVICT_COUNT: u32 = 40;

/// The whole consumer-visible scenario. A throwaway process calls this; it
/// does not take a runtime or a socket of its own.
pub fn run_smoke() {
    run_local(async {
        smoke().await;
    });
}

#[test]
fn agent_resume_live() {
    run_smoke();
}

#[derive(Clone, Debug)]
struct Record {
    seq: u64,
    mark: String,
}

#[derive(Debug)]
struct Batch {
    detail: u32,
    seq: u64,
    stream_id: u64,
    bootstrap_id: u64,
    records: Vec<Record>,
}

struct HostLog {
    folded: Vec<Record>,
}

impl HostLog {
    fn seqs(&self) -> Vec<u64> {
        self.folded.iter().map(|record| record.seq).collect()
    }

    fn marks(&self) -> Vec<&str> {
        self.folded
            .iter()
            .map(|record| record.mark.as_str())
            .collect()
    }
}

enum ProxyEvent {
    Severed,
    Attach {
        resource: u32,
        after_seq: Option<u64>,
    },
    BootstrapReady {
        resource: u32,
    },
}

struct Proxy {
    stop: watch::Sender<bool>,
    sever: watch::Sender<u64>,
    events: mpsc::UnboundedReceiver<ProxyEvent>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Proxy {
    fn spawn(front: &Path, backend: PathBuf) -> Self {
        let _ = std::fs::remove_file(front);
        let listener = std::os::unix::net::UnixListener::bind(front).expect("proxy bind");
        listener.set_nonblocking(true).expect("proxy nonblocking");
        let (stop, _) = watch::channel(false);
        let (sever, _) = watch::channel(0_u64);
        let (events_tx, events) = mpsc::unbounded_channel();
        let stop_thread = stop.clone();
        let sever_thread = sever.clone();
        let thread = std::thread::Builder::new()
            .name("agent-resume-proxy".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("proxy runtime");
                runtime.block_on(serve(
                    listener,
                    backend,
                    stop_thread,
                    sever_thread,
                    events_tx,
                ));
            })
            .expect("proxy thread");
        Self {
            stop,
            sever,
            events,
            thread: Some(thread),
        }
    }

    fn sever_now(&self) {
        self.sever.send_modify(|generation| {
            *generation = generation.saturating_add(1);
        });
    }

    async fn wait<T>(&mut self, what: &str, mut pred: impl FnMut(ProxyEvent) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + DEADLINE;
        loop {
            let event = tokio::time::timeout_at(deadline, self.events.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .unwrap_or_else(|| panic!("proxy ended while waiting for {what}"));
            if let Some(found) = pred(event) {
                return found;
            }
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        self.sever_now();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn serve(
    listener: std::os::unix::net::UnixListener,
    backend: PathBuf,
    stop: watch::Sender<bool>,
    sever: watch::Sender<u64>,
    events: mpsc::UnboundedSender<ProxyEvent>,
) {
    let listener = UnixListener::from_std(listener).expect("proxy listener");
    let mut stop_rx = stop.subscribe();
    loop {
        tokio::select! {
            changed = stop_rx.changed() => {
                if changed.is_err() || *stop_rx.borrow() {
                    return;
                }
            }
            accepted = listener.accept() => {
                let Ok((frontend, _)) = accepted else {
                    return;
                };
                relay_one(frontend, &backend, &mut stop_rx, &sever, &events).await;
            }
        }
    }
}

async fn relay_one(
    mut frontend: UnixStream,
    backend: &Path,
    stop: &mut watch::Receiver<bool>,
    sever: &watch::Sender<u64>,
    events: &mpsc::UnboundedSender<ProxyEvent>,
) {
    let Some(mut backend) = connect_backend(backend, stop).await else {
        return;
    };
    let sever_at = *sever.borrow();
    let (mut front_read, mut front_write) = frontend.split();
    let (mut back_read, mut back_write) = backend.split();
    let mut client_sever = sever.subscribe();
    let mut server_sever = sever.subscribe();
    tokio::select! {
        () = copy_client(&mut front_read, &mut back_write, events, &mut client_sever, sever_at) => {}
        () = copy_server(&mut back_read, &mut front_write, events, &mut server_sever, sever_at) => {}
        _ = stop.changed() => return,
    }
    let _ = events.send(ProxyEvent::Severed);
}

async fn connect_backend(path: &Path, stop: &mut watch::Receiver<bool>) -> Option<UnixStream> {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        if *stop.borrow() {
            return None;
        }
        if let Ok(stream) = UnixStream::connect(path).await {
            return Some(stream);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::select! {
            _ = stop.changed() => return None,
            () = tokio::time::sleep(Duration::from_millis(5)) => {}
        }
    }
}

fn sever_requested(sever: &watch::Receiver<u64>, sever_at: u64) -> bool {
    *sever.borrow() != sever_at
}

async fn copy_client(
    reader: &mut (impl AsyncRead + Unpin),
    writer: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::UnboundedSender<ProxyEvent>,
    sever: &mut watch::Receiver<u64>,
    sever_at: u64,
) {
    loop {
        if sever_requested(sever, sever_at) {
            return;
        }
        let frame = tokio::select! {
            changed = sever.changed() => {
                if changed.is_err() || sever_requested(sever, sever_at) {
                    return;
                }
                continue;
            }
            frame = read_frame(reader) => frame,
        };
        let Ok(frame) = frame else {
            return;
        };
        if sever_requested(sever, sever_at) {
            return;
        }
        if let Ok((kind, rest)) = FrameKind::decode(&frame) {
            debug_assert!(rest.is_empty());
            if let FrameKind::Command {
                command:
                    Command::AttachResource {
                        terminal_id,
                        agent_after_seq,
                        ..
                    },
                ..
            } = kind
            {
                let _ = events.send(ProxyEvent::Attach {
                    resource: terminal_id.local_id().unwrap_or(0),
                    after_seq: agent_after_seq,
                });
            }
        }
        if writer.write_all(&frame).await.is_err() {
            return;
        }
    }
}

async fn copy_server(
    reader: &mut (impl AsyncRead + Unpin),
    writer: &mut (impl AsyncWrite + Unpin),
    events: &mpsc::UnboundedSender<ProxyEvent>,
    sever: &mut watch::Receiver<u64>,
    sever_at: u64,
) {
    loop {
        if sever_requested(sever, sever_at) {
            return;
        }
        let frame = tokio::select! {
            changed = sever.changed() => {
                if changed.is_err() || sever_requested(sever, sever_at) {
                    return;
                }
                continue;
            }
            frame = read_frame(reader) => frame,
        };
        let Ok(frame) = frame else {
            return;
        };
        if sever_requested(sever, sever_at) {
            return;
        }
        if let Ok((kind, rest)) = FrameKind::decode(&frame) {
            debug_assert!(rest.is_empty());
            if let FrameKind::BootstrapReady { terminal_id, .. } = kind {
                let _ = events.send(ProxyEvent::BootstrapReady {
                    resource: terminal_id.local_id().unwrap_or(0),
                });
            }
        }
        if writer.write_all(&frame).await.is_err() {
            return;
        }
        let _ = writer.flush().await;
    }
}

async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Vec<u8>> {
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).await?;
    let len = usize::try_from(u32::from_be_bytes(header)).unwrap_or(usize::MAX);
    if len > 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "fixture frame exceeds 1 MiB",
        ));
    }
    let mut frame = vec![0; len + 4];
    frame[..4].copy_from_slice(&header);
    reader.read_exact(&mut frame[4..]).await?;
    Ok(frame)
}

struct Producer {
    stream: UnixStream,
    next_id: u32,
}

impl Producer {
    async fn connect(socket: &Path) -> Self {
        Self {
            stream: wait_for_socket(socket, SOCKET_CONNECT_DEADLINE).await,
            next_id: 10,
        }
    }

    const fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    async fn open_session(&mut self) -> (ResourceId, ResourceId) {
        send_frame(&mut self.stream, &attach_by_name(SESSION)).await;
        let snapshot = recv_until(&mut self.stream, |_, frame| match frame {
            FrameKind::Attached { snapshot, .. } => Some(snapshot),
            FrameKind::Error { code, message, .. } => {
                panic!("producer attach refused: {code:?} {message}")
            }
            _ => None,
        })
        .await;
        let parent = snapshot
            .resources
            .iter()
            .find(|resource| resource.kind == ResourceKind::Terminal)
            .unwrap_or_else(|| panic!("seed terminal missing: {snapshot:?}"))
            .id
            .clone();
        let request_id = self.id();
        let spawned = spawn_resource(
            &mut self.stream,
            request_id,
            Spawn {
                resource: Some(Box::new(SpawnResource::agent_session(
                    parent.clone(),
                    "claude",
                ))),
                ..Spawn::default()
            },
        )
        .await;
        let SpawnResult::Ok(agent) = spawned else {
            panic!("spawn agent session: {spawned:?}");
        };
        (parent, agent)
    }

    async fn append(&mut self, agent: &ResourceId, marks: &[&str]) {
        let bytes = marks
            .iter()
            .map(|mark| record_line(mark))
            .collect::<String>();
        let request_id = self.id();
        let result = command(
            &mut self.stream,
            request_id,
            Command::AppendResourceOutput {
                terminal_id: agent.clone(),
                bytes: bytes.into_bytes(),
            },
        )
        .await;
        assert!(
            matches!(result, CommandResult::Ok | CommandResult::OkWith(_)),
            "append {marks:?}: {result:?}"
        );
    }

    async fn paste(&mut self, terminal: &ResourceId, text: &str) {
        send_frame(
            &mut self.stream,
            &FrameKind::InputPaste {
                terminal_id: terminal.clone(),
                event: PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: text.as_bytes().to_vec(),
                },
            },
        )
        .await;
    }

    async fn kill(&mut self, agent: &ResourceId) {
        let request_id = self.id();
        let result = command(
            &mut self.stream,
            request_id,
            Command::KillResource {
                terminal_id: agent.clone(),
                operation_id: None,
            },
        )
        .await;
        assert!(
            matches!(result, CommandResult::Ok | CommandResult::OkWith(_)),
            "kill: {result:?}"
        );
    }
}

fn record_line(mark: &str) -> String {
    format!("{{\"type\":\"prompt\",\"data\":{{\"mark\":\"{mark}\"}}}}\n")
}

struct Wake {
    tx: tokio::sync::mpsc::UnboundedSender<()>,
}

unsafe extern "C" fn on_wake(context: *mut c_void) {
    // SAFETY: the boxed callback context outlives the client; free joins its driver.
    let wake = unsafe { &*context.cast::<Wake>() };
    let _ = wake.tx.send(());
}

struct Consumer {
    client: *mut PhuxClient,
    wakes: mpsc::UnboundedReceiver<()>,
    attach_id: u32,
    host_request: u32,
}

impl Consumer {
    fn connect(socket: &str, wake: &Wake, wakes: mpsc::UnboundedReceiver<()>) -> Self {
        let options = PhuxConnectOptions {
            size: std::mem::size_of::<PhuxConnectOptions>(),
            version: ABI_VERSION,
            base: base_options(),
            target: PhuxBytes::default(),
            socket_path: span(socket),
            config_path: PhuxBytes::default(),
            client_name: span("phux-client-ffi-agent-resume"),
            wake: Some(on_wake),
            wake_context: std::ptr::from_ref(wake).cast_mut().cast(),
        };
        let mut client = std::ptr::null_mut();
        // SAFETY: options and their spans are live; the output pointer is writable.
        let result = unsafe { phux_client_connect(&raw const options, &raw mut client) };
        assert_eq!(result, PhuxClientResult::Ok, "connect");
        Self {
            client,
            wakes,
            attach_id: 1,
            host_request: 1,
        }
    }

    async fn until(&mut self, what: &str, mut ready: impl FnMut(*mut PhuxClient) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what} ({})",
                diagnose(self.client)
            );
            // SAFETY: this consumer owns the live client and stays on its owning thread.
            let result = unsafe { phux_client_poll(self.client) };
            assert_eq!(
                result,
                PhuxClientResult::Ok,
                "poll failed while {what} ({})",
                diagnose(self.client)
            );
            if ready(self.client) {
                return;
            }
            while unsafe { phux_client_poll_pending(self.client) } {
                let result = unsafe { phux_client_poll(self.client) };
                assert_eq!(result, PhuxClientResult::Ok, "pending poll while {what}");
                if ready(self.client) {
                    return;
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            tokio::select! {
                biased;
                maybe = self.wakes.recv() => {
                    assert!(maybe.is_some(), "wake channel closed while {what}");
                }
                () = tokio::time::sleep(remaining) => {}
            }
        }
    }

    async fn ensure_attached(&mut self) {
        self.until("negotiated or attached", |client| {
            matches!(
                unsafe { phux_client_state(client) },
                PhuxClientState::Negotiated | PhuxClientState::Attached
            )
        })
        .await;
        if unsafe { phux_client_state(self.client) } == PhuxClientState::Negotiated {
            self.queue_attach();
        }
        self.until("attached", |client| {
            (unsafe { phux_client_state(client) }) == PhuxClientState::Attached
        })
        .await;
    }

    fn queue_attach(&mut self) {
        self.attach_id = self.attach_id.saturating_add(1);
        let attach_id = self.attach_id;
        let options = PhuxAttachOptions {
            size: std::mem::size_of::<PhuxAttachOptions>(),
            version: ABI_VERSION,
            attach_id,
            target_kind: 1,
            session_id: 0,
            name: span(SESSION),
            cols: 40,
            rows: 8,
            has_pixel_size: false,
            pixel_width: 0,
            pixel_height: 0,
            request_scrollback: false,
            scrollback_limit_lines: 0,
        };
        let result = unsafe { phux_client_queue_attach(self.client, &raw const options) };
        assert_eq!(
            result,
            PhuxClientResult::Ok,
            "queue_attach ({})",
            diagnose(self.client)
        );
    }

    fn nudge(&self) {
        assert_eq!(
            unsafe { phux_client_nudge(self.client) },
            PhuxClientResult::Ok,
            "nudge"
        );
    }

    async fn reconnect_after(&mut self, previous_epoch: u64) {
        self.nudge();
        self.until("new connection epoch", |client| {
            // SAFETY: until supplies this consumer's live client on its owning thread.
            (unsafe { phux_client_connection_epoch(client) }) > previous_epoch
        })
        .await;
        self.ensure_attached().await;
        // ATTACHED alone has not processed the C lane's authoritative GET_STATE.
        // Drive that reply before waiting for the proxy to see a new subscription.
        self.until("rebound arrangement registry", |client| {
            arrangement_status(client) == 2
        })
        .await;
    }

    async fn sever_and_reattach(&mut self, proxy: &mut Proxy) {
        // SAFETY: this consumer owns the live client on this thread.
        let previous_epoch = unsafe { phux_client_connection_epoch(self.client) };
        proxy.sever_now();
        proxy
            .wait("packet-gated disconnect", |event| {
                matches!(event, ProxyEvent::Severed).then_some(())
            })
            .await;
        self.reconnect_after(previous_epoch).await;
    }

    fn drain(&mut self, agent: u32, log: &mut HostLog) -> Vec<Batch> {
        let batches = take_batches(self.client, agent);
        for batch in &batches {
            apply_batch(log, batch);
        }
        assert_eq!(
            unsafe { phux_client_effect_clear(self.client) },
            PhuxClientResult::Ok,
            "effect_clear"
        );
        batches
    }

    fn peek_agent(&self, agent: u32) -> Vec<Batch> {
        take_batches(self.client, agent)
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        if !self.client.is_null() {
            unsafe { phux_client_free(self.client) };
            self.client = std::ptr::null_mut();
        }
    }
}

const fn base_options() -> PhuxClientOptions {
    PhuxClientOptions {
        size: std::mem::size_of::<PhuxClientOptions>(),
        version: ABI_VERSION,
        max_bootstrap_chunk_bytes: 64 * 1024,
        max_history_page_bytes: 64 * 1024,
        max_history_page_rows: 500,
        max_history_cache_bytes: 4 * 1024 * 1024,
        max_history_materialized_rows: 1000,
        history_prefetch_rows: 100,
    }
}

const fn span(text: &str) -> PhuxBytes {
    PhuxBytes {
        data: text.as_ptr(),
        len: text.len(),
    }
}

fn diagnose(client: *mut PhuxClient) -> String {
    let state = unsafe { phux_client_state(client) };
    let epoch = unsafe { phux_client_connection_epoch(client) };
    let mut error = PhuxBytes::default();
    let _ = unsafe { phux_client_connection_error(client, &raw mut error) };
    format!(
        "state={state:?} epoch={epoch} error={}",
        String::from_utf8_lossy(&copy_bytes(error))
    )
}

fn copy_bytes(bytes: PhuxBytes) -> Vec<u8> {
    if bytes.data.is_null() || bytes.len == 0 {
        return Vec::new();
    }
    // SAFETY: callers copy the complete borrowed C span before any mutable C call.
    unsafe { std::slice::from_raw_parts(bytes.data, bytes.len).to_vec() }
}

fn take_batches(client: *mut PhuxClient, agent: u32) -> Vec<Batch> {
    let mut batches = Vec::new();
    let count = unsafe { phux_client_effect_count(client) };
    for index in 0..count {
        let mut effect = PhuxClientEffect::default();
        assert_eq!(
            unsafe { phux_client_effect_get(client, index, &raw mut effect) },
            PhuxClientResult::Ok
        );
        if effect.kind != EFFECT_AGENT_RECORDS || effect.terminal_id.id != agent {
            continue;
        }
        let bytes = copy_bytes(effect.bytes);
        batches.push(Batch {
            detail: effect.detail,
            seq: effect.seq,
            stream_id: effect.stream_id,
            bootstrap_id: effect.bootstrap_id,
            records: parse_records(&bytes),
        });
    }
    batches
}

fn parse_records(bytes: &[u8]) -> Vec<Record> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let value: serde_json::Value = serde_json::from_slice(line).expect("agent record json");
            Record {
                seq: value["seq"].as_u64().expect("record seq"),
                mark: value["data"]["mark"].as_str().unwrap_or("").to_owned(),
            }
        })
        .collect()
}

fn apply_batch(log: &mut HostLog, batch: &Batch) {
    match batch.detail {
        AGENT_RECORDS_RETAINED => log.folded.clone_from(&batch.records),
        AGENT_RECORDS_LIVE => log.folded.extend_from_slice(&batch.records),
        _ => {}
    }
}

fn assert_unique(batches: &[Batch], what: &str) {
    let mut seen = Vec::new();
    for batch in batches {
        for record in &batch.records {
            assert!(
                !seen.contains(&record.seq),
                "{what} delivered seq {} twice: {seen:?}",
                record.seq
            );
            seen.push(record.seq);
        }
        let expected_seq = batch.records.last().map_or(0, |record| record.seq);
        assert_eq!(
            batch.seq, expected_seq,
            "{what} effect seq is the last record, or 0 when empty"
        );
    }
}

fn resource_ids(client: *mut PhuxClient) -> Vec<(u32, u32)> {
    let mut ids = Vec::new();
    let count = unsafe { phux_client_resource_count(client) };
    for index in 0..count {
        let mut info = PhuxResourceInfo {
            size: std::mem::size_of::<PhuxResourceInfo>(),
            version: ABI_VERSION,
            ..PhuxResourceInfo::default()
        };
        assert_eq!(
            unsafe { phux_client_resource_get(client, index, &raw mut info) },
            PhuxClientResult::Ok
        );
        ids.push((info.kind, info.terminal_id.id));
    }
    ids
}

fn grid_text(client: *mut PhuxClient, terminal: u32) -> String {
    let id = phux_client_ffi::PhuxResourceId {
        kind: 0,
        id: terminal,
        host: PhuxBytes::default(),
    };
    let mut grid = PhuxTerminalGridView::default();
    if unsafe { phux_client_terminal_grid(client, &raw const id, &raw mut grid) }
        != PhuxClientResult::Ok
    {
        return String::new();
    }
    String::from_utf8_lossy(&copy_bytes(grid.utf8)).into_owned()
}

fn start_server(socket: &Path) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    let seed = CommandBuilder::new("/bin/cat");
    spawn_server_with(socket.to_path_buf(), Some(SESSION), move |config| {
        config.agent_log_bytes = RING_BYTES;
        seed_pty(config, seed);
    })
}

async fn smoke() {
    let tmp = TempDir::new().expect("temp dir");
    let backend = tmp.path().join("server.sock");
    let front = tmp.path().join("proxy.sock");
    let (shutdown, server) = start_server(&backend);
    let mut proxy = Proxy::spawn(&front, backend.clone());
    let mut producer = Producer::connect(&backend).await;
    let (parent, agent) = producer.open_session().await;
    let agent_local = agent.local_id().expect("local agent id");

    let (wake_tx, wake_rx) = mpsc::unbounded_channel();
    let wake = Box::new(Wake { tx: wake_tx });
    let socket_text = front.to_str().expect("proxy path").to_owned();
    let mut consumer = Consumer::connect(&socket_text, &wake, wake_rx);
    consumer.ensure_attached().await;
    let (mut log, initial_generation) =
        prove_empty_replace(&mut consumer, &mut proxy, agent_local).await;
    prove_committed_prefix(&mut producer, &mut consumer, &parent, &agent).await;
    prove_replayed_prefix(&mut consumer, &mut proxy, agent_local, &mut log).await;
    prove_resume_suffix(&mut producer, &mut consumer, &mut proxy, &agent, &mut log).await;
    prove_empty_resume_and_close(
        &mut producer,
        &mut consumer,
        &mut proxy,
        &agent,
        &mut log,
        initial_generation,
    )
    .await;

    prove_eviction(&mut producer, &mut consumer, &mut proxy, &parent).await;
    let (shutdown, server) = prove_incarnation(
        producer,
        &mut consumer,
        shutdown,
        server,
        &backend,
        agent_local,
    )
    .await;

    drop(consumer);
    drop(wake);
    drop(proxy);
    join_after_shutdown(shutdown, server).await;
}

async fn prove_eviction(
    producer: &mut Producer,
    consumer: &mut Consumer,
    proxy: &mut Proxy,
    parent: &ResourceId,
) {
    let request_id = producer.id();
    let spawned = spawn_resource(
        &mut producer.stream,
        request_id,
        Spawn {
            resource: Some(Box::new(SpawnResource::agent_session(
                parent.clone(),
                "claude",
            ))),
            ..Spawn::default()
        },
    )
    .await;
    let SpawnResult::Ok(agent) = spawned else {
        panic!("eviction session: {spawned:?}");
    };
    let agent_local = agent.local_id().expect("eviction agent");
    producer
        .append(&agent, &["evict-base-a", "evict-base-b"])
        .await;
    refresh_until(consumer, agent_local, "eviction base").await;
    let mut log = HostLog { folded: Vec::new() };
    let base = consumer.drain(agent_local, &mut log);
    let cursor = base
        .iter()
        .flat_map(|batch| batch.records.iter().map(|record| record.seq))
        .max()
        .expect("eviction base cursor");
    assert!(cursor >= 2, "base records committed: {base:?}");

    let marks = (0..EVICT_COUNT)
        .map(|index| format!("evict-{index:02}"))
        .collect::<Vec<_>>();
    let mark_refs = marks.iter().map(String::as_str).collect::<Vec<_>>();
    // Evict while disconnected, before the consumer can advance its cursor.
    // SAFETY: this consumer owns the live client on this thread.
    let previous_epoch = unsafe { phux_client_connection_epoch(consumer.client) };
    proxy.sever_now();
    proxy
        .wait("eviction disconnect", |event| {
            matches!(event, ProxyEvent::Severed).then_some(())
        })
        .await;
    producer.append(&agent, &mark_refs).await;
    consumer.reconnect_after(previous_epoch).await;
    proxy
        .wait("eviction rebind", |event| match event {
            ProxyEvent::BootstrapReady { resource } if resource == agent_local => Some(()),
            _ => None,
        })
        .await;
    consumer
        .until("eviction replacement", |client| {
            take_batches(client, agent_local)
                .iter()
                .any(|batch| batch.detail == AGENT_RECORDS_RETAINED && !batch.records.is_empty())
        })
        .await;
    let replaced = consumer.drain(agent_local, &mut log);
    assert!(
        replaced
            .iter()
            .any(|batch| batch.detail == AGENT_RECORDS_RETAINED),
        "evicted cursor is a full replacement, not an append: {replaced:?}"
    );
    assert_unique(&replaced, "eviction");
    assert!(
        log.marks().iter().all(|mark| mark.starts_with("evict-")),
        "replacement drops the pre-eviction prefix: {:?}",
        log.marks()
    );
    let seqs = log.seqs();
    assert!(
        seqs.windows(2).all(|pair| pair[1] == pair[0] + 1)
            && seqs.first().is_some_and(|seq| *seq > cursor),
        "retained suffix is contiguous and starts after the evicted cursor {cursor}: {seqs:?}"
    );
    assert_eq!(
        log.marks().last().copied(),
        Some(marks.last().expect("tail").as_str()),
        "the newest evicted-ring record is present"
    );
}

async fn prove_incarnation(
    producer: Producer,
    consumer: &mut Consumer,
    shutdown: oneshot::Sender<()>,
    server: JoinHandle<Result<(), ServerError>>,
    backend: &Path,
    previous_agent: u32,
) -> (oneshot::Sender<()>, JoinHandle<Result<(), ServerError>>) {
    // SAFETY: this consumer owns the live client on this thread.
    let previous_epoch = unsafe { phux_client_connection_epoch(consumer.client) };
    drop(producer);
    join_after_shutdown(shutdown, server).await;
    let (shutdown, server) = start_server(backend);

    let mut producer = Producer::connect(backend).await;
    let (_parent, agent) = producer.open_session().await;
    let agent_local = agent.local_id().expect("reused agent id");
    assert_eq!(
        agent_local, previous_agent,
        "a new incarnation reuses the first agent resource id"
    );
    producer
        .append(&agent, &["incarnation-a", "incarnation-b"])
        .await;

    consumer.reconnect_after(previous_epoch).await;
    refresh_until(consumer, agent_local, "incarnation records").await;
    let mut log = HostLog { folded: Vec::new() };
    let batches = consumer.drain(agent_local, &mut log);
    assert!(
        batches
            .iter()
            .any(|batch| batch.detail == AGENT_RECORDS_RETAINED),
        "reused resource id is a replacement, not a resume onto the old log: {batches:?}"
    );
    assert_unique(&batches, "incarnation");
    assert_eq!(log.marks(), vec!["incarnation-a", "incarnation-b"]);
    assert!(
        log.seqs().first().is_some_and(|seq| *seq == 1),
        "the new stream starts at seq 1 despite the reused id: {:?}",
        log.seqs()
    );
    drop(producer);
    (shutdown, server)
}

async fn refresh_until(consumer: &mut Consumer, agent: u32, what: &str) {
    consumer.host_request = consumer.host_request.saturating_add(1);
    let request = consumer.host_request;
    consumer
        .until("arrangement idle", |client| arrangement_status(client) != 1)
        .await;
    let refreshed = unsafe { phux_client_arrangement_refresh(consumer.client, request) };
    assert_eq!(
        refreshed,
        PhuxClientResult::Ok,
        "arrangement refresh before {what} ({})",
        diagnose(consumer.client)
    );
    consumer
        .until(what, |client| {
            take_batches(client, agent)
                .iter()
                .any(|batch| !batch.records.is_empty() || batch.detail == AGENT_RECORDS_RETAINED)
        })
        .await;
}

fn arrangement_status(client: *mut PhuxClient) -> u32 {
    let mut info = PhuxArrangementInfo {
        size: std::mem::size_of::<PhuxArrangementInfo>(),
        version: ABI_VERSION,
        ..PhuxArrangementInfo::default()
    };
    // SAFETY: callers supply a live client on its owning thread and a writable result.
    assert_eq!(
        unsafe { phux_client_arrangement_info(client, &raw mut info) },
        PhuxClientResult::Ok,
        "arrangement_info"
    );
    info.status
}

async fn prove_empty_replace(
    consumer: &mut Consumer,
    proxy: &mut Proxy,
    agent_local: u32,
) -> (HostLog, (u64, u64)) {
    // Empty Replace is visible. An empty Append would be suppressible; this
    // first cut has no cursor, so the publication is a replacement.
    consumer
        .until("empty retained prefix", |client| {
            take_batches(client, agent_local)
                .iter()
                .any(|batch| batch.detail == AGENT_RECORDS_RETAINED && batch.records.is_empty())
        })
        .await;
    let mut log = HostLog { folded: Vec::new() };
    let empty = consumer.drain(agent_local, &mut log);
    assert!(
        empty.iter().any(|batch| {
            batch.detail == AGENT_RECORDS_RETAINED && batch.records.is_empty() && batch.seq == 0
        }),
        "empty Replace stays visible with seq 0: {empty:?}"
    );
    assert!(log.folded.is_empty(), "empty Replace clears the host log");
    let initial_generation = empty
        .iter()
        .find(|batch| batch.detail == AGENT_RECORDS_RETAINED)
        .map(|batch| (batch.stream_id, batch.bootstrap_id))
        .expect("empty replace generation");
    proxy
        .wait("initial agent ready", |event| match event {
            ProxyEvent::BootstrapReady { resource } if resource == agent_local => Some(()),
            _ => None,
        })
        .await;
    (log, initial_generation)
}

async fn prove_committed_prefix(
    producer: &mut Producer,
    consumer: &mut Consumer,
    parent: &ResourceId,
    agent: &ResourceId,
) {
    let agent_local = agent.local_id().expect("local agent id");
    let parent_local = parent.local_id().expect("local parent id");
    producer
        .append(agent, &["prefix-a", "prefix-b", "prefix-c"])
        .await;
    consumer
        .until("live prefix", |client| {
            take_batches(client, agent_local)
                .iter()
                .any(|batch| batch.records.iter().any(|record| record.seq == 3))
        })
        .await;
    // Committed and visible, but not drained: effect_clear is the only ACK.
    let peeked = consumer.peek_agent(agent_local);
    assert!(
        peeked.iter().any(|batch| batch.records.len() == 3),
        "prefix is staged before drain: {peeked:?}"
    );
    assert_eq!(
        peeked
            .iter()
            .flat_map(|batch| batch.records.iter().map(|record| record.seq))
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );

    // A later public poll feeds terminal output. That mutable feed must not
    // ACK or drop the uncleared agent prefix. The producer holds the input
    // lease so the paste actually reaches the PTY.
    let request_id = producer.id();
    let acquired = command(
        &mut producer.stream,
        request_id,
        Command::AcquireInput {
            terminal_id: parent.clone(),
            mode: InputMode::Seize,
            ttl_ms: 0,
        },
    )
    .await;
    assert!(
        matches!(acquired, CommandResult::Ok | CommandResult::OkWith(_)),
        "acquire input: {acquired:?}"
    );
    producer.paste(parent, "TERMFEED\n").await;
    consumer
        .until("terminal feed", |client| {
            grid_text(client, parent_local).contains("TERMFEED")
        })
        .await;
    let after_feed = consumer.peek_agent(agent_local);
    assert_eq!(
        after_feed
            .iter()
            .flat_map(|batch| batch.records.iter().map(|record| record.seq))
            .collect::<Vec<_>>(),
        vec![1, 2, 3],
        "terminal feed leaves the uncleared prefix staged"
    );
}

async fn prove_replayed_prefix(
    consumer: &mut Consumer,
    proxy: &mut Proxy,
    agent_local: u32,
    log: &mut HostLog,
) {
    // Loss after the prefix is committed and a later poll has fed terminal
    // output. `phux_client_effect_clear` is the only ACK, so this uncleared
    // view must be replayed exactly once. The wire cursor proves the runtime
    // committed the prefix; the empty resume must not be what fills the log.
    consumer.sever_and_reattach(proxy).await;
    let after_seq = proxy
        .wait("same-incarnation agent rebind", |event| match event {
            ProxyEvent::Attach {
                resource,
                after_seq,
            } if resource == agent_local => Some(after_seq),
            _ => None,
        })
        .await;
    assert_eq!(
        after_seq,
        Some(3),
        "rebind resumes from the committed cursor, not a full replay"
    );
    proxy
        .wait("resume bootstrap ready", |event| match event {
            ProxyEvent::BootstrapReady { resource } if resource == agent_local => Some(()),
            _ => None,
        })
        .await;
    consumer
        .until("catalog rebind published", |client| {
            take_batches(client, agent_local)
                .iter()
                .any(|batch| batch.records.iter().any(|record| record.seq == 3))
        })
        .await;
    let replayed = consumer.drain(agent_local, log);
    assert_unique(&replayed, "same-incarnation rebind");
    assert_eq!(
        log.seqs(),
        vec![1, 2, 3],
        "uncleared prefix is delivered exactly once after catalog rebind, not by the empty resume"
    );
    assert_eq!(log.marks(), vec!["prefix-a", "prefix-b", "prefix-c"]);
    println!("uncleared prefix resumed exactly once: {:?}", log.seqs());
}

async fn prove_resume_suffix(
    producer: &mut Producer,
    consumer: &mut Consumer,
    proxy: &mut Proxy,
    agent: &ResourceId,
    log: &mut HostLog,
) {
    let agent_local = agent.local_id().expect("local agent id");
    // Wire resume of records appended after the committed cursor.
    // SAFETY: this consumer owns the live client on this thread.
    let previous_epoch = unsafe { phux_client_connection_epoch(consumer.client) };
    proxy.sever_now();
    proxy
        .wait("suffix disconnect", |event| {
            matches!(event, ProxyEvent::Severed).then_some(())
        })
        .await;
    producer.append(agent, &["resume-d", "resume-e"]).await;
    consumer.reconnect_after(previous_epoch).await;
    let suffix_cursor = proxy
        .wait("suffix rebind", |event| match event {
            ProxyEvent::Attach {
                resource,
                after_seq,
            } if resource == agent_local => Some(after_seq),
            _ => None,
        })
        .await;
    assert_eq!(suffix_cursor, Some(3));
    consumer
        .until("resume suffix", |client| {
            take_batches(client, agent_local)
                .iter()
                .any(|batch| batch.records.iter().any(|record| record.seq == 5))
        })
        .await;
    let suffix = consumer.drain(agent_local, log);
    assert_unique(&suffix, "wire resume");
    assert!(
        suffix
            .iter()
            .all(|batch| batch.detail == AGENT_RECORDS_LIVE),
        "a resume suffix appends: {suffix:?}"
    );
    assert_eq!(log.seqs(), vec![1, 2, 3, 4, 5]);
    println!("wire suffix appended from cursor 3: {:?}", log.seqs());
}

async fn prove_empty_resume_and_close(
    producer: &mut Producer,
    consumer: &mut Consumer,
    proxy: &mut Proxy,
    agent: &ResourceId,
    log: &mut HostLog,
    initial_generation: (u64, u64),
) {
    let agent_local = agent.local_id().expect("local agent id");
    // Empty resume, then live output and closure. The empty Append is not a
    // second copy; closure still carries the generation the live output advanced.
    consumer.sever_and_reattach(proxy).await;
    let empty_cursor = proxy
        .wait("empty resume cursor", |event| match event {
            ProxyEvent::Attach {
                resource,
                after_seq,
            } if resource == agent_local => Some(after_seq),
            _ => None,
        })
        .await;
    assert_eq!(empty_cursor, Some(5));
    proxy
        .wait("empty resume ready", |event| match event {
            ProxyEvent::BootstrapReady { resource } if resource == agent_local => Some(()),
            _ => None,
        })
        .await;
    consumer
        .until("empty resume settled", |client| !unsafe {
            phux_client_poll_pending(client)
        })
        .await;
    let empty_resume = consumer.drain(agent_local, log);
    assert!(
        empty_resume.iter().all(|batch| batch.records.is_empty()),
        "empty resume does not repeat committed records: {empty_resume:?}"
    );
    assert_eq!(log.seqs(), vec![1, 2, 3, 4, 5]);

    producer.append(agent, &["live-f"]).await;
    consumer
        .until("live output", |client| {
            take_batches(client, agent_local)
                .iter()
                .any(|batch| batch.records.iter().any(|record| record.mark == "live-f"))
        })
        .await;
    let live = consumer.drain(agent_local, log);
    let live_batch = live
        .iter()
        .find(|batch| batch.records.iter().any(|record| record.mark == "live-f"))
        .expect("live batch");
    assert_eq!(live_batch.detail, AGENT_RECORDS_LIVE);
    assert_eq!(live_batch.seq, 6);
    assert_ne!(
        (live_batch.stream_id, live_batch.bootstrap_id),
        initial_generation,
        "live output after resume carries the advanced generation"
    );
    let live_generation = (live_batch.stream_id, live_batch.bootstrap_id);
    assert_eq!(log.seqs(), vec![1, 2, 3, 4, 5, 6]);

    producer.kill(agent).await;
    consumer
        .until("closure", |client| {
            take_batches(client, agent_local)
                .iter()
                .any(|batch| batch.detail == AGENT_RECORDS_CLOSED)
        })
        .await;
    let closed = take_batches(consumer.client, agent_local);
    let closed = closed
        .iter()
        .find(|batch| batch.detail == AGENT_RECORDS_CLOSED)
        .expect("closed");
    assert!(closed.records.is_empty(), "closure has no records");
    assert_eq!(closed.seq, 0, "closure seq stays 0");
    assert_eq!(
        (closed.stream_id, closed.bootstrap_id),
        live_generation,
        "closure keeps the watermark the empty resume advanced"
    );
    assert_eq!(
        unsafe { phux_client_effect_clear(consumer.client) },
        PhuxClientResult::Ok
    );
    consumer
        .until("agent left the catalog", |client| {
            !resource_ids(client)
                .into_iter()
                .any(|(kind, id)| kind == RESOURCE_KIND_AGENT_SESSION && id == agent_local)
        })
        .await;
}
