//! Control-plane command handlers and pane spawning.

use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::RolePolicy;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ControlAction, DetachReason, ErrorCode,
    FrameKind, InputMode, ResourceLifecycle, StateScope, TerminalSignal, ViewportInfo,
};
use std::collections::HashSet;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use super::client::PaneEvents;
use super::input_lane::InputLaneHandle;
use super::{AttachPrepared, spawn_agent_state_drain, spawn_terminal_exit_watcher};
use crate::agent_asked::{AskedPayload, AskedSource};
use crate::resource::{ResourceHandle, WrongResourceKind};
use crate::runtime::pump;
use crate::state::RoleEffects;
use crate::state::{
    ClientId, Outbound, RelayRoute, Resolved, ResolvedOwned, ServerInterceptedKey, SharedState,
    TerminalInput,
};
use crate::terminal_actor::{
    ConsumerAckRequest, ControlRequest, ResizeRequest, ScreenReply, ScreenRequest, TerminalActor,
    TerminalHandle,
};

/// The per-pane answer to "does this Terminal own a live `AgentSession`
/// child?" (ADR-0103 §5), read once per detector tick.
fn live_session_probe(
    state: &SharedState,
    terminal: phux_core::ids::ResourceId,
) -> crate::agent_detect::live_session::LiveSessionProbe {
    let state = state.clone();
    std::rc::Rc::new(move || state.with(|s| s.has_live_agent_session_child(terminal)))
}

/// The command-result shape of a Terminal-only request aimed at a resource
/// of another kind.
pub(crate) fn wrong_resource_kind(error: WrongResourceKind) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::WrongResourceKind,
        message: error.to_string(),
    }
}

/// The `TERMINAL_NOT_FOUND` refusal for `terminal_id`.
fn terminal_not_found(terminal_id: &phux_protocol::ids::ResourceId) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::TerminalNotFound,
        message: format!("no such terminal: {terminal_id:?}"),
    }
}

/// The grid a pane is built at when its real geometry arrives later (an
/// attaching viewport, or a spawn without `SPAWN_RESOURCE.initial_size`).
pub(crate) const DEFAULT_SPAWN_DIMS: (u16, u16) = crate::state::HEADLESS_TERMINAL_DIMS;

/// Who and what caused a pane's spawn, for its `pane_spawned` stamp
/// (ADR-0123). `Default` is a server-driven spawn, such as a seed pane.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SpawnAttribution {
    /// The connection that asked for the pane.
    pub(crate) actor: Option<ClientId>,
    /// The spawn's idempotency key (ADR-0126).
    pub(crate) operation_id: Option<phux_protocol::ids::IdempotencyKey>,
    /// Seconds the pane is retained after its process exits (ADR-0124);
    /// `None` falls back to `defaults.retain-on-exit`.
    pub(crate) retain_secs: Option<u32>,
}

/// Where a session's seed pane came from: its agent-session provenance and
/// who asked for it (ADR-0123). `Default` is a server-driven seed.
#[derive(Debug, Default)]
pub(crate) struct SeedOrigin {
    /// Opaque native agent-session provenance to install on the pane.
    pub(crate) agent_session: Option<Vec<u8>>,
    /// The seed pane's `pane_spawned` attribution.
    pub(crate) attribution: SpawnAttribution,
}

/// Journal a pane's `pane_spawned` (L1 §7.1) in the lock that registers its
/// actor, so it always takes a lower `seq` than the pane's `pane_closed`
/// (ADR-0123).
fn journal_pane_spawned(
    s: &mut crate::state::ServerState,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    attribution: SpawnAttribution,
) {
    let record = crate::state::EventRecord::new(
        Some(wire_terminal_id.clone()),
        AgentEvent::ResourceSpawned {
            kind: phux_protocol::ids::ResourceKind::Terminal,
            parent: None,
        },
    )
    .with_actor(attribution.actor)
    .with_operation_id(attribution.operation_id);
    let _ = s.record_and_fanout(record);
}

/// Install agent-session provenance on a freshly registered pane.
fn set_agent_session(
    s: &mut crate::state::ServerState,
    wire: phux_protocol::ids::ResourceId,
    agent_session: Option<Vec<u8>>,
) {
    if let Some(value) = agent_session {
        s.metadata_set(
            &phux_protocol::wire::frame::Scope::Resource(wire),
            phux_protocol::wire::frame::RESOURCE_AGENT_SESSION_KEY,
            value,
        );
    }
}

/// Registry-side setup every PTY pane shares: stamp its spawn cwd and tell
/// the child which pane and server it belongs to. Returns the wire id.
fn register_pty_command(
    s: &mut crate::state::ServerState,
    terminal: phux_core::ids::ResourceId,
    cmd: &mut portable_pty::CommandBuilder,
) -> phux_protocol::ids::ResourceId {
    stamp_spawn_cwd(s, terminal, spawn_cwd_of(cmd));
    let wire = s.intern_terminal_wire(terminal);
    crate::terminal_actor::apply_terminal_id(cmd, &wire);
    crate::terminal_actor::apply_server_socket(cmd, s.server_socket_path());
    wire
}

/// Build and start the actor for an already-registered pane: install its
/// event (and, with a PTY, agent-state) sinks, journal `pane_spawned`, and
/// start its drains and exit watcher. A build failure reaps the pane.
#[allow(
    clippy::too_many_arguments,
    reason = "every input must be true of the pane before its actor becomes visible to another client"
)]
fn launch_pane_actor(
    state: &SharedState,
    terminal: phux_core::ids::ResourceId,
    cmd: Option<portable_pty::CommandBuilder>,
    (cols, rows): (u16, u16),
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    attribution: SpawnAttribution,
) -> Result<phux_protocol::ids::ResourceId, crate::terminal_actor::TerminalActorError> {
    let has_pty = cmd.is_some();
    let terminal_token = root_token.child_token();
    let bundle = match TerminalActor::build_with_token_and_colors(
        cols,
        rows,
        cmd,
        scrollback,
        terminal_token.clone(),
        default_colors,
    ) {
        Ok(bundle) => bundle,
        Err(err) => {
            state.with_mut(|s| s.reap_terminal(terminal));
            return Err(err);
        }
    };
    let crate::terminal_actor::TerminalActorBundle {
        mut actor,
        handle,
        exit_notify,
        ..
    } = bundle;
    // Sinks go in before `actor.run()` consumes the actor.
    let (event_sink, event_source) = crate::resource::event_sink::event_sink(EVENT_SINK_CAPACITY);
    actor.set_event_sink(event_sink);
    let agent_rx = has_pty.then(|| {
        let (agent_tx, agent_rx) = tokio::sync::mpsc::channel(AGENT_STATE_SINK_CAPACITY);
        actor.set_agent_state_sink(agent_tx);
        actor.set_live_session_probe(live_session_probe(state, terminal));
        agent_rx
    });
    let wire_terminal_id = state.with_mut(|s| {
        let _ = s.spawn_resource_actor(terminal, handle, terminal_token, actor.run());
        if has_pty {
            // ADR-0124: a spawn that did not ask gets the operator's default.
            let retain = attribution
                .retain_secs
                .or_else(|| s.retain_policy().resolve(None));
            if let Some(secs) = retain {
                s.note_retain_request(terminal, secs);
            }
        }
        let wire = s.intern_terminal_wire(terminal);
        journal_pane_spawned(s, &wire, attribution);
        wire
    });
    if let Some(agent_rx) = agent_rx {
        spawn_agent_state_drain(state.clone(), wire_terminal_id.clone(), agent_rx);
    }
    spawn_terminal_exit_watcher(
        state.clone(),
        terminal,
        exit_notify,
        root_token.clone(),
        Some(PaneEvents {
            wire: wire_terminal_id.clone(),
            source: event_source,
        }),
    );
    Ok(wire_terminal_id)
}

/// Seed `(session, window, pane)` named `name`; the pane runs `cmd` in a PTY,
/// or is a no-PTY actor when `cmd` is `None`.
fn seed_session(
    state: &SharedState,
    name: &str,
    mut cmd: Option<portable_pty::CommandBuilder>,
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    origin: SeedOrigin,
) -> Result<phux_core::ids::ResourceId, crate::terminal_actor::TerminalActorError> {
    let terminal = state.with_mut(|s| {
        let terminal = s.seed_session(name).2;
        let wire = match cmd.as_mut() {
            Some(cmd) => register_pty_command(s, terminal, cmd),
            None => s.intern_terminal_wire(terminal),
        };
        set_agent_session(s, wire, origin.agent_session);
        terminal
    });
    let wire_terminal_id = launch_pane_actor(
        state,
        terminal,
        cmd,
        DEFAULT_SPAWN_DIMS,
        scrollback,
        root_token,
        default_colors,
        origin.attribution,
    )?;
    crate::hooks::fire_hook(
        state,
        crate::hooks::HookEvent::after_new_pane(&wire_terminal_id, Some(name)),
    );
    Ok(terminal)
}

/// Seed a session whose pane is a no-PTY actor (no child process).
pub(crate) fn seed_session_with_actor(
    state: &SharedState,
    name: &str,
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
) -> Result<phux_core::ids::ResourceId, crate::terminal_actor::TerminalActorError> {
    seed_session(
        state,
        name,
        None,
        scrollback,
        root_token,
        None,
        SeedOrigin::default(),
    )
}

/// Seed `(session, window, pane)` with a PTY-backed pane running `cmd`.
pub fn seed_session_with_pty(
    state: &SharedState,
    name: &str,
    cmd: portable_pty::CommandBuilder,
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
) -> Result<phux_core::ids::ResourceId, crate::terminal_actor::TerminalActorError> {
    seed_session_with_pty_and_colors(state, name, cmd, scrollback, root_token, None)
}

/// Palette-seeded variant used when a client's HELLO creates the session.
pub fn seed_session_with_pty_and_colors(
    state: &SharedState,
    name: &str,
    cmd: portable_pty::CommandBuilder,
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Result<phux_core::ids::ResourceId, crate::terminal_actor::TerminalActorError> {
    seed_session(
        state,
        name,
        Some(cmd),
        scrollback,
        root_token,
        default_colors,
        SeedOrigin::default(),
    )
}

/// Registry ownership address for a newly spawned pane.
#[derive(Debug)]
pub(crate) enum SpawnOwnership {
    /// Legacy session ownership (first window in v0.x).
    Session(phux_core::ids::SessionId),
    /// Exact window ownership derived from an existing wire Terminal id.
    Terminal(phux_protocol::ids::ResourceId),
}

/// Add a PTY pane under `ownership`. `initial_size` (`SPAWN_RESOURCE`) sizes
/// the grid, PTY, and registry dims in the transaction that creates the pane,
/// so its first bootstrap is already at the client's geometry. `Ok(None)`
/// means the owner has no window to host the pane.
#[allow(
    clippy::too_many_arguments,
    reason = "every input must be true of the pane before its actor becomes visible to another client"
)]
pub(crate) fn spawn_pane_with_pty_and_colors(
    state: &SharedState,
    ownership: &SpawnOwnership,
    mut cmd: portable_pty::CommandBuilder,
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    agent_session: Option<Vec<u8>>,
    initial_size: Option<(u16, u16)>,
    attribution: SpawnAttribution,
) -> Result<Option<phux_core::ids::ResourceId>, crate::terminal_actor::TerminalActorError> {
    // libghostty has no zero-dimension grid.
    let dims = initial_size.map_or(DEFAULT_SPAWN_DIMS, |(cols, rows)| {
        (cols.max(1), rows.max(1))
    });
    let Some(terminal) = state.with_mut(|s| {
        let terminal = match ownership {
            SpawnOwnership::Session(session) => s.add_pane_to_session(*session)?,
            SpawnOwnership::Terminal(owner) => s.add_pane_to_terminal_owner(owner)?,
        };
        if let Some(pane) = s.registry_mut().terminal_mut(terminal) {
            pane.dims = dims;
        }
        let wire = register_pty_command(s, terminal, &mut cmd);
        set_agent_session(s, wire, agent_session);
        Some(terminal)
    }) else {
        return Ok(None);
    };
    let wire_terminal_id = launch_pane_actor(
        state,
        terminal,
        Some(cmd),
        dims,
        scrollback,
        root_token,
        default_colors,
        attribution,
    )?;
    let session_name = state.with(|s| {
        let window = s.registry().resource(terminal)?.window?;
        let session = s.registry().window(window)?.session;
        s.registry().session(session).map(|sess| sess.name.clone())
    });
    crate::hooks::fire_hook(
        state,
        crate::hooks::HookEvent::after_new_pane(&wire_terminal_id, session_name.as_deref()),
    );
    Ok(Some(terminal))
}

/// The directory a PTY child spawned from `cmd` starts in: the builder's cwd,
/// else the server's own (which the child inherits).
fn spawn_cwd_of(cmd: &portable_pty::CommandBuilder) -> Option<std::path::PathBuf> {
    cmd.get_cwd()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

/// Stamp a new pane's spawn cwd onto its registry descriptor; without it the
/// ATTACHED snapshot reports no cwd until attach refreshes it from the child.
fn stamp_spawn_cwd(
    s: &mut crate::state::ServerState,
    terminal: phux_core::ids::ResourceId,
    cwd: Option<std::path::PathBuf>,
) {
    if let Some(cwd) = cwd
        && let Some(desc) = s.registry_mut().terminal_mut(terminal)
    {
        desc.cwd = cwd;
    }
}

/// Per-pane agent-event sink capacity (SPEC §7.5). A full sink drops and
/// journals a `source_gap` rather than stalling the PTY pump.
pub(crate) const EVENT_SINK_CAPACITY: usize = 64;

/// Per-pane agent-state sink capacity (ADR-0046). The detector is
/// edge-filtered and re-derives each tick, so a dropped event is re-published.
pub(crate) const AGENT_STATE_SINK_CAPACITY: usize = 8;

/// Handle a client's `RESIZE_TERMINAL` (L1 §3.1): set one Terminal's exact
/// size, bypassing the window-size policy, so a headless caller can size a
/// pane. A later view-derived recompute may supersede it (tui.md §4.2).
/// There is no reply frame, so not-found paths log and drop.
pub(crate) fn handle_terminal_resize(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    cols: u16,
    rows: u16,
) {
    state.with_mut(|s| {
        let local = match s.resolve_resource(wire_terminal_id).into_owned() {
            ResolvedOwned::Remote(route) => {
                if !relay_satellite_frame(
                    client_id,
                    wire_terminal_id,
                    &route,
                    "RESIZE_TERMINAL",
                    |id| FrameKind::ResizeTerminal {
                        terminal_id: id,
                        cols,
                        rows,
                    },
                ) {
                    warn!(
                        ?client_id,
                        ?wire_terminal_id,
                        cols,
                        rows,
                        "RESIZE_TERMINAL: SATELLITE-routed pane id rejected on non-federation-hub server",
                    );
                }
                return;
            }
            ResolvedOwned::Unknown => {
                debug!(
                    ?client_id,
                    ?wire_terminal_id,
                    cols,
                    rows,
                    "RESIZE_TERMINAL: unknown pane; dropping (no-reply per wire frame design)",
                );
                return;
            }
            ResolvedOwned::Local(local) => local,
        };
        let terminal = local.id;
        // ADR-0124: a retained pane's grid records how its process ended.
        if s.retained_exit(terminal).is_some() {
            debug!(?client_id, ?wire_terminal_id, "RESIZE_TERMINAL: pane exited; ignored");
            return;
        }
        // The actor clamps to one cell; record the same so `GET_STATE` (which
        // `phux resize` reads back) never reports a size no grid has.
        let cols = cols.max(1);
        let rows = rows.max(1);
        if let Some(pane) = s.registry_mut().terminal_mut(terminal) {
            pane.dims = (cols, rows);
        }
        let terminal = match local.handle.terminal() {
            Ok(terminal) => terminal,
            Err(error) => {
                debug!(?client_id, ?terminal, %error, "RESIZE_TERMINAL: not a Terminal; dropping");
                // L1 §1.1: no reply frame, so an uncorrelated ERROR.
                send_wrong_kind_error(s, client_id, error);
                return;
            }
        };
        // No pixel size rides this frame, so the actor keeps its last one.
        try_resize(terminal, (cols, rows), None, "RESIZE_TERMINAL", client_id);
    });
}

/// Refuse a reply-less frame aimed at the wrong resource kind with an
/// uncorrelated `ERROR` to its sender.
fn send_wrong_kind_error(
    s: &crate::state::ServerState,
    client_id: ClientId,
    error: WrongResourceKind,
) {
    if let Some(mailbox) = s.client_mailbox(client_id) {
        let _ = mailbox.try_send(Outbound::Frame(FrameKind::Error {
            request_id: None,
            code: ErrorCode::WrongResourceKind,
            message: error.to_string(),
        }));
    }
}

/// Queue a live resize that resyncs every client's mirror. Resizes are
/// best-effort (SPEC §10.5): a full or closed mailbox drops this one.
fn try_resize(
    terminal: &TerminalHandle,
    (cols, rows): (u16, u16),
    cell_px: Option<(u16, u16)>,
    verb: &str,
    client_id: ClientId,
) {
    let request = ResizeRequest {
        cols,
        rows,
        cell_px,
        resync_clients: true,
        resync_only: false,
        resync_for: None,
    };
    match terminal.resize.try_send(request) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            warn!(
                ?client_id,
                cols, rows, "{verb}: pane resize mailbox full; dropping"
            );
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            debug!(?client_id, "{verb}: pane actor gone; dropping resize");
        }
    }
}

/// Perform the attach mutation in one critical section: attach, build the
/// snapshot, and collect the panes that can bootstrap plus the focused-session
/// panes without an actor, which must be closed before `ATTACH_READY` rather
/// than stranding the client's attach barrier.
pub(crate) fn prepare_attach(
    state: &SharedState,
    client_id: ClientId,
    session: phux_core::ids::SessionId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
) -> Result<AttachPrepared, crate::state::AttachError> {
    state.with_mut(|s| {
        // The caller pinned the session by id; its name is read here, in
        // the same critical section as the attach, never earlier.
        let session_name = s
            .registry()
            .session(session)
            .map(|found| found.name.clone())
            .ok_or_else(|| crate::state::AttachError::UnknownSession(format!("{session:?}")))?;
        let session_name = session_name.as_str();
        let pane_count = s
            .session_by_name(session_name)
            .ok_or_else(|| crate::state::AttachError::UnknownSession(session_name.to_owned()))?
            .windows
            .iter()
            .filter_map(|window_id| s.registry().window(*window_id))
            .try_fold(0_usize, |count, window| {
                count.checked_add(window.slots.len())
            })
            .ok_or(crate::state::AttachError::ResourceLimit)?;
        if pane_count > crate::runtime::attach::MAX_AGGREGATE_BOOTSTRAP_PANES {
            return Err(crate::state::AttachError::ResourceLimit);
        }
        let sid = s.attach(
            client_id,
            session_name,
            out_tx.clone(),
            client_caps,
            bootstrap_profile,
            bootstrap_limits,
        )?;
        s.touch_session(sid);
        let snapshot = s
            .build_session_snapshot(sid)
            .ok_or_else(|| crate::state::AttachError::UnknownSession(session_name.to_owned()))?;
        // workload-auth §6: every returned Terminal is filtered by OBSERVE.
        let snapshot = crate::policy::filter::filter_snapshot(
            s,
            client_id,
            phux_protocol::kinds::Verb::Observe,
            snapshot,
        );
        let panes_to_snapshot = s.attach_snapshot_panes(sid);
        let bootstrapped: HashSet<_> = panes_to_snapshot
            .iter()
            .map(|pane| pane.wire_terminal_id.clone())
            .collect();
        let focused_windows: HashSet<_> = snapshot
            .windows
            .iter()
            .filter(|window| window.session_id == snapshot.focused_session)
            .map(|window| window.id)
            .collect();
        let closed_before_ready = snapshot
            .resources
            .iter()
            .filter(|pane| {
                focused_windows.contains(&pane.window_id) && !bootstrapped.contains(&pane.id)
            })
            .map(|pane| pane.id.clone())
            .collect();
        let initial_client_id = super::wire_client(client_id);
        Ok((
            snapshot,
            initial_client_id,
            panes_to_snapshot,
            closed_before_ready,
        ))
    })
}

/// Stable, payload-free label for a [`Command`] variant: the `kind` field on
/// the `handle_command` span, kept free of session names, env, and input.
pub(crate) const fn command_kind(command: &Command) -> &'static str {
    match command {
        Command::AttachResource { .. } => "attach_terminal",
        Command::DetachResource { .. } => "detach_terminal",
        Command::KillResource { .. } => "kill_terminal",
        Command::KillResourceIf { .. } => "kill_resource_if",
        Command::OpenListener { .. } => "open_listener",
        Command::KillResources { .. } => "kill_terminals",
        Command::CloseTabResources { .. } => "close_tab_resources",
        Command::DetachClients { .. } => "detach_clients",
        Command::GetState { .. } => "get_state",
        Command::GetScreen { .. } => "get_screen",
        Command::RouteInput { .. } => "route_input",
        Command::ApplyInput { .. } => "apply_input",
        Command::AcquireInput { .. } => "acquire_input",
        Command::ReleaseInput { .. } => "release_input",
        Command::SignalTerminal { .. } => "signal_terminal",
        Command::PutFile { .. } => "put_file",
        Command::GetPerf { .. } => "get_perf",
        Command::Transcribe { .. } => "transcribe",
        Command::AppendResourceOutput { .. } => "append_resource_output",
        _ => "other",
    }
}

/// Queue `result` as the `COMMAND_RESULT` for `request_id`; a closed mailbox
/// means the connection is already gone.
async fn send_result(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    request_id: u32,
    result: CommandResult,
) {
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::CommandResult {
            request_id,
            result,
        }))
        .await;
}

