//! Submodule for runtime internals.

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use phux_core::TerminalId;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ClientCapabilities,
};
use phux_protocol::ids::{BootstrapId, GroupId, StreamId};
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, ErrorCode, FrameKind, MAX_AGENT_SESSION_RECORD_BYTES, MoveError,
    MoveResult, SpawnError, SpawnResult,
};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, trace, warn};

use super::{
    SpawnOwnership, broadcast_event, prepare_attach, seed_session_with_actor,
    seed_session_with_pty_and_colors, send_error, spawn_pane_with_pty_and_colors,
};
use crate::state::{AttachSnapshotPane, ClientId, Outbound, SharedState};
use crate::terminal_actor::{
    ConsumerAttachRequest, ConsumerDetachRequest, PaneOutput, PwdRequest, ResizeRequest,
    SetDefaultColorsRequest, SnapshotRequest,
};

/// Adapt a broadcast byte chunk to a client's capabilities for the wire:
/// a capable client gets the refcounted bytes verbatim (no copy); an
/// incapable one gets an SGR-downsampled rewrite. Shared by both output
/// pumps (the attach pump and the `SPAWN_TERMINAL` pump).
pub(crate) fn downsample_for_caps(
    bytes: &bytes::Bytes,
    caps: phux_protocol::ClientCapabilities,
) -> bytes::Bytes {
    if crate::downsample::caps_pass_through(caps) {
        bytes.clone()
    } else {
        crate::downsample::rewrite_bytes_with_caps(bytes, caps).into()
    }
}

fn bootstrap_source_ceiling(
    remaining_bytes: usize,
    caps: phux_protocol::ClientCapabilities,
) -> usize {
    if crate::downsample::caps_pass_through(caps) {
        remaining_bytes
    } else {
        // During adaptation the source and one equally bounded output Vec are
        // simultaneously live. The rewriter has no other payload-sized heap
        // scratch, so half the connection budget is the exact source ceiling.
        remaining_bytes / 2
    }
}

#[derive(Debug)]
struct AdaptedBootstrap {
    payloads: Vec<bytes::Bytes>,
    retained_bytes: usize,
    peak_bytes: usize,
}

fn adapt_bootstrap_snapshot(
    snapshot: crate::grid::SnapshotBytes,
    caps: phux_protocol::ClientCapabilities,
    peak_budget: usize,
) -> Result<AdaptedBootstrap, ()> {
    let sources = [snapshot.scrollback, snapshot.bytes];
    let mut remaining_source = sources
        .iter()
        .try_fold(0_usize, |total, source| {
            total.checked_add(source.capacity())
        })
        .ok_or(())?;
    let passthrough = crate::downsample::caps_pass_through(caps);
    if remaining_source > bootstrap_source_ceiling(peak_budget, caps) {
        return Err(());
    }
    let mut peak_bytes = remaining_source;

    let mut retained_output = 0_usize;
    let mut payloads = Vec::new();
    payloads.try_reserve(2).map_err(|_| ())?;
    for source in sources {
        if source.is_empty() {
            remaining_source = remaining_source.checked_sub(source.capacity()).ok_or(())?;
            continue;
        }
        let source_capacity = source.capacity();
        let (output, output_allocation) = if passthrough {
            (bytes::Bytes::from(source), source_capacity)
        } else {
            let rewritten = crate::downsample::rewrite_bytes_with_caps(&source, caps);
            let output_allocation = rewritten.capacity();
            if output_allocation > source_capacity {
                return Err(());
            }
            let peak = retained_output
                .checked_add(remaining_source)
                .and_then(|bytes| bytes.checked_add(output_allocation))
                .ok_or(())?;
            if peak > peak_budget {
                return Err(());
            }
            peak_bytes = peak_bytes.max(peak);
            drop(source);
            (bytes::Bytes::from(rewritten), output_allocation)
        };
        remaining_source = remaining_source.checked_sub(source_capacity).ok_or(())?;
        retained_output = retained_output.checked_add(output_allocation).ok_or(())?;
        payloads.push(output);
    }
    Ok(AdaptedBootstrap {
        payloads,
        retained_bytes: retained_output,
        peak_bytes,
    })
}

pub(crate) const fn bootstrap_stream_profile(
    profile: BootstrapProfile,
) -> Option<BootstrapStreamProfile> {
    match profile {
        BootstrapProfile::NativeState { codec, .. } => {
            Some(BootstrapStreamProfile::NativeState { codec })
        }
        BootstrapProfile::SynthesizedVtStateSync => {
            Some(BootstrapStreamProfile::SynthesizedVtStateSync)
        }
        BootstrapProfile::SynthesizedVtRaw => Some(BootstrapStreamProfile::SynthesizedVtRaw),
        _ => None,
    }
}

pub(crate) const fn stream_id_from(raw: u64) -> StreamId {
    match StreamId::new(raw.saturating_add(1)) {
        Some(id) => id,
        None => unreachable!(),
    }
}

pub(crate) const fn initial_bootstrap_id() -> BootstrapId {
    match BootstrapId::new(1) {
        Some(id) => id,
        None => unreachable!(),
    }
}

pub(crate) const fn next_bootstrap_id(id: BootstrapId) -> BootstrapId {
    let raw = match id.get().checked_add(1) {
        Some(raw) => raw,
        None => 1,
    };
    match BootstrapId::new(raw) {
        Some(next) => next,
        None => unreachable!(),
    }
}

struct OutputPumpStart {
    published_cut: u64,
    replay: Vec<(u64, bytes::Bytes)>,
    live: Option<tokio::sync::broadcast::Receiver<PaneOutput>>,
}

struct SnapshotGate {
    terminal_id: TerminalId,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    wire_terminal_id: phux_protocol::ids::TerminalId,
    handle: crate::terminal_actor::TerminalHandle,
    gate: oneshot::Sender<OutputPumpStart>,
    cut: Option<u64>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_cursor: Option<crate::native_state::OpaqueHistoryCursor>,
}

/// Connection-wide retention ceiling for an aggregate ATTACH preflight.
///
/// A session can contain many panes, but the server must hold every pane's
/// complete bootstrap until the atomic publication cut. Keep that aggregate no
/// larger than one maximally bounded native prefix rather than multiplying the
/// per-pane allowance by the pane count.
const MAX_STAGED_BOOTSTRAP_BYTES: usize = 64 * 1024 * 1024;
const MAX_STAGED_BOOTSTRAP_FRAMES: usize = 4_096 + 2;

/// Maximum pane sources admitted to one aggregate bootstrap.
/// Every supported profile consumes at least `BEGIN`, one opaque `CHUNK`, and
/// `READY`, so a larger source set cannot fit the connection-wide frame budget.
/// The preflight runs before the session snapshot and pane-handle vectors are
/// allocated.
pub(crate) const MAX_AGGREGATE_BOOTSTRAP_PANES: usize = MAX_STAGED_BOOTSTRAP_FRAMES / 3;

#[derive(Debug)]
struct BootstrapStagingBudget {
    max_bytes: usize,
    max_frames: usize,
    staged_bytes: usize,
    staged_frames: usize,
}

impl BootstrapStagingBudget {
    const fn new() -> Self {
        Self::with_limits(MAX_STAGED_BOOTSTRAP_BYTES, MAX_STAGED_BOOTSTRAP_FRAMES)
    }

    const fn with_limits(max_bytes: usize, max_frames: usize) -> Self {
        Self {
            max_bytes,
            max_frames,
            staged_bytes: 0,
            staged_frames: 0,
        }
    }

    const fn remaining_bytes(&self) -> usize {
        self.max_bytes.saturating_sub(self.staged_bytes)
    }

    const fn remaining_frames(&self) -> usize {
        self.max_frames.saturating_sub(self.staged_frames)
    }

    #[cfg(test)]
    fn append(
        &mut self,
        staged: &mut Vec<FrameKind>,
        incoming: &mut Vec<FrameKind>,
    ) -> Result<(), ()> {
        let incoming_bytes = incoming
            .iter()
            .try_fold(0_usize, |total, frame| {
                total.checked_add(match frame {
                    FrameKind::BootstrapChunk { payload, .. } => payload.len(),
                    FrameKind::BootstrapReady { history_cursor, .. } => {
                        history_cursor.as_ref().map_or(0, bytes::Bytes::len)
                    }
                    _ => 0,
                })
            })
            .ok_or(())?;
        self.append_accounted(staged, incoming, incoming_bytes)
    }

    fn append_accounted(
        &mut self,
        staged: &mut Vec<FrameKind>,
        incoming: &mut Vec<FrameKind>,
        incoming_bytes: usize,
    ) -> Result<(), ()> {
        let incoming_frames = incoming.len();
        let next_frames = self.staged_frames.checked_add(incoming_frames).ok_or(())?;
        let next_bytes = self.staged_bytes.checked_add(incoming_bytes).ok_or(())?;
        if next_frames > self.max_frames || next_bytes > self.max_bytes {
            return Err(());
        }
        staged.try_reserve(incoming_frames).map_err(|_| ())?;
        staged.append(incoming);
        self.staged_frames = next_frames;
        self.staged_bytes = next_bytes;
        Ok(())
    }
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(crate) async fn publish_native_bootstrap(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    reply: crate::terminal_actor::NativeBootstrapReply,
) -> Result<(u64, crate::native_state::OpaqueHistoryCursor), ()> {
    let cut = reply.base_seq;
    let cursor = reply.publication_cursor;
    for frame in reply.frames {
        out_tx.send(Outbound::Frame(frame)).await.map_err(|_| ())?;
    }
    Ok((cut, cursor))
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(crate) async fn activate_native_publication(
    handle: &crate::terminal_actor::TerminalHandle,
    owner: u64,
    terminal_id: phux_protocol::ids::TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    cursor: crate::native_state::OpaqueHistoryCursor,
) -> Result<crate::terminal_actor::NativePublicationReply, ()> {
    let (reply, publication) = oneshot::channel();
    handle
        .native_publication
        .send(crate::terminal_actor::NativePublicationRequest {
            owner,
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reply,
        })
        .await
        .map_err(|_| ())?;
    publication.await.map_err(|_| ())?.map_err(|_| ())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn synthesized_bootstrap_frames(
    terminal_id: phux_protocol::ids::TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    profile: BootstrapStreamProfile,
    limits: BootstrapLimits,
    cols: u16,
    rows: u16,
    base_seq: u64,
    payloads: impl IntoIterator<Item = bytes::Bytes>,
) -> Result<Vec<FrameKind>, ()> {
    let mut frames = Vec::new();
    frames.try_reserve(2).map_err(|_| ())?;
    frames.push(FrameKind::BootstrapBegin {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        profile,
        cols,
        rows,
        base_seq,
    });
    let max_chunk = usize::try_from(limits.max_chunk_bytes()).map_err(|_| ())?;
    if max_chunk == 0 {
        return Err(());
    }
    let mut chunk_seq = 0_u32;
    for payload in payloads {
        let mut offset = 0_usize;
        while offset < payload.len() {
            let end = offset.saturating_add(max_chunk).min(payload.len());
            frames.try_reserve(1).map_err(|_| ())?;
            frames.push(FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload: payload.slice(offset..end),
            });
            chunk_seq = chunk_seq.checked_add(1).ok_or(())?;
            offset = end;
        }
    }
    frames.try_reserve(1).map_err(|_| ())?;
    frames.push(FrameKind::BootstrapReady {
        terminal_id,
        stream_id,
        bootstrap_id,
        history_cursor: None,
    });
    Ok(frames)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_synthesized_bootstrap(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    terminal_id: phux_protocol::ids::TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    profile: BootstrapStreamProfile,
    limits: BootstrapLimits,
    cols: u16,
    rows: u16,
    base_seq: u64,
    payloads: impl IntoIterator<Item = bytes::Bytes>,
) -> Result<(), ()> {
    for frame in synthesized_bootstrap_frames(
        terminal_id,
        stream_id,
        bootstrap_id,
        profile,
        limits,
        cols,
        rows,
        base_seq,
        payloads,
    )? {
        out_tx.send(Outbound::Frame(frame)).await.map_err(|_| ())?;
    }
    Ok(())
}

/// Queue the mandatory in-band resync after a broadcast gap.
///
/// The output pump awaits mailbox capacity and therefore cannot consume or
/// forward a later delta until the actor has accepted the resync request.
/// A closed or persistently full actor mailbox fails boundedly.
pub(crate) async fn enqueue_output_resync(
    resize: &tokio::sync::mpsc::Sender<ResizeRequest>,
) -> bool {
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            resize.send(ResizeRequest {
                cols: 0,
                rows: 0,
                cell_px: None,
                resync_clients: true,
                resync_only: true,
            }),
        )
        .await,
        Ok(Ok(()))
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "single rollback boundary deliberately receives every staged and committed resource so cancellation, producer detach, pump abortion, and the fatal sentinel remain strictly ordered"
)]
async fn fail_aggregate_attach_prepublication(
    state: &SharedState,
    client_id: ClientId,
    attach_id: u32,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    connection_token: &CancellationToken,
    staged_handles: &[crate::terminal_actor::TerminalHandle],
    staged_pumps: &mut JoinSet<()>,
    committed_pumps: &mut JoinSet<()>,
    reason: &str,
) {
    staged_pumps.abort_all();
    while staged_pumps.join_next().await.is_some() {}
    super::client::abort_output_pumps(committed_pumps, client_id, "failed ATTACH").await;

    let wire_client_id =
        phux_protocol::ids::ClientId::new(u32::try_from(client_id.0).unwrap_or(u32::MAX));
    let producer_deadline = std::time::Duration::from_secs(1);
    for handle in staged_handles {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            let _ = tokio::time::timeout(
                producer_deadline,
                handle
                    .native_release
                    .send(crate::terminal_actor::NativeReleaseRequest { owner: client_id.0 }),
            )
            .await;
        }
        let (reply, done) = oneshot::channel();
        if matches!(
            tokio::time::timeout(
                producer_deadline,
                handle.consumer_detach.send(ConsumerDetachRequest {
                    client_id: wire_client_id,
                    reply,
                }),
            )
            .await,
            Ok(Ok(()))
        ) {
            let _ = tokio::time::timeout(producer_deadline, done).await;
        }
    }
    crate::runtime::client::detach_and_release_consumer_state(state, client_id);

    // Queue one ordered terminal sentinel after rollback. Even if an old
    // state-sync producer survives its bounded detach acknowledgement and
    // races another frame, the writer closes immediately after this ERROR and
    // discards everything behind it.
    if !matches!(
        tokio::time::timeout(
            producer_deadline,
            out_tx.send(Outbound::TerminalError {
                request_id: None,
                code: ErrorCode::CodecUnavailable,
                message: format!("ATTACH {attach_id} failed before publication: {reason}"),
            }),
        )
        .await,
        Ok(Ok(()))
    ) {
        warn!(client = ?client_id, attach_id, "failed to enqueue terminal ATTACH error");
    }
    connection_token.cancel();
}

