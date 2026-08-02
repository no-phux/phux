//! Submodule for runtime internals.

use phux_protocol::caps::ClientCapabilities;
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ControlAction, ErrorCode, FrameKind,
    InputMode, StateScope, TerminalLifecycle, TerminalSignal, ViewportInfo,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use super::input_lane::InputLaneHandle;
use super::{
    AttachPrepared, spawn_agent_state_drain, spawn_pane_event_drain, spawn_terminal_exit_watcher,
};
use crate::agent_asked::{AskedPayload, AskedSource};
use crate::state::{ClientId, Outbound, SharedState, TerminalInput};
use crate::terminal_actor::{
    ConsumerAckRequest, ControlRequest, ResizeRequest, ScreenRequest, TerminalActor, TerminalHandle,
};

pub(crate) fn seed_session_with_actor(
    state: &SharedState,
    name: &str,
    history_limit: u32,
    root_token: &CancellationToken,
) -> Result<phux_core::ids::TerminalId, crate::terminal_actor::TerminalActorError> {
    seed_session_with_actor_and_metadata(state, name, history_limit, root_token, None)
}

fn seed_session_with_actor_and_metadata(
    state: &SharedState,
    name: &str,
    history_limit: u32,
    root_token: &CancellationToken,
    agent_session: Option<Vec<u8>>,
) -> Result<phux_core::ids::TerminalId, crate::terminal_actor::TerminalActorError> {
    use phux_core::ids::TerminalId;
    let terminal: TerminalId = state.with_mut(|s| {
        let terminal = s.seed_session(name).2;
        if let Some(value) = agent_session {
            let wire = s.intern_terminal_wire(terminal);
            s.metadata_set(
                &phux_protocol::wire::frame::Scope::Terminal(wire),
                phux_protocol::wire::frame::TERMINAL_AGENT_SESSION_KEY,
                value,
            );
        }
        terminal
    });
    // Default 80x24 — same as `phux_core::Pane::new`'s default dims.
    // Real resize wiring lands with VIEWPORT_RESIZE (phux-4hp).
    let terminal_token = root_token.child_token();
    let bundle = match TerminalActor::build_with_token(
        80,
        24,
        None,
        history_limit,
        terminal_token.clone(),
    ) {
        Ok(bundle) => bundle,
        Err(err) => {
            state.with_mut(|s| s.reap_terminal(terminal));
            return Err(err);
        }
    };
    let crate::terminal_actor::TerminalActorBundle {
        actor,
        handle,
        exit_notify,
        ..
    } = bundle;
    let wire_terminal_id = state.with_mut(|s| {
        let _ = s.spawn_terminal_actor(terminal, handle, terminal_token, actor.run());
        s.intern_terminal_wire(terminal)
    });
    spawn_terminal_exit_watcher(state.clone(), terminal, exit_notify, root_token.clone());
    // docs/consumers/tui.md §9 (phux-r82.1): the pane's actor is live.
    crate::hooks::fire_hook(
        state,
        crate::hooks::HookEvent::after_new_pane(&wire_terminal_id, Some(name)),
    );
    Ok(terminal)
}

/// Seed `(session, window, pane)` and spawn a **PTY-backed**
/// `TerminalActor` running `cmd`. Sibling of the private
/// `seed_session_with_actor` helper for the real server path
/// (`phux-byc.5`).
///
/// Call sites:
///
/// * The `phux server` binary entry point, via
///   [`super::ServerConfig::seed_with_pty`] (with
///   [`super::ServerConfig::seed_command`]
///   left `None` to fall back to
///   [`crate::terminal_actor::default_shell_command`] — the resolved
///   default shell: `defaults.shell`, then `$SHELL`, then `/bin/sh`
///   per the byc.5 convention).
/// * Anything embedding `phux-server` and wanting a specific command
///   (e.g. an integration test driving a known fixture; see the
///   `input_dispatch.rs` test, which seeds with `cat` to get
///   deterministic echo).
pub fn seed_session_with_pty(
    state: &SharedState,
    name: &str,
    cmd: portable_pty::CommandBuilder,
    history_limit: u32,
    root_token: &CancellationToken,
) -> Result<phux_core::ids::TerminalId, crate::terminal_actor::TerminalActorError> {
    seed_session_with_pty_and_colors(state, name, cmd, history_limit, root_token, None)
}