/// Dispatch a `COMMAND` and reply with its correlated `COMMAND_RESULT`
/// (SPEC §5). The result may follow frames the command triggered.
#[tracing::instrument(
    level = "info",
    name = "handle_command",
    skip_all,
    fields(?client_id, request_id, kind = command_kind(&command)),
)]
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one flat dispatch arm per wire command keeps the catalog and negotiated connection context auditable"
)]
pub(crate) async fn handle_command(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    command: Command,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    input_lane: Option<&InputLaneHandle>,
    connection_token: &CancellationToken,
    root_token: &CancellationToken,
    // QUIC multi-stream (proto.md §4.2): subscribe now, bootstrap when the
    // client binds a stream (`STREAM_BIND`).
    defer_subscription: bool,
) {
    // UPGRADE and SHUTDOWN ack the client themselves and then re-exec or end
    // the process, so neither reaches the shared result send below.
    if matches!(command, Command::Upgrade) {
        handle_upgrade(state, request_id, out_tx).await;
        return;
    }
    if matches!(command, Command::Shutdown) {
        handle_shutdown(state, client_id, request_id, out_tx, root_token).await;
        return;
    }

    // PUT_FILE chunks may carry 8 MiB: rewrite a satellite id in place rather
    // than cloning the payload through the generic router below.
    let mut command = command;
    if let Command::PutFile { terminal_id, .. } = &mut command
        && let Some((sat_host, local_id)) = crate::hub::relay::satellite_route(terminal_id)
    {
        *terminal_id = phux_protocol::ids::ResourceId::local(local_id);
        handle_satellite_command(
            state,
            client_id,
            request_id,
            &sat_host,
            command,
            out_tx,
            bootstrap_profile,
            bootstrap_limits,
            connection_token,
        )
        .await;
        return;
    }

    // ADR-0127: `{ VIEWER, DELIBERATE }` is refused before anything is
    // subscribed or relayed.
    if let Some(result) = refuse_invalid_role_policy(&command) {
        send_result(out_tx, request_id, result).await;
        return;
    }

    // ADR-0109: a verb that drives or reads a resource another connection
    // spawned counts as attaching it; noted before the verb runs.
    note_local_use(state, client_id, &command);

    // ADR-0007 §4: a satellite-owned target never touches local dispatch.
    if let Some((sat_host, local_command)) = crate::hub::relay::route_to_satellite(&command) {
        handle_satellite_command(
            state,
            client_id,
            request_id,
            &sat_host,
            local_command,
            out_tx,
            bootstrap_profile,
            bootstrap_limits,
            connection_token,
        )
        .await;
        return;
    }

    // L1 §5.1.1: keyed commands are admitted after routing; a satellite owns
    // the dedupe of what it runs.
    let claim = match super::keyed_ops::admit(state, &command).await {
        super::keyed_ops::KeyedAdmission::Unkeyed => None,
        super::keyed_ops::KeyedAdmission::Owner(claim) => Some(claim),
        super::keyed_ops::KeyedAdmission::Answer(result) => {
            send_result(out_tx, request_id, result).await;
            return;
        }
    };

    let result = match command {
        Command::AttachResource {
            terminal_id,
            role_policy,
        } => {
            // Absent is `{ PRIMARY, NEVER }` (ADR-0127); an invalid policy
            // was refused before routing.
            let role = role_policy.unwrap_or_default();
            if defer_subscription {
                // Content starts at STREAM_BIND on the stream's mailbox.
                subscribe_deferred_attach(state, client_id, &terminal_id, out_tx, role).await
            } else {
                handle_attach_terminal(
                    state,
                    client_id,
                    &terminal_id,
                    out_tx,
                    client_caps,
                    bootstrap_profile,
                    bootstrap_limits,
                    connection_token,
                    role,
                )
                .await
            }
        }
        Command::DetachResource { terminal_id } => {
            handle_detach_terminal(state, client_id, &terminal_id).await
        }
        Command::GetState { scope } => {
            handle_get_state_federated(state, client_id, &scope, out_tx).await
        }
        Command::GetPerf { reset } => handle_get_perf(state, reset),
        Command::Transcribe {
            upload_id,
            terminal_id,
        } => {
            super::voice::handle_transcribe(state, client_id, upload_id, &terminal_id, input_lane)
                .await
        }
        Command::GetScreen {
            terminal_id,
            request_scrollback,
            cells,
            format,
        } => handle_get_screen(state, &terminal_id, request_scrollback, cells, format).await,
        Command::RouteInput { terminal_id, event } => match input_lane {
            Some(lane) => lane.begin_route(client_id, terminal_id, event).await,
            None => handle_route_input(state, client_id, &terminal_id, event),
        },
        Command::ApplyInput {
            operation_id,
            terminal_id,
            events,
        } => match input_lane {
            Some(lane) => {
                lane.begin_apply(client_id, operation_id, terminal_id, events)
                    .await
            }
            None => CommandResult::Error {
                code: ErrorCode::InternalError,
                message: "acknowledged input lane unavailable".to_owned(),
            },
        },
        Command::KillResources { ids, operation_id } => {
            handle_kill_terminals(state, client_id, &ids, operation_id).await
        }
        Command::CloseTabResources { ids } => handle_close_tab_resources(state, &ids),
        Command::DetachClients { session } => handle_detach_clients(state, session.as_deref()),
        Command::KillResource {
            terminal_id,
            operation_id,
        } => handle_kill_terminal(
            state,
            &terminal_id,
            kill_attribution(client_id, operation_id),
        ),
        Command::KillResourceIf {
            terminal_id,
            precondition,
            operation_id,
        } => handle_kill_resource_if(
            state,
            &terminal_id,
            &precondition,
            kill_attribution(client_id, operation_id),
        ),
        Command::OpenListener {
            transport,
            port_range,
            linger_secs,
        } => super::ephemeral_listener::handle_open_listener(
            state,
            client_id,
            super::ephemeral_listener::OpenRequest {
                transport,
                port_range,
                linger_secs,
            },
            input_lane,
            root_token,
        ),
        Command::GetTerminalState {
            terminal_id,
            include_scrollback,
            max_scrollback_lines,
        } => {
            handle_get_terminal_state(
                state,
                &terminal_id,
                include_scrollback,
                max_scrollback_lines,
            )
            .await
        }
        Command::SubscribeResourceEvents {
            terminal_id,
            event_types,
        } => handle_subscribe_terminal_events(state, client_id, &terminal_id, event_types, out_tx),
        Command::AcquireInput {
            terminal_id,
            mode,
            ttl_ms,
        } => handle_acquire_input(state, client_id, &terminal_id, mode, ttl_ms).await,
        Command::ReleaseInput { terminal_id } => {
            handle_release_input(state, client_id, &terminal_id).await
        }
        Command::SignalTerminal {
            terminal_id,
            signal,
            operation_id,
        } => {
            handle_signal_terminal(
                state,
                client_id,
                &terminal_id,
                signal,
                operation_id,
                claim.as_ref(),
            )
            .await
        }
        Command::PutFile {
            upload_id,
            terminal_id,
            extension,
            offset,
            data,
            final_chunk,
            sha256,
        } => {
            super::upload::handle_put_file(
                state,
                super::upload::PutFileChunk {
                    upload_id,
                    terminal_id,
                    extension,
                    offset,
                    data,
                    final_chunk,
                    sha256,
                    principal: state.with(|s| super::upload::upload_principal(s, client_id)),
                },
            )
            .await
        }
        Command::ReportAsked {
            terminal_id,
            id,
            question,
            suggestions,
            elapsed_seconds,
        } => handle_report_asked(
            state,
            &terminal_id,
            id,
            question,
            suggestions,
            elapsed_seconds,
        ),
        Command::ReportAgentState {
            terminal_id,
            state: reported,
        } => handle_report_agent_state(state, &terminal_id, reported).await,
        Command::AppendResourceOutput { terminal_id, bytes } => {
            crate::runtime::resource_commands::handle_append_resource_output(
                state,
                client_id,
                &terminal_id,
                bytes::Bytes::from(bytes),
            )
            .await
        }
        // A known-but-unwired tag of the `#[non_exhaustive]` catalog (SPEC §5).
        _ => CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: "command not supported by this server".to_owned(),
        },
    };
    super::keyed_ops::settle(claim, &result);
    debug!(
        ?client_id,
        request_id, "COMMAND dispatched; sending COMMAND_RESULT"
    );
    send_result(out_tx, request_id, result).await;
}

/// The attribution a kill stamps on the `pane_closed` of everything it
/// closes: the sending connection and, when keyed, its `operation_id`.
const fn kill_attribution(
    client_id: ClientId,
    operation_id: Option<phux_protocol::ids::IdempotencyKey>,
) -> crate::state::CloseAttribution {
    crate::state::CloseAttribution {
        actor: Some(client_id),
        operation_id,
    }
}

/// `KILL_RESOURCE`: close the pane through the same teardown a natural exit
/// takes, so `RESOURCE_CLOSED` still fires.
fn handle_kill_terminal(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    attribution: crate::state::CloseAttribution,
) -> CommandResult {
    state
        .with(|s| s.terminal_from_wire(terminal_id))
        .map_or_else(
            || terminal_not_found(terminal_id),
            |core_id| {
                // ADR-0104 §2: the target and its children close in one lock.
                state.with_mut(|s| {
                    s.close_resources_attributed(
                        &[core_id],
                        phux_protocol::wire::frame::CloseReason::Killed,
                        attribution,
                    )
                });
                CommandResult::Ok
            },
        )
}

/// `KILL_RESOURCE_IF` (ADR-0109, L1 §5.2.1): check and kill under one lock so
/// no attach lands between them. A refusal kills nothing.
fn handle_kill_resource_if(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    precondition: &phux_protocol::wire::frame::KillPrecondition,
    attribution: crate::state::CloseAttribution,
) -> CommandResult {
    match state.with_mut(|s| s.kill_resource_if_attributed(terminal_id, precondition, attribution))
    {
        Ok(()) => CommandResult::Ok,
        Err(refusal) => kill_if_refusal(terminal_id, refusal),
    }
}

/// The typed refusal a `KILL_RESOURCE_IF` answers when it killed nothing.
fn kill_if_refusal(
    terminal_id: &phux_protocol::ids::ResourceId,
    refusal: crate::state::KillIfRefusal,
) -> CommandResult {
    match refusal {
        crate::state::KillIfRefusal::NotFound => terminal_not_found(terminal_id),
        crate::state::KillIfRefusal::Precondition(why) => CommandResult::Error {
            code: ErrorCode::PreconditionFailed,
            message: format!("{terminal_id} not killed: {why}"),
        },
    }
}

/// Handle `ATTACH_RESOURCE` (SPEC §5.1): subscribe the caller to one
/// Terminal's content stream, snapshot first, without a session `ATTACH`.
/// Idempotent: a re-attach re-sends a snapshot without a second pump. It
/// never resizes; interactive callers follow with `RESIZE_TERMINAL`.
#[allow(
    clippy::too_many_arguments,
    reason = "negotiated connection context threaded from dispatch"
)]
async fn handle_attach_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    connection_token: &CancellationToken,
    role: RolePolicy,
) -> CommandResult {
    let subscription =
        match subscribe_attach_terminal(state, client_id, terminal_id, out_tx, Some(role)) {
            Ok(subscription) => subscription,
            Err(refusal) => return refusal,
        };
    announce_role_effects(&subscription, client_id).await;
    let AttachSubscription {
        core,
        handle,
        effects,
        was_viewer,
        ..
    } = subscription;

    let stream_id = crate::runtime::attach::stream_id_from(client_id.0);
    match bootstrap_attach_terminal(
        state,
        client_id,
        terminal_id,
        core,
        &handle,
        out_tx,
        stream_id,
        client_caps,
        bootstrap_profile,
        bootstrap_limits,
        connection_token,
    )
    .await
    {
        Ok(()) => CommandResult::Ok,
        Err(failure) => {
            // The COMMAND path drops its subscription on failure and undoes
            // any lease seizure or role widening it made (ADR-0127).
            let restore = was_viewer && !role.is_viewer();
            let (released, holder) = state.with_mut(|s| {
                s.unsubscribe_terminal(client_id, core);
                if restore {
                    s.set_viewer_mark(client_id, terminal_id, true);
                }
                let released = s.undo_attach_takeover(client_id, core, effects);
                (released, s.input_lease_holder(core))
            });
            if restore && effects.role_changed {
                let _ = handle
                    .control
                    .send(ControlRequest::LeaseChanged {
                        input_holder: holder.map(super::wire_client),
                        action: ControlAction::RoleChanged,
                        actor: Some(super::wire_client(client_id)),
                    })
                    .await;
            }
            if released {
                let _ = handle
                    .control
                    .send(ControlRequest::LeaseChanged {
                        input_holder: None,
                        action: ControlAction::Released,
                        actor: Some(super::wire_client(client_id)),
                    })
                    .await;
            }
            CommandResult::Error {
                code: failure.code,
                message: failure.message,
            }
        }
    }
}

/// Bootstrap one Terminal's content stream into `content_tx` without
/// touching the caller's subscription. The QUIC `STREAM_BIND` path
/// (proto.md §4.2) subscribes on the control mailbox and bootstraps on the
/// stream's; the COMMAND path uses one mailbox for both.
///
/// Failures cancel the generation but never unsubscribe; the returned
/// [`AttachResourceFailure`] shapes the caller's refusal.
#[allow(
    clippy::too_many_arguments,
    reason = "negotiated connection context plus the split mailboxes"
)]
pub(crate) async fn bootstrap_attach_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    core: phux_core::ids::ResourceId,
    handle: &ResourceHandle,
    content_tx: &tokio::sync::mpsc::Sender<Outbound>,
    stream_id: phux_protocol::ids::StreamId,
    client_caps: ClientCapabilities,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    connection_token: &CancellationToken,
) -> Result<(), AttachResourceFailure> {
    let Some(stream_profile) = crate::runtime::attach::bootstrap_stream_profile(bootstrap_profile)
    else {
        return Err(AttachResourceFailure {
            code: ErrorCode::CodecUnavailable,
            message: "ATTACH_RESOURCE selected an unsupported bootstrap profile".to_owned(),
        });
    };
    // A non-Terminal stream has its own bootstrap shape (ADR-0103 §4).
    if handle.kind == crate::resource::ResourceKind::AgentSession {
        return match crate::runtime::resource_commands::attach_agent_session(
            state,
            client_id,
            terminal_id,
            core,
            handle,
            content_tx,
            bootstrap_limits,
            connection_token,
        )
        .await
        {
            CommandResult::Ok | CommandResult::OkWith(_) => Ok(()),
            CommandResult::Error { code, message } => Err(AttachResourceFailure { code, message }),
            other => Err(AttachResourceFailure::internal(&format!(
                "agent session attach returned an unexpected result: {other:?}"
            ))),
        };
    }

    let Some(bootstrap_id) = state.with_mut(|s| s.next_attach_terminal_bootstrap_id(client_id))
    else {
        return Err(AttachResourceFailure {
            code: ErrorCode::ResourceExhausted,
            message: "ATTACH_RESOURCE bootstrap id space exhausted".to_owned(),
        });
    };

    let terminal = match handle.terminal() {
        Ok(terminal) => terminal.clone(),
        Err(error) => {
            state.with_mut(|s| s.cancel_attach_terminal_pump(client_id, core));
            return Err(AttachResourceFailure {
                code: ErrorCode::WrongResourceKind,
                message: error.to_string(),
            });
        }
    };
    let session = AttachResourceSession {
        state,
        out_tx: content_tx,
        connection_token,
        terminal_id,
        core,
        handle: handle.clone(),
        terminal,
        client_id,
        stream_id,
        client_caps,
        stream_profile,
        bootstrap_limits,
    };

    let mut generation = match session.establish_generation(bootstrap_id).await {
        Ok(generation) => generation,
        Err(failure) => return Err(session.failed(failure)),
    };

    let finished = if let Some(state_sync) = generation.state_sync_bootstrap.take() {
        session.finish_state_sync(&generation, state_sync).await
    } else {
        session.finish_bootstrap(&mut generation).await
    };
    finished.map_err(|failure| session.failed(failure))
}

/// Resolve the wire terminal id and register the caller as an output
/// subscriber in one critical section.
///
/// The mailbox is remembered with the subscription because a session-less
/// subscriber has no other route for lifecycle fanout such as
/// `RESOURCE_CLOSED` (L1 §3.1). `role` (ADR-0127) applies in the same
/// critical section; `None` leaves the role untouched (a `STREAM_BIND`).
pub(crate) fn subscribe_attach_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    role: Option<RolePolicy>,
) -> Result<AttachSubscription, CommandResult> {
    let not_found = || terminal_not_found(terminal_id);
    state.with_mut(|s| {
        let core = s.terminal_from_wire(terminal_id).ok_or_else(not_found)?;
        let handle = s.resource_handle(core).cloned().ok_or_else(not_found)?;
        // The lease is a Terminal-facet concept (L1 §1.1), so a takeover of
        // any other kind is refused the way `ACQUIRE_INPUT` refuses it.
        if role.is_some_and(RolePolicy::takes_over)
            && let Err(error) = handle.terminal()
        {
            return Err(wrong_resource_kind(error));
        }
        let was_subscribed = s.subscribers_for_terminal(core).contains(&client_id);
        let was_viewer = s.is_viewer(client_id, terminal_id);
        s.subscribe_terminal(client_id, core, Some(out_tx.clone()));
        let effects = role.map_or_else(RoleEffects::default, |policy| {
            s.apply_attach_role(client_id, terminal_id, Some(core), policy, was_subscribed)
        });
        Ok(AttachSubscription {
            core,
            handle,
            effects,
            was_viewer,
            holder: s.input_lease_holder(core),
        })
    })
}

/// What [`subscribe_attach_terminal`] registered, and what its declared role
/// changed (ADR-0127).
pub(crate) struct AttachSubscription {
    /// The subscribed Terminal.
    pub(crate) core: phux_core::ids::ResourceId,
    /// Its engine handle.
    pub(crate) handle: ResourceHandle,
    /// What the role changed; empty when no role was applied.
    pub(crate) effects: RoleEffects,
    /// Whether the connection had declared `VIEWER` on this Terminal
    /// before this attach, so a failed widening can put the tombstone back.
    pub(crate) was_viewer: bool,
    /// The lease holder once the role applied, which every broadcast names.
    pub(crate) holder: Option<ClientId>,
}

/// Broadcast what an attach's declared role changed (ADR-0127) through the
/// pane's engine: `ROLE_CHANGED` first, then the lease transition.
pub(crate) async fn announce_role_effects(subscription: &AttachSubscription, client_id: ClientId) {
    announce_role_effects_on(
        &subscription.handle,
        client_id,
        subscription.effects,
        subscription.holder,
    )
    .await;
}

/// [`announce_role_effects`] for a caller holding the parts rather than an
/// [`AttachSubscription`] (the session `ATTACH` sweep).
pub(crate) async fn announce_role_effects_on(
    handle: &ResourceHandle,
    client_id: ClientId,
    effects: RoleEffects,
    holder: Option<ClientId>,
) {
    let changed = effects.role_changed.then_some(ControlAction::RoleChanged);
    for action in [changed, effects.lease].into_iter().flatten() {
        let _ = handle
            .control
            .send(ControlRequest::LeaseChanged {
                input_holder: holder.map(super::wire_client),
                action,
                actor: Some(super::wire_client(client_id)),
            })
            .await;
    }
}

/// The QUIC multi-stream `ATTACH_RESOURCE`: subscribe and apply the role
/// against the control mailbox, announce what the role changed, and stop.
async fn subscribe_deferred_attach(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    role: RolePolicy,
) -> CommandResult {
    match subscribe_attach_terminal(state, client_id, terminal_id, out_tx, Some(role)) {
        Ok(subscription) => {
            announce_role_effects(&subscription, client_id).await;
            CommandResult::Ok
        }
        Err(refusal) => refusal,
    }
}

/// `{ VIEWER, DELIBERATE }` on `ATTACH_RESOURCE` is `INVALID_COMMAND`
/// (L1 §8.1): a viewer cannot take the wheel.
fn refuse_invalid_role_policy(command: &Command) -> Option<CommandResult> {
    let Command::AttachResource {
        role_policy: Some(policy),
        ..
    } = command
    else {
        return None;
    };
    (!policy.is_valid()).then(|| CommandResult::Error {
        code: ErrorCode::InvalidCommand,
        message: "role_policy { VIEWER, DELIBERATE } is invalid: a viewer cannot take over"
            .to_owned(),
    })
}

/// Whether a negotiated bootstrap profile streams the official progressive
/// libghostty snapshot rather than a synthesized VT bootstrap.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
const fn native_checkpoint_profile(profile: phux_protocol::caps::BootstrapStreamProfile) -> bool {
    matches!(
        profile,
        phux_protocol::caps::BootstrapStreamProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1
        }
    )
}

/// Why an `ATTACH_RESOURCE` stage failed; [`AttachResourceSession::failed`]
/// rolls the partial generation back exactly once.
#[derive(Debug)]
pub(crate) struct AttachResourceFailure {
    /// Wire error code the caller receives.
    pub(crate) code: ErrorCode,
    /// Human-readable explanation attached to that code.
    pub(crate) message: String,
}

impl AttachResourceFailure {
    /// An internal fault with a fixed explanation — the dominant shape.
    fn internal(message: &str) -> Self {
        Self {
            code: ErrorCode::InternalError,
            message: message.to_owned(),
        }
    }
}