/// Tuple bundling everything `handle_attach` needs after it is done
/// touching [`ServerState`]. Cloned out of the critical section so the
/// remaining awaits do not hold the state lock.
pub(crate) type AttachPrepared = (
    phux_protocol::wire::info::SessionSnapshot,
    phux_protocol::ids::ClientId,
    Vec<AttachSnapshotPane>,
);

/// Resolve `target` to a session name. SPEC §13: `ByName` is the only
/// fully-implemented mode in byc.8; the others fail with
/// `SessionNotFound` until follow-up tickets land.
pub(crate) async fn resolve_attach_target(
    state: &SharedState,
    target: AttachTarget,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Option<String> {
    match target {
        AttachTarget::ByName(name) => Some(name),
        AttachTarget::ById(id) => {
            let resolved = state
                .with(|s| s.idspace.resolve_session(id))
                .and_then(|sid| {
                    state.with(|s| s.registry().session(sid).map(|sess| sess.name.clone()))
                });
            if resolved.is_none() {
                send_error(
                    out_tx,
                    ErrorCode::SessionNotFound,
                    &format!("session id {} not found", id.get()),
                )
                .await;
            }
            resolved
        }
        AttachTarget::Last => {
            // Resolve against the global per-server "last touched
            // session" order (see ServerState::touch_session). If a
            // prior touch exists and that session is still live in the
            // registry, return its name; otherwise treat as "not found"
            // — matches SPEC §13's allowance that "implementations
            // without prior-attach memory MAY return SESSION_NOT_FOUND".
            // We follow the same code path when the prior session has
            // been killed since the last touch.
            //
            // TODO(error-codes): introduce ErrorCode::NoLastSession
            // (and a sibling variant for "last session killed") so
            // clients can distinguish "no history" from "history is
            // stale" without parsing the message string. Additive
            // ErrorCode work is intentionally out of scope here.
            let resolved = state.with(|s| {
                s.most_recently_touched_session()
                    .and_then(|sid| s.registry().session(sid).map(|sess| sess.name.clone()))
            });
            if resolved.is_none() {
                send_error(
                    out_tx,
                    ErrorCode::SessionNotFound,
                    "no prior session activity: AttachTarget::Last has nothing to resolve",
                )
                .await;
            }
            resolved
        }
        AttachTarget::CreateIfMissing { name, command, cwd } => {
            resolve_create_if_missing(
                state,
                name,
                command,
                cwd,
                out_tx,
                root_token,
                default_colors,
            )
            .await
        }
        _ => {
            send_error(
                out_tx,
                ErrorCode::SessionNotFound,
                "unknown AttachTarget variant",
            )
            .await;
            None
        }
    }
}

/// Handle [`AttachTarget::CreateIfMissing`] (phux-k61.3, SPEC §13).
///
/// Behavior:
///
/// * If a session with `name` already exists in the registry, return
///   its name unchanged — the caller's `prepare_attach` then runs the
///   normal `ByName` attach path against it. No duplicate session is
///   created.
/// * Otherwise, seed a fresh `(session, window, pane)` triple, spawn
///   the seed pane's actor in the mode the server was configured
///   with (PTY-backed via [`seed_session_with_pty`] when
///   [`crate::state::ServerState::attach_create_seeds_pty`] is `true`,
///   or no-PTY via [`seed_session_with_actor`] otherwise), and return
///   the name so the caller proceeds with the normal attach path.
///
/// `command` from the wire frame is honored only when the PTY mode is
/// on AND no explicit
/// [`crate::state::ServerState::attach_create_seed_command`] preempts
/// it: an explicit per-server seed command always wins (it's how the
/// `phux server` binary pins the default-shell command for the user).
/// `cwd` from the wire frame (phux-3mtf) seeds the PTY child's working
/// directory when it names an existing directory on the server host; a
/// missing or non-directory path falls back to the pre-existing
/// behavior (the builder's cwd stays unset, so the spawn lands where a
/// `cwd: None` spawn would) rather than failing the attach — the
/// client's idea of a path may be stale or belong to another host. A
/// cwd already set on the server-wide override command is never
/// clobbered. The no-PTY path ignores both, matching the existing
/// `seed_session_with_actor` shape.
///
/// On terminal-actor spawn failure (e.g. PTY allocation fails on a
/// host with no remaining ptys), emits a `SessionNotFound` error
/// frame (mirroring how the pre-seed path logs-and-continues at
/// startup) and returns `None` so the attach fails atomically. We
/// reuse `SessionNotFound` rather than introducing a new error code:
/// the user-visible effect is "the requested session is not available
/// to attach to", which is what `SessionNotFound` already means on
/// the wire. A richer error code (e.g. `SessionCreateFailed`) is a
/// SPEC-level follow-up.
pub(crate) async fn resolve_create_if_missing(
    state: &SharedState,
    name: String,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Option<String> {
    // Fast path: a session with this name already exists. Fall through
    // to the normal `ByName(name)` attach by returning `name` as-is.
    // The lookup is read-only so we hold only an immutable borrow.
    if state.with(|s| s.session_by_name(&name).is_some()) {
        debug!(session = %name, "CreateIfMissing: session already exists, attaching");
        return Some(name);
    }

    // Slow path: create the session + seed pane. Snapshot the server's
    // configured PTY mode and (optional) override command before
    // releasing the state borrow.
    let (with_pty, override_cmd, history_limit, term, shell, login_shell) = state.with(|s| {
        (
            s.attach_create_seeds_pty(),
            s.attach_create_seed_command(),
            s.history_limit(),
            s.term().to_owned(),
            s.shell().to_owned(),
            s.login_shell(),
        )
    });

    let seed_result = if with_pty {
        // Resolve the command. Precedence:
        //   1. The server-wide override stashed via
        //      `set_attach_create_pty(_, Some(cmd))`. Set explicitly by
        //      the runtime (or by tests that want a deterministic
        //      child like `cat`).
        //   2. The wire-level `command` from the CreateIfMissing
        //      variant. This is the per-attach command knob clients
        //      use to spawn (e.g.) `phux new -- vim foo.txt`.
        //   3. `default_shell_command` over the resolved default shell
        //      (`defaults.shell` → `$SHELL` → `/bin/sh`, phux-i0e8.4.1)
        //      — same fallback the pre-seed path uses.
        let mut seed_cmd = override_cmd.unwrap_or_else(|| match command {
            Some(argv) if !argv.is_empty() => {
                let mut head = argv.into_iter();
                // Safe: argv is non-empty here.
                let program = head.next().unwrap_or_default();
                let mut builder = portable_pty::CommandBuilder::new(program);
                for arg in head {
                    builder.arg(arg);
                }
                builder
            }
            _ => crate::terminal_actor::default_shell_command(&shell, login_shell),
        });
        // phux-3mtf / phux-0v1l: honor the wire `cwd` through the shared
        // validate-and-fall-back helper, uniform with the
        // `SESSION_CREATE_KEY` create-without-attach path. The wire cwd is
        // applied only over a cwd-less builder (a server-wide override's cwd
        // wins wholesale), honored only when it names an existing, enterable
        // directory, and dropped with a warn otherwise — never failing the
        // attach. The stamp in `seed_session_with_pty_and_colors` reads the
        // builder's cwd back (`spawn_cwd_of`), so the honored value also
        // lands on the pane's registry descriptor for the ATTACHED snapshot.
        crate::terminal_actor::apply_spawn_cwd(&mut seed_cmd, cwd.as_deref(), &name);
        // Apply the server-wide `defaults.term` (phux-ign); this overrides
        // whatever baseline the builder carried.
        crate::terminal_actor::apply_term(&mut seed_cmd, &term);
        seed_session_with_pty_and_colors(
            state,
            &name,
            seed_cmd,
            history_limit,
            root_token,
            default_colors,
        )
    } else {
        // No-PTY path: the wire `command` is meaningless without a
        // child to exec it on. We still create the session+pane so
        // the snapshot path has a target — this is the shape every
        // existing `spawn_server` test uses.
        seed_session_with_actor(state, &name, history_limit, root_token)
    };

    if let Err(err) = seed_result {
        warn!(
            session = %name,
            error = %err,
            "CreateIfMissing: failed to spawn pane actor for newly-created session",
        );
        send_error(
            out_tx,
            ErrorCode::SessionNotFound,
            &format!("CreateIfMissing: failed to create session {name:?}: {err}"),
        )
        .await;
        return None;
    }

    debug!(
        session = %name,
        pty = with_pty,
        "CreateIfMissing: created session and seeded pane"
    );
    Some(name)
}

/// Resolve a freshly-spawned pane's working directory from
/// `defaults.cwd-inheritance` (phux-cs6) when the `SPAWN_TERMINAL` wire
/// frame left `cwd` unset.
///
/// Returns the directory to seed the new pane's `CommandBuilder.cwd`
/// with, or `None` to inherit the server process's CWD (no override) —
/// the same effect the wire-`cwd = None` path had before this policy
/// existed.
///
/// Policy mapping:
/// * [`InheritFocused`](phux_config::CwdInheritance::InheritFocused) —
///   look up the spawning client's focused pane and ask its actor for
///   the live PTY CWD (a kernel query on the PTY child, see
///   [`crate::cwd_query`]). `None` when the client is not attached, has
///   no focused pane, the pane has no live handle, or the query is
///   unsupported/denied — each falls through to no override.
/// * [`Home`](phux_config::CwdInheritance::Home) — `$HOME`, or `None`
///   when unset.
/// * [`SessionRoot`](phux_config::CwdInheritance::SessionRoot) — the
///   session's creation directory: the live CWD of the session's seed
///   (oldest) pane, captured once and frozen in
///   [`crate::state::ServerState::record_session_root`] so a later `cd`
///   in the seed pane does not move the root. `None` when the client is
///   not attached, the session has no live seed pane, or the query is
///   unsupported/denied (with no previously frozen value to fall back on).
/// * [`LastCwdPerWindow`](phux_config::CwdInheritance::LastCwdPerWindow) —
///   the most-recent CWD observed in the spawning client's active window.
///   Resolved from the active pane's live CWD, recorded into
///   [`crate::state::ServerState::record_window_last_cwd`], and reused as
///   the fallback when a subsequent live query fails. `None` when there is
///   no active window and nothing was ever recorded.
pub(crate) async fn resolve_inherited_cwd(
    state: &SharedState,
    client_id: ClientId,
) -> Option<String> {
    let mode = state.with(crate::state::ServerState::cwd_inheritance);
    match mode {
        phux_config::CwdInheritance::InheritFocused => {
            // Find the spawning client's focused pane's actor handle in a
            // single critical section, then query it off-lock (the actor
            // runs on the same LocalSet; `with` must not be held across
            // the await).
            let handle = state.with(|s| {
                let session = s.attached().get(&client_id)?.session;
                let focused = s.active_pane_of_session(session)?;
                s.terminal_handle(focused).cloned()
            })?;
            query_pane_cwd(handle).await
        }
        phux_config::CwdInheritance::Home => std::env::var("HOME").ok().filter(|h| !h.is_empty()),
        phux_config::CwdInheritance::SessionRoot => {
            // The session root is the seed pane's directory at session
            // creation, frozen on first observation. Query the seed pane
            // live; if a root was already frozen, reuse it (and the live
            // query is redundant). The freeze happens in `with_mut` after
            // the off-lock query so a concurrent spawn cannot move it.
            let (session, handle) = state.with(|s| {
                let session = s.attached().get(&client_id)?.session;
                if let Some(root) = s.session_root(session) {
                    // Already frozen — return it without a live query.
                    return Some((session, FrozenOrQuery::Frozen(path_to_string(root)?)));
                }
                let seed = s.seed_pane_of_session(session)?;
                let handle = s.terminal_handle(seed).cloned()?;
                Some((session, FrozenOrQuery::Query(handle)))
            })?;
            match handle {
                FrozenOrQuery::Frozen(root) => Some(root),
                FrozenOrQuery::Query(handle) => {
                    let resolved = query_pane_cwd(handle).await?;
                    // Freeze the first observed root; reuse any value a
                    // racing spawn already inserted.
                    let frozen = state.with_mut(|s| {
                        path_to_string(
                            s.record_session_root(session, std::path::PathBuf::from(&resolved)),
                        )
                    });
                    frozen.or(Some(resolved))
                }
            }
        }
        phux_config::CwdInheritance::LastCwdPerWindow => {
            // Resolve the active window and its active pane's handle. If the
            // window has no live active pane, fall back to the last value we
            // recorded for that window.
            let (window, handle) = state.with(|s| {
                let session = s.attached().get(&client_id)?.session;
                let window = s.active_window_of_session(session)?;
                let handle = s
                    .active_pane_of_session(session)
                    .and_then(|p| s.terminal_handle(p).cloned());
                Some((window, handle))
            })?;
            let resolved = match handle {
                Some(handle) => query_pane_cwd(handle).await,
                None => None,
            };
            if let Some(cwd) = resolved {
                // Record the freshly observed CWD and seed the new pane with
                // it.
                state.with_mut(|s| {
                    s.record_window_last_cwd(window, std::path::PathBuf::from(&cwd));
                });
                return Some(cwd);
            }
            // Live query unavailable — reuse the most recent recorded value
            // for this window, if any.
            state.with(|s| s.window_last_cwd(window).and_then(|p| path_to_string(p)))
        }
    }
}

/// Either a directory already frozen as a session root or the actor handle
/// to query for it. Lets `resolve_inherited_cwd` decide whether a live PTY
/// query is needed inside a single `with` critical section without holding
/// the lock across the `await`.
pub(crate) enum FrozenOrQuery {
    Frozen(String),
    Query(crate::terminal_actor::TerminalHandle),
}

/// Render `path` as a UTF-8 string, or `None` if it is not valid UTF-8 — the
/// wire `cwd` and `CommandBuilder.cwd` plumbing are string-based, so a
/// non-UTF-8 directory simply yields no override.
pub(crate) fn path_to_string(path: &std::path::Path) -> Option<String> {
    path.to_str().map(ToOwned::to_owned)
}

/// Ask `handle`'s actor for its live PTY child CWD (a kernel query, see
/// [`crate::cwd_query`]). `None` when the actor has gone away or the query
/// is unsupported/denied. The handle must be cloned out of state before the
/// call: `with` must not be held across the `await`.
pub(crate) async fn query_pane_cwd(
    handle: crate::terminal_actor::TerminalHandle,
) -> Option<String> {
    let (reply, rx) = tokio::sync::oneshot::channel();
    handle.pwd.send(PwdRequest { reply }).await.ok()?;
    rx.await.ok().flatten()
}

/// Refresh every live pane's registry `cwd` from its PTY child's kernel
/// CWD (phux-p4vp).
///
/// `TerminalDescriptor.cwd` is stamped once at spawn time (see
/// `stamp_spawn_cwd` in `runtime::commands`) and would otherwise go stale
/// as soon as the shell `cd`s. `handle_attach` calls this right before
/// `prepare_attach` builds the `ATTACHED` snapshot, so
/// `SessionSnapshot.panes[].cwd` reflects each pane's *current* directory
/// — the TUI sidebar derives its per-window VCS branch line from it.
///
/// Best-effort per pane: a dead child, an unsupported platform, or a
/// vanished actor leaves that pane's stamped value untouched. Queries fan
/// out concurrently (same `FuturesUnordered` rationale as the snapshot
/// fan-out below: attach latency scales with the MAX pane reply time, not
/// the SUM) and the whole drain is capped by [`CWD_REFRESH_DEADLINE`]:
/// an actor that never services its `pwd` mailbox (wedged, or a
/// synthetic test handle) must not stall the `ATTACHED` frame. Panes
/// whose replies miss the deadline keep their stamped spawn-time value;
/// replies that landed before it still apply. Handles are cloned out of
/// state first — `with` must not be held across an await.
pub(crate) async fn refresh_registry_cwds(state: &SharedState) {
    /// Upper bound on the attach-time kernel-cwd fan-out. Real actors
    /// answer a `PwdRequest` in well under a millisecond (one kernel
    /// call, no PTY I/O), so this only ever fires for a wedged or
    /// mock actor — where waiting longer buys nothing and every 100ms
    /// visibly delays the attacher's first paint.
    const CWD_REFRESH_DEADLINE: std::time::Duration = std::time::Duration::from_millis(250);

    let handles: Vec<(TerminalId, crate::terminal_actor::TerminalHandle)> =
        state.with(crate::state::ServerState::all_terminal_handles);
    if handles.is_empty() {
        return;
    }
    let mut queries: FuturesUnordered<_> = handles
        .into_iter()
        .map(|(id, handle)| async move { (id, query_pane_cwd(handle).await) })
        .collect();
    let mut resolved: Vec<(TerminalId, std::path::PathBuf)> = Vec::new();
    let drain = async {
        while let Some((id, cwd)) = queries.next().await {
            if let Some(cwd) = cwd {
                resolved.push((id, std::path::PathBuf::from(cwd)));
            }
        }
    };
    if tokio::time::timeout(CWD_REFRESH_DEADLINE, drain)
        .await
        .is_err()
    {
        debug!("attach cwd refresh hit deadline; using stamped values for stragglers");
    }
    if resolved.is_empty() {
        return;
    }
    state.with_mut(|s| {
        for (id, cwd) in resolved {
            if let Some(desc) = s.registry_mut().terminal_mut(id) {
                desc.cwd = cwd;
            }
        }
    });
}

/// The decoded `SPAWN_TERMINAL` payload, bundled 1:1 with the wire frame
/// (minus `request_id`, threaded separately like every reply-correlated
/// handler). Keeps [`handle_spawn_terminal`]'s signature stable as the
/// frame grows additive fields (`term` — phux-ign, `satellite` —
/// phux-v45.6).
#[derive(Debug)]
pub(crate) struct SpawnRequest {
    /// Group under which to spawn (v0.1 servers expose `GroupId(1)`).
    pub(crate) group: GroupId,
    /// Command + argv, or `None` for the server's default shell.
    pub(crate) command: Option<Vec<String>>,
    /// Working directory, or `None` for the server's default policy.
    pub(crate) cwd: Option<String>,
    /// Environment pairs, `None` = inherit the server's environment.
    pub(crate) env: Option<Vec<(String, String)>>,
    /// First-class `TERM` override (phux-ign).
    pub(crate) term: Option<String>,
    /// Satellite host to route the spawn to (phux-v45.6), `None` = local.
    pub(crate) satellite: Option<phux_protocol::ids::SatelliteHost>,
    /// Existing local Terminal whose exact window must own the new pane.
    pub(crate) owner_terminal: Option<phux_protocol::ids::TerminalId>,
    /// Opaque native agent-session provenance to install before publication.
    pub(crate) agent_session: Option<Vec<u8>>,
}

/// Relay one satellite-addressed spawn over the owning hub link
/// (phux-v45.6, L1 §3.1 / §9.1) and return the re-tagged result. A
/// missing route — non-hub server, or `host` absent from the hub's
/// registry — is the typed configuration refusal; an unreachable
/// satellite fails fast inside [`crate::hub::relay::RelayHandle::spawn`].
async fn relay_spawn_to_satellite(
    state: &SharedState,
    host: &phux_protocol::ids::SatelliteHost,
    group: GroupId,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    env: Option<Vec<(String, String)>>,
    term: Option<String>,
) -> SpawnResult {
    let Some(relay) = state.with(|s| s.hub_relay(host)) else {
        debug!(
            satellite = %host,
            "SPAWN_TERMINAL: no route to satellite (non-hub server, or host not in the registry)",
        );
        return SpawnResult::Err(SpawnError::UnsupportedSatelliteRoute);
    };
    relay.spawn(group, command, cwd, env, term).await
}

/// Handle `MOVE_TERMINAL` (ADR-0056, L1 §10.1).
///
/// Re-parents `terminal` into the window that currently owns
/// `owner_terminal`, atomically under the state lock: resolve both
/// Terminals, move the registry entry, and reap the source window if the
/// move emptied it — either the whole re-parent lands or none of it does.
/// The pane's process, PTY, scrollback, metadata, and agent record are
/// untouched; its `TerminalId` is stable across the move, so subscriptions
/// and outstanding waits survive. Layout is deliberately NOT written here:
/// geometry is the caller's L3 concern (the ADR-0019 seam), exactly as
/// with spawn placement.
///
/// Local-only: a satellite-tagged id on either end is the typed
/// [`MoveError::UnsupportedSatelliteRoute`], matching spawn's refusal.
pub(crate) async fn handle_move_terminal(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    terminal: phux_protocol::ids::TerminalId,
    owner_terminal: phux_protocol::ids::TerminalId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    debug!(
        ?client_id,
        request_id,
        terminal = ?terminal,
        owner_terminal = ?owner_terminal,
        "MOVE_TERMINAL",
    );

    let (result, clients_to_detach) =
        if !matches!(terminal, phux_protocol::ids::TerminalId::Local { .. })
            || !matches!(owner_terminal, phux_protocol::ids::TerminalId::Local { .. })
        {
            (
                MoveResult::Err(MoveError::UnsupportedSatelliteRoute),
                Vec::new(),
            )
        } else {
            state.with_mut(|s| {
                let Some(moved) = s.terminal_from_wire(&terminal) else {
                    return (
                        MoveResult::Err(MoveError::MoveFailed(
                            "terminal was not found on this server".to_owned(),
                        )),
                        Vec::new(),
                    );
                };
                let Some(owner) = s.terminal_from_wire(&owner_terminal) else {
                    return (
                        MoveResult::Err(MoveError::MoveFailed(
                            "owner terminal was not found on this server".to_owned(),
                        )),
                        Vec::new(),
                    );
                };
                let Some(dest_window) = s.registry().terminal(owner).map(|t| t.window) else {
                    return (
                        MoveResult::Err(MoveError::MoveFailed(
                            "owner terminal has no window on this server".to_owned(),
                        )),
                        Vec::new(),
                    );
                };
                let source_window = s.registry().terminal(moved).map(|t| t.window);
                let source_session = source_window
                    .and_then(|window| s.registry().window(window))
                    .map(|window| window.session);
                match s.registry_mut().move_terminal(moved, dest_window) {
                    Ok(()) => {
                        // A move that emptied its source window leaves it for
                        // the same cascade pane death uses (ADR-0056: "the
                        // server already reaps by its existing rules").
                        if let Some(source_window) = source_window {
                            s.reap_window_if_empty(source_window);
                        }
                        let clients = source_session
                            .filter(|session| s.registry().session(*session).is_none())
                            .map_or_else(Vec::new, |session| {
                                s.attached_clients_in_session(session)
                            });
                        (MoveResult::Ok(terminal), clients)
                    }
                    Err(err) => (
                        MoveResult::Err(MoveError::MoveFailed(err.to_string())),
                        Vec::new(),
                    ),
                }
            })
        };

    let _ = out_tx
        .send(Outbound::Frame(FrameKind::TerminalMoved {
            request_id,
            result,
        }))
        .await;

    // A session-scoped ATTACH cannot remain coherent after its session was
    // reaped. Reply to the move first, then queue DETACHED for only those
    // attached TUIs. Each delivery waits in its own task so a wedged client's
    // full mailbox cannot block this command or the mover's follow-up requests.
    // Headless ATTACH_TERMINAL subscriptions are not session-attached and keep
    // streaming the stable TerminalId as ADR-0056 requires.
    for (detached_client, tx) in clients_to_detach {
        let detached_state = state.clone();
        tokio::task::spawn_local(async move {
            let _ = tx.send(Outbound::Frame(FrameKind::Detached)).await;
            super::client::detach_and_release_consumer_state(&detached_state, detached_client);
        });
    }
}

/// Handle `SPAWN_TERMINAL` (phux-4li.11, SPEC §7.2 / §10.1).
///
/// v0.1 servers expose a single default Group at
/// [`crate::state::DEFAULT_GROUP_ID`] (= `GroupId(1)`). Any
/// other id is rejected with [`SpawnError::GroupNotFound`] inside
/// the [`SpawnResult::Err`] arm of the reply frame — separate from
/// the catch-all `Error` channel so command-correlated failures stay
/// typed end-to-end (the same precedent the metadata reply path uses).
///
/// On success the spawn reuses the same PTY primitive
/// [`seed_session_with_pty`] that
/// [`resolve_create_if_missing`] threads through. We always go PTY-
/// backed: a `SPAWN_TERMINAL` with no PTY would be functionally
/// indistinguishable from "nothing happened," and the wire frame
/// commits to a runnable Terminal (the `command = None` ↔ "use the
/// server's default shell" contract from
/// `FrameKind::SpawnTerminal`'s doc).
///
/// `command`/`cwd`/`env` from the wire frame populate the
/// `portable_pty::CommandBuilder`:
///   * `command = None`  → fall back to
///     [`crate::terminal_actor::default_shell_command`] over the
///     resolved default shell (`defaults.shell` → `$SHELL` → `/bin/sh`;
///     same as `AttachTarget::CreateIfMissing.command = None`).
///   * `cwd = Some(p)`    → `builder.cwd(p)`.
///   * `env = Some(v)`    → each `(k, v)` set via `builder.env(k, v)`,
///     additive over the parent environment. `env = Some(vec![])` is
///     distinct from `None` per the wire schema but has no observable
///     effect on the resulting child today (we don't `env_clear`).
///
/// The spawning client is auto-subscribed to the new pane and gets an
/// output-pump task fanning the actor's broadcast into its outbound
/// mailbox — the same machinery `handle_attach` uses for the session's
/// initial panes. Without that, an `INPUT_KEY` to the freshly-spawned
/// id would be rejected at [`crate::runtime::commands::handle_terminal_input`]'s
/// subscription
/// gate and the user would see nothing.
///
/// The pane joins the spawning client's CURRENT session's window
/// (phux-i9zl): a TUI split keeps the session intact so `phux ls` shows one
/// session and a reattach resolves every split pane. The session is
/// resolved from the client's attachment; a `SPAWN_TERMINAL` from a
/// non-attached client (the headless `phux spawn` CLI, or a hub's relayed
/// spawn arriving over the link — phux-v45.6) falls back to the server's
/// most recently active session, and is refused only when the server has
/// no session at all to host the pane.
///
/// A `satellite: Some(host)` spawn never touches local dispatch: on a hub
/// it is relayed over `host`'s link and the reply carries the new
/// Terminal re-tagged `Satellite { host, id }`; a non-hub server (or a
/// hub without `host` in its registry) refuses with the typed
/// [`SpawnError::UnsupportedSatelliteRoute`], and an unreachable
/// satellite fails fast with [`SpawnError::SatelliteUnreachable`]
/// (L1 §3.1 / §9.1).
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "linear orchestration: route satellite spawns → validate group → build CommandBuilder from wire frame → resolve hosting session → spawn PTY-backed pane into its window → auto-subscribe spawning client + spawn output pump → reply on the wire. The explicit context arguments preserve cancellation and output-pump ownership; splitting the flow would scatter the SPAWN_TERMINAL contract without simplifying it."
)]
pub(crate) async fn handle_spawn_terminal(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    request: SpawnRequest,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    root_token: &CancellationToken,
    connection_token: &CancellationToken,
    output_pumps: &mut JoinSet<()>,
) {
    let Some(profile) = bootstrap_stream_profile(bootstrap_profile) else {
        let _ = out_tx
            .send(Outbound::Frame(FrameKind::Error {
                request_id: Some(request_id),
                code: ErrorCode::CodecUnavailable,
                message: "SPAWN_TERMINAL selected an unsupported bootstrap profile".to_owned(),
            }))
            .await;
        return;
    };
    let SpawnRequest {
        group,
        command,
        cwd,
        env,
        term,
        satellite,
        owner_terminal,
        agent_session,
    } = request;
    debug!(
        ?client_id,
        request_id,
        group = ?group,
        command = ?command,
        cwd = ?cwd,
        env_count = env.as_ref().map_or(0, Vec::len),
        satellite = ?satellite,
        owner_terminal = ?owner_terminal,
        "SPAWN_TERMINAL",
    );

    // Satellite-targeted spawn (phux-v45.6, L1 §3.1 / §9.1): relay over
    // the owning hub link; the group and PTY details are validated on the
    // satellite, whose errors relay back verbatim. Never falls through to
    // local dispatch.
    if let Some(host) = satellite {
        let result = if owner_terminal.is_some() || agent_session.is_some() {
            SpawnResult::Err(SpawnError::SpawnFailed(
                "owner-terminal targeting and agent-session provenance are local-only".to_owned(),
            ))
        } else {
            relay_spawn_to_satellite(state, &host, group, command, cwd, env, term).await
        };
        let _ = out_tx
            .send(Outbound::Frame(FrameKind::TerminalSpawned {
                request_id,
                result,
            }))
            .await;
        return;
    }

    if agent_session
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > MAX_AGENT_SESSION_RECORD_BYTES)
    {
        let _ = out_tx
            .send(Outbound::Frame(FrameKind::TerminalSpawned {
                request_id,
                result: SpawnResult::Err(SpawnError::SpawnFailed(
                    "agent-session provenance must contain 1..=4096 bytes".to_owned(),
                )),
            }))
            .await;
        return;
    }

    if group != crate::state::DEFAULT_GROUP_ID {
        let _ = out_tx
            .send(Outbound::Frame(FrameKind::TerminalSpawned {
                request_id,
                result: SpawnResult::Err(SpawnError::GroupNotFound),
            }))
            .await;
        return;
    }

    // Build the `CommandBuilder` from the wire frame. `command = None`
    // mirrors `AttachTarget::CreateIfMissing.command = None`: fall back
    // to the resolved default shell (`defaults.shell` → `$SHELL` →
    // `/bin/sh`, phux-i0e8.4.1).
    let mut builder = match command {
        Some(argv) if !argv.is_empty() => {
            let mut head = argv.into_iter();
            let program = head.next().unwrap_or_default();
            let mut b = portable_pty::CommandBuilder::new(program);
            for arg in head {
                b.arg(arg);
            }
            b
        }
        _ => {
            let (shell, login_shell) = state.with(|s| (s.shell().to_owned(), s.login_shell()));
            crate::terminal_actor::default_shell_command(&shell, login_shell)
        }
    };
    // TERM precedence (phux-ign): each later tier overrides the prior via
    // `CommandBuilder::env`, which overwrites. So the order is:
    //   1. compiled-in DEFAULT_TERM (from `default_shell_command`)
    //   2. server `defaults.term` (here)
    //   3. per-spawn first-class `SPAWN_TERMINAL.term` field (below)
    //   4. per-spawn `SPAWN_TERMINAL.env` entry for `TERM` (wire `env`
    //      loop, which runs last) — authoritative for the Terminal.
    let default_term = state.with(|s| s.term().to_owned());
    crate::terminal_actor::apply_term(&mut builder, &default_term);
    if let Some(t) = term.as_deref() {
        crate::terminal_actor::apply_term(&mut builder, t);
    }
    // Working directory precedence (phux-cs6): an explicit wire `cwd`
    // always wins; otherwise fall back to `defaults.cwd-inheritance`. The
    // inherit-focused policy reads the spawning client's focused pane's
    // live PTY CWD via a kernel query, so `C-a |` from a pane cd'd to
    // /tmp opens the new pane in /tmp.
    if let Some(path) = cwd {
        builder.cwd(path);
    } else if let Some(path) = resolve_inherited_cwd(state, client_id).await {
        builder.cwd(path);
    }
    if let Some(pairs) = env {
        for (k, v) in pairs {
            builder.env(k, v);
        }
    }

    // phux-i9zl: a split spawns into the spawning client's CURRENT session's
    // window, not a fresh `spawn-N` wrapper session. Resolve that session
    // from the client's attachment (the same `s.attached()` lookup the cwd
    // inheritance above uses). A non-attached spawner — the headless
    // `phux spawn` CLI, or a hub's relayed spawn arriving over the link
    // (phux-v45.6; the hub's link consumer never attaches) — falls back to
    // the server's most recently active session (the same focus heuristic
    // `GET_STATE` snapshots use). Only a server with no session at all
    // refuses, rather than orphan a PTY nothing can list.
    let session = state.with(|s| {
        s.attached()
            .get(&client_id)
            .map(|c| c.session)
            .or_else(|| s.most_recently_touched_session())
            .or_else(|| s.registry().sessions().next().map(|(id, _)| id))
    });
    let ownership = if let Some(owner) = owner_terminal {
        if !matches!(owner, phux_protocol::ids::TerminalId::Local { .. })
            || state.with(|s| s.terminal_from_wire(&owner).is_none())
        {
            let _ = out_tx
                .send(Outbound::Frame(FrameKind::TerminalSpawned {
                    request_id,
                    result: SpawnResult::Err(SpawnError::SpawnFailed(
                        "owner terminal was not found on this server".to_owned(),
                    )),
                }))
                .await;
            return;
        }
        SpawnOwnership::Terminal(owner)
    } else if let Some(session) = session {
        SpawnOwnership::Session(session)
    } else {
        let _ = out_tx
            .send(Outbound::Frame(FrameKind::TerminalSpawned {
                request_id,
                result: SpawnResult::Err(SpawnError::SpawnFailed(
                    "server has no session to host the spawned pane".to_owned(),
                )),
            }))
            .await;
        return;
    };

    let (history_limit, default_colors) = state.with(|s| {
        (
            s.history_limit(),
            s.attached()
                .get(&client_id)
                .and_then(|client| client.client_caps.default_colors),
        )
    });
    let core_terminal_id = match spawn_pane_with_pty_and_colors(
        state,
        &ownership,
        builder,
        history_limit,
        root_token,
        default_colors,
        agent_session,
    ) {
        Ok(Some(id)) => id,
        Ok(None) => {
            warn!(
                ?client_id,
                request_id, "SPAWN_TERMINAL: selected owner has no window to host the pane",
            );
            let _ = out_tx
                .send(Outbound::Frame(FrameKind::TerminalSpawned {
                    request_id,
                    result: SpawnResult::Err(SpawnError::SpawnFailed(
                        "selected owner has no window to host the pane".to_owned(),
                    )),
                }))
                .await;
            return;
        }
        Err(err) => {
            warn!(
                ?client_id,
                request_id,
                error = %err,
                "SPAWN_TERMINAL: failed to spawn pane actor",
            );
            let _ = out_tx
                .send(Outbound::Frame(FrameKind::TerminalSpawned {
                    request_id,
                    result: SpawnResult::Err(SpawnError::SpawnFailed(format!("{err}"))),
                }))
                .await;
            return;
        }
    };

    // Auto-subscribe the spawning client to the new pane and snapshot
    // its `TerminalHandle` so we can spawn an output pump. Without
    // subscription the `INPUT_*` dispatch path's
    // `subscribers_for_terminal(...).contains(&client_id)` gate would
    // reject every keystroke the spawning client sends to the new id.
    //
    // The subscribe-and-handle lookup happens in a single `with_mut`
    // critical section so the wire-id allocation and the subscriber
    // append observe the same registry state.
    let wire_and_handle: Option<(
        phux_protocol::ids::TerminalId,
        crate::terminal_actor::TerminalHandle,
        ClientCapabilities,
    )> = state.with_mut(|s| {
        let wire_terminal_id = s.intern_terminal_wire(core_terminal_id);
        let client_caps = s
            .attached()
            .get(&client_id)
            .map(|c| c.client_caps)
            .unwrap_or_default();
        // Only auto-subscribe if the client is currently attached —
        // a bare `SPAWN_TERMINAL` from a non-attached client is legal
        // wire-wise (the frame doesn't require ATTACH first) but the
        // subscription would have no `attached` slot to live in.
        if s.attached().contains_key(&client_id) {
            s.subscribe_terminal(client_id, core_terminal_id);
        }
        s.terminal_handle(core_terminal_id)
            .cloned()
            .map(|h| (wire_terminal_id, h, client_caps))
    });

    if let Some((wire_terminal_id, handle, client_caps)) = wire_and_handle {
        // `profile` was validated before spawning the pane, so an unknown
        // future profile can never publish a partial bootstrap generation.
        // Spawn the output pump BEFORE replying with `TerminalSpawned`
        // so any bytes the freshly-spawned PTY emits in the gap between
        // exec and the client's first read are queued on the broadcast
        // channel (broadcasts buffer per subscriber). Mirrors the
        // subscribe-before-snapshot ordering in `handle_attach`.
        let mut output_rx = handle.output.subscribe();
        let pump_out_tx = out_tx.clone();
        let pump_wire_terminal_id = wire_terminal_id.clone();
        let stream_id = stream_id_from(u64::from(request_id));
        let pump_resize = handle.resize.clone();
        let pump_state = state.clone();
        let pump_connection_token = connection_token.clone();
        let pump_core_terminal_id = core_terminal_id;
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let pump_native_bootstrap = handle.native_bootstrap.clone();
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let pump_native_handle = handle.clone();
        let (bootstrap_gate_tx, bootstrap_gate_rx) = oneshot::channel::<OutputPumpStart>();
        output_pumps.spawn_local(async move {
            let Ok(start) = bootstrap_gate_rx.await else {
                return;
            };
            let mut published_cut = start.published_cut;
            let mut last_forwarded_seq = published_cut;
            let mut bootstrap_id = initial_bootstrap_id();
            let mut generation_active = true;
            if let Some(live) = start.live {
                output_rx = live;
            }
            for (seq, bytes) in start.replay {
                if seq <= published_cut {
                    continue;
                }
                if pump_out_tx
                    .send(Outbound::Frame(FrameKind::TerminalOutput {
                        terminal_id: pump_wire_terminal_id.clone(),
                        stream_id,
                        bootstrap_id,
                        seq,
                        bytes: downsample_for_caps(&bytes, client_caps),
                    }))
                    .await
                    .is_err()
                {
                    return;
                }
                last_forwarded_seq = seq;
            }
            loop {
                match output_rx.recv().await {
                    Ok(PaneOutput::Live { seq, bytes }) => {
                        if !generation_active || seq <= published_cut {
                            continue;
                        }
                        let frame = FrameKind::TerminalOutput {
                            terminal_id: pump_wire_terminal_id.clone(),
                            stream_id,
                            bootstrap_id,
                            seq,
                            bytes: downsample_for_caps(&bytes, client_caps),
                        };
                        if pump_out_tx.send(Outbound::Frame(frame)).await.is_err() {
                            break;
                        }
                        last_forwarded_seq = seq;
                    }
                    Ok(PaneOutput::Control { owner, frame }) => {
                        if owner != client_id.0 {
                            continue;
                        }
                        let (targets_pump, ends_generation) = match &frame {
                            FrameKind::BootstrapTombstone {
                                terminal_id,
                                stream_id: control_stream_id,
                                bootstrap_id: control_bootstrap_id,
                                ..
                            } => (
                                terminal_id == &pump_wire_terminal_id
                                    && *control_stream_id == stream_id
                                    && *control_bootstrap_id == bootstrap_id,
                                true,
                            ),
                            FrameKind::HistoryTombstone {
                                terminal_id,
                                stream_id: control_stream_id,
                                bootstrap_id: control_bootstrap_id,
                                ..
                            } => (
                                terminal_id == &pump_wire_terminal_id
                                    && *control_stream_id == stream_id
                                    && *control_bootstrap_id == bootstrap_id,
                                false,
                            ),
                            _ => (false, false),
                        };
                        if !targets_pump {
                            continue;
                        }
                        if pump_out_tx.send(Outbound::Frame(frame)).await.is_err() {
                            break;
                        }
                        if ends_generation {
                            generation_active = false;
                        }
                    }
                    Ok(PaneOutput::Resync {
                        cols,
                        rows,
                        bytes,
                        reason: tombstone_reason,
                        base_seq,
                    }) => {
                        // Resync is a control event, not replayable live data:
                        // even an unchanged cut (for example resize directly
                        // after READY) invalidates and replaces the generation.
                        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                        let prior_bootstrap_id = bootstrap_id;
                        bootstrap_id = next_bootstrap_id(bootstrap_id);
                        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                        if matches!(
                            profile,
                            BootstrapStreamProfile::NativeState {
                                codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2
                            }
                        ) {
                            if generation_active
                                && pump_out_tx
                                .send(Outbound::Frame(FrameKind::BootstrapTombstone {
                                    terminal_id: pump_wire_terminal_id.clone(),
                                    stream_id,
                                    bootstrap_id: prior_bootstrap_id,
                                    reason: match tombstone_reason {
                                        crate::terminal_actor::ResyncReason::Resize => {
                                            phux_protocol::wire::frame::TombstoneReason::Resize
                                        }
                                        crate::terminal_actor::ResyncReason::OutboundGap => {
                                            phux_protocol::wire::frame::TombstoneReason::OutboundGap
                                        }
                                    },
                                    last_valid_seq: last_forwarded_seq,
                                }))
                                .await
                                .is_err()
                            {
                                break;
                            }
                            let (reply_tx, reply_rx) = oneshot::channel();
                            if pump_native_bootstrap
                                .send(crate::terminal_actor::NativeBootstrapRequest {
                                    owner: client_id.0,
                                    terminal_id: pump_wire_terminal_id.clone(),
                                    stream_id,
                                    bootstrap_id,
                                    limits: bootstrap_limits,
                                    max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                                    max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
                                    reply: reply_tx,
                                })
                                .await
                                .is_err()
                            {
                                pump_state.with_mut(|s| {
                                    s.reap_terminal(pump_core_terminal_id);
                                });
                                pump_connection_token.cancel();
                                break;
                            }
                            let Ok(Ok(reply)) = reply_rx.await else {
                                let _ = pump_out_tx
                                    .send(Outbound::Frame(FrameKind::Error {
                                        request_id: None,
                                        code: ErrorCode::CodecUnavailable,
                                        message: "native checkpoint resync failed".to_owned(),
                                    }))
                                    .await;
                                pump_state.with_mut(|s| {
                                    s.reap_terminal(pump_core_terminal_id);
                                });
                                pump_connection_token.cancel();
                                break;
                            };
                            let Ok((cut, cursor)) =
                                publish_native_bootstrap(&pump_out_tx, reply).await
                            else {
                                pump_state.with_mut(|s| {
                                    s.reap_terminal(pump_core_terminal_id);
                                });
                                pump_connection_token.cancel();
                                break;
                            };
                            let Ok(publication) = activate_native_publication(
                                &pump_native_handle,
                                client_id.0,
                                pump_wire_terminal_id.clone(),
                                stream_id,
                                bootstrap_id,
                                cursor,
                            )
                            .await
                            else {
                                pump_connection_token.cancel();
                                break;
                            };
                            published_cut = cut;
                            last_forwarded_seq = cut;
                            output_rx = publication.live;
                            for (seq, bytes) in publication.replay {
                                if pump_out_tx
                                    .send(Outbound::Frame(FrameKind::TerminalOutput {
                                        terminal_id: pump_wire_terminal_id.clone(),
                                        stream_id,
                                        bootstrap_id,
                                        seq,
                                        bytes: downsample_for_caps(&bytes, client_caps),
                                    }))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                                last_forwarded_seq = seq;
                            }
                            generation_active = true;
                            continue;
                        }
                        let payload = downsample_for_caps(&bytes, client_caps);
                        if send_synthesized_bootstrap(
                            &pump_out_tx,
                            pump_wire_terminal_id.clone(),
                            stream_id,
                            bootstrap_id,
                            profile,
                            bootstrap_limits,
                            cols,
                            rows,
                            base_seq,
                            [payload],
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                        published_cut = base_seq;
                        last_forwarded_seq = base_seq;
                        generation_active = true;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            terminal_id = ?pump_wire_terminal_id,
                            dropped = n,
                            "SPAWN_TERMINAL output pump lagged; requesting in-band resync",
                        );
                        if !enqueue_output_resync(&pump_resize).await {
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(1),
                                pump_out_tx.send(Outbound::Frame(FrameKind::Error {
                                    request_id: None,
                                    code: ErrorCode::InternalError,
                                    message: "terminal output gap could not be resynchronized"
                                        .to_owned(),
                                })),
                            )
                            .await;
                            pump_state.with_mut(|s| {
                                s.reap_terminal(pump_core_terminal_id);
                            });
                            pump_connection_token.cancel();
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if matches!(
            profile,
            BootstrapStreamProfile::NativeState {
                codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2
            }
        ) {
            let (reply_tx, reply_rx) = oneshot::channel();
            let sent = handle
                .native_bootstrap
                .send(crate::terminal_actor::NativeBootstrapRequest {
                    owner: client_id.0,
                    terminal_id: wire_terminal_id.clone(),
                    stream_id,
                    bootstrap_id: initial_bootstrap_id(),
                    limits: bootstrap_limits,
                    max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                    max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
                    reply: reply_tx,
                })
                .await
                .is_ok();
            let reply = if sent {
                match reply_rx.await {
                    Ok(Ok(reply)) => Some(reply),
                    Ok(Err(error)) => {
                        warn!(?core_terminal_id, %error, "native spawn preflight failed");
                        None
                    }
                    Err(_) => None,
                }
            } else {
                None
            };
            let Some(reply) = reply else {
                state.with_mut(|s| {
                    s.reap_terminal(core_terminal_id);
                });
                let _ = out_tx
                    .send(Outbound::Frame(FrameKind::TerminalSpawned {
                        request_id,
                        result: SpawnResult::Err(SpawnError::SpawnFailed(
                            "native checkpoint preflight failed".to_owned(),
                        )),
                    }))
                    .await;
                return;
            };
            let cut = reply.base_seq;
            let cursor = reply.publication_cursor;
            if out_tx
                .send(Outbound::Frame(FrameKind::TerminalSpawned {
                    request_id,
                    result: SpawnResult::Ok(wire_terminal_id.clone()),
                }))
                .await
                .is_err()
            {
                state.with_mut(|s| {
                    s.reap_terminal(core_terminal_id);
                });
                return;
            }
            for frame in reply.frames {
                if out_tx.send(Outbound::Frame(frame)).await.is_err() {
                    state.with_mut(|s| {
                        s.reap_terminal(core_terminal_id);
                    });
                    return;
                }
            }
            let Ok(publication) = activate_native_publication(
                &handle,
                client_id.0,
                wire_terminal_id.clone(),
                stream_id,
                initial_bootstrap_id(),
                cursor,
            )
            .await
            else {
                state.with_mut(|s| {
                    s.reap_terminal(core_terminal_id);
                });
                return;
            };
            let _ = bootstrap_gate_tx.send(OutputPumpStart {
                published_cut: cut,
                replay: publication.replay,
                live: Some(publication.live),
            });
            broadcast_event(state, Some(&wire_terminal_id), &AgentEvent::PaneSpawned);
            return;
        }

        let (snapshot_tx, snapshot_rx) = oneshot::channel();
        if handle
            .snapshot
            .send(SnapshotRequest {
                scrollback: None,
                max_bytes: usize::MAX,
                max_frames: usize::MAX,
                chunk_bytes: 1,
                reply: snapshot_tx,
            })
            .await
            .is_err()
        {
            state.with_mut(|s| {
                s.reap_terminal(core_terminal_id);
            });
            let _ = out_tx
                .send(Outbound::Frame(FrameKind::TerminalSpawned {
                    request_id,
                    result: SpawnResult::Err(SpawnError::SpawnFailed(
                        "snapshot preflight failed".to_owned(),
                    )),
                }))
                .await;
            return;
        }
        let Ok(Ok((snapshot, cut))) = snapshot_rx.await else {
            state.with_mut(|s| {
                s.reap_terminal(core_terminal_id);
            });
            let _ = out_tx
                .send(Outbound::Frame(FrameKind::TerminalSpawned {
                    request_id,
                    result: SpawnResult::Err(SpawnError::SpawnFailed(
                        "snapshot preflight failed".to_owned(),
                    )),
                }))
                .await;
            return;
        };
        let replay = downsample_for_caps(&bytes::Bytes::from(snapshot.bytes), client_caps);
        let Ok(frames) = synthesized_bootstrap_frames(
            wire_terminal_id.clone(),
            stream_id,
            initial_bootstrap_id(),
            profile,
            bootstrap_limits,
            snapshot.cols,
            snapshot.rows,
            cut,
            [replay],
        ) else {
            state.with_mut(|s| {
                s.reap_terminal(core_terminal_id);
            });
            let _ = out_tx
                .send(Outbound::Frame(FrameKind::TerminalSpawned {
                    request_id,
                    result: SpawnResult::Err(SpawnError::SpawnFailed(
                        "bootstrap limits rejected snapshot".to_owned(),
                    )),
                }))
                .await;
            return;
        };
        if out_tx
            .send(Outbound::Frame(FrameKind::TerminalSpawned {
                request_id,
                result: SpawnResult::Ok(wire_terminal_id.clone()),
            }))
            .await
            .is_err()
        {
            state.with_mut(|s| {
                s.reap_terminal(core_terminal_id);
            });
            return;
        }
        for frame in frames {
            if out_tx.send(Outbound::Frame(frame)).await.is_err() {
                state.with_mut(|s| {
                    s.reap_terminal(core_terminal_id);
                });
                return;
            }
        }
        let _ = bootstrap_gate_tx.send(OutputPumpStart {
            published_cut: cut,
            replay: Vec::new(),
            live: None,
        });
        // phux-y2t: fan a `pane_spawned` agent event to event-stream
        // subscribers (SPEC §7.5). The new pane's wire id rides the
        // `EVENT` envelope; server-wide subscribers and any per-pane
        // subscribers for this id receive it.
        broadcast_event(state, Some(&wire_terminal_id), &AgentEvent::PaneSpawned);
    } else {
        // Defensive: seed_session_with_pty succeeded but the handle
        // somehow vanished before we could clone it. Treat as a spawn
        // failure on the wire so the client doesn't hang on a reply
        // that will never arrive.
        warn!(
            ?client_id,
            request_id,
            ?core_terminal_id,
            "SPAWN_TERMINAL: spawn succeeded but TerminalHandle vanished",
        );
        state.with_mut(|s| {
            s.reap_terminal(core_terminal_id);
        });
        let _ = out_tx
            .send(Outbound::Frame(FrameKind::TerminalSpawned {
                request_id,
                result: SpawnResult::Err(SpawnError::SpawnFailed(
                    "internal state inconsistency: handle missing after spawn".to_owned(),
                )),
            }))
            .await;
    }
}

/// Handle `TERMINAL_RESIZE` (phux-4li.11, SPEC §7.2 / §10.2).
///
/// Look up the target Terminal by its wire id, then `try_send` the new
/// `(cols, rows)` into the actor's resize mailbox. The actor's existing
/// `handle_resize` (built for `VIEWPORT_RESIZE` in phux-byc.5) drives
/// both `libghostty_vt::Terminal::resize` and the PTY
/// `ioctl(TIOCSWINSZ)` from one place — we reuse it verbatim so the
/// per-Terminal resize and the per-Viewport resize stay in lockstep.
///
/// Silent on every "not found" path per the wire frame's
/// no-reply-by-design contract. The frame label distinguishes this
/// path from `VIEWPORT_RESIZE` in logs.
///
/// `client_id` is unused today (the wire frame is unauthenticated;
/// SATELLITE-routed ids are rejected before we get here). It's wired
/// through anyway so future per-client validation (e.g. checking that
/// the client is subscribed to the pane) doesn't require widening the
/// helper signature.
/// Resolve `target`, call [`prepare_attach`], and queue the
/// `ATTACHED` + per-pane `TERMINAL_SNAPSHOT` frames on `out_tx`.
///
/// On any failure path, emits an `ERROR` frame and returns. We never
/// partially-attach: either every frame queues or none does.
#[allow(
    clippy::too_many_lines,
    reason = "linear attach orchestration: resolve target -> prepare -> stage per-pane output pumps -> capture each bounded source against the remaining aggregate budget -> publish atomically; splitting it would scatter the rollback contract"
)]
#[allow(
    clippy::too_many_arguments,
    reason = "the ATTACH branch in handle_client pre-decomposes the FrameKind::Attach payload (target/viewport/request_scrollback/scrollback_limit_lines) and threads the negotiated ColorSupport alongside the SharedState + client_id + out_tx; rebundling into a struct would just move the arity from the call site to a builder"
)]
// Lifecycle span (info): one ATTACH per client. Its CLOSE duration is the
// attach-handshake timing (bounded per-pane capture is the slow part); the
// fields correlate it to a client + target + requested dims. `skip_all` keeps the
// large arg list (state handle, channels, token) out of the span.
#[tracing::instrument(
    level = "info",
    name = "handle_attach",
    skip_all,
    fields(?client_id, target = ?target, cols = viewport.cols, rows = viewport.rows),
)]
pub(crate) async fn handle_attach(
    state: &SharedState,
    client_id: ClientId,
    attach_id: u32,
    target: AttachTarget,
    viewport: phux_protocol::wire::frame::ViewportInfo,
    request_scrollback: bool,
    scrollback_limit_lines: u32,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    negotiated_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    root_token: &CancellationToken,
    output_pumps: &mut JoinSet<()>,
    connection_token: &CancellationToken,
) {
    let Some(stream_profile) = bootstrap_stream_profile(negotiated_profile) else {
        send_error(
            out_tx,
            ErrorCode::CodecUnavailable,
            "ATTACH selected an unsupported bootstrap profile",
        )
        .await;
        return;
    };
    // phux-9q5f: honor the ATTACH scrollback request. `request_scrollback`
    // gates the feature; `scrollback_limit_lines` caps it (0 ⇒ all retained
    // history, the SCROLLBACK_ALL sentinel). The per-pane SnapshotRequest
    // carries this so the actor primes TERMINAL_SNAPSHOT.scrollback_bytes.
    let scrollback_req: Option<u32> = request_scrollback.then_some(scrollback_limit_lines);

    let Some(session_name) = resolve_attach_target(
        state,
        target,
        out_tx,
        root_token,
        client_caps.default_colors,
    )
    .await
    else {
        return;
    };

    // phux-p4vp: fold each live pane's kernel CWD into its registry
    // descriptor before the snapshot is built, so ATTACHED carries a
    // current `cwd` per pane (the sidebar's VCS branch line depends on it).
    refresh_registry_cwds(state).await;

    let same_session_reattach = state.with(|server| {
        let target = server.find_session_by_name(&session_name);
        matches!(
            (server.attached().get(&client_id), target),
            (Some(attached), Some(target)) if attached.session == target
        )
    });

    let (snapshot, initial_client_id, panes_to_snapshot) = match prepare_attach(
        state,
        client_id,
        &session_name,
        out_tx,
        client_caps,
        negotiated_profile,
        bootstrap_limits,
    ) {
        Ok(prepared) => prepared,
        Err(crate::state::AttachError::UnknownSession(name)) => {
            send_error(
                out_tx,
                ErrorCode::SessionNotFound,
                &format!("session {name:?} not found"),
            )
            .await;
            return;
        }
        Err(crate::state::AttachError::AlreadyAttached(_)) => {
            send_error(
                out_tx,
                ErrorCode::AlreadyAttached,
                "client is already attached",
            )
            .await;
            return;
        }
        Err(crate::state::AttachError::ResourceLimit) => {
            send_error(
                out_tx,
                ErrorCode::CodecUnavailable,
                "session exceeds bounded aggregate attach limits",
            )
            .await;
            return;
        }
    };
    let wire_client_id =
        phux_protocol::ids::ClientId::new(u32::try_from(client_id.0).unwrap_or(u32::MAX));
    if same_session_reattach
        && matches!(
            client_caps.output_mode,
            phux_protocol::caps::OutputMode::StateSync
        )
    {
        // Stop the prior generation's actor-side tick emitters before the new
        // ATTACHED frame is visible. Raw pumps were aborted above; without
        // this matching teardown, a state-sync delta from the old stream
        // could interleave between ATTACHED and the replacement bootstrap.
        for pane in &panes_to_snapshot {
            let (reply, done) = oneshot::channel();
            if pane
                .handle
                .consumer_detach
                .send(ConsumerDetachRequest {
                    client_id: wire_client_id,
                    reply,
                })
                .await
                .is_ok()
            {
                let _ = done.await;
            }
        }
    }

    // Terminal defaults are shared pane state. The most recently attached
    // interactive client that advertises a palette wins; palette-less agent
    // and legacy attaches leave the last known values untouched. Await each
    // acknowledgement before snapshotting so OSC 10/11 queries parsed after
    // ATTACHED observe the selected host palette.
    if let Some(colors) = client_caps.default_colors {
        for pane in &panes_to_snapshot {
            let (reply, done) = oneshot::channel();
            if pane
                .handle
                .set_default_colors
                .send(SetDefaultColorsRequest { colors, reply })
                .await
                .is_ok()
            {
                let _ = done.await;
            }
        }
    }

    // phux-2lj: apply the client's ATTACH viewport to every pane so
    // freshly-spawned PTYs (currently built at hardcoded 80x24, see
    // `seed_session_with_pty`) are resized to match the attaching
    // client's host terminal. Without this, e.g. `vim` running in a
    // 120x48 host terminal only fills the top 24 rows of the screen
    // until SIGWINCH or an explicit VIEWPORT_RESIZE drives a resize.
    //
    // SPEC §10.5: ATTACH.viewport is the outer client viewport. Single-
    // pane: the server applies it directly as the PTY's winsize (matches
    // the existing `handle_viewport_resize` convention; the off-by-one
    // for a host-side status bar is the client's concern via the
    // post-attach `TERMINAL_RESIZE` reflow path used by multi-pane).
    apply_attach_viewport(state, client_id, &panes_to_snapshot, viewport);

    // Capture sources one pane at a time. Each completed result is charged to
    // the aggregate staging budget before the next actor receives its remaining
    // byte/frame ceiling, so no set of concurrent actor allocations can exceed
    // the connection-wide cap.
    let stream_id = stream_id_from(u64::from(attach_id));
    let bootstrap_id = initial_bootstrap_id();
    // `stream_profile` was validated before resolving or mutating the attach
    // target, so no ATTACHED/BOOTSTRAP_BEGIN can precede this preflight.
    // phux-7w1j: per-pane "snapshot has been sent" gates. The output pump
    // subscribes to the broadcast in this loop (BEFORE the SnapshotRequest, so
    // no live bytes are lost), but must not FORWARD a `TerminalOutput` frame
    // until the pane's `TerminalSnapshot` has been written to `out_tx` — else a
    // PTY-active pane races output ahead of its snapshot and the client sees
    // frame 2 = OUTPUT instead of SNAPSHOT. The pump parks on `gate_rx`;
    // the barrier releases every pump only after every pane has queued READY
    // (or a close outcome) and the aggregate ATTACH_READY has been queued.
    let mut snapshot_gates: Vec<SnapshotGate> = Vec::new();
    // A failed replacement is connection-fatal: preserving an older producer
    // would allow output to overtake the terminal ERROR.
    let mut staged_output_pumps = JoinSet::new();
    let mut staged_handles = Vec::new();
    macro_rules! fail_prepublication {
        ($reason:expr) => {{
            fail_aggregate_attach_prepublication(
                state,
                client_id,
                attach_id,
                out_tx,
                connection_token,
                &staged_handles,
                &mut staged_output_pumps,
                output_pumps,
                $reason,
            )
            .await;
            return;
        }};
    }
    if staged_handles.try_reserve(panes_to_snapshot.len()).is_err() {
        fail_prepublication!("host allocation failed");
    }

    // Captures are deliberately awaited one pane at a time. The retained
    // result from earlier panes is charged before the next actor receives its
    // remaining source-allocation ceiling.
    let mut bootstrap_frames = Vec::new();
    let mut staging_budget = BootstrapStagingBudget::new();
    let (live_gate_tx, live_gate_rx) = tokio::sync::watch::channel(false);
    let Ok(aggregate_chunk_bytes) = usize::try_from(bootstrap_limits.max_chunk_bytes()) else {
        fail_prepublication!("bootstrap chunk bound cannot fit host");
    };
    for pane in panes_to_snapshot {
        let synthesized_source_max =
            bootstrap_source_ceiling(staging_budget.remaining_bytes(), client_caps);
        let terminal_id = pane.terminal_id;
        let handle = pane.handle;
        staged_handles.push(handle.clone());
        let wire_terminal_id = pane.wire_terminal_id;
        // ADR-0018 / phux-0q8: register the per-consumer state-sync entry
        // so the actor allocates and primes a per-consumer `RenderState`
        // cache for this client/pane, keyed by `wire_client_id`. We do
        // this BEFORE emitting the snapshot so the per-consumer cache is
        // primed against the same canonical state the snapshot installs
        // on the client mirror (see `register_consumer`'s doc).
        //
        // phux-3uv: the register reply reports whether the actor is
        // tick-managing this consumer (`consumer_tick_emits == true`). If
        // so, the actor's `tick_emit` is the sole emitter and we MUST
        // suppress the broadcast pump below — otherwise two independent
        // `seq` streams land on one consumer mailbox (double-paint, SPEC
        // §12.2 monotonic-per-consumer violation). If not tick-managed
        // (gate off, or register failed / actor gone / no local id), the
        // broadcast pump stays the live emitter and the per-consumer
        // entry just drives the dormant `FRAME_ACK` eviction loop.
        //
        // Awaited (not fire-and-forget) so the cache is primed before the
        // pump starts streaming deltas; a dropped reply or actor-gone is
        // logged and we fall back to the broadcast path.
        let mut tick_managed = false;
        let mut state_sync_bootstrap = None;
        let mut consumer_registered = false;
        if let Some(wire_id) = wire_terminal_id.local_id() {
            let (attach_reply_tx, attach_reply_rx) = oneshot::channel();
            if handle
                .consumer_attach
                .send(ConsumerAttachRequest {
                    client_id: wire_client_id,
                    outbound: out_tx.clone(),
                    wire_terminal_id: wire_id,
                    stream_id,
                    bootstrap_id,
                    // phux-fseo: honor the consumer's negotiated output mode.
                    // StateSync ⇒ the actor's tick is this consumer's emitter
                    // and the broadcast pump below is suppressed for it; Raw
                    // (the human-TUI default) keeps the pump.
                    wants_state_sync: matches!(
                        client_caps.output_mode,
                        phux_protocol::caps::OutputMode::StateSync
                    ),
                    state_sync_scrollback: scrollback_req,
                    bootstrap_max_bytes: synthesized_source_max,
                    bootstrap_max_frames: staging_budget.remaining_frames(),
                    bootstrap_chunk_bytes: aggregate_chunk_bytes,
                    // phux-v45.8: a directly-attached consumer rides a reliable,
                    // ordered transport (UDS / SSH stdio / WebSocket / QUIC
                    // stream), so the emit-once model is correct and cheapest —
                    // no loss-tolerant re-diff needed. Activation for a
                    // forwarded (hub->satellite->consumer) leg, where the hub's
                    // fan-out can drop whole frames, is the deferred follow-up
                    // (the satellite cannot see the downstream drop from the
                    // link's reliable transport); the advance-on-ack mechanism
                    // it flips on is fully implemented here (ADR-0042).
                    live_gate: live_gate_rx.clone(),
                    loss_tolerant: false,
                    reply: attach_reply_tx,
                })
                .await
                .is_ok()
            {
                match attach_reply_rx.await {
                    Ok(Ok(outcome)) => {
                        consumer_registered = true;
                        tick_managed = outcome.tick_managed;
                        state_sync_bootstrap = outcome.state_sync_bootstrap;
                        trace!(
                            ?terminal_id,
                            tick_managed, "per-consumer state-sync entry registered",
                        );
                    }
                    Ok(Err(err)) => {
                        warn!(
                            ?terminal_id,
                            error = %err,
                            "per-consumer state-sync register failed; broadcast path still serves this pane",
                        );
                    }
                    Err(_) => {
                        warn!(
                            ?terminal_id,
                            "per-consumer state-sync register: actor dropped reply",
                        );
                    }
                }
            } else {
                warn!(
                    ?terminal_id,
                    "per-consumer state-sync register: actor mailbox closed",
                );
            }
        }
        if matches!(
            client_caps.output_mode,
            phux_protocol::caps::OutputMode::StateSync
        ) && !consumer_registered
        {
            warn!(
                ?terminal_id,
                "state-sync registration failed before aggregate attach publication"
            );
            fail_prepublication!("state-sync consumer registration failed");
        }

        // phux-3uv: suppress the broadcast pump for a tick-managed
        // consumer — the actor's `tick_emit` is the single emitter for
        // this pane. Non-tick-managed consumers keep the broadcast pump.
        if !tick_managed {
            // Subscribe to live PTY output BEFORE requesting the snapshot.
            // Subscribing first means anything the TerminalActor broadcasts
            // after this point lands in our receiver; we then ask for a
            // snapshot so the client has a complete starting picture, and
            // any subsequent TerminalOutput we forward is "post-snapshot
            // delta" rather than racing against it.
            let mut output_rx = handle.output.subscribe();
            let pump_out_tx = out_tx.clone();
            let pump_wire_terminal_id = wire_terminal_id.clone();
            let pump_client_caps = client_caps;
            // phux-y8v6: lets a lagged pump ask the actor to broadcast an
            // in-band resync (a full grid snapshot on the same ordered channel)
            // so a consumer that dropped bytes reconverges.
            let pump_resize = handle.resize.clone();
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            let pump_native_bootstrap = handle.native_bootstrap.clone();
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            let pump_native_handle = handle.clone();
            let pump_state = state.clone();
            let pump_connection_token = connection_token.clone();
            // phux-7w1j: hold this pump's first forward until the pane's
            // snapshot has been sent (the drain loop fires `gate_tx`).
            let (gate_tx, gate_rx) = oneshot::channel::<OutputPumpStart>();
            snapshot_gates.push(SnapshotGate {
                terminal_id,
                #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                wire_terminal_id: wire_terminal_id.clone(),
                handle: handle.clone(),
                gate: gate_tx,
                cut: None,
                #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                native_cursor: None,
            });
            staged_output_pumps.spawn_local(async move {
                let Ok(start) = gate_rx.await else {
                    return;
                };
                let mut published_cut = start.published_cut;
                let mut last_forwarded_seq = published_cut;
                let mut bootstrap_id = bootstrap_id;
                let mut generation_active = true;
                if let Some(live) = start.live {
                    output_rx = live;
                }
                for (seq, bytes) in start.replay {
                    if seq <= published_cut {
                        continue;
                    }
                    let frame = FrameKind::TerminalOutput {
                        terminal_id: pump_wire_terminal_id.clone(),
                        stream_id,
                        bootstrap_id,
                        seq,
                        bytes: downsample_for_caps(&bytes, pump_client_caps),
                    };
                    if pump_out_tx.send(Outbound::Frame(frame)).await.is_err() {
                        return;
                    }
                    last_forwarded_seq = seq;
                }
                loop {
                    match output_rx.recv().await {
                        Ok(PaneOutput::Live { seq, bytes }) => {
                            if !generation_active || seq <= published_cut {
                                continue;
                            }
                            let frame = FrameKind::TerminalOutput {
                                terminal_id: pump_wire_terminal_id.clone(),
                                stream_id,
                                bootstrap_id,
                                seq,
                                bytes: downsample_for_caps(&bytes, pump_client_caps),
                            };
                            if pump_out_tx.send(Outbound::Frame(frame)).await.is_err() {
                                break;
                            }
                            last_forwarded_seq = seq;
                        }
                        Ok(PaneOutput::Control { owner, frame }) => {
                            if owner != client_id.0 {
                                continue;
                            }
                            let (targets_pump, ends_generation) = match &frame {
                                FrameKind::BootstrapTombstone {
                                    terminal_id,
                                    stream_id: control_stream_id,
                                    bootstrap_id: control_bootstrap_id,
                                    ..
                                } => (
                                    terminal_id == &pump_wire_terminal_id
                                        && *control_stream_id == stream_id
                                        && *control_bootstrap_id == bootstrap_id,
                                    true,
                                ),
                                FrameKind::HistoryTombstone {
                                    terminal_id,
                                    stream_id: control_stream_id,
                                    bootstrap_id: control_bootstrap_id,
                                    ..
                                } => (
                                    terminal_id == &pump_wire_terminal_id
                                        && *control_stream_id == stream_id
                                        && *control_bootstrap_id == bootstrap_id,
                                    false,
                                ),
                                _ => (false, false),
                            };
                            if !targets_pump {
                                continue;
                            }
                            if pump_out_tx.send(Outbound::Frame(frame)).await.is_err() {
                                break;
                            }
                            if ends_generation {
                                generation_active = false;
                            }
                        }
                        Ok(PaneOutput::Resync {
                            cols,
                            rows,
                            bytes,
                            reason: tombstone_reason,
                            base_seq,
                        }) => {
                            // Resync is control, so an unchanged cut still
                            // tombstones and replaces the published generation.
                            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                            let prior_bootstrap_id = bootstrap_id;
                            bootstrap_id = next_bootstrap_id(bootstrap_id);
                            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                            if matches!(
                                stream_profile,
                                BootstrapStreamProfile::NativeState {
                                    codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2
                                }
                            ) {
                                if generation_active
                                    && pump_out_tx
                                    .send(Outbound::Frame(FrameKind::BootstrapTombstone {
                                        terminal_id: pump_wire_terminal_id.clone(),
                                        stream_id,
                                        bootstrap_id: prior_bootstrap_id,
                                        reason: match tombstone_reason {
                                            crate::terminal_actor::ResyncReason::Resize => {
                                                phux_protocol::wire::frame::TombstoneReason::Resize
                                            }
                                            crate::terminal_actor::ResyncReason::OutboundGap => {
                                                phux_protocol::wire::frame::TombstoneReason::OutboundGap
                                            }
                                        },
                                        last_valid_seq: last_forwarded_seq,
                                    }))
                                    .await
                                    .is_err()
                                {
                                    crate::runtime::client::detach_and_release_consumer_state(
                                        &pump_state,
                                        client_id,
                                    );
                                    pump_connection_token.cancel();
                                    break;
                                }
                                let (reply_tx, reply_rx) = oneshot::channel();
                                if pump_native_bootstrap
                                    .send(crate::terminal_actor::NativeBootstrapRequest {
                                        owner: client_id.0,
                                        terminal_id: pump_wire_terminal_id.clone(),
                                        stream_id,
                                        bootstrap_id,
                                        limits: bootstrap_limits,
                                        max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                                        max_frames:
                                            crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
                                        reply: reply_tx,
                                    })
                                    .await
                                    .is_err()
                                {
                                    crate::runtime::client::detach_and_release_consumer_state(
                                        &pump_state,
                                        client_id,
                                    );
                                    pump_connection_token.cancel();
                                    break;
                                }
                                let Ok(Ok(reply)) = reply_rx.await else {
                                    let _ = pump_out_tx
                                        .send(Outbound::Frame(FrameKind::Error {
                                            request_id: None,
                                            code: ErrorCode::CodecUnavailable,
                                            message: "native checkpoint resync failed".to_owned(),
                                        }))
                                        .await;
                                    crate::runtime::client::detach_and_release_consumer_state(
                                        &pump_state,
                                        client_id,
                                    );
                                    pump_connection_token.cancel();
                                    break;
                                };
                                let Ok((cut, cursor)) =
                                    publish_native_bootstrap(&pump_out_tx, reply).await
                                else {
                                    crate::runtime::client::detach_and_release_consumer_state(
                                        &pump_state,
                                        client_id,
                                    );
                                    pump_connection_token.cancel();
                                    break;
                                };
                                let Ok(publication) = activate_native_publication(
                                    &pump_native_handle,
                                    client_id.0,
                                    pump_wire_terminal_id.clone(),
                                    stream_id,
                                    bootstrap_id,
                                    cursor,
                                )
                                .await
                                else {
                                    pump_connection_token.cancel();
                                    break;
                                };
                                published_cut = cut;
                                last_forwarded_seq = cut;
                                output_rx = publication.live;
                                for (seq, bytes) in publication.replay {
                                    if pump_out_tx
                                        .send(Outbound::Frame(FrameKind::TerminalOutput {
                                            terminal_id: pump_wire_terminal_id.clone(),
                                            stream_id,
                                            bootstrap_id,
                                            seq,
                                            bytes: downsample_for_caps(
                                                &bytes,
                                                pump_client_caps,
                                            ),
                                        }))
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                    last_forwarded_seq = seq;
                                }
                                generation_active = true;
                                continue;
                            }
                            let payload = downsample_for_caps(&bytes, pump_client_caps);
                            if send_synthesized_bootstrap(
                                &pump_out_tx,
                                pump_wire_terminal_id.clone(),
                                stream_id,
                                bootstrap_id,
                                stream_profile,
                                bootstrap_limits,
                                cols,
                                rows,
                                base_seq,
                                [payload],
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                            published_cut = base_seq;
                            last_forwarded_seq = base_seq;
                            generation_active = true;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!(
                                terminal_id = ?pump_wire_terminal_id,
                                dropped = n,
                                "TerminalOutput pump lagged; requesting in-band resync",
                            );
                            if !enqueue_output_resync(&pump_resize).await {
                                let _ = tokio::time::timeout(
                                    std::time::Duration::from_secs(1),
                                    pump_out_tx.send(Outbound::Frame(FrameKind::Error {
                                        request_id: None,
                                        code: ErrorCode::InternalError,
                                        message: "terminal output gap could not be resynchronized"
                                            .to_owned(),
                                    })),
                                )
                                .await;
                                crate::runtime::client::detach_and_release_consumer_state(
                                    &pump_state,
                                    client_id,
                                );
                                pump_connection_token.cancel();
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }
        if let Some(state_sync) = state_sync_bootstrap {
            let snap = state_sync.snapshot;
            let cols = snap.cols;
            let rows = snap.rows;
            let Ok(adapted) =
                adapt_bootstrap_snapshot(snap, client_caps, staging_budget.remaining_bytes())
            else {
                fail_prepublication!("state-sync bootstrap adaptation exceeded source budget");
            };
            debug_assert!(adapted.peak_bytes <= staging_budget.remaining_bytes());
            let retained_bytes = adapted.retained_bytes;
            let payloads = adapted.payloads;
            let Ok(mut frames) = synthesized_bootstrap_frames(
                wire_terminal_id,
                stream_id,
                bootstrap_id,
                stream_profile,
                bootstrap_limits,
                cols,
                rows,
                state_sync.base_seq,
                payloads,
            ) else {
                fail_prepublication!("state-sync bootstrap exceeded negotiated bounds");
            };
            if staging_budget
                .append_accounted(&mut bootstrap_frames, &mut frames, retained_bytes)
                .is_err()
            {
                fail_prepublication!("aggregate bootstrap staging budget exceeded");
            }
            continue;
        }

        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if matches!(
            stream_profile,
            BootstrapStreamProfile::NativeState {
                codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2
            }
        ) {
            let (reply_tx, reply_rx) = oneshot::channel();
            if handle
                .native_bootstrap
                .send(crate::terminal_actor::NativeBootstrapRequest {
                    owner: client_id.0,
                    terminal_id: wire_terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    limits: bootstrap_limits,
                    max_bytes: staging_budget.remaining_bytes(),
                    max_frames: staging_budget.remaining_frames(),
                    reply: reply_tx,
                })
                .await
                .is_err()
            {
                warn!(?terminal_id, "pane actor dropped before native bootstrap");
                fail_prepublication!("pane actor dropped native bootstrap request");
            }
            match reply_rx.await {
                Ok(Ok(mut reply)) => {
                    let cut = reply.base_seq;
                    let publication_cursor = reply.publication_cursor;
                    if staging_budget
                        .append_accounted(
                            &mut bootstrap_frames,
                            &mut reply.frames,
                            reply.retained_bytes,
                        )
                        .is_err()
                    {
                        fail_prepublication!("aggregate bootstrap staging budget exceeded");
                    }
                    if let Some(gate) = snapshot_gates
                        .iter_mut()
                        .find(|gate| gate.terminal_id == terminal_id)
                    {
                        gate.cut = Some(cut);
                        gate.native_cursor = Some(publication_cursor);
                    }
                }
                Ok(Err(error)) => {
                    warn!(?terminal_id, %error, "native checkpoint failed before attach publication");
                    fail_prepublication!("native checkpoint capture failed");
                }
                Err(_) => {
                    warn!(?terminal_id, "pane actor dropped native checkpoint reply");
                    fail_prepublication!("pane actor dropped native checkpoint reply");
                }
            }
            continue;
        }

        let (reply_tx, reply_rx) = oneshot::channel();
        if handle
            .snapshot
            .send(SnapshotRequest {
                scrollback: scrollback_req,
                max_bytes: synthesized_source_max,
                max_frames: staging_budget.remaining_frames(),
                chunk_bytes: aggregate_chunk_bytes,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!(
                ?terminal_id,
                "pane actor dropped before synthesized bootstrap"
            );
            fail_prepublication!("pane actor dropped synthesized bootstrap request");
        }
        let (snap, cut) = match reply_rx.await {
            Ok(Ok(reply)) => reply,
            Ok(Err(error)) => {
                warn!(?terminal_id, %error, "bounded snapshot synthesis failed");
                fail_prepublication!("synthesized bootstrap source limit exceeded");
            }
            Err(_) => {
                warn!(
                    ?terminal_id,
                    "pane actor dropped synthesized snapshot reply"
                );
                fail_prepublication!("pane actor dropped synthesized snapshot reply");
            }
        };
        let cols = snap.cols;
        let rows = snap.rows;
        let Ok(adapted) =
            adapt_bootstrap_snapshot(snap, client_caps, staging_budget.remaining_bytes())
        else {
            fail_prepublication!("synthesized bootstrap adaptation exceeded source budget");
        };
        debug_assert!(adapted.peak_bytes <= staging_budget.remaining_bytes());
        let retained_bytes = adapted.retained_bytes;
        let payloads = adapted.payloads;
        let Ok(mut frames) = synthesized_bootstrap_frames(
            wire_terminal_id,
            stream_id,
            bootstrap_id,
            stream_profile,
            bootstrap_limits,
            cols,
            rows,
            cut,
            payloads,
        ) else {
            fail_prepublication!("synthesized bootstrap exceeded negotiated bounds");
        };
        if staging_budget
            .append_accounted(&mut bootstrap_frames, &mut frames, retained_bytes)
            .is_err()
        {
            fail_prepublication!("aggregate bootstrap staging budget exceeded");
        }
        if let Some(gate) = snapshot_gates
            .iter_mut()
            .find(|gate| gate.terminal_id == terminal_id)
        {
            gate.cut = Some(cut);
        }
    }

    // Commit the replacement only after every pane has produced a complete,
    // bounded bootstrap. Until this point the prior generation's pumps remain
    // live and every new pump is parked on its unpublished gate.
    if same_session_reattach {
        super::client::abort_output_pumps(output_pumps, client_id, "replacement ATTACH").await;
    }
    let mut committed_output_pumps = staged_output_pumps;
    output_pumps
        .spawn_local(async move { while committed_output_pumps.join_next().await.is_some() {} });

    if out_tx
        .send(Outbound::Frame(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id,
        }))
        .await
        .is_err()
    {
        crate::runtime::client::detach_and_release_consumer_state(state, client_id);
        return;
    }
    crate::hooks::fire_hook(
        state,
        crate::hooks::HookEvent::client_attached(client_id, &session_name),
    );
    for frame in bootstrap_frames {
        if out_tx.send(Outbound::Frame(frame)).await.is_err() {
            crate::runtime::client::detach_and_release_consumer_state(state, client_id);
            return;
        }
    }

    if out_tx
        .send(Outbound::Frame(FrameKind::AttachReady { attach_id }))
        .await
        .is_err()
    {
        crate::runtime::client::detach_and_release_consumer_state(state, client_id);
        return;
    }
    let _ = live_gate_tx.send(true);
    for gate in snapshot_gates {
        let Some(cut) = gate.cut else {
            continue;
        };
        let mut start = OutputPumpStart {
            published_cut: cut,
            replay: Vec::new(),
            live: None,
        };
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if let Some(cursor) = gate.native_cursor {
            let (reply, publication) = oneshot::channel();
            if gate
                .handle
                .native_publication
                .send(crate::terminal_actor::NativePublicationRequest {
                    owner: client_id.0,
                    terminal_id: gate.wire_terminal_id,
                    stream_id,
                    bootstrap_id,
                    cursor,
                    reply,
                })
                .await
                .is_err()
            {
                crate::runtime::client::detach_and_release_consumer_state(state, client_id);
                connection_token.cancel();
                return;
            }
            let Ok(Ok(publication)) = publication.await else {
                crate::runtime::client::detach_and_release_consumer_state(state, client_id);
                connection_token.cancel();
                return;
            };
            start.replay = publication.replay;
            start.live = Some(publication.live);
        }
        let _ = gate.gate.send(start);
    }
}

/// phux-2lj: Apply the ATTACH viewport to every pane in the freshly-
/// attached session.
///
/// Panes are spawned at a hardcoded 80x24 default ([`seed_session_with_pty`]
/// / [`seed_session_with_actor`]) because the session may exist before any
/// client attaches (e.g. `phux-server` pre-seeding). On the first attach
/// we have to size the PTY to match the client's outer viewport, otherwise
/// full-screen TUIs (vim, htop) think they're running in 24 rows and
/// render into a fraction of the visible area. This mirrors what
/// [`crate::runtime::commands::handle_viewport_resize`] does for a live
/// `VIEWPORT_RESIZE` frame.
///
/// The resize is fire-and-forget on the per-actor mpsc channel — same
/// primitive `handle_viewport_resize` and `handle_terminal_resize` use.
/// We `try_send` rather than `.await` so we can stay in a sync helper
/// (no impact on `handle_attach`'s lock ordering) and because the
/// resize channel is sized at `DEFAULT_INPUT_MAILBOX = 64`, which is
/// well above the worst-case number of panes per attach (1 today; would
/// stay << 64 even with multi-window sessions).
///
/// The `pane.dims` update is wrapped in `with_mut` once so the registry
/// stays consistent with what future `TERMINAL_SNAPSHOT` payloads will
/// report; the resize sends are emitted while holding the same lock,
/// matching `handle_viewport_resize`'s pattern (the actor's mailbox is
/// independent of the state lock).
pub(crate) fn apply_attach_viewport(
    state: &SharedState,
    client_id: ClientId,
    panes_to_snapshot: &[AttachSnapshotPane],
    viewport: phux_protocol::wire::frame::ViewportInfo,
) {
    let cols = viewport.cols;
    let rows = viewport.rows;
    if cols == 0 || rows == 0 {
        // SPEC §10.5: zero-dimension viewports are treated as no-ops
        // rather than kernel errors. Skip the resize entirely.
        return;
    }
    state.with_mut(|s| {
        // phux-nk07: this client now contributes its viewport to every pane
        // it just subscribed to; each pane's geometry is the window-size
        // policy applied across all subscribers (so a second, smaller client
        // attaching under `smallest` shrinks the grid rather than the
        // last-writer winning). `Manual` (or no usable viewport) skips the
        // resize, leaving the pane at its current size.
        s.set_client_viewport(client_id, viewport);
        for pane in panes_to_snapshot {
            let Some((cols, rows)) =
                s.resolve_terminal_geometry(pane.terminal_id, Some(viewport))
            else {
                continue;
            };
            if let Some(pane_entry) = s.registry_mut().terminal_mut(pane.terminal_id) {
                pane_entry.dims = (cols, rows);
            }
            // ATTACH-time resize: do NOT resync — the attach handshake
            // already sends an authoritative TERMINAL_SNAPSHOT, and a
            // resync broadcast here would race ahead of it (phux-8v1).
            // Pixel geometry rides along (most recent usable subscriber
            // report — normally the viewport recorded above).
            match pane.handle.resize.try_send(ResizeRequest {
                cols,
                rows,
                cell_px: s.resolve_terminal_cell_px(pane.terminal_id),
                resync_clients: false,
                resync_only: false,
            }) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    warn!(
                        terminal_id = ?pane.terminal_id,
                        cols,
                        rows,
                        "ATTACH viewport apply: pane resize mailbox full; dropping (next VIEWPORT_RESIZE will retry)",
                    );
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    debug!(
                        terminal_id = ?pane.terminal_id,
                        "ATTACH viewport apply: pane actor gone; dropping resize",
                    );
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_staged_pane(pane: u32) -> Vec<FrameKind> {
        let terminal_id = phux_protocol::ids::TerminalId::local(pane + 1);
        let stream_id = StreamId::new(1).expect("stream id");
        let bootstrap_id = BootstrapId::new(1).expect("bootstrap id");
        vec![
            FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 80,
                rows: 24,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: bytes::Bytes::from_static(b"pane"),
            },
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ]
    }

    #[test]
    fn aggregate_staging_budget_rejects_many_panes_without_large_allocations() {
        let mut budget = BootstrapStagingBudget::with_limits(8 * 4, 8 * 3);
        let mut staged = Vec::new();

        for pane in 0..16 {
            let mut frames = tiny_staged_pane(pane);
            let result = budget.append(&mut staged, &mut frames);
            if pane < 8 {
                assert!(result.is_ok(), "pane {pane} fits the aggregate budget");
                assert!(frames.is_empty(), "accepted frames move into staging");
            } else {
                assert!(result.is_err(), "pane {pane} exceeds the aggregate budget");
                assert_eq!(frames.len(), 3, "rejected frames are not appended");
            }
        }

        assert_eq!(staged.len(), 8 * 3);
        assert_eq!(budget.staged_bytes, 8 * 4);
        assert_eq!(budget.staged_frames, 8 * 3);
    }

    #[test]
    fn bootstrap_adaptation_peak_includes_sources_scratch_and_outputs() {
        let mut scrollback = Vec::new();
        scrollback
            .try_reserve_exact(512)
            .expect("scrollback reserve");
        scrollback.resize(512, b's');
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(1_024).expect("snapshot reserve");
        bytes.resize(1_024, b'x');
        let source_capacity = scrollback.capacity() + bytes.capacity();
        let peak_budget = source_capacity.checked_mul(2).expect("peak budget");
        let caps = ClientCapabilities::default()
            .with_color_support(phux_protocol::caps::ColorSupport::Indexed256);
        assert!(!crate::downsample::caps_pass_through(caps));

        let adapted = adapt_bootstrap_snapshot(
            crate::grid::SnapshotBytes {
                cols: 80,
                rows: 24,
                bytes,
                scrollback,
            },
            caps,
            peak_budget,
        )
        .expect("bounded capability adaptation");

        assert_eq!(
            adapted
                .payloads
                .iter()
                .map(bytes::Bytes::len)
                .sum::<usize>(),
            source_capacity,
        );
        assert_eq!(adapted.retained_bytes, source_capacity);
        assert!(adapted.peak_bytes <= peak_budget);
    }

    #[test]
    fn aggregate_staging_charges_many_tiny_native_records_by_capacity() {
        const RECORDS: usize = 64;
        const RECORD_CAPACITY: usize = 1_024;
        let retained_per_pane = RECORDS * RECORD_CAPACITY;
        let mut budget = BootstrapStagingBudget::with_limits(retained_per_pane * 3, usize::MAX);
        let mut staged = Vec::new();

        for pane in 0..4_u32 {
            let terminal_id = phux_protocol::ids::TerminalId::local(pane + 1);
            let stream_id = StreamId::new(u64::from(pane) + 1).expect("stream id");
            let bootstrap_id = BootstrapId::new(u64::from(pane) + 1).expect("bootstrap id");
            let mut frames = Vec::new();
            frames.push(FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::NativeState {
                    codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2,
                },
                cols: 80,
                rows: 24,
                base_seq: 0,
            });
            let mut retained_bytes = 0_usize;
            for chunk_seq in 0..RECORDS {
                let mut record = Vec::with_capacity(RECORD_CAPACITY);
                record.push(b'x');
                retained_bytes += record.capacity();
                frames.push(FrameKind::BootstrapChunk {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    chunk_seq: u32::try_from(chunk_seq).expect("chunk sequence"),
                    payload: bytes::Bytes::from(record),
                });
            }
            frames.push(FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            });
            let wire_bytes = frames
                .iter()
                .map(|frame| match frame {
                    FrameKind::BootstrapChunk { payload, .. } => payload.len(),
                    _ => 0,
                })
                .sum::<usize>();
            assert_eq!(retained_bytes, retained_per_pane);
            assert!(retained_bytes > wire_bytes);

            let result = budget.append_accounted(&mut staged, &mut frames, retained_bytes);
            assert_eq!(result.is_ok(), pane < 3);
        }
        assert_eq!(budget.staged_bytes, retained_per_pane * 3);
    }

    #[test]
    fn aggregate_staging_charges_tiny_rewrites_by_retained_capacity() {
        fn kitty_snapshot() -> crate::grid::SnapshotBytes {
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(64 * 1024).expect("kitty reserve");
            bytes.extend_from_slice(b"\x1b_Gf=100,a=T;");
            bytes.resize((64 * 1024) - 2, b'A');
            bytes.extend_from_slice(b"\x1b\\");
            crate::grid::SnapshotBytes {
                cols: 80,
                rows: 24,
                bytes,
                scrollback: Vec::new(),
            }
        }
        let caps = ClientCapabilities::default()
            .with_color_support(phux_protocol::caps::ColorSupport::Indexed256)
            .with_image_protocols(phux_protocol::caps::ImageProtocolSet::new());
        let sample =
            adapt_bootstrap_snapshot(kitty_snapshot(), caps, 2 * 64 * 1024).expect("rewrite");
        let retained_per_pane = sample.retained_bytes;
        let wire_per_pane = sample.payloads.iter().map(bytes::Bytes::len).sum::<usize>();
        assert!(
            retained_per_pane > wire_per_pane,
            "dropped Kitty payload retains rewrite allocation capacity"
        );
        drop(sample);

        let mut budget = BootstrapStagingBudget::with_limits(retained_per_pane * 3, usize::MAX);
        let mut staged = Vec::new();
        for pane in 0..4_u32 {
            let adapted = adapt_bootstrap_snapshot(
                kitty_snapshot(),
                caps,
                retained_per_pane.checked_mul(2).expect("peak budget"),
            )
            .expect("bounded pane rewrite");
            let retained_bytes = adapted.retained_bytes;
            let mut frames = synthesized_bootstrap_frames(
                phux_protocol::ids::TerminalId::local(pane + 1),
                StreamId::new(u64::from(pane) + 1).expect("stream id"),
                BootstrapId::new(u64::from(pane) + 1).expect("bootstrap id"),
                BootstrapStreamProfile::SynthesizedVtRaw,
                BootstrapLimits::new(
                    phux_protocol::MAX_BOOTSTRAP_CHUNK_BYTES,
                    phux_protocol::DEFAULT_HISTORY_PAGE_BYTES,
                )
                .expect("limits"),
                80,
                24,
                0,
                adapted.payloads,
            )
            .expect("bootstrap frames");
            let result = budget.append_accounted(&mut staged, &mut frames, retained_bytes);
            assert_eq!(result.is_ok(), pane < 3);
        }
        assert_eq!(budget.staged_bytes, retained_per_pane * 3);
    }

    #[test]
    fn synthesized_bootstrap_is_built_completely_before_publication() {
        let terminal_id = phux_protocol::ids::TerminalId::local(7);
        let stream_id = StreamId::new(3).expect("stream id");
        let bootstrap_id = BootstrapId::new(5).expect("bootstrap id");
        let limits = BootstrapLimits::new(3, phux_protocol::DEFAULT_HISTORY_PAGE_BYTES)
            .expect("bounded test limits");
        let frames = synthesized_bootstrap_frames(
            terminal_id.clone(),
            stream_id,
            bootstrap_id,
            BootstrapStreamProfile::SynthesizedVtRaw,
            limits,
            80,
            24,
            11,
            [bytes::Bytes::from_static(b"abcdefg")],
        )
        .expect("build complete bootstrap");

        assert!(matches!(
            frames.first(),
            Some(FrameKind::BootstrapBegin {
                terminal_id: id,
                stream_id: stream,
                bootstrap_id: bootstrap,
                base_seq: 11,
                ..
            }) if id == &terminal_id && *stream == stream_id && *bootstrap == bootstrap_id
        ));
        let chunks: Vec<_> = frames
            .iter()
            .filter_map(|frame| match frame {
                FrameKind::BootstrapChunk {
                    chunk_seq, payload, ..
                } => Some((*chunk_seq, payload.as_ref())),
                _ => None,
            })
            .collect();
        assert_eq!(
            chunks,
            vec![
                (0, b"abc".as_slice()),
                (1, b"def".as_slice()),
                (2, b"g".as_slice())
            ]
        );
        assert!(matches!(
            frames.last(),
            Some(FrameKind::BootstrapReady {
                terminal_id: id,
                stream_id: stream,
                bootstrap_id: bootstrap,
                history_cursor: None,
            }) if id == &terminal_id && *stream == stream_id && *bootstrap == bootstrap_id
        ));
    }

    #[test]
    fn prepare_attach_rejects_pane_source_count_before_registration() {
        let state = SharedState::new();
        let (_session, window, _pane) = state.with_mut(|server| server.seed_session("bounded"));
        state.with_mut(|server| {
            for _ in 0..MAX_AGGREGATE_BOOTSTRAP_PANES {
                server
                    .registry_mut()
                    .new_terminal(window)
                    .expect("bounded test pane");
            }
        });
        let client_id = state.with_mut(crate::state::ServerState::new_client_id);
        let (out_tx, _out_rx) = tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);
        assert!(matches!(
            prepare_attach(
                &state,
                client_id,
                "bounded",
                &out_tx,
                ClientCapabilities::default(),
                BootstrapProfile::SynthesizedVtRaw,
                BootstrapLimits::default(),
            ),
            Err(crate::state::AttachError::ResourceLimit)
        ));
        assert!(!state.with(|server| server.attached().contains_key(&client_id)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn saturated_resync_mailbox_blocks_until_actor_accepts_request() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        tx.send(ResizeRequest {
            cols: 80,
            rows: 24,
            cell_px: None,
            resync_clients: false,
            resync_only: false,
        })
        .await
        .expect("occupy resize mailbox");

        let mut pending = Box::pin(enqueue_output_resync(&tx));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut pending)
                .await
                .is_err(),
            "lagged pump must not resume while the resync mailbox is full"
        );
        assert!(
            !rx.recv().await.expect("occupied request").resync_only,
            "first request is the existing mailbox occupant"
        );
        assert!(pending.await, "resync queues once capacity is available");
        let queued = rx.recv().await.expect("queued resync");
        assert!(queued.resync_only && queued.resync_clients);

        drop(rx);
        assert!(
            !enqueue_output_resync(&tx).await,
            "closed actor mailbox fails instead of resuming delta forwarding"
        );
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn native_attach_handle() -> (
        crate::terminal_actor::TerminalHandle,
        tokio::sync::mpsc::Receiver<crate::terminal_actor::ConsumerAttachRequest>,
        tokio::sync::mpsc::Receiver<crate::terminal_actor::NativeBootstrapRequest>,
        tokio::sync::mpsc::Receiver<crate::terminal_actor::NativePublicationRequest>,
    ) {
        use tokio::sync::{broadcast, mpsc, watch};

        let (output, _seed) = broadcast::channel(8);
        let (consumer_attach, consumer_attach_rx) = mpsc::channel(8);
        let (native_bootstrap, native_bootstrap_rx) = mpsc::channel(8);
        let (native_publication, native_publication_rx) = mpsc::channel(8);
        (
            crate::terminal_actor::TerminalHandle {
                input: mpsc::channel(8).0,
                encoded_input: mpsc::channel(8).0,
                input_snapshot: watch::channel(crate::input::InputEncoderSnapshot::default()).1,
                snapshot: mpsc::channel(8).0,
                native_bootstrap,
                native_publication,
                native_history: mpsc::channel(8).0,
                native_release: mpsc::channel(8).0,
                set_default_colors: mpsc::channel(8).0,
                screen: mpsc::channel(8).0,
                upgrade: mpsc::channel(8).0,
                pwd: mpsc::channel(8).0,
                output,
                resize: mpsc::channel(8).0,
                consumer_attach,
                consumer_detach: mpsc::channel(8).0,
                consumer_ack: mpsc::channel(8).0,
                subscribe_to_events: mpsc::channel(8).0,
                unsubscribe_from_events: mpsc::channel(8).0,
                control: mpsc::channel(8).0,
                cols: 80,
                rows: 24,
            },
            consumer_attach_rx,
            native_bootstrap_rx,
            native_publication_rx,
        )
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn answer_native_attach(
        consumer_attach_rx: &mut tokio::sync::mpsc::Receiver<
            crate::terminal_actor::ConsumerAttachRequest,
        >,
        native_bootstrap_rx: &mut tokio::sync::mpsc::Receiver<
            crate::terminal_actor::NativeBootstrapRequest,
        >,
        native_publication_rx: &mut tokio::sync::mpsc::Receiver<
            crate::terminal_actor::NativePublicationRequest,
        >,
        succeed: bool,
    ) {
        let registration = consumer_attach_rx
            .recv()
            .await
            .expect("consumer registration");
        registration
            .reply
            .send(Ok(crate::terminal_actor::ConsumerAttachOutcome {
                tick_managed: false,
                state_sync_bootstrap: None,
            }))
            .expect("consumer registration reply");
        let native = native_bootstrap_rx.recv().await.expect("native preflight");
        if !succeed {
            native
                .reply
                .send(Err(crate::native_state::NativeStateError::LimitExceeded))
                .expect("continuation-cap failure reply");
            return;
        }
        let terminal_id = native.terminal_id.clone();
        native
            .reply
            .send(Ok(crate::terminal_actor::NativeBootstrapReply {
                frames: vec![
                    FrameKind::BootstrapBegin {
                        terminal_id: terminal_id.clone(),
                        stream_id: native.stream_id,
                        bootstrap_id: native.bootstrap_id,
                        profile: BootstrapStreamProfile::NativeState {
                            codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2,
                        },
                        cols: 80,
                        rows: 24,
                        base_seq: 0,
                    },
                    FrameKind::BootstrapChunk {
                        terminal_id: terminal_id.clone(),
                        stream_id: native.stream_id,
                        bootstrap_id: native.bootstrap_id,
                        chunk_seq: 0,
                        payload: bytes::Bytes::from_static(b"opaque"),
                    },
                    FrameKind::BootstrapReady {
                        terminal_id,
                        stream_id: native.stream_id,
                        bootstrap_id: native.bootstrap_id,
                        history_cursor: None,
                    },
                ],
                retained_bytes: b"opaque".len(),
                base_seq: 0,
                publication_cursor: [7; 32],
            }))
            .expect("native success reply");
        let publication = native_publication_rx
            .recv()
            .await
            .expect("native publication fence");
        assert_eq!(publication.cursor, [7; 32]);
        publication
            .reply
            .send(Ok(crate::terminal_actor::NativePublicationReply {
                replay: Vec::new(),
                live: tokio::sync::broadcast::channel(1).1,
            }))
            .expect("native publication reply");
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    fn native_profile() -> BootstrapProfile {
        BootstrapProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2,
            features: phux_protocol::caps::EngineFeatureSet::required_native(),
        }
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    #[tokio::test(flavor = "current_thread")]
    async fn fresh_native_capacity_failure_sends_error_then_closes_without_publication() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let (_session, _window, terminal) =
                    state.with_mut(|s| s.seed_session("fresh-failure"));
                let (
                    handle,
                    mut consumer_attach_rx,
                    mut native_bootstrap_rx,
                    mut native_publication_rx,
                ) = native_attach_handle();
                state.with_mut(|s| {
                    let _ = s.register_terminal_handle(terminal, handle, CancellationToken::new());
                });
                let client_id = state.with_mut(crate::state::ServerState::new_client_id);
                let (out_tx, mut out_rx) =
                    tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);
                let root_token = CancellationToken::new();
                let connection_token = CancellationToken::new();
                let mut output_pumps = JoinSet::new();

                let attach = handle_attach(
                    &state,
                    client_id,
                    41,
                    AttachTarget::ByName("fresh-failure".to_owned()),
                    phux_protocol::wire::frame::ViewportInfo::new(80, 24),
                    false,
                    0,
                    &out_tx,
                    ClientCapabilities::default(),
                    native_profile(),
                    BootstrapLimits::default(),
                    &root_token,
                    &mut output_pumps,
                    &connection_token,
                );
                let actor = answer_native_attach(
                    &mut consumer_attach_rx,
                    &mut native_bootstrap_rx,
                    &mut native_publication_rx,
                    false,
                );
                tokio::join!(attach, actor);

                assert!(matches!(
                    out_rx.recv().await,
                    Some(Outbound::TerminalError {
                        code: ErrorCode::CodecUnavailable,
                        ..
                    })
                ));
                assert!(out_rx.try_recv().is_err(), "no ATTACHED or BEGIN may leak");
                assert!(connection_token.is_cancelled());
                assert!(state.with(|s| !s.attached().contains_key(&client_id)));
                drop(out_tx);
                assert!(
                    out_rx.recv().await.is_none(),
                    "fatal fresh attach must reach EOF"
                );
            })
            .await;
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    #[tokio::test(flavor = "current_thread")]
    async fn replacement_native_capacity_failure_closes_but_preserves_terminal_state() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let (_session, _window, terminal) =
                    state.with_mut(|s| s.seed_session("replacement-failure"));
                let (
                    handle,
                    mut consumer_attach_rx,
                    mut native_bootstrap_rx,
                    mut native_publication_rx,
                ) = native_attach_handle();
                state.with_mut(|s| {
                    let _ = s.register_terminal_handle(terminal, handle, CancellationToken::new());
                });
                let client_id = state.with_mut(crate::state::ServerState::new_client_id);
                let (out_tx, mut out_rx) =
                    tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);
                let root_token = CancellationToken::new();
                let connection_token = CancellationToken::new();
                let mut output_pumps = JoinSet::new();

                let first = handle_attach(
                    &state,
                    client_id,
                    51,
                    AttachTarget::ByName("replacement-failure".to_owned()),
                    phux_protocol::wire::frame::ViewportInfo::new(80, 24),
                    false,
                    0,
                    &out_tx,
                    ClientCapabilities::default(),
                    native_profile(),
                    BootstrapLimits::default(),
                    &root_token,
                    &mut output_pumps,
                    &connection_token,
                );
                tokio::join!(
                    first,
                    answer_native_attach(
                        &mut consumer_attach_rx,
                        &mut native_bootstrap_rx,
                        &mut native_publication_rx,
                        true,
                    )
                );
                for _ in 0..5 {
                    out_rx.recv().await.expect("initial attach publication");
                }
                assert!(state.with(|s| s.attached().contains_key(&client_id)));

                let replacement = handle_attach(
                    &state,
                    client_id,
                    52,
                    AttachTarget::ByName("replacement-failure".to_owned()),
                    phux_protocol::wire::frame::ViewportInfo::new(80, 24),
                    false,
                    0,
                    &out_tx,
                    ClientCapabilities::default(),
                    native_profile(),
                    BootstrapLimits::default(),
                    &root_token,
                    &mut output_pumps,
                    &connection_token,
                );
                tokio::join!(
                    replacement,
                    answer_native_attach(
                        &mut consumer_attach_rx,
                        &mut native_bootstrap_rx,
                        &mut native_publication_rx,
                        false,
                    )
                );
                assert!(matches!(
                    out_rx.recv().await,
                    Some(Outbound::TerminalError {
                        code: ErrorCode::CodecUnavailable,
                        ..
                    })
                ));
                assert!(connection_token.is_cancelled());
                assert!(state.with(|s| !s.attached().contains_key(&client_id)));
                assert!(
                    state.with(|s| s.registry().terminal(terminal).is_some()),
                    "failed replacement must not reap canonical terminal state"
                );
                output_pumps.abort_all();
                while output_pumps.join_next().await.is_some() {}
                drop(out_tx);
                assert!(
                    out_rx.recv().await.is_none(),
                    "fatal replacement must close cleanly after ERROR"
                );
            })
            .await;
    }
}