/// Palette-seeded variant used when a client's HELLO creates the session.
pub fn seed_session_with_pty_and_colors(
    state: &SharedState,
    name: &str,
    cmd: portable_pty::CommandBuilder,
    history_limit: u32,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Result<phux_core::ids::TerminalId, crate::terminal_actor::TerminalActorError> {
    seed_session_with_pty_and_colors_and_metadata(
        state,
        name,
        cmd,
        history_limit,
        root_token,
        default_colors,
        None,
    )
}

fn seed_session_with_pty_and_colors_and_metadata(
    state: &SharedState,
    name: &str,
    mut cmd: portable_pty::CommandBuilder,
    history_limit: u32,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    agent_session: Option<Vec<u8>>,
) -> Result<phux_core::ids::TerminalId, crate::terminal_actor::TerminalActorError> {
    use phux_core::ids::TerminalId;
    // phux-p4vp: capture the spawn-time working directory before `cmd`
    // is moved into the actor build below, so it can be stamped onto the
    // pane's registry descriptor (see `stamp_spawn_cwd`).
    let spawn_cwd = spawn_cwd_of(&cmd);
    let terminal: TerminalId = state.with_mut(|s| {
        let terminal = s.seed_session(name).2;
        stamp_spawn_cwd(s, terminal, spawn_cwd);
        let wire = s.intern_terminal_wire(terminal);
        crate::terminal_actor::apply_terminal_id(&mut cmd, &wire);
        crate::terminal_actor::apply_server_socket(&mut cmd, s.server_socket_path());
        if let Some(value) = agent_session {
            s.metadata_set(
                &phux_protocol::wire::frame::Scope::Terminal(wire),
                phux_protocol::wire::frame::TERMINAL_AGENT_SESSION_KEY,
                value,
            );
        }
        terminal
    });
    let terminal_token = root_token.child_token();
    let bundle = match TerminalActor::build_with_token_and_colors(
        80,
        24,
        Some(cmd),
        history_limit,
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
    // phux-y2t: wire the actor's agent-event sink and spawn a drain task
    // that fans bell / title / dirty / idle events out to event-stream
    // subscribers scoped to this pane. The wire `TerminalId` is interned
    // up front (stable for the pane's lifetime) and captured by the drain.
    let (event_tx, event_rx) = tokio::sync::mpsc::channel(EVENT_SINK_CAPACITY);
    actor.set_event_sink(event_tx);
    // ADR-0046: same shape as the event sink, and for the same reason — the
    // sink MUST be installed before `actor.run()` moves the actor into the
    // spawn, while the wire `TerminalId` the drain needs only exists after.
    let (agent_tx, agent_rx) = tokio::sync::mpsc::channel(AGENT_STATE_SINK_CAPACITY);
    actor.set_agent_state_sink(agent_tx);
    let wire_terminal_id = state.with_mut(|s| {
        let _ = s.spawn_terminal_actor(terminal, handle, terminal_token, actor.run());
        s.intern_terminal_wire(terminal)
    });
    spawn_pane_event_drain(state.clone(), wire_terminal_id.clone(), event_rx);
    spawn_agent_state_drain(state.clone(), wire_terminal_id.clone(), agent_rx);
    spawn_terminal_exit_watcher(state.clone(), terminal, exit_notify, root_token.clone());
    // docs/consumers/tui.md §9 (phux-r82.1): the pane's actor is live and
    // its PTY child spawned.
    crate::hooks::fire_hook(
        state,
        crate::hooks::HookEvent::after_new_pane(&wire_terminal_id, Some(name)),
    );
    Ok(terminal)
}

/// Add a **PTY-backed** pane to an existing `session`'s window and spawn its
/// `TerminalActor` — the split counterpart to [`seed_session_with_pty`]
/// (phux-i9zl).
///
/// Identical to `seed_session_with_pty` except the new pane joins
/// `session`'s window via `add_pane_to_session` instead of
/// creating a fresh `spawn-N` session. A TUI split routes here so the new
/// L1 Terminal stays in the spawning client's current session.
///
/// Returns `Ok(None)` when `session` has no window to host the pane
/// (unreachable for a seeded session); the caller maps that to a wire
/// `SpawnError`. `Err` is an actor-build failure, same as the seed path.
pub fn spawn_pane_with_pty(
    state: &SharedState,
    session: phux_core::ids::SessionId,
    cmd: portable_pty::CommandBuilder,
    history_limit: u32,
    root_token: &CancellationToken,
) -> Result<Option<phux_core::ids::TerminalId>, crate::terminal_actor::TerminalActorError> {
    spawn_pane_with_pty_and_colors(
        state,
        &SpawnOwnership::Session(session),
        cmd,
        history_limit,
        root_token,
        None,
        None,
    )
}

/// Registry ownership address for a newly spawned pane.
#[derive(Debug)]
pub(crate) enum SpawnOwnership {
    /// Legacy session ownership (first window in v0.x).
    Session(phux_core::ids::SessionId),
    /// Exact window ownership derived from an existing wire Terminal id.
    Terminal(phux_protocol::ids::TerminalId),
}

/// Palette-seeded split variant. The spawning client's advertised defaults
/// are installed before the child PTY is parsed.
pub(crate) fn spawn_pane_with_pty_and_colors(
    state: &SharedState,
    ownership: &SpawnOwnership,
    mut cmd: portable_pty::CommandBuilder,
    history_limit: u32,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    agent_session: Option<Vec<u8>>,
) -> Result<Option<phux_core::ids::TerminalId>, crate::terminal_actor::TerminalActorError> {
    use phux_core::ids::TerminalId;
    // phux-p4vp: same spawn-time cwd capture as `seed_session_with_pty`.
    let spawn_cwd = spawn_cwd_of(&cmd);
    let Some(terminal): Option<TerminalId> = state.with_mut(|s| {
        let terminal = match ownership {
            SpawnOwnership::Session(session) => s.add_pane_to_session(*session)?,
            SpawnOwnership::Terminal(owner) => s.add_pane_to_terminal_owner(owner)?,
        };
        stamp_spawn_cwd(s, terminal, spawn_cwd);
        let wire_terminal = s.intern_terminal_wire(terminal);
        crate::terminal_actor::apply_terminal_id(&mut cmd, &wire_terminal);
        crate::terminal_actor::apply_server_socket(&mut cmd, s.server_socket_path());
        if let Some(value) = agent_session {
            s.metadata_set(
                &phux_protocol::wire::frame::Scope::Terminal(wire_terminal),
                phux_protocol::wire::frame::TERMINAL_AGENT_SESSION_KEY,
                value,
            );
        }
        Some(terminal)
    }) else {
        return Ok(None);
    };
    let terminal_token = root_token.child_token();
    let bundle = match TerminalActor::build_with_token_and_colors(
        80,
        24,
        Some(cmd),
        history_limit,
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
    // Same agent-event wiring as the seed path (phux-y2t): intern the wire id
    // up front and spawn the per-pane event drain.
    let (event_tx, event_rx) = tokio::sync::mpsc::channel(EVENT_SINK_CAPACITY);
    actor.set_event_sink(event_tx);
    // ADR-0046: same shape as the event sink, and for the same reason — the
    // sink MUST be installed before `actor.run()` moves the actor into the
    // spawn, while the wire `TerminalId` the drain needs only exists after.
    let (agent_tx, agent_rx) = tokio::sync::mpsc::channel(AGENT_STATE_SINK_CAPACITY);
    actor.set_agent_state_sink(agent_tx);
    let wire_terminal_id = state.with_mut(|s| {
        let _ = s.spawn_terminal_actor(terminal, handle, terminal_token, actor.run());
        s.intern_terminal_wire(terminal)
    });
    spawn_pane_event_drain(state.clone(), wire_terminal_id.clone(), event_rx);
    spawn_agent_state_drain(state.clone(), wire_terminal_id.clone(), agent_rx);
    spawn_terminal_exit_watcher(state.clone(), terminal, exit_notify, root_token.clone());
    // docs/consumers/tui.md §9 (phux-r82.1): the split pane's actor is live.
    let session_name = state.with(|s| {
        let window = s.registry.terminal(terminal)?.window;
        let session = s.registry.window(window)?.session;
        s.registry.session(session).map(|sess| sess.name.clone())
    });
    crate::hooks::fire_hook(
        state,
        crate::hooks::HookEvent::after_new_pane(&wire_terminal_id, session_name.as_deref()),
    );
    Ok(Some(terminal))
}

/// The working directory a PTY child spawned from `cmd` starts in
/// (phux-p4vp): the builder's explicit cwd when set, else the server
/// process's own CWD (which the child inherits). `None` only when the
/// server's CWD itself is unreadable.
fn spawn_cwd_of(cmd: &portable_pty::CommandBuilder) -> Option<std::path::PathBuf> {
    cmd.get_cwd()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

/// Stamp a freshly-spawned pane's working directory onto its registry
/// descriptor (phux-p4vp).
///
/// `phux_core::Registry::new_terminal` initializes `TerminalDescriptor.cwd`
/// to the empty path, and `build_session_snapshot` filters an empty path
/// to a wire `cwd: None` — so without this stamp the ATTACHED
/// `SessionSnapshot.panes[].cwd` never populates for normally spawned
/// panes and the TUI sidebar's per-window VCS branch line stays blank.
/// The stamped value is the spawn-time directory; attach refreshes it
/// from the live PTY child (see
/// [`crate::runtime::attach::refresh_registry_cwds`]).
fn stamp_spawn_cwd(
    s: &mut crate::state::ServerState,
    terminal: phux_core::ids::TerminalId,
    cwd: Option<std::path::PathBuf>,
) {
    if let Some(cwd) = cwd
        && let Some(desc) = s.registry.terminal_mut(terminal)
    {
        desc.cwd = cwd;
    }
}

/// Bounded capacity of the per-pane agent-event sink (SPEC §7.5,
/// phux-y2t). Small: events are coalesced (one `dirty` per burst, one
/// `idle` to close it) and the stream tolerates loss — a full sink drops
/// the event rather than stalling the actor's hot PTY-pump loop.
pub(crate) const EVENT_SINK_CAPACITY: usize = 64;

/// Bounded capacity of the per-pane agent-state sink (ADR-0046).
///
/// Tiny, because the detector is edge-filtered: it emits only on a real
/// change of the derived `(kind, name, state)` tuple, so a steady `working`
/// pane produces nothing at all. Eight is already far more than a pane can
/// plausibly queue between drains, and a full sink drops rather than stalls —
/// which is safe here in a way it would not be for an edge-triggered design:
/// the detector re-derives from scratch on its next tick, so a dropped event
/// is re-published, not lost.
pub(crate) const AGENT_STATE_SINK_CAPACITY: usize = 8;

/// Handle a client's `TERMINAL_RESIZE` (L1 §3.1).
///
/// The explicit, per-Terminal counterpart to [`handle_viewport_resize`]: the
/// caller names one Terminal and its exact cell dimensions, rather than
/// reporting a viewport the window-size policy then folds across every
/// subscriber. It deliberately does NOT consult
/// [`crate::state::ServerState::resolve_terminal_geometry`] — the point of
/// the frame is to set a size a view could not have produced, which is what
/// makes it the only way a headless caller (an agent, `phux resize`) can
/// size a pane at all. The resize applies whether or not anyone is attached;
/// under the view-derived `window-size` policies the next attach / detach /
/// `VIEWPORT_RESIZE` recomputes geometry from views and supersedes it, and
/// under `WindowSize::Manual` nothing ever does. That precedence is
/// documented for consumers in `docs/consumers/tui.md` §4.2 and is why
/// `phux resize` reads the result back instead of trusting the send.
///
/// No reply frame: the S→C `TERMINAL_RESIZED` discriminant is spec-only, so
/// every not-found path here is a `debug!` and a drop.
pub(crate) fn handle_terminal_resize(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::TerminalId,
    cols: u16,
    rows: u16,
) {
    if !wire_terminal_id.is_local() {
        // Federation relay (phux-v45.4): forward the frame verbatim with
        // the id rewritten to the satellite's Local space. Off-hub (or
        // for an unknown host) it stays a warn-drop.
        if !relay_satellite_frame(
            state,
            client_id,
            wire_terminal_id,
            "TERMINAL_RESIZE",
            |id| FrameKind::TerminalResize {
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
                "TERMINAL_RESIZE: SATELLITE-routed pane id rejected on non-federation-hub server",
            );
        }
        return;
    }
    state.with_mut(|s| {
        let Some(terminal) = s.terminal_from_wire(wire_terminal_id) else {
            debug!(
                ?client_id,
                ?wire_terminal_id,
                cols,
                rows,
                "TERMINAL_RESIZE: unknown pane; dropping (no-reply per wire frame design)",
            );
            return;
        };
        // Clamp to the same one-cell floor `TerminalActor::handle_resize`
        // applies. libghostty has no zero-dimension grid, so a `0` on either
        // axis becomes a `1` down there regardless; recording the raw request
        // here would leave the registry claiming `0x24` for a grid that is
        // really `1x24`, and `GET_STATE` — which `phux resize` reads back to
        // decide whether the resize took — would report a size no pane has.
        // The wire codec still round-trips zero faithfully (L1 §3.1); this is
        // the "treat zero as a no-op rather than a kernel error" SHOULD,
        // applied consistently on both sides of the actor boundary.
        let cols = cols.max(1);
        let rows = rows.max(1);
        // Keep the registry's recorded dims in sync so future
        // `TERMINAL_SNAPSHOT` payloads report the post-resize cols/rows.
        // Mirrors what `handle_viewport_resize` does for VIEWPORT_RESIZE.
        if let Some(pane) = s.registry.terminal_mut(terminal) {
            pane.dims = (cols, rows);
        }
        let Some(handle) = s.terminals.get(&terminal) else {
            debug!(
                ?client_id,
                ?terminal,
                cols,
                rows,
                "TERMINAL_RESIZE: no TerminalHandle registered for pane; dropping",
            );
            return;
        };
        // Live per-pane resize (TERMINAL_RESIZE): resync clients so their
        // mirrors reconverge after reflow (phux-8v1). An agent's explicit
        // resize carries cell counts only — no pixel truth — so the actor
        // keeps its last-known cell pixel size.
        match handle.resize.try_send(ResizeRequest {
            cols,
            rows,
            cell_px: None,
            resync_clients: true,
            resync_only: false,
        }) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    ?client_id,
                    ?terminal,
                    cols,
                    rows,
                    "TERMINAL_RESIZE: pane resize mailbox full; dropping",
                );
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                debug!(
                    ?client_id,
                    ?terminal,
                    "TERMINAL_RESIZE: pane actor gone; dropping resize",
                );
            }
        }
    });
}

/// Perform the attach mutation in one critical section: call
/// [`crate::state::ServerState::attach`], build the snapshot, collect
/// the per-pane handles + wire ids to snapshot.
///
/// Pulled out so [`crate::runtime::attach::handle_attach`] stays under clippy's
/// `too_many_lines` ceiling.
pub(crate) fn prepare_attach(
    state: &SharedState,
    client_id: ClientId,
    session_name: &str,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
) -> Result<AttachPrepared, crate::state::AttachError> {
    state.with_mut(|s| {
        let sid = s.attach(client_id, session_name, out_tx.clone(), client_caps)?;
        // Record successful attach as session activity before we build
        // the snapshot. The order doesn't matter for
        // correctness (we're still inside the with_mut critical
        // section), but doing it here keeps the recording adjacent to
        // the attach call that justified it — easier to reason about
        // when reading the code.
        s.touch_session(sid);
        let snapshot = s
            .build_session_snapshot(sid)
            .ok_or_else(|| crate::state::AttachError::UnknownSession(session_name.to_owned()))?;
        let panes_to_snapshot = s.attach_snapshot_panes(sid);
        let initial_client_id =
            phux_protocol::ids::ClientId::new(u32::try_from(client_id.0).unwrap_or(u32::MAX));
        Ok((snapshot, initial_client_id, panes_to_snapshot))
    })
}

// -----------------------------------------------------------------------------
// Control-plane command dispatch — SPEC §5 (phux-k61 / ADR-0021).
// -----------------------------------------------------------------------------

/// Dispatch a `COMMAND` envelope and reply with `COMMAND_RESULT`
/// correlated by `request_id`. The control plane for the CLI's `ls` /
/// `kill` verbs. Per SPEC §5 a command is asynchronous: the result MAY
/// follow other frames the command triggered (e.g. `KILL_TERMINAL`'s
/// `TERMINAL_CLOSED`).
/// Stable, payload-free label for a [`Command`] variant — the `kind` field
/// on the `handle_command` lifecycle span. A hand-written map (rather than
/// `?command`) keeps the trace line small and free of user payloads
/// (session names, env, input bytes) while still localizing which control
/// command ran. `Command` is `#[non_exhaustive]`, hence the wildcard; a new
/// variant logs as `"other"` until an arm is added here.
pub(crate) const fn command_kind(command: &Command) -> &'static str {
    match command {
        Command::AttachTerminal { .. } => "attach_terminal",
        Command::DetachTerminal { .. } => "detach_terminal",
        Command::KillTerminal { .. } => "kill_terminal",
        Command::KillTerminals { .. } => "kill_terminals",
        Command::DetachClients { .. } => "detach_clients",
        Command::GetState { .. } => "get_state",
        Command::GetScreen { .. } => "get_screen",
        Command::RouteInput { .. } => "route_input",
        Command::ApplyInput { .. } => "apply_input",
        Command::AcquireInput { .. } => "acquire_input",
        Command::ReleaseInput { .. } => "release_input",
        Command::SignalTerminal { .. } => "signal_terminal",
        Command::PutFile { .. } => "put_file",
        _ => "other",
    }
}

// Lifecycle span (info): one per L2 COMMAND. `kind` is a payload-free
// label so the trace localizes which control command ran without leaking
// session names / env / input bytes; the CLOSE duration times the command
// (some, e.g. GET_SCREEN, round-trip to an actor).
#[tracing::instrument(
    level = "info",
    name = "handle_command",
    skip_all,
    fields(?client_id, request_id, kind = command_kind(&command)),
)]
#[allow(
    clippy::too_many_lines,
    reason = "one flat dispatch arm per wire command keeps the catalog auditable"
)]
pub(crate) async fn handle_command(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    command: Command,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    input_lane: Option<&InputLaneHandle>,
) {
    // UPGRADE is handled out-of-band: `handle_upgrade` acks the client itself
    // and then re-execs the process, so it never returns a `CommandResult` for
    // the shared send below (ADR-0032).
    if matches!(command, Command::Upgrade) {
        handle_upgrade(state, request_id, out_tx).await;
        return;
    }

    // PUT_FILE chunks may carry 8 MiB. Route this variant by ownership before
    // the borrowed generic helper so a hub does not clone the payload merely
    // to rewrite a satellite TerminalId.
    let command = match command {
        Command::PutFile {
            upload_id,
            terminal_id,
            extension,
            offset,
            data,
            final_chunk,
            sha256,
        } => match crate::hub::relay::satellite_route(&terminal_id) {
            Some((sat_host, local_id)) => {
                handle_satellite_command(
                    state,
                    client_id,
                    request_id,
                    &sat_host,
                    Command::PutFile {
                        upload_id,
                        terminal_id: phux_protocol::ids::TerminalId::local(local_id),
                        extension,
                        offset,
                        data,
                        final_chunk,
                        sha256,
                    },
                    out_tx,
                )
                .await;
                return;
            }
            None => Command::PutFile {
                upload_id,
                terminal_id,
                extension,
                offset,
                data,
                final_chunk,
                sha256,
            },
        },
        other => other,
    };

    // Federation relay (phux-v45.4, ADR-0007 §4): a command targeting a
    // satellite-owned terminal never touches local dispatch — see
    // `handle_satellite_command`.
    if let Some((sat_host, local_command)) = crate::hub::relay::route_to_satellite(&command) {
        handle_satellite_command(
            state,
            client_id,
            request_id,
            &sat_host,
            local_command,
            out_tx,
        )
        .await;
        return;
    }

    let result = match command {
        Command::AttachTerminal { terminal_id } => {
            handle_attach_terminal(state, client_id, &terminal_id, out_tx).await
        }
        Command::DetachTerminal { terminal_id } => {
            handle_detach_terminal(state, client_id, &terminal_id)
        }
        Command::GetState { scope } => handle_get_state_federated(state, &scope, out_tx).await,
        Command::GetScreen {
            terminal_id,
            request_scrollback,
            cells,
        } => handle_get_screen(state, &terminal_id, request_scrollback, cells).await,
        Command::RouteInput { terminal_id, event } => match input_lane {
            Some(lane) => lane.route_command(client_id, terminal_id, event).await,
            None => handle_route_input(state, client_id, &terminal_id, event),
        },
        Command::ApplyInput {
            operation_id,
            terminal_id,
            events,
        } => match input_lane {
            Some(lane) => {
                lane.apply_input(client_id, operation_id, terminal_id, events)
                    .await
            }
            None => CommandResult::Error {
                code: ErrorCode::InternalError,
                message: "acknowledged input lane unavailable".to_owned(),
            },
        },
        Command::KillTerminals { ids } => handle_kill_terminals(state, &ids),
        Command::DetachClients { session } => handle_detach_clients(state, session.as_deref()),
        Command::KillTerminal { terminal_id } => handle_kill_terminal(state, &terminal_id),
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
        Command::SubscribeTerminalEvents {
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
        } => handle_signal_terminal(state, client_id, &terminal_id, signal).await,
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
        // `Command` is `#[non_exhaustive]`: a forward-compat command this
        // server doesn't implement decodes only if a newer peer sent a
        // tag we allocated but haven't wired (the decoder rejects truly
        // unknown tags). Refuse it per SPEC §5 with `INVALID_COMMAND`.
        _ => CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: "command not supported by this server".to_owned(),
        },
    };
    debug!(
        ?client_id,
        request_id, "COMMAND dispatched; sending COMMAND_RESULT"
    );
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::CommandResult {
            request_id,
            result,
        }))
        .await;
}