/// The pump generation an attach established, plus the handshake artifacts
/// its bootstrap publication still has to consume.
struct AttachResourceGeneration {
    /// Replica generation stamped on every frame this attach publishes.
    bootstrap_id: phux_protocol::ids::BootstrapId,
    /// Shared cursor the pump keeps current so a replacement generation can
    /// resume from where this one stopped.
    generation_last_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Releases the actor's live state-sync emission once the bootstrap is on
    /// the wire.
    live_gate_tx: tokio::sync::watch::Sender<bool>,
    /// Releases the raw pump's first delta at the published cut. `None` when
    /// the actor's tick owns emission and no raw pump was spawned.
    snapshot_gate: Option<oneshot::Sender<u64>>,
    /// Atomic synthesized bootstrap captured with state-sync registration.
    state_sync_bootstrap: Option<crate::terminal_actor::StateSyncBootstrap>,
    /// Hands the pump its post-replay live receiver at the native publication
    /// fence.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_publication_gate: Option<oneshot::Sender<crate::terminal_actor::NativePublicationReply>>,
}

/// One in-flight `ATTACH_RESOURCE`: the resolved terminal plus the negotiated
/// connection context every stage needs. Every failure funnels through
/// [`Self::failed`].
struct AttachResourceSession<'a> {
    state: &'a SharedState,
    out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    connection_token: &'a CancellationToken,
    terminal_id: &'a phux_protocol::ids::ResourceId,
    core: phux_core::ids::ResourceId,
    /// The resource's generic channels (output, consumers, control).
    handle: ResourceHandle,
    /// The Terminal facet of `handle`, resolved once at entry.
    terminal: TerminalHandle,
    client_id: ClientId,
    stream_id: phux_protocol::ids::StreamId,
    client_caps: ClientCapabilities,
    stream_profile: phux_protocol::caps::BootstrapStreamProfile,
    bootstrap_limits: BootstrapLimits,
}

impl AttachResourceSession<'_> {
    /// Whether this connection negotiated the actor-emitted state-sync stream
    /// at HELLO.
    const fn wants_state_sync(&self) -> bool {
        matches!(
            self.client_caps.output_mode,
            phux_protocol::caps::OutputMode::StateSync
        )
    }

    /// Undo a partial generation: cancel it, release any native lease, and
    /// detach the per-consumer state entry. The subscription is the caller's.
    fn failed(&self, failure: AttachResourceFailure) -> AttachResourceFailure {
        use crate::terminal_actor::ConsumerDetachRequest;

        self.state.with_mut(|s| {
            s.cancel_attach_terminal_pump(self.client_id, self.core);
        });
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let _ =
            self.terminal
                .native_release
                .try_send(crate::terminal_actor::NativeReleaseRequest {
                    owner: self.client_id.0,
                });
        let (reply, _ack) = oneshot::channel();
        let _ = self.handle.consumer_detach.try_send(ConsumerDetachRequest {
            client_id: super::wire_client(self.client_id),
            reply,
        });
        failure
    }

    /// Register the per-consumer state-sync entry (ADR-0018) so `FRAME_ACK`
    /// drives the actor's eviction loop. `None` when the terminal is not local
    /// or the actor refused.
    async fn register_consumer(
        &self,
        bootstrap_id: phux_protocol::ids::BootstrapId,
        live_gate: tokio::sync::watch::Receiver<bool>,
    ) -> Option<crate::terminal_actor::ConsumerAttachOutcome> {
        use crate::terminal_actor::ConsumerAttachRequest;

        let wire_terminal_id = self.terminal_id.local_id()?;
        let (reply, reply_rx) = oneshot::channel();
        self.handle
            .consumer_attach
            .send(ConsumerAttachRequest {
                client_id: super::wire_client(self.client_id),
                outbound: self.out_tx.clone(),
                wire_terminal_id,
                stream_id: self.stream_id,
                bootstrap_id,
                wants_state_sync: self.wants_state_sync(),
                live_gate,
                state_sync_scrollback: None,
                bootstrap_max_bytes: usize::MAX,
                bootstrap_max_frames: usize::MAX,
                bootstrap_chunk_bytes: 1,
                // Reliable transport: emit-once is correct (ADR-0042).
                loss_tolerant: false,
                reply,
            })
            .await
            .ok()?;
        let Ok(Ok(outcome)) = reply_rx.await else {
            return None;
        };
        Some(outcome)
    }

    /// Allocate the pump generation for this attach: register the consumer,
    /// tombstone whatever generation it replaces, and start the raw output
    /// pump unless the actor's tick owns emission.
    async fn establish_generation(
        &self,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> Result<AttachResourceGeneration, AttachResourceFailure> {
        // Subscribe before stopping the old pump so replacement never loses a
        // byte emitted in the handoff window. The new receiver remains gated
        // until this generation reaches READY.
        let output_rx = self.handle.output.subscribe();
        pump::stop_output(self.state, self.client_id, self.core).await;
        let (token, pump_done, generation_last_seq, prior) = self
            .state
            .with_mut(|s| s.replace_attach_terminal_pump(self.client_id, self.core, bootstrap_id));
        let mut pump_done_guard = Some(pump_done.drop_guard());
        if let Some((_, prior_done, _)) = &prior {
            prior_done.cancelled().await;
        }
        let (live_gate_tx, live_gate_rx) = tokio::sync::watch::channel(false);

        let outcome = self.register_consumer(bootstrap_id, live_gate_rx).await;
        if self.wants_state_sync() && outcome.is_none() {
            return Err(AttachResourceFailure::internal(
                "ATTACH_RESOURCE state-sync registration failed",
            ));
        }
        let tick_managed = outcome.as_ref().is_some_and(|outcome| outcome.tick_managed);
        if tick_managed {
            // No raw pump will own this generation.
            drop(pump_done_guard.take());
        }
        if let Some((prior_bootstrap_id, _, prior_last_seq)) = prior
            && self
                .out_tx
                .send(Outbound::Frame(FrameKind::BootstrapTombstone {
                    terminal_id: self.terminal_id.clone(),
                    stream_id: self.stream_id,
                    bootstrap_id: prior_bootstrap_id,
                    reason: phux_protocol::wire::frame::TombstoneReason::ExplicitReattach,
                    last_valid_seq: prior_last_seq.load(std::sync::atomic::Ordering::Acquire),
                }))
                .await
                .is_err()
        {
            return Err(AttachResourceFailure::internal(
                "consumer went away during ATTACH_RESOURCE replacement",
            ));
        }

        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let (native_publication_gate_tx, native_publication_gate_rx) =
            oneshot::channel::<crate::terminal_actor::NativePublicationReply>();
        let snapshot_gate = if tick_managed {
            None
        } else {
            let Some(pump_done_guard) = pump_done_guard.take() else {
                return Err(AttachResourceFailure::internal(
                    "ATTACH_RESOURCE pump generation lost its completion guard",
                ));
            };
            let (gate_tx, gate_rx) = oneshot::channel::<u64>();
            let channels = AttachResourcePumpChannels {
                token,
                output_rx,
                snapshot_gate: gate_rx,
                #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                native_publication_gate: native_publication_gate_rx,
            };
            let last_seq = std::sync::Arc::clone(&generation_last_seq);
            self.spawn_output_pump(channels, bootstrap_id, last_seq, pump_done_guard);
            Some(gate_tx)
        };

        Ok(AttachResourceGeneration {
            bootstrap_id,
            generation_last_seq,
            live_gate_tx,
            snapshot_gate,
            state_sync_bootstrap: outcome.and_then(|outcome| outcome.state_sync_bootstrap),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_publication_gate: Some(native_publication_gate_tx),
        })
    }

    /// Start the raw broadcast pump for one generation. An unrecoverable
    /// fault releases only this client's consumer state: the pane is shared.
    fn spawn_output_pump(
        &self,
        channels: AttachResourcePumpChannels,
        bootstrap_id: phux_protocol::ids::BootstrapId,
        generation_last_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
        pump_done_guard: tokio_util::sync::DropGuard,
    ) {
        let ctx = crate::runtime::attach::OutputPumpContext {
            out_tx: self.out_tx.clone(),
            resize: self.terminal.resize.clone(),
            wire_terminal_id: self.terminal_id.clone(),
            stream_id: self.stream_id,
            initial_bootstrap_id: bootstrap_id,
            client_id: self.client_id,
            client_caps: self.client_caps,
            profile: self.stream_profile,
            limits: self.bootstrap_limits,
            lag_label: "ATTACH_RESOURCE output pump",
            stale_skip: false,
            cancel: Some(channels.token.clone()),
            last_seq: Some(generation_last_seq),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            terminal: self.terminal.clone(),
        };
        let state = self.state.clone();
        let connection_token = self.connection_token.clone();
        let client_id = self.client_id;
        pump::spawn_tracked(self.state, self.client_id, self.core, None, async move {
            let _done_guard = pump_done_guard;
            let Some((start, output_rx)) = channels.published(ctx.profile).await else {
                return;
            };
            if let Some(fault) =
                crate::runtime::attach::run_started_output_pump(&ctx, start, output_rx).await
            {
                crate::runtime::attach::release_after_pump_fault(
                    fault,
                    &state,
                    client_id,
                    &connection_token,
                );
            }
        });
    }

    /// Publish the atomic state-sync bootstrap the actor captured with
    /// registration, then open the live gate.
    async fn finish_state_sync(
        &self,
        generation: &AttachResourceGeneration,
        state_sync: crate::terminal_actor::StateSyncBootstrap,
    ) -> Result<(), AttachResourceFailure> {
        let snap = state_sync.snapshot;
        let replay = crate::runtime::attach::downsample_for_caps(
            &bytes::Bytes::from(snap.bytes),
            self.client_caps,
        );
        let mut payloads = Vec::with_capacity(2);
        if !snap.scrollback.is_empty() {
            payloads.push(bytes::Bytes::from(snap.scrollback));
        }
        payloads.push(replay);
        crate::runtime::attach::send_synthesized_bootstrap(
            self.out_tx,
            self.terminal_id.clone(),
            self.stream_id,
            generation.bootstrap_id,
            self.stream_profile,
            self.bootstrap_limits,
            snap.cols,
            snap.rows,
            state_sync.base_seq,
            payloads,
        )
        .await
        .map_err(|()| {
            AttachResourceFailure::internal("consumer went away during state-sync ATTACH_RESOURCE")
        })?;
        generation
            .generation_last_seq
            .store(state_sync.base_seq, std::sync::atomic::Ordering::Release);
        let _ = generation.live_gate_tx.send(true);
        Ok(())
    }

    /// Publish the negotiated bootstrap: a native checkpoint or a snapshot.
    async fn finish_bootstrap(
        &self,
        generation: &mut AttachResourceGeneration,
    ) -> Result<(), AttachResourceFailure> {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if native_checkpoint_profile(self.stream_profile) {
            return self.finish_native(generation).await;
        }
        self.finish_snapshot(generation).await
    }

    /// Capture a native checkpoint bootstrap from the pane actor.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn capture_native_bootstrap(
        &self,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> Result<crate::terminal_actor::NativeBootstrapReply, AttachResourceFailure> {
        let (reply, reply_rx) = oneshot::channel();
        self.terminal
            .native_bootstrap
            .send(crate::terminal_actor::NativeBootstrapRequest {
                owner: self.client_id.0,
                terminal_id: self.terminal_id.clone(),
                stream_id: self.stream_id,
                bootstrap_id,
                limits: self.bootstrap_limits,
                max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
                reply,
            })
            .await
            .map_err(|_| {
                AttachResourceFailure::internal("pane actor unavailable for native ATTACH_RESOURCE")
            })?;
        match reply_rx.await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(error)) => Err(AttachResourceFailure {
                code: ErrorCode::CodecUnavailable,
                message: format!("native ATTACH_RESOURCE failed: {error}"),
            }),
            Err(_) => Err(AttachResourceFailure::internal(
                "pane actor dropped native ATTACH_RESOURCE",
            )),
        }
    }

    /// Publish the native checkpoint bootstrap, cross the publication fence,
    /// and release both the live gate and the pump's first delta.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn finish_native(
        &self,
        generation: &mut AttachResourceGeneration,
    ) -> Result<(), AttachResourceFailure> {
        let reply = self
            .capture_native_bootstrap(generation.bootstrap_id)
            .await?;
        let (cut, cursor) = crate::runtime::attach::publish_native_bootstrap(self.out_tx, reply)
            .await
            .map_err(|()| {
                AttachResourceFailure::internal("consumer went away during native ATTACH_RESOURCE")
            })?;
        let publication = crate::runtime::attach::activate_native_publication(
            &self.terminal,
            self.client_id.0,
            self.terminal_id.clone(),
            self.stream_id,
            generation.bootstrap_id,
            cursor,
        )
        .await
        .map_err(|_| {
            AttachResourceFailure::internal("pane actor unavailable at native publication fence")
        })?;
        let Some(publication_gate) = generation.native_publication_gate.take() else {
            return Err(AttachResourceFailure::internal(
                "native publication gate already consumed",
            ));
        };
        publication_gate.send(publication).map_err(|_| {
            AttachResourceFailure::internal(
                "native ATTACH_RESOURCE pump went away before publication",
            )
        })?;
        generation
            .generation_last_seq
            .store(cut, std::sync::atomic::Ordering::Release);
        let _ = generation.live_gate_tx.send(true);
        if let Some(gate) = generation.snapshot_gate.take() {
            let _ = gate.send(cut);
        }
        debug!(
            client_id = ?self.client_id,
            terminal_id = ?self.terminal_id,
            "native ATTACH_RESOURCE subscribed"
        );
        Ok(())
    }

    /// Publish the authoritative snapshot, sent before the pump's first delta
    /// (the gate below releases it) and before the Ok reply.
    async fn finish_snapshot(
        &self,
        generation: &mut AttachResourceGeneration,
    ) -> Result<(), AttachResourceFailure> {
        use crate::terminal_actor::SnapshotRequest;

        let (snapshot_tx, snapshot_rx) = oneshot::channel();
        self.terminal
            .snapshot
            .send(SnapshotRequest {
                scrollback: None,
                max_bytes: usize::MAX,
                max_frames: usize::MAX,
                chunk_bytes: 1,
                reply: snapshot_tx,
            })
            .await
            .map_err(|_| {
                AttachResourceFailure::internal("pane actor unavailable for ATTACH_RESOURCE")
            })?;
        let Ok(Ok((snap, cut))) = snapshot_rx.await else {
            return Err(AttachResourceFailure::internal(
                "pane actor dropped the ATTACH_RESOURCE snapshot",
            ));
        };
        let replay = crate::runtime::attach::downsample_for_caps(
            &bytes::Bytes::from(snap.bytes),
            self.client_caps,
        );
        crate::runtime::attach::send_synthesized_bootstrap(
            self.out_tx,
            self.terminal_id.clone(),
            self.stream_id,
            generation.bootstrap_id,
            self.stream_profile,
            self.bootstrap_limits,
            snap.cols,
            snap.rows,
            cut,
            [replay],
        )
        .await
        .map_err(|()| {
            AttachResourceFailure::internal("consumer went away during ATTACH_RESOURCE")
        })?;
        let _ = generation.live_gate_tx.send(true);
        generation
            .generation_last_seq
            .store(cut, std::sync::atomic::Ordering::Release);
        if let Some(gate) = generation.snapshot_gate.take() {
            let _ = gate.send(cut);
        }
        debug!(
            client_id = ?self.client_id,
            terminal_id = ?self.terminal_id,
            "ATTACH_RESOURCE subscribed"
        );
        Ok(())
    }
}

/// The one-shot gates one `ATTACH_RESOURCE` pump generation waits on before
/// it reaches steady state.
struct AttachResourcePumpChannels {
    /// Cancels this generation when a replacement attach supersedes it.
    token: CancellationToken,
    /// The pane's broadcast output, subscribed before the prior pump stopped.
    output_rx: tokio::sync::broadcast::Receiver<crate::terminal_actor::PaneOutput>,
    /// Delivers the published cut, releasing the first forwarded delta.
    snapshot_gate: oneshot::Receiver<u64>,
    /// Delivers the post-replay live receiver at the native publication fence.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_publication_gate: oneshot::Receiver<crate::terminal_actor::NativePublicationReply>,
}

impl AttachResourcePumpChannels {
    /// Wait for this generation's publication: the published cut, then (for
    /// a native profile) the post-replay live receiver. `None` when the
    /// generation was cancelled or abandoned first.
    async fn published(
        self,
        #[cfg_attr(
            not(all(feature = "native-engine", not(target_arch = "wasm32"))),
            allow(unused_variables)
        )]
        profile: phux_protocol::caps::BootstrapStreamProfile,
    ) -> Option<(
        crate::runtime::attach::OutputPumpStart,
        tokio::sync::broadcast::Receiver<crate::terminal_actor::PaneOutput>,
    )> {
        let published_cut = tokio::select! {
            () = self.token.cancelled() => return None,
            result = self.snapshot_gate => result.ok()?,
        };
        let mut start = crate::runtime::attach::OutputPumpStart {
            published_cut,
            replay: Vec::new(),
            live: None,
        };
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if native_checkpoint_profile(profile) {
            let publication = self.native_publication_gate.await.ok()?;
            start.live = Some(publication.live);
            start.replay = publication.replay;
        }
        Some((start, self.output_rx))
    }
}

/// Handle `DETACH_RESOURCE` (SPEC §5.1): drop every output task and the
/// event subscription the caller holds on the terminal. Idempotent, so a
/// detach never races a natural close into an error.
pub(crate) async fn handle_detach_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
) -> CommandResult {
    use crate::terminal_actor::ConsumerDetachRequest;

    let handle = state.with_mut(|s| {
        s.unsubscribe_terminal_events(client_id, terminal_id);
        // A declared `VIEWER` outlives the subscription (ADR-0127).
        let core = s.terminal_from_wire(terminal_id)?;
        s.unsubscribe_terminal(client_id, core);
        Some((core, s.resource_handle(core).cloned()))
    });
    let Some((core, handle)) = handle else {
        return CommandResult::Ok;
    };
    pump::stop_output(state, client_id, core).await;
    state.with_mut(|s| s.cancel_attach_terminal_pump(client_id, core));
    if let Some(handle) = handle {
        // Fence actor-emitted StateSync output before COMMAND_RESULT is queued.
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = handle
            .consumer_detach
            .send(ConsumerDetachRequest {
                client_id: super::wire_client(client_id),
                reply: reply_tx,
            })
            .await;
        let _ = reply_rx.await;
    }
    debug!(?client_id, ?terminal_id, "DETACH_RESOURCE unsubscribed");
    CommandResult::Ok
}

/// Handle `SHUTDOWN`: cancel the root token, the same shutdown idle-exit and
/// signals take, after acking. Accepted on the Unix socket only (L1 §5.1):
/// whether a remote peer may stop the host is an unanswered policy question.
async fn handle_shutdown(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
) {
    let transport = state.with(|s| s.peer_identity(client_id).map(|peer| peer.transport));
    if !matches!(
        transport,
        Some(phux_protocol::policy::TransportType::UnixSocket)
    ) {
        warn!(
            ?client_id,
            ?transport,
            "SHUTDOWN refused: local socket only"
        );
        let refusal = CommandResult::Error {
            code: ErrorCode::PermissionDenied,
            message: "SHUTDOWN is accepted on the local socket only".to_owned(),
        };
        send_result(out_tx, request_id, refusal).await;
        return;
    }

    info!(?client_id, "SHUTDOWN requested; stopping the server");
    // ADR-0104 §4: record the close reason before the exit watchers run.
    state.with_mut(|s| {
        for resource in s.resource_ids() {
            s.mark_resource_closing(
                resource,
                phux_protocol::wire::frame::CloseReason::ServerShutdown,
            );
        }
    });
    send_result(out_tx, request_id, CommandResult::Ok).await;
    // Let the ack reach the writer before the teardown races it.
    tokio::task::yield_now().await;
    root_token.cancel();
}

/// Handle `UPGRADE` (ADR-0032): prepare the graceful re-exec, ack the client,
/// then replace the process. Acks itself (rather than returning a
/// `CommandResult`) because on success it never returns.
async fn handle_upgrade(
    state: &SharedState,
    request_id: u32,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    let result = match super::upgrade::prepare_upgrade(state).await {
        Ok(plan) => {
            // Best-effort ack flushed before exec; the client reconnects.
            send_result(out_tx, request_id, CommandResult::Ok).await;
            tokio::task::yield_now().await;
            info!("UPGRADE: re-exec'ing the new binary");
            let err = plan.exec();
            // Only reached if exec failed: the old image keeps serving.
            error!(error = %err, "UPGRADE exec failed; continuing on the current image");
            return;
        }
        Err(err) => {
            warn!(error = %err, "UPGRADE preparation failed");
            CommandResult::Error {
                code: ErrorCode::InternalError,
                message: format!("upgrade failed: {err}"),
            }
        }
    };
    send_result(out_tx, request_id, result).await;
}

/// Relay one satellite-targeted command (already rewritten to the
/// satellite's `Local` ids) over the owning hub link and send the correlated
/// `COMMAND_RESULT` (ADR-0007 §4). A non-hub server answers
/// `UnsupportedSatelliteRoute`; an unreachable satellite fails fast.
///
/// Stream-establishing commands register the caller as a hub-side proxy
/// subscriber atomically with the relay; `DETACH_RESOURCE` is resolved
/// hub-side so one consumer's detach never tears down the shared link
/// stream. Every hub consumer is one identity on the satellite, so the hub
/// owns input-lease exclusion between its consumers (L1 §9.1).
#[allow(
    clippy::too_many_arguments,
    reason = "caller identity, routed command, mailbox, and negotiated bootstrap context"
)]
async fn handle_satellite_command(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    host: &phux_protocol::ids::SatelliteHost,
    command: Command,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    connection_token: &CancellationToken,
) {
    note_satellite_use(state, host, client_id, &command);
    let result = match state.with(|s| s.hub_relay(host)) {
        None => CommandResult::Error {
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: format!(
                "no satellite route to {host:?}: this server is not a federation hub \
                 for that host (check `phux server --hub` and the [[satellites]] registry)"
            ),
        },
        Some(relay) => match &command {
            Command::AttachResource {
                terminal_id,
                role_policy,
            } => {
                relay_satellite_attach(
                    &SatelliteLeaseTarget::new(state, host, client_id, terminal_id),
                    &relay,
                    &command,
                    role_policy.unwrap_or_default(),
                    out_tx,
                    bootstrap_profile,
                    bootstrap_limits,
                )
                .await
            }
            Command::SubscribeResourceEvents { terminal_id, .. } => {
                let change =
                    register_satellite_event_filter(state, client_id, host, &command, out_tx);
                let result = relay_stream_establishing(
                    &relay,
                    &unfiltered_for_link(&command),
                    terminal_id,
                    client_id,
                    out_tx,
                    bootstrap_profile,
                    bootstrap_limits,
                    state.with(|server| server.client_connection_cancellation(client_id)),
                )
                .await;
                settle_satellite_event_filter(state, client_id, change, &result);
                result
            }
            Command::DetachResource { terminal_id } => {
                resolve_hub_detach_terminal(state, &relay, client_id, host, terminal_id).await
            }
            Command::AcquireInput {
                terminal_id, mode, ..
            } => {
                relay_satellite_acquire_input(
                    &SatelliteLeaseTarget::new(state, host, client_id, terminal_id),
                    &relay,
                    *mode,
                    &command,
                    out_tx,
                )
                .await
            }
            Command::ReleaseInput { terminal_id } => {
                relay_satellite_release_input(
                    &SatelliteLeaseTarget::new(state, host, client_id, terminal_id),
                    &relay,
                    &command,
                )
                .await
            }
            // L1 §9.1: `APPLY_INPUT` crosses the same lease gate as
            // `ROUTE_INPUT`, then the satellite owns its dedupe.
            Command::RouteInput { terminal_id, .. } | Command::ApplyInput { terminal_id, .. } => {
                relay_satellite_route_input(
                    &SatelliteLeaseTarget::new(state, host, client_id, terminal_id),
                    &relay,
                    &command,
                )
                .await
            }
            Command::KillResourceIf { .. } => {
                relay_conditional_kill(state, &relay, host, &command, client_id).await
            }
            _ => relay.command_from(command.clone(), client_id).await,
        },
    };
    reply_satellite_command(
        state,
        client_id,
        request_id,
        host,
        &command,
        out_tx,
        result,
        connection_token,
    )
    .await;
}

/// ADR-0109 (L1 §5.2.1): the resource `command` attaches, drives, or reads,
/// which counts as attaching it for `UNATTACHED_SINCE_SPAWN` whether or not
/// the command then succeeds.
const fn used_resource(command: &Command) -> Option<&phux_protocol::ids::ResourceId> {
    match command {
        Command::AttachResource { terminal_id, .. }
        | Command::RouteInput { terminal_id, .. }
        | Command::ApplyInput { terminal_id, .. }
        | Command::AcquireInput { terminal_id, .. }
        | Command::PutFile { terminal_id, .. }
        | Command::Transcribe { terminal_id, .. }
        | Command::SignalTerminal { terminal_id, .. }
        | Command::GetScreen { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}

/// Note a local resource `command` uses (see [`used_resource`]) before the
/// command runs, so a conditional kill checked after it sees it. A
/// satellite-tagged id is the hub's to note, in [`note_satellite_use`].
pub(super) fn note_local_use(state: &SharedState, client_id: ClientId, command: &Command) {
    if let Some(terminal_id) = used_resource(command) {
        state.with_mut(|s| s.note_resource_use(terminal_id, client_id));
    }
}

/// ADR-0109: note a hub consumer's use of a satellite resource in the hub's
/// spawn ledger before relaying; only the hub can tell its consumers apart.
fn note_satellite_use(
    state: &SharedState,
    host: &phux_protocol::ids::SatelliteHost,
    client_id: ClientId,
    command: &Command,
) {
    let Some(id) = used_resource(command).and_then(phux_protocol::ids::ResourceId::local_id) else {
        return;
    };
    state.with_mut(|s| s.hub_note_satellite_use(host, id, client_id));
}

/// Relay a `KILL_RESOURCE_IF` once the hub has checked the half of
/// `UNATTACHED_SINCE_SPAWN` only it can see (ADR-0109, L1 §9.1); the
/// satellite checks the rest. A refusal never touches the link.
async fn relay_conditional_kill(
    state: &SharedState,
    relay: &crate::hub::relay::RelayHandle,
    host: &phux_protocol::ids::SatelliteHost,
    command: &Command,
    client_id: ClientId,
) -> CommandResult {
    let Command::KillResourceIf {
        terminal_id,
        precondition,
        ..
    } = command
    else {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "a conditional-kill relay was handed another command".to_owned(),
        };
    };
    if !hub_vouches_for_kill(state, host, terminal_id, precondition) {
        return CommandResult::Error {
            code: ErrorCode::PreconditionFailed,
            message: format!(
                "{host}/{terminal_id} not killed: this hub did not spawn it under that \
                 instance token, no longer remembers it, or another hub consumer has \
                 attached or used it"
            ),
        };
    }
    relay.command_from(command.clone(), client_id).await
}

/// `true` unless the kill asks for `UNATTACHED_SINCE_SPAWN` and the hub's
/// spawn ledger cannot vouch for it under the kill's instance token.
fn hub_vouches_for_kill(
    state: &SharedState,
    host: &phux_protocol::ids::SatelliteHost,
    terminal_id: &phux_protocol::ids::ResourceId,
    precondition: &phux_protocol::wire::frame::KillPrecondition,
) -> bool {
    use phux_protocol::wire::frame::KillConditions;
    if !precondition
        .conditions
        .contains(KillConditions::UNATTACHED_SINCE_SPAWN)
    {
        return true;
    }
    let (Some(id), Some(instance)) = (terminal_id.local_id(), precondition.instance) else {
        return false;
    };
    state.with(|s| s.hub_vouches_unattached(host, id, instance))
}

/// Record the hub-side proxy attach a successful `ATTACH_RESOURCE` just
/// established, then send the relayed reply to the caller.
#[allow(
    clippy::too_many_arguments,
    reason = "routing, command, and cancellation context"
)]
async fn reply_satellite_command(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    host: &phux_protocol::ids::SatelliteHost,
    command: &Command,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    result: CommandResult,
    connection_token: &CancellationToken,
) {
    if !matches!(result, CommandResult::Error { .. })
        && let Command::AttachResource { terminal_id, .. } = command
        && let Some(id) = terminal_id.local_id()
    {
        state.with_mut(|s| {
            s.register_satellite_proxy_attach(client_id, host.clone(), id);
        });
    }
    debug!(
        ?client_id,
        request_id,
        satellite = %host,
        "satellite-routed COMMAND relayed; sending COMMAND_RESULT"
    );
    tokio::select! {
        biased;
        () = connection_token.cancelled() => {}
        _ = out_tx.send(Outbound::Frame(FrameKind::CommandResult { request_id, result })) => {}
    }
}

/// Relay a stream-establishing command and register the caller's mailbox as
/// a hub-side proxy subscriber atomically with it.
#[allow(
    clippy::too_many_arguments,
    reason = "consumer identity, mailbox, cancellation, and negotiated bootstrap"
)]
async fn relay_stream_establishing(
    relay: &crate::hub::relay::RelayHandle,
    command: &Command,
    terminal_id: &phux_protocol::ids::ResourceId,
    client_id: ClientId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    consumer_cancel: Option<CancellationToken>,
) -> CommandResult {
    let Some(terminal) = terminal_id.local_id() else {
        return relay.command(command.clone()).await;
    };
    let Some(consumer_cancel) = consumer_cancel else {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "client connection cancellation is unavailable".to_owned(),
        };
    };
    // Only ATTACH_RESOURCE opens a content stream with a snapshot to await.
    let content = matches!(command, Command::AttachResource { .. });
    relay
        .command_subscribing(
            command.clone(),
            crate::hub::relay::ProxySubscription {
                terminal,
                client: client_id,
                out_tx: out_tx.clone(),
                consumer_cancel,
                // Stamped by `command_subscribing` at enqueue.
                seq: 0,
                awaits_snapshot: content,
                bootstrap_profile: content.then_some(bootstrap_profile),
                bootstrap_limits: content.then_some(bootstrap_limits),
            },
        )
        .await
}

/// Hub-side resolution of `DETACH_RESOURCE`: withdraw this consumer's proxy
/// subscription; the link session emits the satellite-side `DETACH_RESOURCE`
/// iff nobody else still observes the terminal. Success waits for the link's
/// removal receipt; no registry on a disconnected link is an idempotent Ok.
async fn resolve_hub_detach_terminal(
    state: &SharedState,
    relay: &crate::hub::relay::RelayHandle,
    client_id: ClientId,
    host: &phux_protocol::ids::SatelliteHost,
    terminal_id: &phux_protocol::ids::ResourceId,
) -> CommandResult {
    let Some(id) = terminal_id.local_id() else {
        return CommandResult::Ok;
    };
    let result = relay.unsubscribe_terminal(client_id, id).await;
    if result == CommandResult::Ok {
        state.with_mut(|s| {
            s.unregister_satellite_proxy_attach(client_id, host, id);
            let scope = phux_protocol::ids::ResourceId::Satellite {
                host: host.clone(),
                id,
            };
            s.unsubscribe_terminal_events(client_id, &scope);
        });
    }
    result
}

/// The command the link forwards for a hub consumer's
/// `SUBSCRIBE_RESOURCE_EVENTS`: unfiltered, because the link is one client on
/// the satellite; each consumer's filter is applied by the hub (ADR-0123).
fn unfiltered_for_link(command: &Command) -> Command {
    match command {
        Command::SubscribeResourceEvents { terminal_id, .. } => Command::SubscribeResourceEvents {
            terminal_id: terminal_id.clone(),
            event_types: Vec::new(),
        },
        other => other.clone(),
    }
}

/// Finish a relayed `SUBSCRIBE_RESOURCE_EVENTS`: start the event pump on
/// success, or restore the scope [`register_satellite_event_filter`] changed.
fn settle_satellite_event_filter(
    state: &SharedState,
    client_id: ClientId,
    change: Option<crate::state::SatelliteScopeChange>,
    result: &CommandResult,
) {
    let Some(change) = change else {
        return;
    };
    if matches!(result, CommandResult::Error { .. }) {
        state.with_mut(|s| s.restore_satellite_scope(client_id, change));
        return;
    }
    super::client::ensure_event_pump(state, client_id);
}

/// Install the hub registry scope, with the consumer's filter, that a relayed
/// `SUBSCRIBE_RESOURCE_EVENTS` delivers through (ADR-0123). Returns the prior
/// scope for rollback; `None` for any other command.
fn register_satellite_event_filter(
    state: &SharedState,
    client_id: ClientId,
    host: &phux_protocol::ids::SatelliteHost,
    command: &Command,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> Option<crate::state::SatelliteScopeChange> {
    let Command::SubscribeResourceEvents {
        terminal_id,
        event_types,
    } = command
    else {
        return None;
    };
    let id = terminal_id.local_id()?;
    let scope = phux_protocol::ids::ResourceId::Satellite {
        host: host.clone(),
        id,
    };
    let filter = crate::state::EventFilter::of(event_types.clone());
    Some(
        state.with_mut(|s| {
            s.subscribe_satellite_events(client_id, scope, filter, None, out_tx.clone())
        }),
    )
}

/// The hub-side lease coordinates one satellite-routed input command acts on.
/// Hub consumers share one identity on the satellite, so exclusion between
/// them is resolved here before anything touches the link (L1 §9.1).
struct SatelliteLeaseTarget<'a> {
    state: &'a SharedState,
    host: &'a phux_protocol::ids::SatelliteHost,
    /// Satellite-local wire terminal id.
    terminal: u32,
    client_id: ClientId,
}

impl<'a> SatelliteLeaseTarget<'a> {
    /// A command that carries no satellite-local terminal id resolves to
    /// terminal `0`, the lease-table slot such commands have always used.
    fn new(
        state: &'a SharedState,
        host: &'a phux_protocol::ids::SatelliteHost,
        client_id: ClientId,
        terminal_id: &phux_protocol::ids::ResourceId,
    ) -> Self {
        Self {
            state,
            host,
            terminal: terminal_id.local_id().unwrap_or(0),
            client_id,
        }
    }

    /// The hub consumer currently holding this terminal's input lease.
    fn holder(&self) -> Option<ClientId> {
        self.state
            .with(|s| s.satellite_lease_holder(self.host, self.terminal))
    }
}

/// Apply the hub's own lease exclusion to `ACQUIRE_INPUT` before relaying:
/// a cooperative acquire against another hub consumer's lease is refused.
async fn relay_satellite_acquire_input(
    target: &SatelliteLeaseTarget<'_>,
    relay: &crate::hub::relay::RelayHandle,
    mode: InputMode,
    command: &Command,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> CommandResult {
    if mode == InputMode::Cooperative
        && let Some(holder) = target.holder()
        && holder != target.client_id
    {
        return CommandResult::Error {
            code: ErrorCode::InputLeaseHeld,
            message: format!("input lease held by client {}", holder.0),
        };
    }
    // Mark the lease pending before relaying: replies and events are not
    // ordered on the link, so a stale Released/Expired from an earlier lease
    // could otherwise clear the entry this acquire installs. The lease mirror
    // clears the mark on the next event for this terminal.
    target
        .state
        .with_mut(|s| s.mark_satellite_lease_acquire_pending(target.host.clone(), target.terminal));
    let result = relay.command(command.clone()).await;
    settle_satellite_seize(target, &result, out_tx);
    result
}

/// Settle a hub-ledger acquire once its relay has answered, for
/// `ACQUIRE_INPUT` and a deliberate-takeover attach (ADR-0127) alike. The
/// caller marked the terminal pending before relaying.
fn settle_satellite_seize(
    target: &SatelliteLeaseTarget<'_>,
    result: &CommandResult,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    if matches!(result, CommandResult::Error { .. }) {
        // A failed acquire installs nothing, so stop ignoring lease events.
        target
            .state
            .with_mut(|s| s.clear_satellite_lease_acquire_pending(target.host, target.terminal));
        return;
    }
    // On success the pending mark stays; the lease mirror clears it. A seize
    // that evicts another hub consumer tells it that it lost the wheel.
    let evicted = target.state.with_mut(|s| {
        s.set_satellite_lease(
            target.host.clone(),
            target.terminal,
            target.client_id,
            out_tx.clone(),
        )
    });
    if let Some(evicted) = evicted {
        notify_satellite_lease_seized(target.host, target.terminal, target.client_id, &evicted);
    }
}

/// `ATTACH_RESOURCE` for a satellite Terminal on a hub (ADR-0127). The hub
/// holds the consumer's declared role: a viewer mark keyed by the
/// satellite-tagged id, and a takeover as a hub-ledger seize. A satellite
/// without `ATTACH_ROLES` gets a plain attach then a relayed seize; if that
/// seize fails, the attach stands and its refusal is the reply.
#[allow(
    clippy::too_many_arguments,
    reason = "lease target, relay, command, role, and negotiated bootstrap context"
)]
async fn relay_satellite_attach(
    target: &SatelliteLeaseTarget<'_>,
    relay: &crate::hub::relay::RelayHandle,
    command: &Command,
    role: RolePolicy,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
) -> CommandResult {
    let state = target.state;
    let local = phux_protocol::ids::ResourceId::local(target.terminal);
    let wire = phux_protocol::ids::ResourceId::satellite(target.host.clone(), target.terminal);
    // Narrow before the relay so no input slips through; widen only once the
    // satellite accepted the attach.
    let (was_attached, was_viewer, narrowed) = state.with_mut(|s| {
        let was_attached =
            s.has_satellite_proxy_attach(target.client_id, target.host, target.terminal);
        let was_viewer = s.is_viewer(target.client_id, &wire);
        let narrowed = role
            .is_viewer()
            .then(|| s.apply_attach_role(target.client_id, &wire, None, role, was_attached));
        (was_attached, was_viewer, narrowed)
    });
    let seize = role.takes_over();
    if seize {
        state.with_mut(|s| {
            s.mark_satellite_lease_acquire_pending(target.host.clone(), target.terminal);
        });
    }
    let attached = relay_stream_establishing(
        relay,
        command,
        &local,
        target.client_id,
        out_tx,
        bootstrap_profile,
        bootstrap_limits,
        state.with(|s| s.client_connection_cancellation(target.client_id)),
    )
    .await;
    if matches!(attached, CommandResult::Error { .. }) {
        // A refused first narrowing must not leave a viewer tombstone.
        if role.is_viewer() && !was_viewer && !was_attached {
            state.with_mut(|s| s.set_viewer_mark(target.client_id, &wire, false));
        }
        if seize {
            settle_satellite_seize(target, &attached, out_tx);
        }
        return attached;
    }
    // The attach stands even if a degraded seize after it is refused.
    state.with_mut(|s| {
        s.register_satellite_proxy_attach(target.client_id, target.host.clone(), target.terminal);
    });
    let effects = narrowed.unwrap_or_else(|| {
        state.with_mut(|s| s.apply_attach_role(target.client_id, &wire, None, role, was_attached))
    });
    if effects.role_changed {
        journal_satellite_role_change(target, wire);
    }
    if role.is_viewer() {
        release_satellite_lease_for_viewer(target, relay).await;
    }
    if seize {
        return seize_after_satellite_attach(target, relay, attached, out_tx).await;
    }
    attached
}