/// Build the reply for `KILL_TERMINAL`: resolve the wire id to the core
/// pane, then cancel its actor. Cancellation drops the actor's
/// `exit_notify`, which the per-pane EOF watcher (phux-it8) treats
/// identically to PTY EOF: it broadcasts `TERMINAL_CLOSED` and reaps the
/// pane (phux-60s), cascading to session removal + server self-exit when
/// the last session empties. So `KILL_TERMINAL` reuses the exact teardown
/// a natural shell exit takes — no separate kill plumbing, and the async
/// `TERMINAL_CLOSED` still fires.
fn handle_kill_terminal(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::TerminalId,
) -> CommandResult {
    state
        .with(|s| s.terminal_from_wire(terminal_id))
        .map_or_else(
            || CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                message: format!("no such terminal: {terminal_id:?}"),
            },
            |core_id| {
                state.with_mut(|s| s.detach_terminal_actor(core_id));
                CommandResult::Ok
            },
        )
}

/// Handle `ATTACH_TERMINAL` (SPEC §5.1 tag 0x01, phux-v45.7): subscribe the
/// caller to one Terminal's content stream without a session-scoped
/// `ATTACH`. Registers the caller as an output subscriber (which also opens
/// the `INPUT_*` / `FRAME_ACK` gates for it — see `handle_terminal_input`),
/// registers the per-consumer state-sync entry so `FRAME_ACK` eviction
/// works (ADR-0018), spawns a cancellable output pump, and primes the
/// caller with an authoritative `TERMINAL_SNAPSHOT` before any
/// `TERMINAL_OUTPUT` delta (the same snapshot-first gate `handle_attach`
/// enforces — ADR-0007 §4's snapshot-on-attach invariant rides on it
/// across the federation hop).
///
/// Idempotent: a re-attach re-sends a fresh snapshot without spawning a
/// second pump — this is what a federation hub relays when a second
/// consumer attaches to a terminal the link already streams; the
/// duplicate snapshot is a convergent repaint for existing observers.
///
/// Deliberately does NOT resize the Terminal (no viewport rides the
/// command); interactive callers follow with `TERMINAL_RESIZE`.
#[allow(
    clippy::too_many_lines,
    reason = "linear per-terminal attach orchestration: resolve -> subscribe -> register consumer -> pump -> snapshot; mirrors handle_attach's shape for one pane"
)]
async fn handle_attach_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::TerminalId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> CommandResult {
    use crate::terminal_actor::{ConsumerAttachRequest, PaneOutput, SnapshotRequest};

    // Resolve, register the subscription, and snapshot the client's caps in
    // one critical section. A client that never attached a session (the
    // agent / hub-link shape) has no stored caps and gets the pass-through
    // default — for the hub link that is exactly right: the hub relays
    // satellite bytes verbatim (ADR-0007 opaque relay).
    let resolved = state.with_mut(|s| {
        let core = s.terminal_from_wire(terminal_id)?;
        let handle = s.terminal_handle(core).cloned()?;
        let caps = s
            .attached
            .get(&client_id)
            .map(|c| c.client_caps)
            .unwrap_or_default();
        let subs = s.terminal_subscribers.entry(core).or_default();
        if !subs.contains(&client_id) {
            subs.push(client_id);
        }
        Some((core, handle, caps))
    });
    let Some((core, handle, client_caps)) = resolved else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such terminal: {terminal_id:?}"),
        };
    };

    // Register the per-consumer state-sync entry (ADR-0018) so FRAME_ACK
    // from this consumer drives the actor's eviction loop. Mirrors the
    // handle_attach registration; a failure degrades to the broadcast
    // path, never fails the attach.
    let mut tick_managed = false;
    let stream_id = crate::runtime::attach::stream_id_from(client_id.0);
    let bootstrap_id = crate::runtime::attach::initial_bootstrap_id();
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    let server_bootstrap = crate::native_state::native_bootstrap_capabilities();
    #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
    let server_bootstrap = phux_protocol::caps::BootstrapCapabilities::new();
    let Ok((bootstrap_profile, bootstrap_limits)) =
        phux_protocol::caps::select_bootstrap_profile(&client_caps, &server_bootstrap)
    else {
        return CommandResult::Error {
            code: ErrorCode::CodecUnavailable,
            message: "no common bootstrap profile for ATTACH_TERMINAL".to_owned(),
        };
    };
    let stream_profile = match bootstrap_profile {
        phux_protocol::caps::BootstrapProfile::NativeState { codec, .. } => {
            phux_protocol::caps::BootstrapStreamProfile::NativeState { codec }
        }
        phux_protocol::caps::BootstrapProfile::SynthesizedVtStateSync => {
            phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtStateSync
        }
        phux_protocol::caps::BootstrapProfile::SynthesizedVtRaw => {
            phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw
        }
    };
    if let Some(wire_id) = terminal_id.local_id() {
        let (attach_reply_tx, attach_reply_rx) = oneshot::channel();
        if handle
            .consumer_attach
            .send(ConsumerAttachRequest {
                client_id: wire_client_id(client_id),
                outbound: out_tx.clone(),
                wire_terminal_id: wire_id,
                stream_id,
                bootstrap_id,
                wants_state_sync: matches!(
                    client_caps.output_mode,
                    phux_protocol::caps::OutputMode::StateSync
                ),
                // phux-v45.8: `ATTACH_TERMINAL` over a reliable transport; the
                // emit-once model is correct. Forwarded-leg loss-tolerance is
                // the deferred activation (ADR-0042).
                loss_tolerant: false,
                reply: attach_reply_tx,
            })
            .await
            .is_ok()
            && let Ok(Ok(outcome)) = attach_reply_rx.await
        {
            tick_managed = outcome.tick_managed;
        }
    }

    // Spawn the output pump — unless one is already live for this
    // (client, terminal) pair (idempotent re-attach) or the actor's tick
    // is this consumer's emitter (state-sync consumers, phux-3uv).
    // Subscribing to the broadcast BEFORE the snapshot request and gating
    // the pump's first forward on the snapshot send preserves the
    // snapshot-then-deltas order (phux-7w1j).
    let pump_token = state.with_mut(|s| s.register_attach_terminal_pump(client_id, core));
    let mut snapshot_gate: Option<oneshot::Sender<()>> = None;
    // When the actor's tick manages this consumer (state-sync mode) no
    // pump is spawned, but the token stays registered so DETACH_TERMINAL
    // bookkeeping is uniform (cancelling a pump-less token is a no-op).
    if let Some(token) = pump_token
        && !tick_managed
    {
        let mut output_rx = handle.output.subscribe();
        let pump_out_tx = out_tx.clone();
        let pump_wire_terminal_id = terminal_id.clone();
        let pump_resize = handle.resize.clone();
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let pump_native_bootstrap = handle.native_bootstrap.clone();
        let (gate_tx, gate_rx) = oneshot::channel::<()>();
        snapshot_gate = Some(gate_tx);
        tokio::task::spawn_local(async move {
            if gate_rx.await.is_err() {
                return;
            }
            let mut seq: u64 = 0;
            let mut bootstrap_id = bootstrap_id;
            loop {
                let msg = tokio::select! {
                    () = token.cancelled() => break,
                    msg = output_rx.recv() => msg,
                };
                match msg {
                    Ok(PaneOutput::Live(bytes)) => {
                        seq = seq.wrapping_add(1);
                        let frame = FrameKind::TerminalOutput {
                            terminal_id: pump_wire_terminal_id.clone(),
                            stream_id,
                            bootstrap_id,
                            seq,
                            bytes: crate::runtime::attach::downsample_for_caps(
                                &bytes,
                                client_caps,
                            ),
                        };
                        if pump_out_tx.send(Outbound::Frame(frame)).await.is_err() {
                            break;
                        }
                    }
                    Ok(PaneOutput::Resync { cols, rows, bytes, reason }) => {
                        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                        let prior_bootstrap_id = bootstrap_id;
                        bootstrap_id =
                            crate::runtime::attach::next_bootstrap_id(bootstrap_id);
                        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                        if matches!(
                            stream_profile,
                            phux_protocol::caps::BootstrapStreamProfile::NativeState {
                                codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2
                            }
                        ) {
                            if pump_out_tx
                                .send(Outbound::Frame(FrameKind::BootstrapTombstone {
                                    terminal_id: pump_wire_terminal_id.clone(),
                                    stream_id,
                                    bootstrap_id: prior_bootstrap_id,
                                    reason: match reason {
                                        crate::terminal_actor::ResyncReason::Resize => {
                                            phux_protocol::wire::frame::TombstoneReason::Resize
                                        }
                                        crate::terminal_actor::ResyncReason::OutboundGap => {
                                            phux_protocol::wire::frame::TombstoneReason::OutboundGap
                                        }
                                    },
                                    last_valid_seq: seq,
                                }))
                                .await
                                .is_err()
                            {
                                break;
                            }
                            let (reply_tx, reply_rx) = oneshot::channel();
                            if pump_native_bootstrap
                                .send(crate::terminal_actor::NativeBootstrapRequest {
                                    outbound: pump_out_tx.clone(),
                                    owner: client_id.0,
                                    terminal_id: pump_wire_terminal_id.clone(),
                                    stream_id,
                                    bootstrap_id,
                                    limits: bootstrap_limits,
                                    base_seq: seq,
                                    reply: reply_tx,
                                })
                                .await
                                .is_err()
                                || !matches!(reply_rx.await, Ok(Ok(())))
                            {
                                let _ = pump_out_tx
                                    .send(Outbound::Frame(FrameKind::Error {
                                        request_id: None,
                                        code: ErrorCode::CodecUnavailable,
                                        message: "native checkpoint resync failed".to_owned(),
                                    }))
                                    .await;
                                break;
                            }
                            continue;
                        }
                        let payload =
                            crate::runtime::attach::downsample_for_caps(&bytes, client_caps);
                        if crate::runtime::attach::send_synthesized_bootstrap(
                            &pump_out_tx,
                            pump_wire_terminal_id.clone(),
                            stream_id,
                            bootstrap_id,
                            stream_profile,
                            bootstrap_limits,
                            cols,
                            rows,
                            seq,
                            [payload],
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            terminal_id = ?pump_wire_terminal_id,
                            dropped = n,
                            "ATTACH_TERMINAL output pump lagged; requesting in-band resync",
                        );
                        let _ = pump_resize.try_send(ResizeRequest {
                            cols: 0,
                            rows: 0,
                            cell_px: None,
                            resync_clients: true,
                            resync_only: true,
                        });
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    if matches!(
        stream_profile,
        phux_protocol::caps::BootstrapStreamProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2
        }
    ) {
        let (reply_tx, reply_rx) = oneshot::channel();
        if handle
            .native_bootstrap
            .send(crate::terminal_actor::NativeBootstrapRequest {
                outbound: out_tx.clone(),
                owner: client_id.0,
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                limits: bootstrap_limits,
                base_seq: 0,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            return CommandResult::Error {
                code: ErrorCode::InternalError,
                message: "pane actor unavailable for native ATTACH_TERMINAL".to_owned(),
            };
        }
        match reply_rx.await {
            Ok(Ok(())) => {
                if let Some(gate) = snapshot_gate {
                    let _ = gate.send(());
                }
                debug!(?client_id, ?terminal_id, "native ATTACH_TERMINAL subscribed");
                return CommandResult::Ok;
            }
            Ok(Err(error)) => {
                return CommandResult::Error {
                    code: ErrorCode::CodecUnavailable,
                    message: format!("native ATTACH_TERMINAL failed: {error}"),
                };
            }
            Err(_) => {
                return CommandResult::Error {
                    code: ErrorCode::InternalError,
                    message: "pane actor dropped native ATTACH_TERMINAL".to_owned(),
                };
            }
        }
    }

    // Authoritative snapshot, sent before the pump's first delta (the
    // gate below releases it) and before the Ok reply.
    let (reply_tx, reply_rx) = oneshot::channel();
    if handle
        .snapshot
        .send(SnapshotRequest {
            scrollback: None,
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for ATTACH_TERMINAL".to_owned(),
        };
    }
    let Ok(snap) = reply_rx.await else {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor dropped the ATTACH_TERMINAL snapshot".to_owned(),
        };
    };
    let replay =
        crate::runtime::attach::downsample_for_caps(&bytes::Bytes::from(snap.bytes), client_caps);
    if crate::runtime::attach::send_synthesized_bootstrap(
        out_tx,
        terminal_id.clone(),
        stream_id,
        bootstrap_id,
        stream_profile,
        bootstrap_limits,
        snap.cols,
        snap.rows,
        0,
        [replay],
    )
    .await
    .is_err()
    {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "consumer went away during ATTACH_TERMINAL".to_owned(),
        };
    }
    if let Some(gate) = snapshot_gate {
        let _ = gate.send(());
    }
    debug!(?client_id, ?terminal_id, "ATTACH_TERMINAL subscribed");
    CommandResult::Ok
}

/// Handle `DETACH_TERMINAL` (SPEC §5.1 tag 0x02, phux-v45.7): drop the
/// caller's per-terminal subscriptions — the `ATTACH_TERMINAL` output
/// stream (pump cancelled, subscriber entry removed, per-consumer
/// state-sync entry released) and the per-terminal agent-event
/// subscription. Idempotent: unknown terminals and never-attached callers
/// reply `Ok`, so a detach can never race a natural close into an error.
fn handle_detach_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::TerminalId,
) -> CommandResult {
    use crate::terminal_actor::ConsumerDetachRequest;

    let handle = state.with_mut(|s| {
        s.unsubscribe_terminal_events(client_id, terminal_id);
        let core = s.terminal_from_wire(terminal_id)?;
        s.cancel_attach_terminal_pump(client_id, core);
        s.unsubscribe_terminal(client_id, core);
        s.terminal_handle(core).cloned()
    });
    if let Some(handle) = handle {
        // Release the per-consumer RenderState cache (ADR-0018). Best
        // effort, same discipline as detach_and_release_consumer_state:
        // a full mailbox self-heals via the actor's closed-mailbox reap.
        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = handle.consumer_detach.try_send(ConsumerDetachRequest {
            client_id: wire_client_id(client_id),
            reply: reply_tx,
        });
    }
    debug!(?client_id, ?terminal_id, "DETACH_TERMINAL unsubscribed");
    CommandResult::Ok
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
            // Ack `Ok` and let the writer flush it before we replace the
            // process — best-effort; the client reconnects regardless.
            let _ = out_tx
                .send(Outbound::Frame(FrameKind::CommandResult {
                    request_id,
                    result: CommandResult::Ok,
                }))
                .await;
            tokio::task::yield_now().await;
            info!("UPGRADE: re-exec'ing the new binary");
            let err = plan.exec();
            // Only reached if the exec itself failed: nothing was closed, so
            // the old image keeps serving and no child is stranded.
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
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::CommandResult {
            request_id,
            result,
        }))
        .await;
}

/// Relay one satellite-targeted command over the owning hub link and send
/// the correlated `COMMAND_RESULT` (phux-v45.4, ADR-0007 §4): the command
/// arrives here already rewritten to the satellite's `Local` id space by
/// [`crate::hub::relay::route_to_satellite`], and the reply correlates
/// through the link's own request-id remap. On a non-hub server (or for a
/// host absent from the hub table) this resolves to a typed
/// `UnsupportedSatelliteRoute` error, and an unreachable satellite fails
/// fast with `SatelliteUnreachable` — never a hang.
///
/// **Stream-establishing commands** (`SUBSCRIBE_TERMINAL_EVENTS`,
/// `ATTACH_TERMINAL`) register the caller's outbound mailbox as a hub-side
/// proxy subscriber *atomically with* the relayed command
/// ([`crate::hub::relay::RelayHandle::command_subscribing`], phux-v45.11):
/// the return-leg frames the satellite pushes on the link are re-tagged
/// `Local -> Satellite { host, .. }` and fanned out to this consumer, and
/// a satellite error rolls the registration back. `DETACH_TERMINAL` is
/// resolved hub-side: the consumer's proxy subscription is withdrawn and
/// the link session itself relays a satellite-side `DETACH_TERMINAL` only
/// when the **last** proxy subscriber for that terminal is gone —
/// relaying every consumer's detach verbatim would tear down the link's
/// single shared stream under the other consumers still watching it.
///
/// **Input-lease aliasing** (phux-v45.7, L1 §9.1): every hub consumer
/// shares the link's one client identity on the satellite, so the
/// satellite's lease map cannot distinguish them. The hub therefore owns
/// lease exclusion *between its own consumers* via
/// `ServerState::satellite_leases`: a cooperative `ACQUIRE_INPUT` against
/// a terminal another hub consumer holds is refused here without touching
/// the link; `RELEASE_INPUT` from a non-holder is the idempotent no-op
/// `Ok` (never forwarded — forwarding would release the real holder's
/// satellite-side lease); `ROUTE_INPUT` from a non-holder is refused with
/// `InputLeaseHeld`. The relayed lease (held by the link identity) still
/// excludes the satellite's own local clients.
#[allow(
    clippy::too_many_lines,
    reason = "one linear relay dispatch: route -> lease gate -> atomic subscribe -> relay -> ledger update; splitting scatters the two-hop contract"
)]
async fn handle_satellite_command(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    host: &phux_protocol::ids::SatelliteHost,
    command: Command,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    let result = match state.with(|s| s.hub_relay(host)) {
        None => CommandResult::Error {
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: format!(
                "no satellite route to {host:?}: this server is not a federation hub \
                 for that host (check `phux server --hub` and the [[satellites]] registry)"
            ),
        },
        Some(relay) => match &command {
            Command::SubscribeTerminalEvents { terminal_id, .. }
            | Command::AttachTerminal { terminal_id } => match terminal_id.local_id() {
                Some(id) => {
                    relay
                        .command_subscribing(
                            command.clone(),
                            crate::hub::relay::ProxySubscription {
                                terminal: id,
                                client: client_id,
                                out_tx: out_tx.clone(),
                                // Stamped with the issue-order token by
                                // `command_subscribing` at enqueue.
                                seq: 0,
                                // Only ATTACH_TERMINAL opens a content stream
                                // with a return-leg TERMINAL_SNAPSHOT, so only
                                // it gates deltas until that snapshot lands
                                // (phux-v45.14). SUBSCRIBE_TERMINAL_EVENTS
                                // carries no snapshot; gating it would strand
                                // its EVENT stream.
                                awaits_snapshot: matches!(command, Command::AttachTerminal { .. }),
                            },
                        )
                        .await
                }
                None => relay.command(command.clone()).await,
            },
            Command::DetachTerminal { terminal_id } => {
                // Hub-side resolution: withdraw this consumer's proxy
                // subscription; the link session emits the satellite-side
                // DETACH_TERMINAL iff nobody else still observes the
                // terminal. Idempotent Ok, matching the local semantics.
                if let Some(id) = terminal_id.local_id() {
                    relay.unsubscribe_terminal(client_id, id);
                }
                CommandResult::Ok
            }
            Command::AcquireInput {
                terminal_id, mode, ..
            } => {
                let id = terminal_id.local_id().unwrap_or(0);
                let holder = state.with(|s| s.satellite_lease_holder(host, id));
                if *mode == InputMode::Cooperative
                    && let Some(holder) = holder
                    && holder != client_id
                {
                    CommandResult::Error {
                        code: ErrorCode::InputLeaseHeld,
                        message: format!("input lease held by client {}", holder.0),
                    }
                } else {
                    // Cooperative-over-free/self OR a SEIZE takeover. Relay
                    // to the satellite (the link identity's lease keeps
                    // excluding the satellite's own local clients), then
                    // record the new hub-side holder. A SEIZE that preempts
                    // a *different* hub consumer returns the evicted lease:
                    // notify that holder it lost the wheel — mirroring the
                    // local `TerminalControl(Seized)` broadcast (phux-v45.13,
                    // L1 §9.1). Without it the prior holder keeps believing
                    // it holds the wheel while its relayed INPUT_* is silently
                    // dropped at the hub lease gate.
                    let result = relay.command(command.clone()).await;
                    if !matches!(result, CommandResult::Error { .. }) {
                        let evicted = state.with_mut(|s| {
                            s.set_satellite_lease(host.clone(), id, client_id, out_tx.clone())
                        });
                        if let Some(evicted) = evicted {
                            notify_satellite_lease_seized(host, id, client_id, &evicted);
                        }
                    }
                    result
                }
            }
            Command::ReleaseInput { terminal_id } => {
                let id = terminal_id.local_id().unwrap_or(0);
                match state.with(|s| s.satellite_lease_holder(host, id)) {
                    Some(holder) if holder != client_id => {
                        // Idempotent no-op per ADR-0033 — and deliberately
                        // NOT forwarded: on the satellite this consumer is
                        // indistinguishable from the holder, so forwarding
                        // would release the holder's lease (L1 §9.1).
                        CommandResult::Ok
                    }
                    _ => {
                        let result = relay.command(command.clone()).await;
                        if !matches!(result, CommandResult::Error { .. }) {
                            state.with_mut(|s| s.release_satellite_lease(host, id, client_id));
                        }
                        result
                    }
                }
            }
            Command::RouteInput { terminal_id, .. } => {
                let id = terminal_id.local_id().unwrap_or(0);
                let holder = state.with(|s| s.satellite_lease_holder(host, id));
                match holder {
                    Some(holder) if holder != client_id => CommandResult::Error {
                        code: ErrorCode::InputLeaseHeld,
                        message: "input lease held by another client".to_owned(),
                    },
                    _ => relay.command(command.clone()).await,
                }
            }
            _ => relay.command(command.clone()).await,
        },
    };
    debug!(
        ?client_id,
        request_id,
        satellite = %host,
        "satellite-routed COMMAND relayed; sending COMMAND_RESULT"
    );
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::CommandResult {
            request_id,
            result,
        }))
        .await;
}