/// Finish a takeover whose attach the satellite accepted: relay a seize
/// unless the satellite already seized with the attach (`ATTACH_ROLES`).
async fn seize_after_satellite_attach(
    target: &SatelliteLeaseTarget<'_>,
    relay: &crate::hub::relay::RelayHandle,
    attached: CommandResult,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> CommandResult {
    let result = if satellite_seizes_on_attach(target) {
        attached
    } else {
        relay
            .command(Command::AcquireInput {
                terminal_id: phux_protocol::ids::ResourceId::local(target.terminal),
                mode: InputMode::Seize,
                ttl_ms: 0,
            })
            .await
    };
    settle_satellite_seize(target, &result, out_tx);
    result
}

/// A hub consumer that narrows to `VIEWER` releases its satellite lease
/// (L1 §8.1).
async fn release_satellite_lease_for_viewer(
    target: &SatelliteLeaseTarget<'_>,
    relay: &crate::hub::relay::RelayHandle,
) {
    if target.holder() != Some(target.client_id) {
        return;
    }
    let release = Command::ReleaseInput {
        terminal_id: phux_protocol::ids::ResourceId::local(target.terminal),
    };
    let _ = relay_satellite_release_input(target, relay, &release).await;
}

/// Whether the satellite advertised `ATTACH_ROLES`, so the link carried the
/// takeover with the attach.
fn satellite_seizes_on_attach(target: &SatelliteLeaseTarget<'_>) -> bool {
    target.state.with(|s| {
        s.satellite_advertises(target.host, phux_protocol::caps::ServerFeature::AttachRoles)
    })
}

/// Journal a hub consumer's role flip on a satellite Terminal (ADR-0127);
/// the satellite never sees the role.
fn journal_satellite_role_change(
    target: &SatelliteLeaseTarget<'_>,
    wire: phux_protocol::ids::ResourceId,
) {
    target.state.with_mut(|s| {
        let event = AgentEvent::TerminalControl {
            // The hub keeps no satellite lifecycle; `Running` is its default.
            lifecycle: ResourceLifecycle::Running,
            exit_status: None,
            input_holder: s
                .satellite_lease_holder(target.host, target.terminal)
                .map(super::wire_client),
            action: ControlAction::RoleChanged,
            actor: Some(super::wire_client(target.client_id)),
        };
        let _ = s.record_and_fanout(
            crate::state::EventRecord::new(Some(wire), event).with_actor(Some(target.client_id)),
        );
    });
}

/// Resolve `RELEASE_INPUT` against the hub-side lease before relaying it.
async fn relay_satellite_release_input(
    target: &SatelliteLeaseTarget<'_>,
    relay: &crate::hub::relay::RelayHandle,
    command: &Command,
) -> CommandResult {
    if let Some(holder) = target.holder()
        && holder != target.client_id
    {
        // Idempotent no-op (ADR-0033), never forwarded: on the satellite
        // this consumer is the holder's identity (L1 §9.1).
        return CommandResult::Ok;
    }
    let result = relay.command(command.clone()).await;
    if !matches!(result, CommandResult::Error { .. }) {
        target.state.with_mut(|s| {
            s.release_satellite_lease(target.host, target.terminal, target.client_id)
        });
    }
    result
}

/// Refuse `ROUTE_INPUT` from a non-holder before it reaches the link.
async fn relay_satellite_route_input(
    target: &SatelliteLeaseTarget<'_>,
    relay: &crate::hub::relay::RelayHandle,
    command: &Command,
) -> CommandResult {
    if let Some(holder) = target.holder()
        && holder != target.client_id
    {
        return CommandResult::Error {
            code: ErrorCode::InputLeaseHeld,
            message: "input lease held by another client".to_owned(),
        };
    }
    relay.command_from(command.clone(), target.client_id).await
}

/// Tell the hub consumer a satellite SEIZE evicted that it lost the lease
/// (L1 §9.1): the satellite sees one link identity and cannot. Best-effort.
fn notify_satellite_lease_seized(
    host: &phux_protocol::ids::SatelliteHost,
    id: u32,
    new_holder: ClientId,
    evicted: &crate::state::SatelliteLease,
) {
    let frame = FrameKind::Event {
        terminal: Some(phux_protocol::ids::ResourceId::satellite(host.clone(), id)),
        event: AgentEvent::TerminalControl {
            // The hub keeps no satellite lifecycle; the load-bearing fields
            // are the action and the holder.
            lifecycle: ResourceLifecycle::Running,
            exit_status: None,
            input_holder: Some(super::wire_client(new_holder)),
            action: ControlAction::Seized,
            actor: Some(super::wire_client(new_holder)),
        },
        stamp: None,
    };
    if evicted.out_tx.try_send(Outbound::Frame(frame)).is_err() {
        debug!(
            satellite = %host,
            terminal = id,
            prior = ?evicted.holder,
            "evicted hub lease holder unreachable for the SEIZE notification; dropping",
        );
    } else {
        debug!(
            satellite = %host,
            terminal = id,
            prior = ?evicted.holder,
            ?new_holder,
            "notified the evicted hub lease holder of a satellite SEIZE takeover",
        );
    }
}

/// Forward one fire-and-forget frame (`INPUT_*`, `FRAME_ACK`,
/// `RESIZE_TERMINAL`) for a satellite terminal over the hub link; `build`
/// receives the satellite-local id. `false` when there is no route.
fn relay_satellite_frame(
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    route: &RelayRoute,
    frame_label: &'static str,
    build: impl FnOnce(phux_protocol::ids::ResourceId) -> FrameKind,
) -> bool {
    let Some(relay) = route.relay.as_ref() else {
        return false;
    };
    trace!(
        ?client_id,
        ?wire_terminal_id,
        frame_label,
        satellite = %route.host,
        "relaying satellite-routed frame"
    );
    relay.forward(build(route.local_wire_id()));
    true
}

/// Handle `KILL_RESOURCES` (L1 §5.2): close every local id under one lock,
/// then relay each satellite's part and await every answer. An unkeyed batch
/// answers `OK` (skips and relay failures are logged); a keyed batch
/// (L1 §5.1.1) answers one outcome per id, merged across hops.
pub(crate) async fn handle_kill_terminals(
    state: &SharedState,
    client_id: ClientId,
    ids: &[phux_protocol::ids::ResourceId],
    operation_id: Option<phux_protocol::ids::IdempotencyKey>,
) -> CommandResult {
    let partitions = satellite_partitions(ids);
    let attribution = kill_attribution(client_id, operation_id);
    let (killed, not_found) = kill_local_batch(state, ids, attribution, false);
    let relayed = futures_util::future::join_all(
        partitions
            .into_iter()
            .map(|(host, ids)| relay_kill_partition(state, client_id, host, ids, operation_id)),
    )
    .await;
    if operation_id.is_none() {
        warn_failed_partitions(&relayed);
        return CommandResult::Ok;
    }
    let mut results = super::keyed_ops::KillResults::default();
    for id in &killed {
        results.killed(id);
    }
    for id in &not_found {
        results.not_found(id);
    }
    for partition in &relayed {
        results.merge_host(&partition.host, &partition.ids, &partition.result);
    }
    results.into_result()
}

/// `CLOSE_TAB_RESOURCES` (L1 §5.2.2, ADR-0114): the atomic local close of
/// [`handle_kill_terminals`] that keeps keep-empty sessions. Unkeyed; the
/// satellite parts are relayed detached and not awaited.
pub(crate) fn handle_close_tab_resources(
    state: &SharedState,
    ids: &[phux_protocol::ids::ResourceId],
) -> CommandResult {
    relay_close_tab_partitions(state, ids);
    let _ = kill_local_batch(state, ids, crate::state::CloseAttribution::default(), true);
    CommandResult::Ok
}

/// A batch's satellite ids grouped by host, each kept as the caller wrote it.
type KillPartitions = std::collections::BTreeMap<
    phux_protocol::ids::SatelliteHost,
    Vec<phux_protocol::ids::ResourceId>,
>;

fn satellite_partitions(ids: &[phux_protocol::ids::ResourceId]) -> KillPartitions {
    let mut by_host = KillPartitions::new();
    for wire_id in ids {
        if let Some((host, _)) = crate::hub::relay::satellite_route(wire_id) {
            by_host.entry(host).or_default().push(wire_id.clone());
        }
    }
    by_host
}

/// Satellite-tagged ids rewritten into their host's `Local` space.
fn satellite_local_ids(
    wire_ids: &[phux_protocol::ids::ResourceId],
) -> Vec<phux_protocol::ids::ResourceId> {
    wire_ids
        .iter()
        .filter_map(crate::hub::relay::satellite_route)
        .map(|(_, id)| phux_protocol::ids::ResourceId::local(id))
        .collect()
}

/// Close the local ids of a batch under one lock and return which named a
/// live resource and which named nothing. `preserve_keep_empty` is
/// `CLOSE_TAB_RESOURCES`.
fn kill_local_batch(
    state: &SharedState,
    ids: &[phux_protocol::ids::ResourceId],
    attribution: crate::state::CloseAttribution,
    preserve_keep_empty: bool,
) -> (
    Vec<phux_protocol::ids::ResourceId>,
    Vec<phux_protocol::ids::ResourceId>,
) {
    let label = if preserve_keep_empty {
        "CLOSE_TAB_RESOURCES"
    } else {
        "KILL_RESOURCES"
    };
    state.with_mut(|s| {
        let mut targets = Vec::with_capacity(ids.len());
        let (mut killed, mut not_found) = (Vec::new(), Vec::new());
        for wire_id in ids.iter().filter(|id| id.local_id().is_some()) {
            if let Some(core_id) = s.terminal_from_wire(wire_id) {
                targets.push(core_id);
                killed.push(wire_id.clone());
            } else {
                debug!(?wire_id, "{label}: unknown / dead id; skipping");
                not_found.push(wire_id.clone());
            }
        }
        // ADR-0105: a batch naming every pane of a keep-empty session releases
        // its mark; CLOSE_TAB_RESOURCES keeps it (ADR-0114).
        if !preserve_keep_empty {
            for name in s.release_keep_empty_covered_by(&targets) {
                let _ = s.metadata_broadcast(
                    &phux_protocol::wire::frame::Scope::Global,
                    ServerInterceptedKey::SessionKeepEmpty,
                    &phux_protocol::wire::frame::encode_session_keep_empty(&name, false),
                );
            }
        }
        // ADR-0104 §2: targets and their children close once, in this borrow.
        let closed = s.close_resources_attributed(
            &targets,
            phux_protocol::wire::frame::CloseReason::Killed,
            attribution,
        );
        debug!(
            requested = ids.len(),
            closed, "{label}: torn down local ids atomically"
        );
        (killed, not_found)
    })
}

/// `CLOSE_TAB_RESOURCES`'s satellite ids, forwarded detached to each host as
/// that host's own batch of the same command.
fn relay_close_tab_partitions(state: &SharedState, ids: &[phux_protocol::ids::ResourceId]) {
    for (host, wire_ids) in satellite_partitions(ids) {
        let local_ids = satellite_local_ids(&wire_ids);
        match state.with(|s| s.hub_relay(&host)) {
            Some(relay) => {
                debug!(
                    satellite = %host,
                    count = local_ids.len(),
                    "CLOSE_TAB_RESOURCES: relaying satellite partition"
                );
                relay.command_detached(Command::CloseTabResources { ids: local_ids });
            }
            None => {
                debug!(
                    satellite = %host,
                    "CLOSE_TAB_RESOURCES: no route to satellite; skipping its ids"
                );
            }
        }
    }
}

/// One satellite's part of a batch and its host's answer.
struct KillPartition {
    host: phux_protocol::ids::SatelliteHost,
    ids: Vec<phux_protocol::ids::ResourceId>,
    result: CommandResult,
}

/// Relay one satellite's part of a batch as that satellite's own
/// `KILL_RESOURCES`, under the batch's key. A server with no route to `host`
/// answers for it.
async fn relay_kill_partition(
    state: &SharedState,
    client_id: ClientId,
    host: phux_protocol::ids::SatelliteHost,
    ids: Vec<phux_protocol::ids::ResourceId>,
    operation_id: Option<phux_protocol::ids::IdempotencyKey>,
) -> KillPartition {
    let command = Command::KillResources {
        ids: satellite_local_ids(&ids),
        operation_id,
    };
    let result = match state.with(|s| s.hub_relay(&host)) {
        Some(relay) => relay.command_from(command, client_id).await,
        None => CommandResult::Error {
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: format!(
                "no satellite route to {host}: this server is not a federation hub for that host"
            ),
        },
    };
    KillPartition { host, ids, result }
}

/// An unkeyed batch keeps its `OK` reply, so a satellite part that failed is
/// said here instead.
fn warn_failed_partitions(relayed: &[KillPartition]) {
    for partition in relayed {
        if let CommandResult::Error { code, message } = &partition.result {
            tracing::warn!(
                satellite = %partition.host,
                ?code,
                count = partition.ids.len(),
                %message,
                "KILL_RESOURCES: a satellite part was not killed; the unkeyed reply stays OK"
            );
        }
    }
}

/// Force-detach the session-attached clients of `session` (all when `None`)
/// for `phux detach`, answering the count as JSON. An unknown session
/// detaches nobody. `ATTACH_RESOURCE` subscribers are not swept.
pub(crate) fn handle_detach_clients(state: &SharedState, session: Option<&str>) -> CommandResult {
    let targets = state.with(|s| s.attached_clients_to_detach(session));
    let count = targets.len();
    for (client_id, tx) in targets {
        // `try_send`: a wedged mailbox is the stuck client this verb exists to
        // clear, so never await its capacity; the teardown still runs.
        let _ = tx.try_send(Outbound::Frame(FrameKind::Detached {
            reason: Some(DetachReason::Requested),
            message: "detached by `phux detach`".to_owned(),
        }));
        super::client::detach_and_release_consumer_state(state, client_id);
    }
    debug!(
        ?session,
        count, "DETACH_CLIENTS: force-detached clients from outside the attach UI"
    );
    CommandResult::OkWith(CommandValue::Json(count.to_string()))
}

/// A PTY command from a non-empty request argv.
pub(crate) fn argv_command(argv: Option<Vec<String>>) -> Option<portable_pty::CommandBuilder> {
    let mut argv = argv?.into_iter();
    let mut builder = portable_pty::CommandBuilder::new(argv.next()?);
    for arg in argv {
        builder.arg(arg);
    }
    Some(builder)
}

/// Create a named session and seed its pane without attaching: the
/// `SESSION_CREATE_KEY` metadata write (ADR-0019 / ADR-0027).
///
/// The existence check and seed run on the single-threaded runtime, so two
/// racing creates for one `name` cannot both succeed. Returns the seed pane's
/// wire id; `Err` is log-only because `SET_METADATA` has no reply frame.
pub(crate) fn create_named_session(
    state: &SharedState,
    name: &str,
    command: Option<Vec<String>>,
    cwd: Option<&str>,
    env: std::collections::BTreeMap<String, String>,
    origin: SeedOrigin,
    root_token: &CancellationToken,
) -> Result<phux_protocol::ids::ResourceId, String> {
    if state.with(|s| s.session_by_name(name).is_some()) {
        return Err(format!("session {name:?} already exists"));
    }

    let (with_pty, override_cmd, scrollback, term, shell, login_shell) = state.with(|s| {
        (
            s.attach_create_seeds_pty(),
            s.attach_create_seed_command(),
            s.scrollback_limits(),
            s.term().to_owned(),
            s.shell().to_owned(),
            s.login_shell(),
        )
    });

    let seed_cmd = with_pty.then(|| {
        // A server-wide override wins, then the request argv, then the shell.
        let mut seed_cmd = override_cmd.unwrap_or_else(|| {
            argv_command(command).unwrap_or_else(|| {
                crate::terminal_actor::default_shell_command(&shell, login_shell)
            })
        });
        // An invalid cwd is dropped with a warn, never failing the create.
        crate::terminal_actor::apply_spawn_cwd(&mut seed_cmd, cwd, name);
        for (key, value) in env {
            seed_cmd.env(key, value);
        }
        crate::terminal_actor::apply_term(&mut seed_cmd, &term);
        seed_cmd
    });
    match seed_session(state, name, seed_cmd, scrollback, root_token, None, origin) {
        Ok(core_terminal) => {
            // A headless create arms the last-session self-exit like an attach.
            let wire = state.with_mut(|s| {
                s.arm_self_exit();
                s.intern_terminal_wire(core_terminal)
            });
            Ok(wire)
        }
        Err(err) => {
            warn!(
                session = %name,
                error = %err,
                "session-create: failed to seed pane for new session",
            );
            Err(format!("failed to create session {name:?}: {err}"))
        }
    }
}

/// Create a keep-empty session with zero windows (ADR-0105), the
/// `empty: true` form of the `SESSION_CREATE_KEY` write.
pub(crate) fn create_empty_session(state: &SharedState, name: &str) -> Result<(), String> {
    state.with_mut(|s| {
        if s.session_by_name(name).is_some() {
            return Err(format!("session {name:?} already exists"));
        }
        s.seed_empty_session(name);
        s.arm_self_exit();
        Ok(())
    })
}

/// `GET_PERF`: the server's telemetry as a JSON `phux_perf::PerfReport`,
/// with registry gauges refreshed first; `reset` zeroes metrics afterwards.
pub(crate) fn handle_get_perf(state: &SharedState, reset: bool) -> CommandResult {
    let (sessions, panes) =
        state.with_mut(|s| (s.registry().session_count(), s.registry().terminal_count()));
    let clients = match handle_get_state(state, None, &StateScope::Server) {
        CommandResult::OkWith(CommandValue::State(snapshot)) => snapshot
            .sessions
            .iter()
            .map(|s| u64::from(s.attached_client_count))
            .sum::<u64>(),
        _ => 0,
    };
    crate::perf::SESSIONS.set(u64::try_from(sessions).unwrap_or(u64::MAX));
    crate::perf::PANES.set(u64::try_from(panes).unwrap_or(u64::MAX));
    crate::perf::CLIENTS.set(clients);
    let mut report = crate::perf::report();
    report.stream_diagnostics = serde_json::to_value(crate::stream_diagnostics::snapshot()).ok();
    if reset {
        crate::perf::reset();
        crate::stream_diagnostics::reset();
    }
    CommandResult::OkWith(CommandValue::Json(report.to_json()))
}

/// `GET_STATE { SERVER }`: the whole-server snapshot in the `ATTACHED`
/// `SessionSnapshot` shape, focused on the most recently touched session.
pub(crate) fn handle_get_state(
    state: &SharedState,
    viewer: Option<ClientId>,
    scope: &StateScope,
) -> CommandResult {
    match scope {
        StateScope::Server => {
            let snapshot = state.with_mut(|s| {
                let focus = s
                    .most_recently_touched_session()
                    .or_else(|| s.registry().sessions().next().map(|(id, _)| id));
                let mut snapshot = focus
                    .and_then(|sid| s.build_session_snapshot(sid))
                    .unwrap_or_else(empty_session_snapshot);
                // An empty registry still reports listeners for `phux doctor`.
                if snapshot.listeners().is_none() && s.has_remote_listener_report() {
                    snapshot = snapshot.with_listeners(s.remote_listeners().clone());
                }
                // L1 §7.3: the viewer's journal head at the cut, read under
                // the snapshot's lock.
                let snapshot = snapshot.with_journal_head(Some(s.journal_head_for(viewer)));
                // workload-auth §6: only what the viewer may INVENTORY.
                match viewer {
                    Some(viewer) => crate::policy::filter::filter_snapshot(
                        s,
                        viewer,
                        phux_protocol::kinds::Verb::Inventory,
                        snapshot,
                    ),
                    None => snapshot,
                }
            });
            CommandResult::OkWith(CommandValue::State(snapshot))
        }
        _ => CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: "unsupported GET_STATE scope".to_owned(),
        },
    }
}