/// Notify the hub consumer evicted by a SEIZE takeover over a satellite
/// terminal that it no longer holds the input lease (phux-v45.13, L1
/// §9.1).
///
/// The hub synthesizes the same `TerminalControl(Seized)` event the local
/// takeover path broadcasts to every subscriber. The satellite cannot: all
/// hub consumers reach it through the link's single client identity, so a
/// relayed SEIZE reads there as a same-identity re-acquire and its
/// broadcast names the shared link identity, not the evicted hub consumer.
/// Best-effort (`try_send`, the fire-and-forget event discipline): the
/// evicted holder re-renders the locked state from this event exactly as a
/// local viewer does, and stops sending input the hub would now drop at the
/// lease gate.
fn notify_satellite_lease_seized(
    host: &phux_protocol::ids::SatelliteHost,
    id: u32,
    new_holder: ClientId,
    evicted: &crate::state::SatelliteLease,
) {
    let frame = FrameKind::Event {
        terminal: Some(phux_protocol::ids::TerminalId::satellite(host.clone(), id)),
        event: AgentEvent::TerminalControl {
            // phux-v45.14 sub-finding (b): a Frozen satellite pane would be
            // mis-reported as Running here. The hub keeps no cheaply-readable
            // per-satellite-pane lifecycle at this SEIZE path — `SatelliteLease`
            // carries only the holder and its mailbox, and the aggregate view
            // is a round-trip away — so `Running` is the pragmatic default.
            // The event's load-bearing field for the evicted holder is the
            // `Seized` action + `input_holder` handoff, not the lifecycle;
            // the holder re-renders locked state either way, and a Frozen pane
            // reconciles on its next TERMINAL_CONTROL. Revisit if the hub
            // starts tracking satellite pane lifecycle locally.
            lifecycle: TerminalLifecycle::Running,
            exit_status: None,
            input_holder: Some(wire_client_id(new_holder)),
            action: ControlAction::Seized,
            actor: Some(wire_client_id(new_holder)),
        },
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

/// Forward one fire-and-forget frame (`INPUT_*`, `FRAME_ACK`, `TERMINAL_RESIZE`)
/// targeting a satellite terminal over the hub link (phux-v45.4): `build`
/// receives the id rewritten to the satellite's `Local` space and produces
/// the frame to relay verbatim.
///
/// Returns `true` when a relay route existed (the frame was queued, or
/// dropped under the same bounded-mailbox backpressure contract these
/// frames have locally); `false` when this server has no route to the
/// host — the caller keeps its non-hub warn-drop.
///
/// Scope honesty (phux-v45.7): the satellite applies its own attach /
/// subscription / lease gates to what arrives on the link under the
/// link's single client identity. `ATTACH_TERMINAL` relayed over the link
/// opens those gates for the link consumer, so `INPUT_*` / `FRAME_ACK` from a
/// hub consumer that attached the terminal through the hub flow end to
/// end; `ROUTE_INPUT` remains the attach-free input path.
fn relay_satellite_frame(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::TerminalId,
    frame_label: &'static str,
    build: impl FnOnce(phux_protocol::ids::TerminalId) -> FrameKind,
) -> bool {
    let Some((host, id)) = crate::hub::relay::satellite_route(wire_terminal_id) else {
        return false;
    };
    let Some(relay) = state.with(|s| s.hub_relay(&host)) else {
        return false;
    };
    trace!(
        ?client_id,
        ?wire_terminal_id,
        frame_label,
        satellite = %host,
        "relaying satellite-routed frame"
    );
    relay.forward(build(phux_protocol::ids::TerminalId::local(id)));
    true
}

/// Build the `Ok` reply for `KILL_TERMINALS` — the atomic multi-terminal
/// teardown the v0.3.0 "Option B" re-tier left in place of the dissolved
/// L2 `KILL_COLLECTION` verb (ADR-0019 / ADR-0027).
///
/// Tears down every Terminal in `ids` inside **one** `with_mut` lock scope,
/// so the removals are atomic with respect to every other command: no peer
/// can observe a half-killed group on this server. (Cross-host atomicity is
/// out of scope, as it would be under any tiering.) Each removal cancels the
/// pane actor via [`crate::state::ServerState::detach_terminal_actor`];
/// cancellation drops the actor's `exit_notify`, which the per-pane EOF
/// watcher treats like PTY EOF — it broadcasts `TERMINAL_CLOSED` and reaps
/// the pane, cascading to session removal and (when the last session
/// empties) server self-exit. So this reuses the exact teardown a per-pane
/// `KILL_TERMINAL` (or a natural shell exit) takes, but resolves the whole
/// group in one pass.
///
/// Idempotent: an `id` that is unknown or already-dead is skipped silently
/// rather than failing the batch, so a caller racing a natural pane exit
/// still succeeds. Satellite-routed ids (phux-v45.4) are partitioned by
/// host and forwarded as per-satellite `KILL_TERMINALS` batches over the
/// hub links, detached — the satellite applies the same idempotent
/// semantics, and a down link degrades to the silent skip the contract
/// already allows. The reply is `Ok` the moment the local actors are
/// cancelled and the relays are queued; the `TERMINAL_CLOSED` frames follow
/// asynchronously as the panes reap (SPEC §5). The op is structurally
/// infallible — an empty `ids` list is a no-op that still acks `Ok`.
pub(crate) fn handle_kill_terminals(
    state: &SharedState,
    ids: &[phux_protocol::ids::TerminalId],
) -> CommandResult {
    // Satellite partition first (phux-v45.4): group `Satellite { host, id }`
    // entries per host and forward each group as one satellite-local
    // KILL_TERMINALS over the hub link. Detached relay: the batch op is
    // idempotent and tolerates skips, so the hub does not await or merge
    // per-satellite results. Non-hub servers (no relay) keep the silent
    // skip these ids always had here.
    let mut by_host: std::collections::BTreeMap<
        phux_protocol::ids::SatelliteHost,
        Vec<phux_protocol::ids::TerminalId>,
    > = std::collections::BTreeMap::new();
    for wire_id in ids {
        if let Some((host, id)) = crate::hub::relay::satellite_route(wire_id) {
            by_host
                .entry(host)
                .or_default()
                .push(phux_protocol::ids::TerminalId::local(id));
        }
    }
    for (host, local_ids) in by_host {
        match state.with(|s| s.hub_relay(&host)) {
            Some(relay) => {
                debug!(
                    satellite = %host,
                    count = local_ids.len(),
                    "KILL_TERMINALS: relaying satellite partition"
                );
                relay.command_detached(Command::KillTerminals { ids: local_ids });
            }
            None => {
                debug!(
                    satellite = %host,
                    "KILL_TERMINALS: no route to satellite; skipping its ids"
                );
            }
        }
    }

    // Single lock scope: resolve every wire id to its core pane and cancel
    // its actor before releasing the lock. All-or-nothing for a local
    // server — no other command interleaves between the first and last
    // removal. `detach_terminal_actor` is idempotent (cancelling an
    // already-cancelled token is a no-op), so an id racing a natural exit
    // and an unknown id both collapse to a silent skip (satellite ids were
    // partitioned above and resolve to no local pane here).
    let killed = state.with_mut(|s| {
        let mut killed = 0u32;
        for wire_id in ids {
            if let Some(core_id) = s.terminal_from_wire(wire_id) {
                s.detach_terminal_actor(core_id);
                killed = killed.saturating_add(1);
            } else {
                debug!(?wire_id, "KILL_TERMINALS: unknown / dead id; skipping");
            }
        }
        killed
    });
    debug!(
        requested = ids.len(),
        killed, "KILL_TERMINALS: torn down group atomically"
    );
    CommandResult::Ok
}

/// Force-detach clients from *outside* the attach UI (`phux detach`).
///
/// Gathers the target clients — those attached to `session`, or every attached
/// client when `session` is `None` — and their outbound mailboxes under one
/// read borrow, then (off-lock, since the teardown re-locks) pushes a
/// `DETACHED` frame to each so its TUI exits cleanly and runs the normal
/// per-client detach teardown. Returns the count as a JSON number so the CLI
/// can report how many clients it detached. An unknown session name detaches
/// nobody and reports `0` — not an error, matching `KILL_TERMINALS`'s
/// skip-silently shape.
///
/// Scope: this targets *session-attached* clients (the `ATTACH` consumers the
/// `C-a d` keybinding serves) only. Terminal-level subscribers riding
/// `ATTACH_TERMINAL` are a different consumer surface with their own detach
/// verb (`DETACH_TERMINAL`) and are deliberately not swept here.
pub(crate) fn handle_detach_clients(state: &SharedState, session: Option<&str>) -> CommandResult {
    let targets = state.with(|s| s.attached_clients_to_detach(session));
    let count = targets.len();
    for (client_id, tx) in targets {
        // Best-effort DETACHED push via `try_send`: a full or wedged mailbox
        // is exactly the "stuck client" case `phux detach` exists to clear,
        // so we must never await capacity here — that would hang the command
        // loop on the victim's back-pressure. If the frame is dropped, the
        // teardown below still removes the client and its connection closes,
        // which the TUI treats as a disconnect and exits anyway.
        let _ = tx.try_send(Outbound::Frame(FrameKind::Detached));
        super::client::detach_and_release_consumer_state(state, client_id);
    }
    debug!(
        ?session,
        count, "DETACH_CLIENTS: force-detached clients from outside the attach UI"
    );
    CommandResult::OkWith(CommandValue::Json(count.to_string()))
}

/// Create a named session and seed its pane, *without* attaching — the
/// create-without-attach path the v0.3.0 "Option B" re-tier (ADR-0019 /
/// ADR-0027) routes through the conventional
/// [`phux_protocol::wire::frame::SESSION_CREATE_KEY`] L3 metadata write
/// (replacing the removed `CREATE_SESSION` verb).
///
/// Existence check and seed both run on the single-threaded runtime, so the
/// lookup→create sequence is atomic with respect to other clients: two
/// racing create requests for the same `name` cannot both succeed. Returns
/// `Ok(wire_id)` on success (the seed pane's wire [`phux_core::ids::TerminalId`],
/// which the
/// caller publishes under a result key for the client to read back), or
/// `Err(message)` if `name` is already taken or the seed fails. Because
/// `SET_METADATA` has no reply frame, the error is for logging only.
pub(crate) fn create_named_session(
    state: &SharedState,
    name: &str,
    command: Option<Vec<String>>,
    cwd: Option<&str>,
    env: std::collections::BTreeMap<String, String>,
    agent_session: Option<Vec<u8>>,
    root_token: &CancellationToken,
) -> Result<phux_protocol::ids::TerminalId, String> {
    if state.with(|s| s.session_by_name(name).is_some()) {
        return Err(format!("session {name:?} already exists"));
    }

    let (with_pty, override_cmd, history_limit, term, shell) = state.with(|s| {
        (
            s.attach_create_seeds_pty(),
            s.attach_create_seed_command(),
            s.history_limit(),
            s.term().to_owned(),
            s.shell().to_owned(),
        )
    });

    let seed_result = if with_pty {
        // Command precedence mirrors `resolve_create_if_missing`: an explicit
        // server-wide override (set by tests for a deterministic child) wins,
        // then the request `command`, then the default shell.
        let mut seed_cmd = override_cmd.unwrap_or_else(|| match command {
            Some(argv) if !argv.is_empty() => {
                let mut head = argv.into_iter();
                let program = head.next().unwrap_or_default();
                let mut builder = portable_pty::CommandBuilder::new(program);
                for arg in head {
                    builder.arg(arg);
                }
                builder
            }
            _ => crate::terminal_actor::default_shell_command(&shell),
        });
        // phux-0v1l: apply the wire cwd through the shared validate-and-fall-
        // back helper, uniform with the attach CreateIfMissing seed path.
        // Previously this passed the wire cwd through UNVALIDATED (a stale
        // path failed the seed) and only applied it when there was no
        // override command; now it is validated (existence + enterability),
        // applied over a cwd-less builder, and dropped with a warn on an
        // invalid path so a bad cwd never fails the create.
        crate::terminal_actor::apply_spawn_cwd(&mut seed_cmd, cwd, name);
        for (key, value) in env {
            seed_cmd.env(key, value);
        }
        crate::terminal_actor::apply_term(&mut seed_cmd, &term);
        seed_session_with_pty_and_colors_and_metadata(
            state,
            name,
            seed_cmd,
            history_limit,
            root_token,
            None,
            agent_session,
        )
    } else {
        seed_session_with_actor_and_metadata(state, name, history_limit, root_token, agent_session)
    };

    match seed_result {
        Ok(core_terminal) => {
            // A successful headless create arms the same last-session
            // self-exit as an attached client. This keeps control-only
            // servers from lingering after their managed sessions stop.
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

/// Build the `OK_WITH(STATE(..))` reply for `GET_STATE`.
///
/// v0.1 supports only [`StateScope::Server`] (the whole-server snapshot).
/// The snapshot reuses the `ATTACHED`
/// [`phux_protocol::wire::info::SessionSnapshot`] shape; `phux ls`
/// and client-side selector resolution read its `sessions` list and ignore
/// the focused-* fields. An empty server yields an empty session list with
/// sentinel focus ids (the wire requires the focus fields to be present).
pub(crate) fn handle_get_state(state: &SharedState, scope: &StateScope) -> CommandResult {
    match scope {
        StateScope::Server => {
            let snapshot = state.with_mut(|s| {
                let focus = s
                    .most_recently_touched_session()
                    .or_else(|| s.registry.sessions().next().map(|(id, _)| id));
                focus.and_then(|sid| s.build_session_snapshot(sid))
            });
            CommandResult::OkWith(CommandValue::State(
                snapshot.unwrap_or_else(empty_session_snapshot),
            ))
        }
        // `StateScope` is `#[non_exhaustive]`; a narrower scope a newer
        // peer requests is not yet supported.
        _ => CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message: "unsupported GET_STATE scope".to_owned(),
        },
    }
}

/// `GET_STATE` with federation aggregation (phux-v45.5, L1 §9.1): on a
/// hub, the local snapshot from [`handle_get_state`] is merged with every
/// dialed satellite's terminal inventory. Off-hub (no relays) this is
/// exactly the local path.
///
/// Per satellite the hub relays `GET_STATE { scope: SERVER }` over the
/// link (all links queried concurrently, each bounded by the relay's
/// per-command deadline — see `crate::hub::relay::RELAY_COMMAND_TIMEOUT`)
/// and appends the returned `panes` re-tagged
/// `Local { id }` -> `Satellite { host, id }`.
///
/// **Result-shape honesty.** Only *terminals* aggregate. Session and
/// window identities are not federation-routable (ADR-0016 makes
/// `TerminalId` the wire primary), so the satellite's `sessions` /
/// `windows` lists and focus fields are discarded — their `u32` ids
/// would collide with the hub's own. A satellite pane's `window_id` is
/// passed through **verbatim**: it is satellite-local, resolvable only on
/// the satellite, and has no entry in the merged snapshot's `windows`
/// list. `cols` / `rows` / `title` / `cwd` are likewise relayed verbatim
/// from the satellite's snapshot; the hub synthesizes nothing.
///
/// **Degradation.** A satellite that is unreachable, saturated, or
/// answers with an error contributes an empty set and NEVER fails the
/// aggregate. The indication is the spec's observable-teardown shape: one
/// un-correlated `ERROR` frame (typically `SatelliteUnreachable`), naming
/// the host, pushed to the requesting consumer before the
/// `COMMAND_RESULT`.
pub(crate) async fn handle_get_state_federated(
    state: &SharedState,
    scope: &StateScope,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> CommandResult {
    let local = handle_get_state(state, scope);
    if !matches!(scope, StateScope::Server) {
        return local;
    }
    let relays = state.with(crate::state::ServerState::hub_relays_all);
    if relays.is_empty() {
        // Non-hub server (or hub with an empty table): the local snapshot
        // is the whole truth.
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
    for (host, result) in futures_util::future::join_all(queries).await {
        match result {
            CommandResult::OkWith(CommandValue::State(sat)) => {
                for mut pane in sat.panes {
                    match pane.id {
                        phux_protocol::ids::TerminalId::Local { id } => {
                            pane.id = phux_protocol::ids::TerminalId::satellite(host.clone(), id);
                            snapshot.panes.push(pane);
                        }
                        // Hub-and-spoke does not chain (L1 §9.1): a
                        // satellite must never report Satellite-tagged
                        // terminals of its own.
                        phux_protocol::ids::TerminalId::Satellite { .. } => {
                            warn!(
                                satellite = %host,
                                pane = %pane.id,
                                "satellite listed a Satellite-tagged terminal; dropping (no chaining)"
                            );
                        }
                    }
                }
            }
            CommandResult::Error { code, message } => {
                debug!(
                    satellite = %host,
                    ?code,
                    %message,
                    "GET_STATE aggregation: satellite contributes nothing"
                );
                // Observable degradation, not silence: the same
                // un-correlated typed ERROR shape the relay uses for
                // teardown notification (L1 §9.1). Sent before the
                // COMMAND_RESULT the caller emits on return.
                let _ = out_tx
                    .send(Outbound::Frame(FrameKind::Error {
                        request_id: None,
                        code,
                        message,
                    }))
                    .await;
            }
            other => {
                warn!(
                    satellite = %host,
                    ?other,
                    "GET_STATE aggregation: unexpected satellite result shape; skipping"
                );
            }
        }
    }
    CommandResult::OkWith(CommandValue::State(snapshot))
}

/// Build the `OK_WITH(JSON(..))` reply for `GET_SCREEN`.
///
/// Resolves the wire id to its pane actor, then asks the actor to project
/// its own `Terminal` grid into a [`phux_core::screen::ScreenState`]
/// serialized as JSON — the stable agent-surface contract (ADR-0022 §2).
/// This is side-effect-free: it neither attaches nor resizes, so polling
/// it (the `phux wait`/`run` floor) never disturbs the live pane.
pub(crate) async fn handle_get_screen(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::TerminalId,
    request_scrollback: Option<u32>,
    cells: bool,
) -> CommandResult {
    // Clone the (Send) handle out of the lock; the actor reply is awaited
    // outside the critical section.
    let handle = state.with(|s| {
        s.terminal_from_wire(terminal_id)
            .and_then(|core| s.terminal_handle(core).cloned())
    });
    let Some(handle) = handle else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such terminal: {terminal_id:?}"),
        };
    };
    let pane = terminal_id.local_id().unwrap_or(0);
    let (reply_tx, reply_rx) = oneshot::channel();
    if handle
        .screen
        .send(ScreenRequest {
            pane,
            scrollback: request_scrollback,
            cells,
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for GET_SCREEN".to_owned(),
        };
    }
    reply_rx.await.map_or_else(
        |_| CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor dropped the GET_SCREEN reply".to_owned(),
        },
        |screen| {
            serde_json::to_string(&screen).map_or_else(
                |err| CommandResult::Error {
                    code: ErrorCode::InternalError,
                    message: format!("screen serialization failed: {err}"),
                },
                |json| CommandResult::OkWith(CommandValue::Json(json)),
            )
        },
    )
}

/// Build the `Ok_With(Json(TerminalState))` reply for `GET_TERMINAL_STATE`.
///
/// L2 Collection-aware counterpart to [`handle_get_screen`]: returns a
/// comprehensive snapshot of terminal state (grid, scrollback, cursor, shell
/// metadata, sequence number, and timestamp) in a structured JSON format.
/// Backs agent polling and state inspection without requiring an attach or
/// subscription (ADR-0022, ADR-0015 L2).
///
/// Unlike `GET_SCREEN` which returns raw `ScreenState` with only grid
/// dimensions and viewport text, `GET_TERMINAL_STATE` returns structured
/// JSON with:
/// - Grid cells with text and styling
/// - Cursor position and visibility
/// - Optional scrollback history (if `include_scrollback` is true)
/// - Shell process metadata (PID, name, jobs, copy-mode state)
/// - Pending command tracking (overlay layer)
/// - Logical sequence number (for change detection)
/// - Timestamp (for agent polling)
///
/// Handler flow:
/// 1. Resolve `terminal_id` to a `TerminalActor` handle (reuse same pattern as
///    `handle_get_screen`)
/// 2. Query screen state via `ScreenRequest` (reuse existing path)
/// 3. Walk grid cells: parse `ScreenState.lines` and merge styling from
///    `ScreenState.cells` (`CellInfo`)
/// 4. Extract cursor, scrollback, and dimensions
/// 5. Query shell state (gracefully degrade to None if unavailable)
/// 6. Build JSON and encode as JSON
/// 7. Return as `COMMAND_RESULT Ok_With(Json(TerminalState))`
///
/// Error cases:
/// - Unknown `terminal_id` → `TERMINAL_NOT_FOUND`
/// - Actor unavailable → `INTERNAL_ERROR`
/// - Shell query fails → populate `shell_state: None`, continue gracefully
#[allow(clippy::too_many_lines)]
pub(crate) async fn handle_get_terminal_state(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::TerminalId,
    include_scrollback: bool,
    max_scrollback_lines: u16,
) -> CommandResult {
    use std::time::{SystemTime, UNIX_EPOCH};

    // Step 1: Resolve terminal_id to TerminalActor handle (same pattern as
    // handle_get_screen).
    let handle = state.with(|s| {
        s.terminal_from_wire(terminal_id)
            .and_then(|core| s.terminal_handle(core).cloned())
    });

    let Some(handle) = handle else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such terminal: {terminal_id:?}"),
        };
    };

    let pane = terminal_id.local_id().unwrap_or(0);

    // Step 2: Query screen state via ScreenRequest (reuse existing path).
    // This gives us canonical grid snapshot, scrollback (if requested), and
    // cell styling information.
    let (reply_tx, reply_rx) = oneshot::channel();
    if handle
        .screen
        .send(ScreenRequest {
            pane,
            scrollback: if include_scrollback {
                Some(u32::from(max_scrollback_lines))
            } else {
                None
            },
            cells: true, // Always request cells for semantic info (styles, OSC-133 marks)
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for GET_TERMINAL_STATE".to_owned(),
        };
    }

    let Ok(screen_state) = reply_rx.await else {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor dropped the GET_TERMINAL_STATE reply".to_owned(),
        };
    };

    // Step 3: Convert ScreenState viewport to JSON cells array.
    // ScreenState carries:
    // - lines: Vec<String> — viewport text, one row per element, right-trimmed
    // - cells: Option<Vec<CellInfo>> — sparse: only cells with non-default
    //   style or OSC-133 semantic marks, in row-major order
    //
    // We parse each line into characters and emit cells as JSON objects.
    // Note: a full implementation using unicode-segmentation::Graphemes
    // would handle combining marks, emoji, and wide glyphs more precisely;
    // for now we estimate width based on ASCII vs. non-ASCII.

    let mut viewport_cells = Vec::new();

    // Emit viewport cells by parsing each line.
    // Each line is right-trimmed, so we don't need to emit trailing blanks.
    #[allow(clippy::cast_possible_truncation)]
    for (row_idx, line_text) in screen_state.lines.iter().enumerate() {
        let row = row_idx as u16;
        let mut col = 0u16;

        for ch in line_text.chars() {
            // Estimate cell width: ASCII is 1 column, everything else is 2
            // (emoji, CJK). libghostty tracks actual widths; we approximate.
            let width = if ch.is_ascii() { 1u16 } else { 2u16 };

            // Emit this cell as JSON.
            viewport_cells.push(serde_json::json!({
                "col": col,
                "row": row,
                "text": ch.to_string(),
                "width": width as u8,
                "selected": false,
            }));

            col += width;
            // Stop if we exceed grid width (shouldn't happen in right-trimmed lines)
            if col >= screen_state.cols {
                break;
            }
        }
    }

    // Extract cursor state as JSON.
    let cursor = screen_state.cursor.map(|cs| {
        serde_json::json!({
            "x": cs.x,
            "y": cs.y,
            "visible": cs.visible,
        })
    });

    // Step 4: Convert scrollback lines to JSON.
    let mut scrollback_lines = Vec::new();
    #[allow(clippy::cast_possible_truncation)]
    let scrollback_count_total = screen_state.scrollback.len() as u32;

    if include_scrollback {
        for line_text in &screen_state.scrollback {
            scrollback_lines.push(serde_json::json!({
                "text": line_text,
                "cells": [],
            }));
        }
    }

    // Step 5: Query shell state.
    // The TerminalActor could provide shell PID (child of PTY master),
    // shell name, job list, and in_copy_mode. For now, set to None;
    // a future iteration adds a GetShellStateRequest channel and wires
    // shell state queries (phux-y2t Phase 2).
    //
    // Graceful degrade: if the actor has no PTY (no-PTY test actor),
    // or the query fails, leave shell_state as None. Agents can work
    // with partial snapshots.
    let shell_state: Option<serde_json::Value> = None;

    // Step 6: Compute timestamp and sequence number.
    let timestamp_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    // Sequence number is a logical clock maintained per terminal for change
    // detection. For now, placeholder; should be sourced from actor's state
    // in a future iteration (phux-y2t Phase 2). See ADR-0015 for the versioning model.
    let seq = 0u64;

    // Step 7: Build the TerminalState as JSON.
    let terminal_state_json = serde_json::json!({
        "cols": screen_state.cols,
        "rows": screen_state.rows,
        "cells": viewport_cells,
        "cursor": cursor,
        "scrollback": scrollback_lines,
        "scrollback_count_total": scrollback_count_total,
        "shell_state": shell_state,
        "pending_command": serde_json::Value::Null,
        "timestamp_secs": timestamp_secs,
        "seq": seq,
    });

    // Step 8: Serialize to JSON string and return.
    match serde_json::to_string(&terminal_state_json) {
        Ok(json) => CommandResult::OkWith(CommandValue::Json(json)),
        Err(err) => CommandResult::Error {
            code: ErrorCode::InternalError,
            message: format!("terminal state serialization failed: {err}"),
        },
    }
}

/// Build the `Ok` reply for `ROUTE_INPUT`.
///
/// The write counterpart to [`handle_get_screen`]: it resolves the wire id
/// to its pane actor with no attach / subscription gate and, crucially, no
/// resize. Production routes through the dedicated lane's encoder; the inline
/// path used by direct-drive tests feeds the actor's legacy event mailbox.
/// Unlike the ATTACH-then-`INPUT_KEY` path, routing input here
/// never transiently shrinks the pane to the caller's viewport; the live
/// dimensions are preserved (ADR-0022, `phux-3j3`).
///
/// `ROUTE_INPUT` is the side-effect-free agent path (ADR-0022): it
/// delivers input to a Terminal WITHOUT an attach or subscription, which is
/// exactly how `phux run` / `send-keys` drive a pane headlessly. It must
/// therefore NOT require the caller to be a subscriber. An earlier interim
/// gate (phux-nlo) approximated "PRIMARY" by subscription and rejected any
/// unsubscribed caller — but that is precisely the headless agent, so it
/// broke the agent surface; it is removed. v0.1 is single-trust-domain (one
/// server per user, ADR-0003), so there is no untrusted observer to fence
/// off here. Genuine viewer-vs-primary authority (SPEC `input.md` §7 /
/// `L1.md` §7.1) returns when per-connection roles are materialized, and
/// must gate an *attached read-only viewer*, never the headless
/// control-plane caller. `client_id` is kept for that future policy and for
/// the observability trace below.
///
/// Both the lane's encoded-byte handoff and the inline fallback use
/// non-blocking `try_send`: input is fire-and-forget per SPEC §9, so a
/// full mailbox drops the event rather than blocking the read loop. The
/// command still acks `Ok` (the event was accepted for delivery); an
/// unknown Terminal or a gone actor produces an `Error`.
#[derive(Debug)]
pub(crate) struct InputDestination {
    pub(crate) pane: phux_core::ids::TerminalId,
    pub(crate) handle: TerminalHandle,
}