/// `GET_STATE` with federation aggregation (L1 §9.1): on a hub, merge every
/// satellite's terminals (retagged `Satellite { host, id }`, queried
/// concurrently) into the local snapshot. Satellite sessions and windows do
/// not merge; each satellite contributes one `HostInventory` row instead,
/// sorted by host (no chaining). An unreachable satellite never fails the
/// aggregate: its row is marked unreachable and the caller gets one
/// uncorrelated `ERROR` before the result.
pub(crate) async fn handle_get_state_federated(
    state: &SharedState,
    viewer: ClientId,
    scope: &StateScope,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> CommandResult {
    let local = handle_get_state(state, Some(viewer), scope);
    if !matches!(scope, StateScope::Server) {
        return local;
    }
    // workload-auth §6: only the satellites the viewer may inventory are
    // asked, so another host's name and health never reach it.
    let relays: Vec<_> = state.with(|s| {
        s.hub_relays_all()
            .into_iter()
            .filter(|relay| {
                crate::policy::filter::admits_satellite(
                    s,
                    viewer,
                    phux_protocol::kinds::Verb::Inventory,
                    relay.host().as_str(),
                )
            })
            .collect()
    });
    if relays.is_empty() {
        // Non-hub server (or hub with an empty table): the local snapshot
        // is the whole truth, and its empty `hosts` says so.
        return local;
    }
    let CommandResult::OkWith(CommandValue::State(mut snapshot)) = local else {
        return local;
    };
    // Query every satellite concurrently: the aggregate's latency bound
    // is one relay deadline, not one per satellite.
    let queries = relays.into_iter().map(|relay| async move {
        let result = relay
            .command(Command::GetState {
                scope: StateScope::Server,
            })
            .await;
        (relay.host().clone(), result)
    });
    let mut hosts = Vec::new();
    for (host, result) in futures_util::future::join_all(queries).await {
        hosts.push(fold_satellite_state(&mut snapshot, host, result, out_tx).await);
    }
    hosts.sort_by(|a, b| a.host.as_str().cmp(b.host.as_str()));
    mirror_federated_agent_metadata(state, &snapshot);
    // workload-auth §6: the satellites' rows and Terminals, filtered too.
    let snapshot = state.with(|s| {
        crate::policy::filter::filter_snapshot(
            s,
            viewer,
            phux_protocol::kinds::Verb::Inventory,
            snapshot.with_hosts(hosts),
        )
    });
    CommandResult::OkWith(CommandValue::State(snapshot))
}

/// Ask each satellite link to mirror the agent allowlist for every terminal
/// the aggregate just listed (ADR-0136). A terminal the inventory does not
/// name is mirrored later, when a consumer subscribes or reads it.
fn mirror_federated_agent_metadata(
    state: &SharedState,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
) {
    for resource in &snapshot.resources {
        if !resource.kind.is_terminal() {
            continue;
        }
        let phux_protocol::ids::ResourceId::Satellite { host, id } = &resource.id else {
            continue;
        };
        if let Some(relay) = state.with(|s| s.hub_relay(host)) {
            relay.mirror_terminal(*id);
        }
    }
}

/// Fold one satellite's `GET_STATE` answer into the hub's aggregate: merge
/// its resources into `snapshot` and return its inventory row. An error
/// answer becomes an unreachable row plus the un-correlated degradation
/// `ERROR` pushed ahead of the `COMMAND_RESULT` (L1 §9.1).
async fn fold_satellite_state(
    snapshot: &mut phux_protocol::wire::info::SessionSnapshot,
    host: phux_protocol::ids::SatelliteHost,
    result: CommandResult,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> phux_protocol::wire::info::HostInventory {
    match result {
        CommandResult::OkWith(CommandValue::State(sat)) => {
            let inventory = satellite_host_inventory(&host, &sat);
            merge_satellite_resources(snapshot, &host, sat.resources);
            inventory
        }
        CommandResult::Error { code, message } => {
            debug!(
                satellite = %host,
                ?code,
                %message,
                "GET_STATE aggregation: satellite contributes nothing"
            );
            let row = phux_protocol::wire::info::HostInventory::unreachable(host, message.clone());
            // Observable degradation, not silence: the same un-correlated
            // typed ERROR shape the relay uses for teardown notification.
            let _ = out_tx
                .send(Outbound::Frame(FrameKind::Error {
                    request_id: None,
                    code,
                    message,
                }))
                .await;
            row
        }
        other => {
            warn!(
                satellite = %host,
                ?other,
                "GET_STATE aggregation: unexpected satellite result shape; skipping"
            );
            phux_protocol::wire::info::HostInventory::unreachable(
                host,
                "satellite answered GET_STATE with an unexpected result",
            )
        }
    }
}

/// Append a satellite's resources to the hub's aggregate, re-tagged into the
/// hub's id space. A Satellite-tagged id is dropped (no chaining).
fn merge_satellite_resources(
    snapshot: &mut phux_protocol::wire::info::SessionSnapshot,
    host: &phux_protocol::ids::SatelliteHost,
    resources: Vec<phux_protocol::wire::info::ResourceInfo>,
) {
    for mut pane in resources {
        let Some(id) = retag_satellite_resource_id(host, Some(&pane.id)) else {
            warn!(
                satellite = %host,
                pane = %pane.id,
                "satellite listed a Satellite-tagged terminal; dropping (no chaining)"
            );
            continue;
        };
        pane.id = id;
        // ADR-0104: a parent retags by the same rule; a chained one drops.
        pane.parent = retag_satellite_resource_id(host, pane.parent.as_ref());
        snapshot.resources.push(pane);
    }
}

/// The inventory row for a satellite that answered: one entry per session it
/// reported, under its own satellite-local id.
fn satellite_host_inventory(
    host: &phux_protocol::ids::SatelliteHost,
    sat: &phux_protocol::wire::info::SessionSnapshot,
) -> phux_protocol::wire::info::HostInventory {
    let sessions = sat
        .sessions
        .iter()
        .map(|session| satellite_host_session(host, sat, session))
        .collect();
    phux_protocol::wire::info::HostInventory::reachable(host.clone(), sessions)
}

/// One satellite session's inventory entry, with its pane count and its
/// active pane derived from the satellite's own windows and resources.
fn satellite_host_session(
    host: &phux_protocol::ids::SatelliteHost,
    sat: &phux_protocol::wire::info::SessionSnapshot,
    session: &phux_protocol::wire::info::SessionInfo,
) -> phux_protocol::wire::info::HostSessionInfo {
    phux_protocol::wire::info::HostSessionInfo::new(session.id, session.name.clone())
        .with_created_at_unix_secs(session.created_at_unix_secs)
        .with_window_count(session.window_count)
        .with_pane_count(session_pane_count(sat, session.id))
        .with_attached_client_count(session.attached_client_count)
        .with_active_resource(session_active_resource(host, sat, session))
}

/// Terminal-kind resources across a satellite session's windows.
fn session_pane_count(
    sat: &phux_protocol::wire::info::SessionSnapshot,
    session: phux_protocol::SessionId,
) -> u16 {
    let panes = sat
        .resources
        .iter()
        .filter(|pane| pane.kind.is_terminal() && window_in_session(sat, pane.window_id, session))
        .count();
    u16::try_from(panes).unwrap_or(u16::MAX)
}

fn window_in_session(
    sat: &phux_protocol::wire::info::SessionSnapshot,
    window: phux_protocol::WindowId,
    session: phux_protocol::SessionId,
) -> bool {
    sat.windows
        .iter()
        .any(|w| w.id == window && w.session_id == session)
}

/// A satellite session's remembered focused pane, re-tagged for the hub:
/// the active window's active resource, falling back to the session's
/// first window and then to that window's first Terminal.
fn session_active_resource(
    host: &phux_protocol::ids::SatelliteHost,
    sat: &phux_protocol::wire::info::SessionSnapshot,
    session: &phux_protocol::wire::info::SessionInfo,
) -> Option<phux_protocol::ids::ResourceId> {
    let window = session
        .active_window
        .and_then(|id| sat.windows.iter().find(|w| w.id == id))
        .or_else(|| sat.windows.iter().find(|w| w.session_id == session.id))?;
    let local = window.active_resource.clone().or_else(|| {
        sat.resources
            .iter()
            .find(|pane| pane.kind.is_terminal() && pane.window_id == window.id)
            .map(|pane| pane.id.clone())
    });
    retag_satellite_resource_id(host, local.as_ref())
}

/// Retag a satellite's `Local { id }` as `Satellite { host, id }`. A
/// `Satellite`-tagged id has no hub-side form: hub-and-spoke does not chain
/// (L1 §9.1).
fn retag_satellite_resource_id(
    host: &phux_protocol::ids::SatelliteHost,
    id: Option<&phux_protocol::ids::ResourceId>,
) -> Option<phux_protocol::ids::ResourceId> {
    match id? {
        phux_protocol::ids::ResourceId::Local { id } => {
            Some(phux_protocol::ids::ResourceId::satellite(host.clone(), *id))
        }
        phux_protocol::ids::ResourceId::Satellite { .. } => None,
    }
}

/// Highest `GET_SCREEN.format` selector (low 7 bits) this build renders:
/// `1` HTML, `2` VT.
const MAX_SCREEN_FORMAT: u8 = 2;

/// Resolve `terminal_id` to its Terminal facet and ask the actor for a screen
/// projection; `verb` names the command in failure messages.
async fn request_screen(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    scrollback: Option<u32>,
    cells: bool,
    format: u8,
    verb: &str,
) -> Result<(TerminalHandle, ScreenReply), CommandResult> {
    let (_, handle) = state.with(|s| terminal_handle(s, terminal_id))?;
    let terminal = handle.terminal().map_err(wrong_resource_kind)?.clone();
    let internal = |message: String| CommandResult::Error {
        code: ErrorCode::InternalError,
        message,
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    terminal
        .screen
        .send(ScreenRequest {
            pane: terminal_id.local_id().unwrap_or(0),
            scrollback,
            cells,
            format,
            reply: reply_tx,
        })
        .await
        .map_err(|_| internal(format!("pane actor unavailable for {verb}")))?;
    let reply = reply_rx
        .await
        .map_err(|_| internal(format!("pane actor dropped the {verb} reply")))?;
    Ok((terminal, reply))
}

/// `GET_SCREEN`: the pane's [`phux_core::screen::ScreenState`] as JSON
/// (ADR-0022 §2), without attaching or resizing. `format` optionally adds a
/// rendering (D9); an unknown selector is `INVALID_COMMAND` and one over the
/// byte budget is `RESOURCE_EXHAUSTED`.
pub(crate) async fn handle_get_screen(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    request_scrollback: Option<u32>,
    cells: bool,
    format: u8,
) -> CommandResult {
    let selector = format & phux_protocol::wire::frame::GET_SCREEN_FORMAT_SELECTOR_MASK;
    if selector > MAX_SCREEN_FORMAT {
        return CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: format!("unknown GET_SCREEN format: {format}"),
        };
    }
    match request_screen(
        state,
        terminal_id,
        request_scrollback,
        cells,
        format,
        "GET_SCREEN",
    )
    .await
    {
        Ok((_, reply)) => screen_reply_result(reply),
        Err(refusal) => refusal,
    }
}

/// Turn a [`ScreenReply`] into the `GET_SCREEN` `CommandResult`.
fn screen_reply_result(reply: ScreenReply) -> CommandResult {
    match reply {
        ScreenReply::TooLarge {
            required_bytes,
            budget_bytes,
        } => CommandResult::Error {
            code: ErrorCode::ResourceExhausted,
            message: format!(
                "rendered capture would be {required_bytes} bytes, over the \
                 {budget_bytes}-byte budget; retry with a narrower --tail/--scrollback"
            ),
        },
        ScreenReply::Projection(screen) => serde_json::to_string(&screen).map_or_else(
            |err| CommandResult::Error {
                code: ErrorCode::InternalError,
                message: format!("screen serialization failed: {err}"),
            },
            |json| CommandResult::OkWith(CommandValue::Json(json)),
        ),
    }
}

/// `GET_TERMINAL_STATE` (ADR-0022, ADR-0015 L2): a structured JSON snapshot
/// of the grid, cursor, optional scrollback, and process facet, without an
/// attach. An actor that cannot report its process degrades to
/// `process: null`.
pub(crate) async fn handle_get_terminal_state(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    include_scrollback: bool,
    max_scrollback_lines: u16,
) -> CommandResult {
    let scrollback = include_scrollback.then_some(u32::from(max_scrollback_lines));
    // Cells are always requested for their semantic marks; no rendering.
    let (terminal, reply) = match request_screen(
        state,
        terminal_id,
        scrollback,
        true,
        0,
        "GET_TERMINAL_STATE",
    )
    .await
    {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    // `format: 0` never yields `TooLarge`.
    let ScreenReply::Projection(screen_state) = reply else {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "unexpected oversized-capture refusal for GET_TERMINAL_STATE".to_owned(),
        };
    };

    let viewport_cells = viewport_cells_json(&screen_state);
    let cursor = screen_state.cursor.map(|cs| {
        serde_json::json!({
            "x": cs.x,
            "y": cs.y,
            "visible": cs.visible,
        })
    });
    #[allow(clippy::cast_possible_truncation)]
    let scrollback_count_total = screen_state.scrollback.len() as u32;
    let scrollback_lines: Vec<_> = if include_scrollback {
        screen_state
            .scrollback
            .iter()
            .map(|line_text| serde_json::json!({ "text": line_text, "cells": [] }))
            .collect()
    } else {
        Vec::new()
    };

    let mut process = query_process_facet(&terminal).await;
    reconcile_process_exit(state, terminal_id, process.as_mut());
    let shell_state = process
        .as_ref()
        .and_then(|process| serde_json::to_value(process.prompt).ok());
    let timestamp_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    let terminal_state_json = serde_json::json!({
        "schema_version": phux_core::process::TERMINAL_STATE_SCHEMA_VERSION,
        "process": process,
        "cols": screen_state.cols,
        "rows": screen_state.rows,
        "cells": viewport_cells,
        "cursor": cursor,
        "scrollback": scrollback_lines,
        "scrollback_count_total": scrollback_count_total,
        "shell_state": shell_state,
        "pending_command": serde_json::Value::Null,
        "timestamp_secs": timestamp_secs,
        // No per-terminal logical clock exists yet.
        "seq": 0u64,
    });
    match serde_json::to_string(&terminal_state_json) {
        Ok(json) => CommandResult::OkWith(CommandValue::Json(json)),
        Err(err) => CommandResult::Error {
            code: ErrorCode::InternalError,
            message: format!("terminal state serialization failed: {err}"),
        },
    }
}

/// The viewport as JSON cells, one per char of each right-trimmed line. Width
/// is approximated: ASCII is one column, anything else two.
fn viewport_cells_json(screen_state: &phux_core::screen::ScreenState) -> Vec<serde_json::Value> {
    let mut viewport_cells = Vec::new();
    #[allow(clippy::cast_possible_truncation)]
    for (row_idx, line_text) in screen_state.lines.iter().enumerate() {
        let row = row_idx as u16;
        let mut col = 0u16;
        for ch in line_text.chars() {
            let width = if ch.is_ascii() { 1u16 } else { 2u16 };
            viewport_cells.push(serde_json::json!({
                "col": col,
                "row": row,
                "text": ch.to_string(),
                "width": width as u8,
                "selected": false,
            }));
            col += width;
            if col >= screen_state.cols {
                break;
            }
        }
    }
    viewport_cells
}

/// Make `process.exit` the record `GET_STATE` reports (ADR-0124, L1 §1.1):
/// a retained pane's exit wins, and a closing pane takes its ledger reason so
/// a kill reads `killed` on both surfaces.
fn reconcile_process_exit(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    process: Option<&mut phux_core::process::TerminalProcessState>,
) {
    let Some(process) = process else {
        return;
    };
    let (retained, pending) = state.with(|s| {
        let pane = s.terminal_from_wire(terminal_id);
        (
            pane.and_then(|pane| s.retained_process_exit(pane)),
            pane.and_then(|pane| s.pending_close_reason(pane)),
        )
    });
    if let Some(exit) = retained {
        process.exit = Some(exit);
        return;
    }
    if let (Some(exit), Some(reason)) = (process.exit.as_mut(), pending) {
        crate::state::close_reason_name(reason).clone_into(&mut exit.reason);
    }
}

/// Ask a Terminal's actor for its typed process facet; `None` when it cannot
/// answer.
async fn query_process_facet(
    terminal: &crate::terminal_actor::TerminalHandle,
) -> Option<phux_core::process::TerminalProcessState> {
    let (reply, reply_rx) = oneshot::channel();
    terminal
        .process
        .send(crate::terminal_actor::ProcessFacetRequest { reply })
        .await
        .ok()?;
    reply_rx.await.ok()
}

/// A resolved, lease-checked local input destination.
#[derive(Debug)]
pub(crate) struct InputDestination {
    pub(crate) pane: phux_core::ids::ResourceId,
    /// The Terminal facet: input atoms only ever go to a Terminal, so the
    /// resolver settles the kind before `action` runs.
    pub(crate) handle: TerminalHandle,
}

/// Resolve and lease-gate a local headless destination, then run `action`
/// while the authority lock remains held.
pub(crate) fn with_route_input_destination<R>(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    action: impl FnOnce(InputDestination) -> R,
) -> Result<R, CommandResult> {
    state.with(|s| {
        // workload-auth §7 step 2: nothing queued before a revocation lands.
        if s.connection_revoked(client_id) {
            return Err(CommandResult::Error {
                code: ErrorCode::PermissionDenied,
                message: "permission denied".to_owned(),
            });
        }
        let local = match s.resolve_resource(terminal_id) {
            Resolved::Remote(_) => {
                return Err(CommandResult::Error {
                    code: ErrorCode::UnsupportedSatelliteRoute,
                    message: format!("ROUTE_INPUT to satellite route unsupported: {terminal_id:?}"),
                });
            }
            Resolved::Unknown => {
                return Err(terminal_not_found(terminal_id));
            }
            Resolved::Local(local) => local,
        };
        let pane = local.id;
        if s.retained_exit(pane).is_some() {
            return Err(exited_input_refusal());
        }
        if s.input_blocked(pane, client_id) {
            debug!(
                ?client_id,
                ?terminal_id,
                "ROUTE_INPUT blocked: another client holds the input lease (ADR-0033)"
            );
            return Err(CommandResult::Error {
                code: ErrorCode::InputLeaseHeld,
                message: "input lease held by another client".to_owned(),
            });
        }
        let handle = local
            .handle
            .terminal()
            .map_err(wrong_resource_kind)?
            .clone();
        Ok(action(InputDestination { pane, handle }))
    })
}

/// Input to a Terminal whose process exited and which is retained read-only
/// (ADR-0124): nothing was written, as for a pane with no PTY (L1 §6.2.1).
fn exited_input_refusal() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InputNotWritten,
        message: "the pane's process exited; it is retained read-only".to_owned(),
    }
}

pub(crate) fn terminal_input_from_event(event: InputEvent) -> Result<TerminalInput, CommandResult> {
    match event {
        InputEvent::Key(event) => Ok(TerminalInput::Key(event)),
        InputEvent::Mouse(event) => Ok(TerminalInput::Mouse(event)),
        InputEvent::Focus(event) => Ok(TerminalInput::Focus(event)),
        InputEvent::Paste(event) => Ok(TerminalInput::Paste(event)),
        _ => Err(CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: "unsupported ROUTE_INPUT event".to_owned(),
        }),
    }
}

pub(crate) fn handle_route_input(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    event: InputEvent,
) -> CommandResult {
    debug!(?client_id, ?terminal_id, "ROUTE_INPUT delivering input");
    let input = match terminal_input_from_event(event) {
        Ok(input) => input,
        Err(result) => return result,
    };
    let send = match with_route_input_destination(state, client_id, terminal_id, |destination| {
        destination.handle.input.try_send(input)
    }) {
        Ok(send) => send,
        Err(result) => return result,
    };
    match send {
        Ok(()) => CommandResult::Ok,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            warn!(
                ?terminal_id,
                "ROUTE_INPUT mailbox full; dropping (fire-and-forget per SPEC §9)"
            );
            CommandResult::Ok
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for ROUTE_INPUT".to_owned(),
        },
    }
}

/// The local Terminal behind `terminal_id` as `(pane, handle)`, or the
/// not-found / wrong-kind refusal.
fn terminal_handle(
    s: &crate::state::ServerState,
    terminal_id: &phux_protocol::ids::ResourceId,
) -> Result<(phux_core::ids::ResourceId, ResourceHandle), CommandResult> {
    let not_found = || terminal_not_found(terminal_id);
    let core = s.terminal_from_wire(terminal_id).ok_or_else(not_found)?;
    let handle = s.resource_handle(core).cloned().ok_or_else(not_found)?;
    handle.terminal().map_err(wrong_resource_kind)?;
    Ok((core, handle))
}

/// A granted input lease (ADR-0033), to broadcast through the pane's actor.
struct LeaseGrant {
    handle: ResourceHandle,
    /// `Acquired` (was free / self) or `Seized` (preempted another).
    action: ControlAction,
    core: phux_core::ids::ResourceId,
    /// `(ttl_ms, expiry generation)`; `None` for no TTL.
    expiry: Option<(u32, u64)>,
}

/// Handle `ACQUIRE_INPUT` (ADR-0033): take the pane's exclusive input lease.
/// `Cooperative` fails with `InputLeaseHeld` against another holder; `Seize`
/// preempts. A grant broadcasts `TerminalControl` and arms `ttl_ms`
/// (`0` = never). Satellite ids never reach here (`route_to_satellite`).
pub(crate) async fn handle_acquire_input(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    mode: InputMode,
    ttl_ms: u32,
) -> CommandResult {
    let granted = state.with_mut(|s| {
        let (core, handle) = terminal_handle(s, terminal_id)?;
        let prior = s.input_lease_holder(core);
        if mode == InputMode::Cooperative
            && let Some(holder) = prior
            && holder != client_id
        {
            return Err(CommandResult::Error {
                code: ErrorCode::InputLeaseHeld,
                message: format!("input lease held by client {}", holder.0),
            });
        }
        s.set_input_lease(core, client_id);
        let action = match prior {
            Some(holder) if holder != client_id => ControlAction::Seized,
            _ => ControlAction::Acquired,
        };
        // Every grant re-arms the TTL, superseding the prior timer.
        let expiry = s
            .refresh_input_lease_expiry(core, ttl_ms != 0)
            .map(|generation| (ttl_ms, generation));
        Ok(LeaseGrant {
            handle,
            action,
            core,
            expiry,
        })
    });
    let grant = match granted {
        Ok(grant) => grant,
        Err(refusal) => return refusal,
    };
    let _ = grant
        .handle
        .control
        .send(ControlRequest::LeaseChanged {
            input_holder: Some(super::wire_client(client_id)),
            action: grant.action,
            actor: Some(super::wire_client(client_id)),
        })
        .await;
    if let Some((ttl_ms, generation)) = grant.expiry {
        spawn_input_lease_expiry(state, grant.core, generation, ttl_ms);
    }
    CommandResult::Ok
}

/// Schedule the timer that expires `terminal`'s input lease after `ttl_ms`
/// (ADR-0033). Any later acquire, release, or disconnect bumps the pane's
/// expiry generation and aborts this task; the generation re-check under the
/// lock covers the window before the abort handle is attached, so exactly
/// one `terminal_control` results.
fn spawn_input_lease_expiry(
    state: &SharedState,
    terminal: phux_core::ids::ResourceId,
    generation: u64,
    ttl_ms: u32,
) {
    let task_state = state.clone();
    #[cfg(test)]
    let counter = state.with(crate::state::ServerState::input_lease_expiry_task_counter);
    let join = tokio::task::spawn_local(async move {
        #[cfg(test)]
        let _live = LiveExpiryTaskGuard::new(counter);
        tokio::time::sleep(std::time::Duration::from_millis(u64::from(ttl_ms))).await;
        let expired = task_state.with_mut(|s| {
            if !s.input_lease_expiry_is_current(terminal, generation) {
                return None;
            }
            // Not `refresh_input_lease_expiry`, which would abort this task.
            s.clear_input_lease_expiry(terminal);
            let holder = s.input_lease_holder(terminal)?;
            s.release_input_lease(terminal, holder);
            s.resource_handle(terminal).cloned()
        });
        let Some(handle) = expired else {
            return;
        };
        let _ = handle
            .control
            .send(ControlRequest::LeaseChanged {
                input_holder: None,
                action: ControlAction::Expired,
                actor: None,
            })
            .await;
    });
    // `false`: `generation` was already superseded, so abort now.
    let abort = join.abort_handle();
    let attached =
        state.with_mut(|s| s.attach_input_lease_expiry_abort(terminal, generation, abort.clone()));
    if !attached {
        abort.abort();
    }
}

/// Counts live lease-expiry tasks for `lease_expiry_task_tests`; an abort
/// drops it too.
#[cfg(test)]
struct LiveExpiryTaskGuard(std::sync::Arc<std::sync::atomic::AtomicUsize>);

#[cfg(test)]
impl LiveExpiryTaskGuard {
    fn new(counter: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(counter)
    }
}

#[cfg(test)]
impl Drop for LiveExpiryTaskGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod lease_expiry_task_tests {
    use super::*;

    /// Superseded expiry timers are aborted, not left sleeping: repeated
    /// re-arms once leaked one live task per grant.
    #[tokio::test(flavor = "current_thread")]
    async fn rearming_a_lease_repeatedly_leaves_at_most_one_live_timer_task() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let core = state.with_mut(|s| s.seed_session("default")).2;
                let counter =
                    state.with(crate::state::ServerState::input_lease_expiry_task_counter);

                for _ in 0..20 {
                    let generation = state
                        .with_mut(|s| s.refresh_input_lease_expiry(core, true))
                        .expect("scheduled");
                    spawn_input_lease_expiry(&state, core, generation, 60_000);
                    // Let the new task start and the aborted one unwind.
                    tokio::task::yield_now().await;
                    tokio::task::yield_now().await;
                }

                assert!(
                    counter.load(std::sync::atomic::Ordering::SeqCst) <= 1,
                    "20 re-arms must not leave more than one live timer task, got {}",
                    counter.load(std::sync::atomic::Ordering::SeqCst)
                );
            })
            .await;
    }
}

/// Handle `RELEASE_INPUT` (ADR-0033): drop the caller's input lease.
/// Idempotent; broadcasts `Released` only when a lease was given up.
pub(crate) async fn handle_release_input(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
) -> CommandResult {
    let released = state.with_mut(|s| {
        let (core, handle) = terminal_handle(s, terminal_id)?;
        let did_release = s.release_input_lease(core, client_id);
        if did_release {
            // The lease's TTL goes with it.
            s.refresh_input_lease_expiry(core, false);
        }
        Ok(did_release.then_some(handle))
    });
    let handle = match released {
        Ok(Some(handle)) => handle,
        Ok(None) => return CommandResult::Ok,
        Err(refusal) => return refusal,
    };
    let _ = handle
        .control
        .send(ControlRequest::LeaseChanged {
            input_holder: None,
            action: ControlAction::Released,
            actor: Some(super::wire_client(client_id)),
        })
        .await;
    CommandResult::Ok
}