/// Resolve and lease-gate a local headless destination, then run `action`
/// while the authority lock remains held.
pub(crate) fn with_route_input_destination<R>(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::TerminalId,
    action: impl FnOnce(InputDestination) -> R,
) -> Result<R, CommandResult> {
    if !terminal_id.is_local() {
        return Err(CommandResult::Error {
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: format!("ROUTE_INPUT to satellite route unsupported: {terminal_id:?}"),
        });
    }
    state.with(|s| {
        let Some(pane) = s.terminal_from_wire(terminal_id) else {
            return Err(CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                message: format!("no such terminal: {terminal_id:?}"),
            });
        };
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
        let Some(handle) = s.terminal_handle(pane).cloned() else {
            return Err(CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                message: format!("no such terminal: {terminal_id:?}"),
            });
        };
        Ok(action(InputDestination { pane, handle }))
    })
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
    terminal_id: &phux_protocol::ids::TerminalId,
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

/// Bridge `state::ClientId` (u64 newtype) → `phux_protocol::ClientId` (u32),
/// the wire id that rides in `TerminalControl` events (ADR-0033). Matches the
/// conversion the per-consumer state map and `FRAME_ACK` path already use; the
/// wire `ClientId` space caps at `u32::MAX` (widening needs a protocol bump).
fn wire_client_id(id: ClientId) -> phux_protocol::ids::ClientId {
    phux_protocol::ids::ClientId::new(u32::try_from(id.0).unwrap_or(u32::MAX))
}

/// Outcome of resolving an `ACQUIRE_INPUT` against the lease map (ADR-0033).
enum AcquireOutcome {
    /// The wire id resolved to no pane.
    NotFound,
    /// A cooperative acquire lost to an existing holder (carried for the
    /// diagnostic).
    Denied(ClientId),
    /// The lease was granted; broadcast the change via the pane's actor.
    Granted {
        /// The pane actor to notify.
        handle: Box<TerminalHandle>,
        /// `Acquired` (was free / self) or `Seized` (preempted another).
        action: ControlAction,
    },
}

/// Handle `ACQUIRE_INPUT` (ADR-0033, "take the wheel"): assert an exclusive
/// input lease over a pane. `Cooperative` mode fails with `InputLeaseHeld`
/// when another client holds it; `Seize` preempts. On grant, broadcasts a
/// `TerminalControl` event so every subscriber re-renders who has the wheel.
///
/// `ttl_ms` is advisory in this server: the lease is held until the holder
/// releases it or its connection drops (see [`crate::state::ServerState::detach`]).
pub(crate) async fn handle_acquire_input(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::TerminalId,
    mode: InputMode,
    _ttl_ms: u32,
) -> CommandResult {
    // No satellite guard here (phux-v45.11 finding 5): `route_to_satellite`
    // intercepts every satellite-tagged ACQUIRE_INPUT in `handle_command`
    // before local dispatch — on a hub it relays, elsewhere it resolves to
    // the typed UnsupportedSatelliteRoute reply. A satellite id can never
    // reach this function.
    let outcome = state.with_mut(|s| {
        let Some(core) = s.terminal_from_wire(terminal_id) else {
            return AcquireOutcome::NotFound;
        };
        let Some(handle) = s.terminal_handle(core).cloned() else {
            return AcquireOutcome::NotFound;
        };
        let prior = s.input_lease_holder(core);
        if mode == InputMode::Cooperative
            && let Some(holder) = prior
            && holder != client_id
        {
            return AcquireOutcome::Denied(holder);
        }
        s.set_input_lease(core, client_id);
        let action = match prior {
            Some(holder) if holder != client_id => ControlAction::Seized,
            _ => ControlAction::Acquired,
        };
        AcquireOutcome::Granted {
            handle: Box::new(handle),
            action,
        }
    });
    match outcome {
        AcquireOutcome::NotFound => CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such terminal: {terminal_id:?}"),
        },
        AcquireOutcome::Denied(holder) => CommandResult::Error {
            code: ErrorCode::InputLeaseHeld,
            message: format!("input lease held by client {}", holder.0),
        },
        AcquireOutcome::Granted { handle, action } => {
            let _ = handle
                .control
                .send(ControlRequest::LeaseChanged {
                    input_holder: Some(wire_client_id(client_id)),
                    action,
                    actor: wire_client_id(client_id),
                })
                .await;
            CommandResult::Ok
        }
    }
}

/// Handle `RELEASE_INPUT` (ADR-0033): drop the input lease the caller holds
/// over a pane, returning it to `Open`. Idempotent — a no-op (still `Ok`) if
/// the caller does not hold the lease. Broadcasts `Released` when a lease was
/// actually given up.
pub(crate) async fn handle_release_input(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::TerminalId,
) -> CommandResult {
    // No satellite guard here (phux-v45.11 finding 5): same rationale as
    // `handle_acquire_input` — `route_to_satellite` owns that dispatch.
    let released = state.with_mut(|s| {
        let core = s.terminal_from_wire(terminal_id)?;
        let handle = s.terminal_handle(core).cloned()?;
        Some((handle, s.release_input_lease(core, client_id)))
    });
    match released {
        None => CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such terminal: {terminal_id:?}"),
        },
        Some((handle, did_release)) => {
            if did_release {
                let _ = handle
                    .control
                    .send(ControlRequest::LeaseChanged {
                        input_holder: None,
                        action: ControlAction::Released,
                        actor: wire_client_id(client_id),
                    })
                    .await;
            }
            CommandResult::Ok
        }
    }
}

/// Handle `SIGNAL_TERMINAL` (ADR-0033): deliver a POSIX signal to the pane's
/// process group. Distinct from `KILL_TERMINAL` (which removes the pane) —
/// this signals the process and leaves the pane addressable. The actor owns
/// the PTY child pid, so the work happens there; the broadcast follows.
pub(crate) async fn handle_signal_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::TerminalId,
    signal: TerminalSignal,
) -> CommandResult {
    if !terminal_id.is_local() {
        return CommandResult::Error {
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: format!("SIGNAL_TERMINAL on satellite route unsupported: {terminal_id:?}"),
        };
    }
    let resolved = state.with(|s| {
        let core = s.terminal_from_wire(terminal_id)?;
        let holder = s.input_lease_holder(core).map(wire_client_id);
        s.terminal_handle(core).cloned().map(|h| (h, holder))
    });
    let Some((handle, input_holder)) = resolved else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such terminal: {terminal_id:?}"),
        };
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    if handle
        .control
        .send(ControlRequest::Signal {
            signal,
            input_holder,
            by: wire_client_id(client_id),
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for SIGNAL_TERMINAL".to_owned(),
        };
    }
    match reply_rx.await {
        Ok(Ok(())) => CommandResult::Ok,
        Ok(Err(msg)) => CommandResult::Error {
            code: ErrorCode::InternalError,
            message: msg,
        },
        Err(_) => CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor dropped SIGNAL_TERMINAL reply".to_owned(),
        },
    }
}

/// Handle `SUBSCRIBE_TERMINAL_EVENTS` command.
///
/// Resolves the wire `terminal_id` to a pane actor and registers the caller
/// as an event subscriber. The server will broadcast semantic events
/// (`CommandStarted`, `CommandEnded`, `GridChanged`, etc.) as they occur, filtered
/// by `event_types` (empty = all types). The subscription persists until the
/// client detaches or the connection closes.
///
/// Replies `CommandResult::Ok` immediately; events flow asynchronously as
/// `Event` frames to the client's outbound mailbox. `try_send` semantics:
/// a full subscriber mailbox drops events (accelerator semantics, not
/// guaranteed delivery).
pub(crate) fn handle_subscribe_terminal_events(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::TerminalId,
    event_types: Vec<phux_protocol::wire::frame::TerminalEventType>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> CommandResult {
    use crate::terminal_actor::{SubscribeToEventsRequest, TerminalEventSubscriber};

    // Resolve the wire id to its pane actor (same pattern as handle_route_input).
    let handle = state.with(|s| {
        let core = s.terminal_from_wire(terminal_id)?;
        s.terminal_handle(core).cloned()
    });

    let Some(handle) = handle else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such terminal: {terminal_id:?}"),
        };
    };

    debug!(
        ?client_id,
        ?terminal_id,
        "SUBSCRIBE_TERMINAL_EVENTS registering"
    );

    // Get the wire terminal id for use in Event frames.
    let wire_terminal_id = terminal_id.local_id().unwrap_or(0);

    // Build the subscriber request and send to the actor.
    // The subscriber receives the client's outbound mailbox directly,
    // so events are forwarded straight to the client without an intermediary.
    let req = SubscribeToEventsRequest {
        subscriber: TerminalEventSubscriber {
            outbound: out_tx.clone(),
            event_types,
        },
        wire_terminal_id,
    };

    if handle.subscribe_to_events.try_send(req).is_err() {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for SUBSCRIBE_TERMINAL_EVENTS".to_owned(),
        };
    }

    debug!(
        ?client_id,
        ?terminal_id,
        "SUBSCRIBE_TERMINAL_EVENTS: subscriber registered"
    );
    CommandResult::Ok
}

pub(crate) fn handle_report_asked(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::TerminalId,
    id: String,
    question: String,
    suggestions: Vec<String>,
    elapsed_seconds: Option<u64>,
) -> CommandResult {
    if !terminal_id.is_local() {
        return CommandResult::Error {
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: format!("REPORT_ASKED on satellite route unsupported: {terminal_id:?}"),
        };
    }
    let Some(terminal) = state.with(|s| s.terminal_from_wire(terminal_id)) else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such terminal: {terminal_id:?}"),
        };
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
    let transition = state.with_mut(|s| s.report_agent_asked(terminal, AskedSource::Hook, payload));
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
    use phux_protocol::ids::{SessionId, TerminalId, WindowId};
    phux_protocol::wire::info::SessionSnapshot::new(
        SessionId::new(0),
        WindowId::new(0),
        TerminalId::local(0),
    )
}