/// Handle `SIGNAL_TERMINAL` (ADR-0033): signal the pane's process group via
/// its actor, leaving the pane addressable.
pub(crate) async fn handle_signal_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    signal: TerminalSignal,
    operation_id: Option<phux_protocol::ids::IdempotencyKey>,
    commit: Option<&super::operation_dedupe::OperationClaim>,
) -> CommandResult {
    let resolved = state.with(|s| {
        let resolved = s.resolve_resource(terminal_id).into_owned();
        let holder = match &resolved {
            ResolvedOwned::Local(local) => s.input_lease_holder(local.id).map(super::wire_client),
            ResolvedOwned::Remote(_) | ResolvedOwned::Unknown => None,
        };
        (resolved, holder)
    });
    let (resolved, input_holder) = resolved;
    let (_, handle) = match local_terminal(resolved, terminal_id, "SIGNAL_TERMINAL") {
        Ok(local) => local,
        Err(refusal) => return refusal,
    };
    // ADR-0124: a retained pane's child is already reaped.
    if state.with(|s| {
        s.terminal_from_wire(terminal_id)
            .is_some_and(|pane| s.retained_exit(pane).is_some())
    }) {
        return CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: format!("{terminal_id} exited; its process cannot be signalled"),
        };
    }
    let (reply_tx, reply_rx) = oneshot::channel();
    let request = ControlRequest::Signal {
        signal,
        input_holder,
        by: super::wire_client(client_id),
        operation_id,
        reply: reply_tx,
    };
    if !commit_signal(&handle.control, request, commit).await {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for SIGNAL_TERMINAL".to_owned(),
        };
    }
    let result = actor_reply(reply_rx.await, ErrorCode::InternalError, "SIGNAL_TERMINAL");
    record_signal_failure(commit, &result);
    result
}

/// A command's local Terminal target as `(pane, handle)`, or the refusal for
/// a satellite route, an unknown id, or another resource kind (L1 §1.1).
fn local_terminal(
    resolved: ResolvedOwned,
    terminal_id: &phux_protocol::ids::ResourceId,
    verb: &str,
) -> Result<(phux_core::ids::ResourceId, ResourceHandle), CommandResult> {
    match resolved {
        ResolvedOwned::Local(local) => {
            local.handle.terminal().map_err(wrong_resource_kind)?;
            Ok((local.id, local.handle))
        }
        ResolvedOwned::Remote(_) => Err(CommandResult::Error {
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: format!("{verb} on satellite route unsupported: {terminal_id:?}"),
        }),
        ResolvedOwned::Unknown => Err(terminal_not_found(terminal_id)),
    }
}

/// The command result of a pane actor's `Result<(), String>` reply; a
/// refusal carries `refusal_code`.
fn actor_reply(
    reply: Result<Result<(), String>, oneshot::error::RecvError>,
    refusal_code: ErrorCode,
    verb: &str,
) -> CommandResult {
    match reply {
        Ok(Ok(())) => CommandResult::Ok,
        Ok(Err(message)) => CommandResult::Error {
            code: refusal_code,
            message,
        },
        Err(_) => CommandResult::Error {
            code: ErrorCode::InternalError,
            message: format!("pane actor dropped {verb} reply"),
        },
    }
}

/// A queued signal that failed replaces the success its key was committed
/// with, so a retry answers the failure (L1 §5.1.1).
fn record_signal_failure(
    commit: Option<&super::operation_dedupe::OperationClaim>,
    result: &CommandResult,
) {
    if let (Some(claim), CommandResult::Error { .. }) = (commit, result) {
        claim.bind(&super::operation_dedupe::CachedOutcome::Signal(
            result.clone(),
        ));
    }
}

/// Queue a signal and commit its key at once (L1 §5.1.1): the actor delivers
/// it even if this handler is cancelled before the reply. `false` when the
/// actor is gone.
async fn commit_signal(
    control: &tokio::sync::mpsc::Sender<ControlRequest>,
    request: ControlRequest,
    commit: Option<&super::operation_dedupe::OperationClaim>,
) -> bool {
    if control.send(request).await.is_err() {
        return false;
    }
    if let Some(claim) = commit {
        claim.bind(&super::operation_dedupe::CachedOutcome::Signal(
            CommandResult::Ok,
        ));
    }
    true
}

/// Feed integration-hook lifecycle evidence into a pane (ADR-0085): as a
/// synthesized record on a live `AgentSession` child's stream, else straight
/// into the detector (ADR-0103 decision 6).
pub(crate) async fn handle_report_agent_state(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    reported: phux_protocol::wire::frame::ReportedAgentState,
) -> CommandResult {
    // One borrow, so a `session_end` cannot land between the two reads.
    let (resolved, live_session) = state.with(|server| {
        let resolved = server.resolve_resource(terminal_id).into_owned();
        let live_session = matches!(resolved, ResolvedOwned::Local(_))
            && crate::agent_detect::live_session::server_has_live_session(server, terminal_id);
        (resolved, live_session)
    });
    // The report addresses the instrumented Terminal, never the child stream.
    let (_, handle) = match local_terminal(resolved, terminal_id, "REPORT_AGENT_STATE") {
        Ok(local) => local,
        Err(refusal) => return refusal,
    };
    let (reply, result) = oneshot::channel();
    let request = if live_session {
        ControlRequest::SynthesizeAgentStateRecord {
            state: reported,
            reply,
        }
    } else {
        ControlRequest::ReportAgentState {
            state: reported,
            reply,
        }
    };
    if handle.control.send(request).await.is_err() {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for REPORT_AGENT_STATE".to_owned(),
        };
    }
    actor_reply(
        result.await,
        ErrorCode::InvalidCommand,
        "REPORT_AGENT_STATE",
    )
}

/// Handle `SUBSCRIBE_RESOURCE_EVENTS`: a Terminal-scoped `SUBSCRIBE_EVENTS`
/// with a type filter in the same registry (ADR-0123); a repeat replaces the
/// filter. Registered before `Ok`, so no later event is missed.
pub(crate) fn handle_subscribe_terminal_events(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    event_types: Vec<phux_protocol::wire::frame::ResourceEventType>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> CommandResult {
    let filter = crate::state::EventFilter::of(event_types);
    let registered = state.with_mut(|s| {
        s.terminal_from_wire(terminal_id)?;
        s.subscribe_resource_events(client_id, terminal_id.clone(), filter, out_tx.clone());
        Some(())
    });
    if registered.is_none() {
        return terminal_not_found(terminal_id);
    }
    super::client::ensure_event_pump(state, client_id);
    debug!(
        ?client_id,
        ?terminal_id,
        "SUBSCRIBE_RESOURCE_EVENTS: subscriber registered"
    );
    CommandResult::Ok
}

pub(crate) fn handle_report_asked(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    id: String,
    question: String,
    suggestions: Vec<String>,
    elapsed_seconds: Option<u64>,
) -> CommandResult {
    let resolved = state.with(|s| s.resolve_resource(terminal_id).into_owned());
    let terminal = match local_terminal(resolved, terminal_id, "REPORT_ASKED") {
        Ok((terminal, _)) => terminal,
        Err(refusal) => return refusal,
    };
    if let Some(message) = validate_asked_payload(&id, &question, &suggestions) {
        return CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message,
        };
    }
    let payload = AskedPayload {
        id,
        question,
        suggestions,
        elapsed_seconds,
    };
    let transition = state.with_mut(|s| {
        let transition = s.report_agent_asked(terminal, AskedSource::Hook, payload);
        crate::hub::metadata_mirror::publish_asked_flag(s, terminal_id, s.agent_is_asked(terminal));
        transition
    });
    if let Some(payload) = transition.emit_payload() {
        super::client::broadcast_event(state, Some(terminal_id), &payload.into_event());
    }
    CommandResult::Ok
}

fn validate_asked_payload(id: &str, question: &str, suggestions: &[String]) -> Option<String> {
    const MAX_ID_BYTES: usize = 128;
    const MAX_QUESTION_BYTES: usize = 4096;
    const MAX_SUGGESTIONS: usize = 16;
    const MAX_SUGGESTION_BYTES: usize = 512;

    if question.trim().is_empty() {
        return Some("asked question must not be empty".to_owned());
    }
    if id.len() > MAX_ID_BYTES {
        return Some(format!("asked id exceeds {MAX_ID_BYTES} bytes"));
    }
    if question.len() > MAX_QUESTION_BYTES {
        return Some(format!("asked question exceeds {MAX_QUESTION_BYTES} bytes"));
    }
    if suggestions.len() > MAX_SUGGESTIONS {
        return Some(format!(
            "asked suggestions exceed {MAX_SUGGESTIONS} entries"
        ));
    }
    for suggestion in suggestions {
        if suggestion.trim().is_empty() {
            return Some("asked suggestions must not be empty".to_owned());
        }
        if suggestion.len() > MAX_SUGGESTION_BYTES {
            return Some(format!(
                "asked suggestion exceeds {MAX_SUGGESTION_BYTES} bytes"
            ));
        }
    }
    None
}

/// A `SessionSnapshot` describing a server with no sessions: empty lists,
/// sentinel focus ids. Used by `GET_STATE` when the registry is empty.
pub(crate) const fn empty_session_snapshot() -> phux_protocol::wire::info::SessionSnapshot {
    use phux_protocol::ids::{ResourceId, SessionId, WindowId};
    phux_protocol::wire::info::SessionSnapshot::new(
        SessionId::new(0),
        WindowId::new(0),
        ResourceId::local(0),
    )
}

/// Handle a client's `VIEWPORT_RESIZE` (SPEC §7.1 / §10.5): record the
/// client's viewport, resolve its focused Terminal's geometry across every
/// subscriber under the window-size policy, and resize the pane. Not-found
/// paths are benign races and only log at debug.
pub(crate) fn handle_viewport_resize(
    state: &SharedState,
    client_id: ClientId,
    viewport: &ViewportInfo,
) {
    state.with_mut(|s| {
        let Some(client) = s.attached().get(&client_id) else {
            debug!(
                ?client_id,
                "VIEWPORT_RESIZE from non-attached client; ignoring"
            );
            return;
        };
        let session_id = client.session;
        let Some(session) = s.registry().session(session_id) else {
            debug!(?client_id, "VIEWPORT_RESIZE: client's session vanished");
            return;
        };
        let Some(window_id) = session.active else {
            debug!(?client_id, "VIEWPORT_RESIZE: no active window in session");
            return;
        };
        let Some(window) = s.registry().window(window_id) else {
            return;
        };
        let Some(terminal_id) = window.active else {
            return;
        };
        // Every subscriber's viewport counts, so two clients never thrash
        // the grid; `None` (e.g. `Manual`) leaves the PTY size untouched.
        s.set_client_viewport(client_id, *viewport);
        let Some((cols, rows)) = s.resolve_terminal_geometry(terminal_id, Some(*viewport)) else {
            debug!(
                ?client_id,
                ?terminal_id,
                "VIEWPORT_RESIZE: window-size policy yielded no geometry; PTY size unchanged",
            );
            return;
        };
        if let Some(pane) = s.registry_mut().terminal_mut(terminal_id) {
            pane.dims = (cols, rows);
        }
        // The most recent usable pixel report fixes the advertised cell size.
        let cell_px = s.resolve_terminal_cell_px(terminal_id);
        if let Some(Ok(terminal)) = s.resource_handle(terminal_id).map(ResourceHandle::terminal) {
            try_resize(
                terminal,
                (cols, rows),
                cell_px,
                "VIEWPORT_RESIZE",
                client_id,
            );
        } else {
            debug!(
                ?client_id,
                ?terminal_id,
                "VIEWPORT_RESIZE: no TerminalHandle registered for pane; dropping resize",
            );
        }
    });
}

/// The satellite branch of [`handle_terminal_input`]: gate on the caller's
/// proxy attach and the hub-side lease, then forward the frame with a
/// satellite-local id; warn-drop without a route.
fn relay_satellite_input(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    route: &RelayRoute,
    input: TerminalInput,
    frame_label: &'static str,
) {
    if !state.with(|s| s.has_satellite_proxy_attach(client_id, &route.host, route.id)) {
        warn!(
            ?client_id,
            ?wire_terminal_id,
            frame_label,
            "satellite-routed input requires this client's ATTACH_RESOURCE proxy; dropping",
        );
        return;
    }
    // L1 §9.1: the satellite cannot tell hub consumers apart.
    if state.with(|s| {
        s.satellite_lease_holder(&route.host, route.id)
            .is_some_and(|holder| holder != client_id)
    }) {
        trace!(
            ?client_id,
            ?wire_terminal_id,
            frame_label,
            "satellite-routed input dropped: another hub consumer holds the input lease",
        );
        return;
    }
    let relayed =
        relay_satellite_frame(
            client_id,
            wire_terminal_id,
            route,
            frame_label,
            |id| match input {
                TerminalInput::Key(event) => FrameKind::InputKey {
                    terminal_id: id,
                    event,
                },
                TerminalInput::Mouse(event) => FrameKind::InputMouse {
                    terminal_id: id,
                    event,
                },
                TerminalInput::Focus(event) => FrameKind::InputFocus {
                    terminal_id: id,
                    event,
                },
                TerminalInput::Paste(event) => FrameKind::InputPaste {
                    terminal_id: id,
                    event,
                },
            },
        );
    if !relayed {
        warn!(
            ?client_id,
            ?wire_terminal_id,
            frame_label,
            "input frame carried a SATELLITE ResourceId on a non-federation-hub server; dropping",
        );
    }
}

/// Apply subscription, lease, and activity gates for attached local input,
/// then run `action` while the authority lock is held. Callers relay
/// satellite ids before reaching here.
pub(crate) fn with_attached_input_destination<R>(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    frame_label: &'static str,
    action: impl FnOnce(InputDestination) -> R,
) -> Option<R> {
    state.with_mut(|s| {
        // workload-auth §7 step 2.
        if s.connection_revoked(client_id) {
            trace!(
                ?client_id,
                frame_label, "input from a revoked connection dropped"
            );
            return None;
        }
        let local = match s.resolve_resource(wire_terminal_id).into_owned() {
            ResolvedOwned::Local(local) => local,
            ResolvedOwned::Remote(_) | ResolvedOwned::Unknown => {
                warn!(
                    ?client_id,
                    ?wire_terminal_id,
                    frame_label,
                    "input frame for unknown pane; dropping"
                );
                return None;
            }
        };
        let pane = local.id;
        if s.retained_exit(pane).is_some() {
            trace!(
                ?client_id,
                ?wire_terminal_id,
                frame_label,
                "input dropped: the pane's process exited (retained read-only)"
            );
            return None;
        }
        if !s.subscribers_for_terminal(pane).contains(&client_id) {
            warn!(
                ?client_id,
                ?wire_terminal_id,
                frame_label,
                "client not subscribed to pane (no ATTACH or ATTACH_RESOURCE); dropping input"
            );
            return None;
        }
        if s.input_blocked(pane, client_id) {
            trace!(
                ?client_id,
                ?wire_terminal_id,
                frame_label,
                "input dropped: another client holds the input lease"
            );
            return None;
        }
        let touched_session = s.attached().get(&client_id).map(|c| c.session);
        if let Some(session) = touched_session {
            s.touch_session(session);
        }
        let handle = match local.handle.terminal() {
            Ok(terminal) => terminal.clone(),
            Err(error) => {
                warn!(?client_id, ?wire_terminal_id, frame_label, %error, "dropping input");
                // input.md §9: no reply frame, so an uncorrelated ERROR.
                send_wrong_kind_error(s, client_id, error);
                return None;
            }
        };
        Some(action(InputDestination { pane, handle }))
    })
}

pub(crate) fn handle_terminal_input(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    input: TerminalInput,
    frame_label: &'static str,
) {
    if let ResolvedOwned::Remote(route) =
        state.with(|s| s.resolve_resource(wire_terminal_id).into_owned())
    {
        relay_satellite_input(
            state,
            client_id,
            wire_terminal_id,
            &route,
            input,
            frame_label,
        );
        return;
    }
    // A routed focus-gained fires `focus-changed` (tui.md §9) after the lock.
    let is_focus_gained = matches!(
        input,
        TerminalInput::Focus(phux_protocol::input::focus::FocusEvent::Gained)
    );
    let Some(routed) = with_attached_input_destination(
        state,
        client_id,
        wire_terminal_id,
        frame_label,
        |destination| match destination.handle.input.try_send(input) {
            Ok(()) => {
                trace!(
                    ?client_id,
                    ?wire_terminal_id,
                    frame_label,
                    "input routed to TerminalActor"
                );
                true
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    ?client_id,
                    ?wire_terminal_id,
                    frame_label,
                    "pane input mailbox full; dropping (fire-and-forget per SPEC §9)"
                );
                false
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                debug!(
                    ?client_id,
                    ?wire_terminal_id,
                    frame_label,
                    "pane actor gone; dropping input"
                );
                false
            }
        },
    ) else {
        return;
    };
    if routed && is_focus_gained {
        crate::hooks::fire_hook(
            state,
            crate::hooks::HookEvent::focus_changed(wire_terminal_id, client_id),
        );
    }
}

/// Discard a client terminal-engine reply (input.md §6): the canonical
/// terminal already answered the query once, and a second answer reaches
/// the child as typed input.
pub(crate) fn handle_terminal_reply(
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    bytes: &[u8],
) {
    trace!(
        ?client_id,
        ?wire_terminal_id,
        bytes = bytes.len(),
        "terminal reply discarded: the canonical terminal answers queries",
    );
}

/// Route an inbound `FRAME_ACK` to the pane's actor for per-consumer cache
/// eviction (ADR-0018). Acks are hints, so an unknown pane, a
/// non-subscriber, or a full mailbox just drops it; satellite acks relay.
pub(crate) fn handle_frame_ack(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    seq: u64,
) {
    state.with_mut(|s| {
        let local = match s.resolve_resource(wire_terminal_id).into_owned() {
            ResolvedOwned::Remote(route) => {
                relay_frame_ack(
                    client_id,
                    wire_terminal_id,
                    &route,
                    stream_id,
                    bootstrap_id,
                    seq,
                );
                return;
            }
            ResolvedOwned::Unknown => {
                warn!(
                    ?client_id,
                    ?wire_terminal_id,
                    seq,
                    "FRAME_ACK for unknown pane; dropping",
                );
                return;
            }
            ResolvedOwned::Local(local) => local,
        };
        if !frame_ack_subscribed(s, client_id, wire_terminal_id, local.id, seq) {
            return;
        }
        let dispatched = local.handle.consumer_ack.try_send(ConsumerAckRequest {
            client_id: super::wire_client(client_id),
            stream_id,
            bootstrap_id,
            seq,
        });
        log_frame_ack_dispatch(&dispatched, client_id, wire_terminal_id, seq);
    });
}

/// Forward a satellite-routed `FRAME_ACK` over the hub link, warn-dropping it
/// on a server that is not a federation hub for that host.
fn relay_frame_ack(
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    route: &RelayRoute,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    seq: u64,
) {
    let relayed = relay_satellite_frame(client_id, wire_terminal_id, route, "FRAME_ACK", |id| {
        FrameKind::FrameAck {
            terminal_id: id,
            stream_id,
            bootstrap_id,
            seq,
        }
    });
    if !relayed {
        warn!(
            ?client_id,
            ?wire_terminal_id,
            seq,
            "FRAME_ACK carried a SATELLITE ResourceId on a non-federation-hub server; dropping",
        );
    }
}

/// Whether `client_id` subscribes to `pane` and so may ack it.
fn frame_ack_subscribed(
    s: &crate::state::ServerState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    pane: phux_core::ids::ResourceId,
    seq: u64,
) -> bool {
    if s.subscribers_for_terminal(pane).contains(&client_id) {
        return true;
    }
    warn!(
        ?client_id,
        ?wire_terminal_id,
        seq,
        "FRAME_ACK from client not subscribed to pane; dropping",
    );
    false
}

/// Log the outcome of routing one `FRAME_ACK` to its pane actor.
fn log_frame_ack_dispatch(
    dispatched: &Result<(), tokio::sync::mpsc::error::TrySendError<ConsumerAckRequest>>,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    seq: u64,
) {
    match dispatched {
        Ok(()) => {
            trace!(
                ?client_id,
                ?wire_terminal_id,
                seq,
                "FRAME_ACK routed to TerminalActor"
            );
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            trace!(
                ?client_id,
                ?wire_terminal_id,
                seq,
                "FRAME_ACK mailbox full; dropping (ADR-0018: next ack catches up)",
            );
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            debug!(
                ?client_id,
                ?wire_terminal_id,
                seq,
                "FRAME_ACK: pane actor gone; dropping",
            );
        }
    }
}

#[cfg(test)]
mod hub_detach_fence_tests {
    use super::*;
    use crate::hub::relay::{HubRelays, RelayHandle, RelaySession};

    #[tokio::test]
    async fn cancellation_interrupts_a_blocked_satellite_reply() {
        let state = SharedState::new();
        let host = phux_protocol::ids::SatelliteHost::from("sat");
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(1);
        out_tx
            .send(Outbound::Frame(FrameKind::Pong { nonce: 1 }))
            .await
            .expect("fill consumer mailbox");
        let retained_sender = out_tx.clone();
        let token = CancellationToken::new();
        let command = Command::DetachResource {
            terminal_id: phux_protocol::ids::ResourceId::local(7),
        };
        let reply = reply_satellite_command(
            &state,
            ClientId(17),
            9,
            &host,
            &command,
            &out_tx,
            CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                message: "offline".to_owned(),
            },
            &token,
        );
        tokio::pin!(reply);
        assert!(futures_util::poll!(&mut reply).is_pending());

        token.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), reply)
            .await
            .expect("cancellation interrupts the full-mailbox reply");
        assert!(
            !retained_sender.is_closed(),
            "extra production-equivalent sender remains owned"
        );
        assert!(matches!(
            out_rx.try_recv(),
            Ok(Outbound::Frame(FrameKind::Pong { nonce: 1 }))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_hub_detach_preserves_resumed_generation_with_shared_observer() {
        use crate::hub::relay::{ProxySubscription, RELAY_COMMAND_TIMEOUT, RelayRequest};
        use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
        let state = SharedState::new();
        let host = phux_protocol::ids::SatelliteHost::from("sat");
        let (handle, mut mailbox) = RelayHandle::new(host.clone());
        let relays = HubRelays::default();
        relays.insert(handle);
        state.with_mut(|s| {
            s.set_hub_relays(relays);
            s.register_satellite_proxy_attach(ClientId(1), host.clone(), 7);
        });
        let mut session = RelaySession::new(host.clone(), BootstrapLimits::default());
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
        let (observer_tx, mut observer_rx) = tokio::sync::mpsc::channel(8);
        for (client, out_tx) in [(ClientId(1), out_tx.clone()), (ClientId(2), observer_tx)] {
            session
                .handle_request_checked(RelayRequest::Subscribe {
                    subscription: ProxySubscription {
                        terminal: 7,
                        client,
                        out_tx,
                        consumer_cancel: CancellationToken::new(),
                        seq: 0,
                        awaits_snapshot: false,
                        bootstrap_profile: None,
                        bootstrap_limits: None,
                    },
                    forward: FrameKind::SubscribeEvents {
                        terminal: Some(ResourceId::local(7)),
                        after_seq: None,
                    },
                })
                .unwrap();
        }
        let connection_token = CancellationToken::new();
        let detach = handle_satellite_command(
            &state,
            ClientId(1),
            42,
            &host,
            Command::DetachResource {
                terminal_id: ResourceId::local(7),
            },
            &out_tx,
            BootstrapProfile::SynthesizedVtRaw,
            BootstrapLimits::default(),
            &connection_token,
        );
        tokio::pin!(detach);
        assert!(futures_util::poll!(&mut detach).is_pending());
        tokio::time::advance(RELAY_COMMAND_TIMEOUT).await;
        detach.await;
        assert!(matches!(
            out_rx.try_recv(),
            Ok(Outbound::Frame(FrameKind::CommandResult {
                request_id: 42,
                result: CommandResult::Error {
                    code: ErrorCode::SatelliteUnreachable,
                    ..
                }
            }))
        ));
        assert!(state.with(|s| s.has_satellite_proxy_attach(ClientId(1), &host, 7)));
        // The delayed link processes the withdrawal only after the refusal.
        assert!(
            session
                .handle_unsubscribe(mailbox.unsubscribes.try_recv().unwrap())
                .is_empty()
        );
        let frame = FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq: 12,
            bytes: bytes::Bytes::from_static(b"resumed generation"),
        };
        let mut encoded = bytes::BytesMut::new();
        frame.encode(&mut encoded);
        session.handle_inbound(&encoded).unwrap();
        assert!(matches!(
            observer_rx.try_recv(),
            Ok(Outbound::Frame(FrameKind::ResourceOutput { seq: 12, .. }))
        ));
        assert!(
            matches!(
                out_rx.try_recv(),
                Ok(Outbound::Frame(FrameKind::ResourceOutput { seq: 12, .. }))
            ),
            "ordinary detach refusal must preserve the resumed client's output, not just its input entitlement"
        );
    }

    #[tokio::test]
    async fn satellite_detach_reply_waits_for_proxy_withdrawal() {
        let state = SharedState::new();
        let host = phux_protocol::ids::SatelliteHost::from("sat");
        let (handle, mut mailbox) = RelayHandle::new(host.clone());
        let relays = HubRelays::default();
        relays.insert(handle);
        state.with_mut(|s| s.set_hub_relays(relays));
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
        let connection_token = CancellationToken::new();
        let detach = handle_satellite_command(
            &state,
            ClientId(1),
            42,
            &host,
            Command::DetachResource {
                terminal_id: phux_protocol::ids::ResourceId::local(7),
            },
            &out_tx,
            BootstrapProfile::SynthesizedVtRaw,
            BootstrapLimits::default(),
            &connection_token,
        );
        tokio::pin!(detach);
        assert!(
            futures_util::poll!(&mut detach).is_pending(),
            "DETACH_RESOURCE replied before the link applied its proxy withdrawal"
        );
        assert!(
            out_rx.try_recv().is_err(),
            "no correlated success before withdrawal"
        );
        let withdrawal = mailbox
            .unsubscribes
            .try_recv()
            .expect("undroppable withdrawal");
        let mut session = RelaySession::new(host.clone(), BootstrapLimits::default());
        session.handle_unsubscribe(withdrawal);
        detach.await;
        assert!(matches!(
            out_rx.recv().await,
            Some(Outbound::Frame(FrameKind::CommandResult {
                request_id: 42,
                result: CommandResult::Ok
            }))
        ));
    }

    #[tokio::test]
    async fn refused_hub_detach_preserves_the_newer_proxy_input_registration() {
        let state = SharedState::new();
        let host = phux_protocol::ids::SatelliteHost::from("sat");
        let (handle, mut mailbox) = RelayHandle::new(host.clone());
        state.with_mut(|s| s.register_satellite_proxy_attach(ClientId(1), host.clone(), 7));
        let terminal = phux_protocol::ids::ResourceId::local(7);
        let detach = resolve_hub_detach_terminal(&state, &handle, ClientId(1), &host, &terminal);
        tokio::pin!(detach);
        assert!(futures_util::poll!(&mut detach).is_pending());
        let crate::hub::relay::Unsubscribe::Terminal {
            reply: Some(reply), ..
        } = mailbox.unsubscribes.try_recv().unwrap()
        else {
            panic!("withdrawal receipt");
        };
        reply.apply(|| {
            (
                CommandResult::Error {
                    code: ErrorCode::InvalidCommand,
                    message: "superseded".to_owned(),
                },
                Vec::new(),
            )
        });
        assert!(matches!(detach.await, CommandResult::Error { .. }));
        assert!(state.with(|s| s.has_satellite_proxy_attach(ClientId(1), &host, 7)));
    }
}

#[cfg(test)]
mod relay_satellite_attach_role_tests {
    use super::*;
    use crate::hub::relay::{HubRelays, RelayHandle, RelayMailbox, RelayRequest};
    use phux_protocol::ids::{ResourceId, SatelliteHost};

    struct Fixture {
        state: SharedState,
        host: SatelliteHost,
        client_id: ClientId,
        handle: RelayHandle,
        mailbox: RelayMailbox,
        out_tx: tokio::sync::mpsc::Sender<Outbound>,
    }

    impl Fixture {
        fn new() -> Self {
            let state = SharedState::new();
            let host = SatelliteHost::from("sat");
            let (handle, mailbox) = RelayHandle::new(host.clone());
            let relays = HubRelays::default();
            relays.insert(handle.clone());
            let client_id = ClientId(1);
            let token = CancellationToken::new();
            state.with_mut(|s| {
                s.set_hub_relays(relays);
                s.set_client_connection_cancellation(client_id, token);
            });
            let (out_tx, _out_rx) = tokio::sync::mpsc::channel(8);
            Self {
                state,
                host,
                client_id,
                handle,
                mailbox,
                out_tx,
            }
        }

        fn wire(&self, terminal: u32) -> ResourceId {
            ResourceId::satellite(self.host.clone(), terminal)
        }

        async fn refuse_attach(&mut self, terminal: u32, role: RolePolicy) -> CommandResult {
            self.settle_attach(
                terminal,
                role,
                CommandResult::Error {
                    code: ErrorCode::TerminalNotFound,
                    message: "no such terminal".to_owned(),
                },
            )
            .await
        }

        async fn settle_attach(
            &mut self,
            terminal: u32,
            role: RolePolicy,
            result: CommandResult,
        ) -> CommandResult {
            let local = ResourceId::local(terminal);
            let command = Command::AttachResource {
                terminal_id: local.clone(),
                role_policy: Some(role),
            };
            let target = SatelliteLeaseTarget::new(&self.state, &self.host, self.client_id, &local);
            let attach = relay_satellite_attach(
                &target,
                &self.handle,
                &command,
                role,
                &self.out_tx,
                BootstrapProfile::SynthesizedVtRaw,
                BootstrapLimits::default(),
            );
            tokio::pin!(attach);
            assert!(
                futures_util::poll!(&mut attach).is_pending(),
                "attach must wait on the satellite reply"
            );
            let RelayRequest::Command { reply, .. } = self
                .mailbox
                .requests
                .try_recv()
                .expect("attach must reach the link")
            else {
                panic!("expected a relayed ATTACH_RESOURCE");
            };
            reply.send(result).expect("attach is waiting");
            attach.await
        }
    }

    /// A refused first VIEWER attach must not leave a hub-side tombstone on
    /// a satellite id the hub never subscribed.
    #[tokio::test]
    async fn a_refused_first_satellite_viewer_attach_does_not_leave_a_tombstone() {
        let mut fixture = Fixture::new();
        let result = fixture.refuse_attach(1, RolePolicy::VIEWER).await;
        assert!(matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                ..
            }
        ));
        let wire = fixture.wire(1);
        assert!(
            !fixture
                .state
                .with(|s| s.is_viewer(fixture.client_id, &wire))
        );
        assert!(fixture.state.with(|s| s.terminal_viewers(&wire).is_empty()));
    }

    /// A refused narrowing keeps a prior mark and a proxy-attached mark.
    #[tokio::test]
    async fn a_refused_satellite_viewer_attach_keeps_a_prior_mark_or_proxy() {
        let mut fixture = Fixture::new();
        let prior = fixture.wire(3);
        fixture.state.with_mut(|s| {
            s.set_viewer_mark(fixture.client_id, &prior, true);
        });
        let _ = fixture.refuse_attach(3, RolePolicy::VIEWER).await;
        assert!(
            fixture
                .state
                .with(|s| s.is_viewer(fixture.client_id, &prior)),
            "a refused re-attach must not shed a prior viewer tombstone"
        );

        let attached = fixture.wire(9);
        fixture.state.with_mut(|s| {
            s.register_satellite_proxy_attach(fixture.client_id, fixture.host.clone(), 9);
        });
        let _ = fixture.refuse_attach(9, RolePolicy::VIEWER).await;
        assert!(
            fixture
                .state
                .with(|s| s.is_viewer(fixture.client_id, &attached)),
            "a refused narrowing of an existing proxy attach keeps the mark"
        );
    }

    #[tokio::test]
    async fn a_successful_satellite_viewer_attach_keeps_the_mark() {
        let mut fixture = Fixture::new();
        let result = fixture
            .settle_attach(7, RolePolicy::VIEWER, CommandResult::Ok)
            .await;
        assert!(matches!(result, CommandResult::Ok));
        let wire = fixture.wire(7);
        assert!(
            fixture
                .state
                .with(|s| s.is_viewer(fixture.client_id, &wire))
        );
        assert!(fixture.state.with(|s| s.has_satellite_proxy_attach(
            fixture.client_id,
            &fixture.host,
            7
        )));
    }
}

#[cfg(test)]
mod host_inventory_tests {
    use phux_protocol::ids::{ResourceId, ResourceKind, SatelliteHost};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
    use phux_protocol::{SessionId, WindowId};

    use super::satellite_host_inventory;

    /// A satellite snapshot with two sessions: `build` (two windows, three
    /// terminals, one agent-session child) and `logs` (one window whose
    /// remembered focus is unset).
    fn satellite_snapshot() -> SessionSnapshot {
        let build = SessionId::new(1);
        let logs = SessionId::new(2);
        SessionSnapshot::new(build, WindowId::new(10), ResourceId::local(100))
            .with_sessions(vec![
                SessionInfo::new(build, "build")
                    .with_window_count(2)
                    .with_attached_client_count(1)
                    .with_active_window(Some(WindowId::new(11))),
                SessionInfo::new(logs, "logs").with_window_count(1),
            ])
            .with_windows(vec![
                WindowInfo::new(WindowId::new(10), build, "a")
                    .with_active_resource(Some(ResourceId::local(100))),
                WindowInfo::new(WindowId::new(11), build, "b")
                    .with_active_resource(Some(ResourceId::local(102))),
                WindowInfo::new(WindowId::new(20), logs, "tail"),
            ])
            .with_resources(vec![
                ResourceInfo::new(ResourceId::local(100), WindowId::new(10), 80, 24),
                ResourceInfo::new(ResourceId::local(101), WindowId::new(10), 80, 24),
                ResourceInfo::new(ResourceId::local(102), WindowId::new(11), 80, 24),
                ResourceInfo::resource(ResourceId::local(103), ResourceKind::AgentSession)
                    .with_parent(Some(ResourceId::local(100))),
                ResourceInfo::new(ResourceId::local(200), WindowId::new(20), 80, 24),
            ])
    }

    /// Sessions keep their satellite-local ids and names; counts come from
    /// the satellite's own windows; the active pane is re-tagged for the hub.
    #[test]
    fn inventory_lists_satellite_sessions_without_renumbering() {
        let host = SatelliteHost::from("edge");
        let row = satellite_host_inventory(&host, &satellite_snapshot());

        assert!(row.is_reachable());
        assert_eq!(row.host, host);
        let build = &row.sessions[0];
        assert_eq!(build.id, SessionId::new(1), "satellite-local id, verbatim");
        assert_eq!(build.name, "build");
        assert_eq!(build.window_count, 2);
        assert_eq!(build.pane_count, 3, "agent sessions are not panes");
        assert_eq!(build.attached_client_count, 1);
        assert_eq!(
            build.active_resource,
            Some(ResourceId::satellite(host.clone(), 102)),
            "the active window's active pane, re-tagged"
        );
        let logs = &row.sessions[1];
        assert_eq!(logs.pane_count, 1);
        assert_eq!(
            logs.active_resource,
            Some(ResourceId::satellite(host, 200)),
            "no remembered focus falls back to the first window's first terminal"
        );
    }

    /// A satellite that reports a Satellite-tagged active pane (chaining)
    /// yields no active pane rather than an unroutable id.
    #[test]
    fn chained_active_pane_is_dropped() {
        let session = SessionId::new(1);
        let chained = ResourceId::satellite(SatelliteHost::from("deeper"), 5);
        let sat = SessionSnapshot::new(session, WindowId::new(1), chained.clone())
            .with_sessions(vec![SessionInfo::new(session, "s").with_window_count(1)])
            .with_windows(vec![
                WindowInfo::new(WindowId::new(1), session, "w").with_active_resource(Some(chained)),
            ]);
        let row = satellite_host_inventory(&SatelliteHost::from("edge"), &sat);
        assert_eq!(row.sessions[0].active_resource, None);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, reason = "tests")]
mod kill_merge_tests {
    use super::*;
    use crate::hub::relay::{HubRelays, RelayHandle, RelayMailbox, RelayRequest};
    use crate::runtime::keyed_ops::{KeyedAdmission, admit};
    use phux_protocol::ids::{IdempotencyKey, ResourceId, SatelliteHost};
    use phux_protocol::wire::frame::CommandValue;

    /// A hub whose satellite `up` has a live link, answered by the test, and
    /// whose satellite `down` has none.
    fn hub() -> (SharedState, ClientId, RelayMailbox) {
        let state = SharedState::new();
        let client = state.with_mut(crate::state::ServerState::new_client_id);
        let relays = HubRelays::default();
        let (up, up_mailbox) = RelayHandle::new(SatelliteHost::new("up"));
        let (down, down_mailbox) = RelayHandle::new(SatelliteHost::new("down"));
        drop(down_mailbox);
        relays.insert(up);
        relays.insert(down);
        state.with_mut(|s| s.set_hub_relays(relays));
        (state, client, up_mailbox)
    }

    fn outcomes(result: CommandResult) -> serde_json::Value {
        let CommandResult::OkWith(CommandValue::Json(document)) = result else {
            panic!("a keyed batch answers per-id outcomes, got {result:?}");
        };
        serde_json::from_str(&document).expect("json")
    }

    /// L1 §5.2: a keyed batch split across hosts awaits every host, and is
    /// keyed at every hop: the satellite's own per-id outcome is merged, so a
    /// stale satellite id is `not_found`, not `killed`.
    #[tokio::test(flavor = "current_thread")]
    async fn kill_resources_across_hosts_merges_results_instead_of_fire_and_forget() {
        let (state, client, mut up) = hub();
        let satellite = tokio::spawn(async move {
            let Some(RelayRequest::Keyed { command, reply, .. }) = up.requests.recv().await else {
                panic!("the keyed batch reaches the satellite's link");
            };
            let _ = reply.send(CommandResult::OkWith(CommandValue::Json(
                r#"{"schema_version":1,"killed":["@3"],"not_found":["@5"],"failed":[]}"#.to_owned(),
            )));
            command
        });

        let key = IdempotencyKey::new([9; 16]);
        let ids = [
            ResourceId::local(77),
            ResourceId::satellite("up", 3),
            ResourceId::satellite("up", 5),
            ResourceId::satellite("down", 4),
        ];
        let result = handle_kill_terminals(&state, client, &ids, key).await;
        assert_eq!(
            satellite.await.expect("satellite task"),
            Command::KillResources {
                ids: vec![ResourceId::local(3), ResourceId::local(5)],
                operation_id: key,
            },
            "each host gets its own ids, under the batch's key"
        );
        let document = outcomes(result);
        assert_eq!(document["killed"], serde_json::json!(["up/@3"]));
        assert_eq!(
            document["not_found"],
            serde_json::json!(["@77", "up/@5"]),
            "a stale satellite id is not reported killed"
        );
        assert_eq!(document["failed"][0]["id"], "down/@4");
        assert_eq!(
            document["failed"][0]["code"],
            ErrorCode::SatelliteUnreachable.as_wire()
        );

        assert_eq!(
            outcomes(
                handle_kill_terminals(
                    &state,
                    client,
                    &[ResourceId::local(78)],
                    IdempotencyKey::new([10; 16]),
                )
                .await
            )["not_found"],
            serde_json::json!(["@78"]),
            "what a satellite answers a keyed batch naming a stale id"
        );
    }

    /// An unkeyed batch keeps the reply every older client understands: it
    /// awaits every host, and answers `OK` even when one could not be reached.
    #[tokio::test(flavor = "current_thread")]
    async fn an_unkeyed_cross_host_batch_keeps_its_ok_reply() {
        let (state, client, mut up) = hub();
        let satellite = tokio::spawn(async move {
            let Some(RelayRequest::Command { command, reply, .. }) = up.requests.recv().await
            else {
                panic!("the unkeyed batch relays as a plain command");
            };
            let _ = reply.send(CommandResult::Ok);
            command
        });
        let ids = [
            ResourceId::satellite("up", 3),
            ResourceId::satellite("down", 4),
        ];
        assert_eq!(
            handle_kill_terminals(&state, client, &ids, None).await,
            CommandResult::Ok
        );
        assert_eq!(
            satellite.await.expect("satellite task"),
            Command::KillResources {
                ids: vec![ResourceId::local(3)],
                operation_id: None,
            }
        );
    }

    /// A keyed signal the first attempt owns, committed on its actor queue.
    struct CommittedSignal {
        state: SharedState,
        command: Command,
        claim: crate::runtime::operation_dedupe::OperationClaim,
        actor: tokio::sync::mpsc::Receiver<ControlRequest>,
        reply_rx: oneshot::Receiver<Result<(), String>>,
    }

    async fn committed_signal() -> CommittedSignal {
        let state = SharedState::new();
        let key = IdempotencyKey::new([4; 16]);
        let command = Command::SignalTerminal {
            terminal_id: ResourceId::local(3),
            signal: TerminalSignal::Interrupt,
            operation_id: key,
        };
        let KeyedAdmission::Owner(claim) = admit(&state, &command).await else {
            panic!("the first attempt owns the key");
        };
        let (control, actor) = tokio::sync::mpsc::channel(4);
        let (reply, reply_rx) = oneshot::channel();
        let request = ControlRequest::Signal {
            signal: TerminalSignal::Interrupt,
            input_holder: None,
            by: phux_protocol::ClientId::new(1),
            operation_id: key,
            reply,
        };
        assert!(commit_signal(&control, request, Some(&claim)).await);
        CommittedSignal {
            state,
            command,
            claim,
            actor,
            reply_rx,
        }
    }

    /// L1 §5.1.1: a connection cancelled before the reply does not release
    /// the key, so the retry answers the first result and nothing is
    /// delivered twice.
    #[tokio::test(flavor = "current_thread")]
    async fn a_keyed_signal_cancelled_before_its_reply_is_delivered_once() {
        let CommittedSignal {
            state,
            command,
            claim,
            mut actor,
            reply_rx,
        } = committed_signal().await;
        drop(reply_rx);
        drop(claim);
        assert!(
            matches!(
                admit(&state, &command).await,
                KeyedAdmission::Answer(CommandResult::Ok)
            ),
            "the retry answers the committed signal instead of sending it again"
        );
        assert!(actor.try_recv().is_ok(), "the first attempt was queued");
        assert!(actor.try_recv().is_err(), "and nothing else was");
    }

    /// A committed signal whose delivery failed answers that failure on
    /// retry, never the committed `OK`.
    #[tokio::test(flavor = "current_thread")]
    async fn a_keyed_signal_that_was_not_delivered_answers_its_failure_on_retry() {
        let CommittedSignal {
            state,
            command,
            claim,
            ..
        } = committed_signal().await;
        let failure = CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "no PTY child to signal".to_owned(),
        };
        record_signal_failure(Some(&claim), &failure);
        crate::runtime::keyed_ops::settle(Some(claim), &failure);
        assert!(
            matches!(
                admit(&state, &command).await,
                KeyedAdmission::Answer(CommandResult::Error {
                    code: ErrorCode::InternalError,
                    ..
                })
            ),
            "the retry answers the failure, not a false OK"
        );
    }
}