/// Handle a client's `VIEWPORT_RESIZE` (SPEC §7.1 / §10.5).
///
/// Look up the client's currently-focused pane and update the in-memory
/// `dims` so future `TERMINAL_SNAPSHOT` frames reflect the new size. This is
/// the additive surface for phux-4hp: we deliberately do NOT push a
/// resize into the [`TerminalActor`] (or call `Terminal::set_size` /
/// `pty.resize(...)`) because byc.5's PTY pump owns the actor-side
/// `Terminal` / `portable-pty` resize integration. The follow-up there
/// will consume this state change (or, if it prefers a direct channel,
/// can add a new `TerminalHandle` channel without touching this code).
///
/// Per SPEC §10.5, when multiple clients are attached with different
/// sizes the server uses the smallest common bounding box per window.
/// That negotiation lives with byc.5 too; today the last writer wins,
/// which matches single-attach behavior (the only path exercised).
///
/// Silent on every "not-found" path. A `VIEWPORT_RESIZE` from an
/// unattached client is a benign race (the client may have sent it
/// before its ATTACH completed); logging at `debug!` is enough.
pub(crate) fn handle_viewport_resize(
    state: &SharedState,
    client_id: ClientId,
    viewport: &ViewportInfo,
) {
    state.with_mut(|s| {
        let Some(client) = s.attached.get(&client_id) else {
            debug!(
                ?client_id,
                "VIEWPORT_RESIZE from non-attached client; ignoring"
            );
            return;
        };
        let session_id = client.session;
        let Some(session) = s.registry.session(session_id) else {
            debug!(?client_id, "VIEWPORT_RESIZE: client's session vanished");
            return;
        };
        let Some(window_id) = session.active else {
            debug!(?client_id, "VIEWPORT_RESIZE: no active window in session");
            return;
        };
        let Some(window) = s.registry.window(window_id) else {
            return;
        };
        let Some(terminal_id) = window.active else {
            return;
        };
        // phux-nk07: record this client's viewport, then resolve the
        // Terminal's authoritative geometry by applying the window-size policy
        // across EVERY subscriber's viewport — not last-writer-wins, which let
        // two differently-sized clients thrash each other's grid. `Manual` (or
        // no usable viewport yet) yields `None`: leave the PTY size untouched.
        s.set_client_viewport(client_id, *viewport);
        let Some((cols, rows)) = s.resolve_terminal_geometry(terminal_id, Some(*viewport)) else {
            debug!(
                ?client_id,
                ?terminal_id,
                "VIEWPORT_RESIZE: window-size policy yielded no geometry; PTY size unchanged",
            );
            return;
        };
        if let Some(pane) = s.registry.terminal_mut(terminal_id) {
            pane.dims = (cols, rows);
        }
        // Pixel geometry rides along: the most recent usable pixel report
        // among this Terminal's subscribers — normally the viewport just
        // recorded above — fixes the cell size the PTY winsize and
        // XTWINOPS replies advertise.
        let cell_px = s.resolve_terminal_cell_px(terminal_id);
        // Fan the resize out to the TerminalActor so libghostty's
        // `Terminal::set_size` and the PTY `winsize` ioctl get
        // updated. byc.5 added the `resize` channel on `TerminalHandle`;
        // this is the missing connector (4hp ↔ byc.5).
        //
        // We hold the state lock here so `try_send` is the right
        // primitive: VIEWPORT_RESIZE is fire-and-forget per SPEC §10.5,
        // and an `.await` inside `with_mut` would deadlock the
        // single-threaded runtime. On send failure (actor terminated,
        // mailbox full — both rare; the resize mailbox is sized at
        // `DEFAULT_INPUT_MAILBOX` = 64), we log and continue: a
        // dropped resize is recoverable (the next resize, or the
        // next snapshot, re-syncs) and SPEC §10.5 explicitly classes
        // VIEWPORT_RESIZE as best-effort.
        if let Some(handle) = s.terminals.get(&terminal_id) {
            // Live viewport resize (SIGWINCH): resync clients (phux-8v1).
            match handle.resize.try_send(ResizeRequest {
                cols,
                rows,
                cell_px,
                resync_clients: true,
                resync_only: false,
            }) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    warn!(
                        ?client_id,
                        ?terminal_id,
                        cols,
                        rows,
                        "VIEWPORT_RESIZE: pane resize mailbox full; dropping (fire-and-forget per SPEC §10.5)",
                    );
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    debug!(
                        ?client_id,
                        ?terminal_id,
                        "VIEWPORT_RESIZE: pane actor gone; dropping resize",
                    );
                }
            }
        } else {
            debug!(
                ?client_id,
                ?terminal_id,
                "VIEWPORT_RESIZE: no TerminalHandle registered for pane; dropping resize",
            );
        }
    });
}

/// Route an `INPUT_*` frame body to the target pane's [`TerminalActor`].
///
/// SPEC §9: input frames are fire-and-forget — no `Outbound` reply.
/// On the wire the pane is identified by its `WireTerminalId` (`u32`); we
/// resolve it back to a core [`phux_core::ids::TerminalId`] via
/// [`crate::state::ServerState::terminal_from_wire`],
/// then locate the [`TerminalHandle`] and `try_send` the encoded
/// [`TerminalInput`] onto the actor's input mailbox.
///
/// Validation: we drop with `warn!` (not `debug!`, this is observable
/// misbehavior worth surfacing) on:
///   * Unknown wire pane id (no [`phux_core::ids::TerminalId`] mapping).
///   * Client not subscribed to this pane — prevents one client from
///     steering another's pane (SPEC §9 leaves multi-client subscription
///     rules to per-pane policy; subscription is the gate). Subscription
///     is established by the session-scoped `ATTACH` or the per-terminal
///     `ATTACH_TERMINAL` (phux-v45.7) — a session attachment is NOT
///     required, because the federation hub's link consumer drives
///     satellite panes with `ATTACH_TERMINAL` alone.
///   * Pane has no registered [`TerminalHandle`] (actor never spawned, or
///     spawned but evicted).
///
/// `try_send` is used because we hold the `with_mut` lock while routing:
/// awaiting inside a `with_mut` would deadlock the single-threaded
/// runtime, and an unbounded queue would let a slow PTY producer push
/// memory through the roof. `Full` is treated as a backpressure event
/// (warn-drop); `Closed` is logged at debug and dropped (actor gone).
/// The satellite branch of [`handle_terminal_input`] (phux-v45.4):
/// rebuild the wire `INPUT_*` frame with the id rewritten satellite-local
/// and forward it verbatim over the owning hub link; warn-drop when this
/// server has no route (the non-hub contract).
fn relay_satellite_input(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::TerminalId,
    input: TerminalInput,
    frame_label: &'static str,
) {
    // Hub-side lease gate (phux-v45.7, L1 §9.1): the satellite cannot
    // distinguish hub consumers (they share the link identity), so the
    // ADR-0033 "another client holds the wheel" drop must happen here.
    // Dropped, not errored — the fire-and-forget input invariant holds,
    // exactly like the local gate in `handle_terminal_input`.
    if let Some((host, id)) = crate::hub::relay::satellite_route(wire_terminal_id)
        && state.with(|s| {
            s.satellite_lease_holder(&host, id)
                .is_some_and(|holder| holder != client_id)
        })
    {
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
            state,
            client_id,
            wire_terminal_id,
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
            "input frame carried a SATELLITE TerminalId on a non-federation-hub server; dropping",
        );
    }
}

/// Apply subscription, lease, and activity gates for attached local input,
/// then running `action` with the generational pane and current actor handle
/// while the authority lock remains held.
pub(crate) fn with_attached_input_destination<R>(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::TerminalId,
    frame_label: &'static str,
    action: impl FnOnce(InputDestination) -> R,
) -> Option<R> {
    state.with_mut(|s| {
        let Some(pane) = s.terminal_from_wire(wire_terminal_id) else {
            warn!(
                ?client_id,
                ?wire_terminal_id,
                frame_label,
                "input frame for unknown pane; dropping"
            );
            return None;
        };
        if !s.subscribers_for_terminal(pane).contains(&client_id) {
            warn!(
                ?client_id,
                ?wire_terminal_id,
                frame_label,
                "client not subscribed to pane (no ATTACH or ATTACH_TERMINAL); dropping input"
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
        if let Some(attached) = s.attached.get(&client_id) {
            s.touch_session(attached.session);
        }
        let Some(handle) = s.terminal_handle(pane).cloned() else {
            warn!(
                ?client_id,
                ?wire_terminal_id,
                frame_label,
                "no TerminalHandle for pane; dropping input"
            );
            return None;
        };
        Some(action(InputDestination { pane, handle }))
    })
}

pub(crate) fn handle_terminal_input(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::TerminalId,
    input: TerminalInput,
    frame_label: &'static str,
) {
    // Satellite-routed input (phux-v45.4): on a hub, forward the frame
    // verbatim over the owning link with the id rewritten to the
    // satellite's Local space — the satellite applies its own routing
    // gates (see `relay_satellite_frame`'s scope note). Non-hub servers
    // keep the ADR-0016 / SPEC §10.1 behavior: drop with a warn (the
    // protocol-level response is `ERROR { UnsupportedSatelliteRoute }`;
    // surfacing it from this fire-and-forget helper is still a follow-up
    // tied to phux-byc.9).
    if !wire_terminal_id.is_local() {
        relay_satellite_input(state, client_id, wire_terminal_id, input, frame_label);
        return;
    }
    // docs/consumers/tui.md §9 (phux-r82.1): an INPUT_FOCUS gained event
    // that passes every routing gate below means a client's focus landed
    // on this pane — the `focus-changed` hook point. Computed up front
    // because `input` moves into the closure; fired AFTER the `with_mut`
    // scope closes (the hook helper re-takes the state lock).
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

/// Route an inbound `FRAME_ACK` (SPEC §7.proto.1 / §12.2) to the
/// owning `TerminalActor` so it can evict the per-consumer dirty cache
/// under ADR-0018 lazy state synchronization (phux-q0e.4).
///
/// Validation:
///   * Unknown wire pane id → drop (warn). The client is acking a
///     terminal the server has no mapping for; this is observable
///     misbehavior worth surfacing.
///   * Client not subscribed to this pane → drop (warn). Same gate as
///     `handle_terminal_input`: a client cannot ack a pane it does not
///     observe. Subscription comes from `ATTACH` or `ATTACH_TERMINAL`
///     (phux-v45.7); no session attachment is required.
///   * No `TerminalHandle` (actor evicted) → drop (debug — race against
///     teardown).
///
/// `try_send` is non-blocking by the same `with_mut` locking rationale
/// as `handle_terminal_input`: awaiting inside `with_mut` would
/// deadlock the single-threaded runtime, and `FRAME_ACK` is hint-shaped
/// per ADR-0018 — dropping under backpressure is correct (the next
/// ack the client sends will catch up the per-consumer reference,
/// and unacked diffs stay re-emittable in the meantime).
pub(crate) fn handle_frame_ack(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::TerminalId,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    seq: u64,
) {
    // Satellite-routed acks relay like input frames (phux-v45.4): forward
    // verbatim on a hub, warn-drop off one. FRAME_ACK is hint-shaped
    // (ADR-0018), so the bounded-relay drop contract is safe here too.
    if !wire_terminal_id.is_local() {
        let relayed =
            relay_satellite_frame(state, client_id, wire_terminal_id, "FRAME_ACK", |id| {
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
                "FRAME_ACK carried a SATELLITE TerminalId on a non-federation-hub server; dropping",
            );
        }
        return;
    }
    state.with_mut(|s| {
        let Some(pane) = s.terminal_from_wire(wire_terminal_id) else {
            warn!(
                ?client_id,
                ?wire_terminal_id,
                seq,
                "FRAME_ACK for unknown pane; dropping",
            );
            return;
        };
        // Same gate as `handle_terminal_input` (phux-v45.7): subscription
        // — established by ATTACH or ATTACH_TERMINAL — is the ack gate; a
        // session attachment is not required (the federation hub's link
        // consumer acks relayed frames without one).
        let is_subscribed = s.subscribers_for_terminal(pane).contains(&client_id);
        if !is_subscribed {
            warn!(
                ?client_id,
                ?wire_terminal_id,
                seq,
                "FRAME_ACK from client not subscribed to pane; dropping",
            );
            return;
        }
        let Some(handle): Option<&TerminalHandle> = s.terminal_handle(pane) else {
            warn!(
                ?client_id,
                ?wire_terminal_id,
                seq,
                "FRAME_ACK with no TerminalHandle for pane; dropping",
            );
            return;
        };
        // Bridge `state::ClientId` (u64 newtype) → `phux_protocol::ClientId`
        // (u32), matching the conversion `handle_attach` already does for
        // the per-consumer state map keys. The wire ClientId space caps at
        // u32::MAX; widening would require a protocol bump.
        let wire_client_id =
            phux_protocol::ids::ClientId::new(u32::try_from(client_id.0).unwrap_or(u32::MAX));
        match handle.consumer_ack.try_send(ConsumerAckRequest {
            client_id: wire_client_id,
            stream_id,
            bootstrap_id,
            seq,
        }) {
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
    });
}
