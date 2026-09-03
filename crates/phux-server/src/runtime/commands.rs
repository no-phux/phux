//! Submodule for runtime internals.

use bytes::Bytes;
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ControlAction, DetachReason, ErrorCode,
    FrameKind, InputMode, StateScope, TerminalLifecycle, TerminalSignal, ViewportInfo,
};
use std::collections::HashSet;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use super::input_lane::InputLaneHandle;
use super::{
    AttachPrepared, broadcast_event, spawn_agent_state_drain, spawn_pane_event_drain,
    spawn_terminal_exit_watcher,
};
use crate::agent_asked::{AskedPayload, AskedSource};
use crate::runtime::pump::{self, PumpGeneration};
use crate::state::{ClientId, Outbound, SharedState, TerminalInput};
use crate::terminal_actor::{
    ConsumerAckRequest, ControlRequest, EncodedInputRequest, ResizeRequest, ScreenRequest,
    TerminalActor, TerminalHandle,
};

/// The grid a pane is built at when nothing better is known: the classic
/// VT100 default, and the same dims `phux_core::Registry::new_terminal`
/// stamps on a fresh descriptor.
///
/// Every path that reaches this constant is one where the eventual geometry
/// arrives later — a seed pane whose attaching client applies its viewport
/// through `apply_attach_viewport` before any bootstrap exists, or a spawn
/// from a caller that did not supply `SPAWN_TERMINAL.initial_size`. A
/// layout-owning consumer that DOES know the tile should send it (phux-a5xj)
/// rather than let the pane bootstrap here and be reflowed afterwards.
pub(crate) const DEFAULT_SPAWN_DIMS: (u16, u16) = (80, 24);

/// Announce a freshly-seeded session's **first** pane on the event stream
/// (phux-8uly, [SPEC](../../../../docs/spec/L1.md) §7.1).
///
/// `handle_spawn_terminal` already broadcasts `pane_spawned` for every pane
/// it adds to an *existing* session, but a session's seed pane is created
/// by the helpers below instead and used to announce nothing. That left a
/// hole in push coverage of the session lifecycle — death is carried by
/// `pane_closed` and rename by `METADATA_CHANGED`, while creation was
/// silent — so a server-wide follower (ADR-0089's fleet-inbox roster, the
/// planned `phux agent wait --any`) could not observe a session that
/// appeared after it subscribed until the new pane happened to emit
/// something else.
///
/// Deliberately identical to the spawn path's emission: same helper, same
/// pane scope, same event shape, so a subscriber cannot tell a seeded
/// pane's announcement from a spawned one's. Fanout is best-effort and
/// resolves subscribers at emit time, so this is called once the pane's
/// actor is live and its wire id is interned.
///
/// Both seed helpers call this, covering the attach `CreateIfMissing` path
/// and the headless [`phux_protocol::wire::frame::SESSION_CREATE_KEY`]
/// path. Neither helper is reachable from `handle_spawn_terminal` (which
/// seeds through `spawn_pane_with_pty_and_colors`), so no pane is announced
/// twice.
fn announce_seed_pane(state: &SharedState, wire_terminal_id: &phux_protocol::ids::TerminalId) {
    broadcast_event(state, Some(wire_terminal_id), &AgentEvent::PaneSpawned);
}

pub(crate) fn seed_session_with_actor(
    state: &SharedState,
    name: &str,
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
) -> Result<phux_core::ids::TerminalId, crate::terminal_actor::TerminalActorError> {
    seed_session_with_actor_and_metadata(state, name, scrollback, root_token, None)
}

fn seed_session_with_actor_and_metadata(
    state: &SharedState,
    name: &str,
    scrollback: phux_config::ScrollbackLimits,
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
    // No-PTY actor: nothing to size against, so the default stands until a
    // client's viewport arrives (phux-4hp's VIEWPORT_RESIZE wiring).
    let terminal_token = root_token.child_token();
    let (cols, rows) = DEFAULT_SPAWN_DIMS;
    let bundle =
        match TerminalActor::build_with_token(cols, rows, None, scrollback, terminal_token.clone())
        {
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
    announce_seed_pane(state, &wire_terminal_id);
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
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
) -> Result<phux_core::ids::TerminalId, crate::terminal_actor::TerminalActorError> {
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
) -> Result<phux_core::ids::TerminalId, crate::terminal_actor::TerminalActorError> {
    seed_session_with_pty_and_colors_and_metadata(
        state,
        name,
        cmd,
        scrollback,
        root_token,
        default_colors,
        None,
    )
}

fn seed_session_with_pty_and_colors_and_metadata(
    state: &SharedState,
    name: &str,
    mut cmd: portable_pty::CommandBuilder,
    scrollback: phux_config::ScrollbackLimits,
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
    let (cols, rows) = DEFAULT_SPAWN_DIMS;
    let bundle = match TerminalActor::build_with_token_and_colors(
        cols,
        rows,
        Some(cmd),
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
    announce_seed_pane(state, &wire_terminal_id);
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
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
) -> Result<Option<phux_core::ids::TerminalId>, crate::terminal_actor::TerminalActorError> {
    spawn_pane_with_pty_and_colors(
        state,
        &SpawnOwnership::Session(session),
        cmd,
        scrollback,
        root_token,
        None,
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
///
/// `initial_size` is the `(cols, rows)` the caller already knows the new
/// leaf will occupy (phux-a5xj, `SPAWN_TERMINAL.initial_size`). It sizes the
/// libghostty grid, the PTY winsize, and the registry `dims` in the same
/// transaction that creates the pane, so the bootstrap generation the server
/// then captures is already the client's real geometry and the reflow
/// `TERMINAL_RESIZE` that follows is a no-op instead of a tombstone. `None`
/// keeps [`DEFAULT_SPAWN_DIMS`].
#[allow(
    clippy::too_many_arguments,
    reason = "one pane-creation transaction: ownership, argv, history bound, cancellation, palette, resume provenance, and geometry all have to be in hand before the actor is built, because every one of them must be true of the pane before it becomes visible to another client. A parameter struct would name the same set once instead of at each of the three call sites."
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
) -> Result<Option<phux_core::ids::TerminalId>, crate::terminal_actor::TerminalActorError> {
    use phux_core::ids::TerminalId;
    // Clamp exactly as `TerminalActor::handle_resize` does: libghostty has no
    // zero-dimension grid. Callers upstream already drop an all-zero hint, so
    // this is belt-and-braces for the in-process call sites.
    let (cols, rows) = initial_size.map_or(DEFAULT_SPAWN_DIMS, |(cols, rows)| {
        (cols.max(1), rows.max(1))
    });
    // phux-p4vp: same spawn-time cwd capture as `seed_session_with_pty`.
    let spawn_cwd = spawn_cwd_of(&cmd);
    let Some(terminal): Option<TerminalId> = state.with_mut(|s| {
        let terminal = match ownership {
            SpawnOwnership::Session(session) => s.add_pane_to_session(*session)?,
            SpawnOwnership::Terminal(owner) => s.add_pane_to_terminal_owner(owner)?,
        };
        stamp_spawn_cwd(s, terminal, spawn_cwd);
        // Keep the registry's recorded dims in step with the grid the actor
        // is about to be built at, so `GET_STATE` and the ATTACHED snapshot
        // report the pane's real geometry from its first instant rather than
        // the `Registry::new_terminal` 80x24 placeholder.
        if let Some(pane) = s.registry_mut().terminal_mut(terminal) {
            pane.dims = (cols, rows);
        }
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
        cols,
        rows,
        Some(cmd),
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
        let window = s.registry().terminal(terminal)?.window;
        let session = s.registry().window(window)?.session;
        s.registry().session(session).map(|sess| sess.name.clone())
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
        && let Some(desc) = s.registry_mut().terminal_mut(terminal)
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
        if let Some(pane) = s.registry_mut().terminal_mut(terminal) {
            pane.dims = (cols, rows);
        }
        let Some(handle) = s.terminal_handle(terminal) else {
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
/// [`crate::state::ServerState::attach`], build the snapshot, and collect
/// both the pane handles that can bootstrap and snapshot panes that must be
/// authoritatively closed before `ATTACH_READY`.
///
/// The snapshot is a whole-workspace catalog, while only the focused session
/// participates in this attach generation. Within that session, a registry
/// pane without an actor handle cannot produce `BOOTSTRAP_*`; returning it in
/// the fourth tuple slot lets the publisher resolve the client's attach
/// barrier with `TERMINAL_CLOSED` instead of silently stranding it.
///
/// Pulled out so [`crate::runtime::attach::handle_attach`] stays under clippy's
/// `too_many_lines` ceiling.
pub(crate) fn prepare_attach(
    state: &SharedState,
    client_id: ClientId,
    session_name: &str,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
) -> Result<AttachPrepared, crate::state::AttachError> {
    state.with_mut(|s| {
        let pane_count = s
            .session_by_name(session_name)
            .ok_or_else(|| crate::state::AttachError::UnknownSession(session_name.to_owned()))?
            .windows
            .iter()
            .filter_map(|window_id| s.registry().window(*window_id))
            .try_fold(0_usize, |count, window| {
                count.checked_add(window.panes.len())
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
            .panes
            .iter()
            .filter(|pane| {
                focused_windows.contains(&pane.window_id) && !bootstrapped.contains(&pane.id)
            })
            .map(|pane| pane.id.clone())
            .collect();
        let initial_client_id =
            phux_protocol::ids::ClientId::new(u32::try_from(client_id.0).unwrap_or(u32::MAX));
        Ok((
            snapshot,
            initial_client_id,
            panes_to_snapshot,
            closed_before_ready,
        ))
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
        Command::GetPerf { .. } => "get_perf",
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
) {
    // UPGRADE is handled out-of-band: `handle_upgrade` acks the client itself
    // and then re-execs the process, so it never returns a `CommandResult` for
    // the shared send below (ADR-0032).
    if matches!(command, Command::Upgrade) {
        handle_upgrade(state, request_id, out_tx).await;
        return;
    }

    // SHUTDOWN is the other command whose subject is the server rather than a
    // Terminal, and it is out-of-band for the same reason as its sibling: it
    // acks and then ends the process, so there is no `CommandResult` left for
    // the shared send below (phux-pimp).
    if matches!(command, Command::Shutdown) {
        handle_shutdown(state, client_id, request_id, out_tx, root_token).await;
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
                    bootstrap_profile,
                    bootstrap_limits,
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
            bootstrap_profile,
            bootstrap_limits,
        )
        .await;
        return;
    }

    let result = match command {
        Command::AttachTerminal { terminal_id } => {
            handle_attach_terminal(
                state,
                client_id,
                &terminal_id,
                out_tx,
                client_caps,
                bootstrap_profile,
                bootstrap_limits,
                connection_token,
            )
            .await
        }
        Command::DetachTerminal { terminal_id } => {
            handle_detach_terminal(state, client_id, &terminal_id)
        }
        Command::GetState { scope } => handle_get_state_federated(state, &scope, out_tx).await,
        Command::GetPerf { reset } => handle_get_perf(state, reset),
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
        Command::ReportAgentState {
            terminal_id,
            state: reported,
        } => handle_report_agent_state(state, &terminal_id, reported).await,
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
    clippy::too_many_arguments,
    reason = "the negotiated connection context (caps, bootstrap profile, bootstrap limits) is threaded verbatim from the dispatch surface; boxing it here would only rename the same list"
)]
async fn handle_attach_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::TerminalId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_caps: ClientCapabilities,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    connection_token: &CancellationToken,
) -> CommandResult {
    let Some(stream_profile) = crate::runtime::attach::bootstrap_stream_profile(bootstrap_profile)
    else {
        return CommandResult::Error {
            code: ErrorCode::CodecUnavailable,
            message: "ATTACH_TERMINAL selected an unsupported bootstrap profile".to_owned(),
        };
    };

    let Some((core, handle)) = subscribe_attach_terminal(state, client_id, terminal_id, out_tx)
    else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such terminal: {terminal_id:?}"),
        };
    };

    // Allocated before the session takes ownership of the handle: exhausting
    // the id space leaves the subscription registered above in place, exactly
    // as it did when this was one linear body.
    let Some(bootstrap_id) = state.with_mut(|s| s.next_attach_terminal_bootstrap_id(client_id))
    else {
        return CommandResult::Error {
            code: ErrorCode::ResourceExhausted,
            message: "ATTACH_TERMINAL bootstrap id space exhausted".to_owned(),
        };
    };

    let session = AttachTerminalSession {
        state,
        out_tx,
        connection_token,
        terminal_id,
        core,
        handle,
        client_id,
        stream_id: crate::runtime::attach::stream_id_from(client_id.0),
        client_caps,
        stream_profile,
        bootstrap_limits,
    };

    let mut generation = match session.establish_generation(bootstrap_id).await {
        Ok(generation) => generation,
        Err(failure) => return session.failed(failure),
    };

    if let Some(state_sync) = generation.state_sync_bootstrap.take() {
        return match session.finish_state_sync(&generation, state_sync).await {
            Ok(()) => CommandResult::Ok,
            Err(failure) => session.failed(failure),
        };
    }

    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    if native_checkpoint_profile(stream_profile) {
        return match session.finish_native(&mut generation).await {
            Ok(()) => CommandResult::Ok,
            Err(failure) => session.failed(failure),
        };
    }

    match session.finish_snapshot(&mut generation).await {
        Ok(()) => CommandResult::Ok,
        Err(failure) => session.failed(failure),
    }
}

/// Resolve the wire terminal id and register the caller as an output
/// subscriber in one critical section.
///
/// Codec selection is connection-scoped and comes directly from HELLO; it is
/// never re-probed or replaced with compatibility defaults here.
///
/// The caller's mailbox is captured with the subscription (phux-w7z2.56):
/// `ATTACH_TERMINAL` does not require a session-scoped `ATTACH` (L1 §5.1),
/// so this consumer may have no `attached` entry, and the server's
/// out-of-band terminal-scoped fanout — `TERMINAL_CLOSED`, which L1 §3.1
/// requires for "every client subscribed to the Terminal" — has no other
/// way to reach it. Content rides the pump; lifecycle does not.
fn subscribe_attach_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::TerminalId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> Option<(phux_core::ids::TerminalId, TerminalHandle)> {
    state.with_mut(|s| {
        let core = s.terminal_from_wire(terminal_id)?;
        let handle = s.terminal_handle(core).cloned()?;
        s.subscribe_terminal(client_id, core, Some(out_tx.clone()));
        Some((core, handle))
    })
}

/// Whether a negotiated bootstrap profile streams incremental libghostty
/// checkpoints rather than a synthesized VT bootstrap.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
const fn native_checkpoint_profile(profile: phux_protocol::caps::BootstrapStreamProfile) -> bool {
    matches!(
        profile,
        phux_protocol::caps::BootstrapStreamProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2
        }
    )
}

/// Why an `ATTACH_TERMINAL` stage failed.
///
/// Carried back to [`AttachTerminalSession::failed`] so the partial attach is
/// rolled back exactly once on the way to the caller's `COMMAND_RESULT`.
#[derive(Debug)]
struct AttachTerminalFailure {
    /// Wire error code the caller receives.
    code: ErrorCode,
    /// Human-readable explanation attached to that code.
    message: String,
}

impl AttachTerminalFailure {
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
struct AttachTerminalGeneration {
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

/// One in-flight `ATTACH_TERMINAL`: the resolved terminal plus the negotiated
/// connection context every stage of the handshake needs.
///
/// The stages run as methods so the `resolve -> subscribe -> register -> pump
/// -> snapshot` ordering stays explicit without threading a dozen arguments
/// through each one, and so every failure funnels through the single
/// [`Self::failed`] rollback.
struct AttachTerminalSession<'a> {
    state: &'a SharedState,
    out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    connection_token: &'a CancellationToken,
    terminal_id: &'a phux_protocol::ids::TerminalId,
    core: phux_core::ids::TerminalId,
    handle: TerminalHandle,
    client_id: ClientId,
    stream_id: phux_protocol::ids::StreamId,
    client_caps: ClientCapabilities,
    stream_profile: phux_protocol::caps::BootstrapStreamProfile,
    bootstrap_limits: BootstrapLimits,
}

impl AttachTerminalSession<'_> {
    /// Whether this connection negotiated the actor-emitted state-sync stream
    /// at HELLO.
    const fn wants_state_sync(&self) -> bool {
        matches!(
            self.client_caps.output_mode,
            phux_protocol::caps::OutputMode::StateSync
        )
    }

    /// Undo a partial attach: cancel the generation, drop the subscription,
    /// release any native lease, and detach the per-consumer state entry.
    fn roll_back(&self) {
        use crate::terminal_actor::ConsumerDetachRequest;

        self.state.with_mut(|s| {
            s.cancel_attach_terminal_pump(self.client_id, self.core);
            s.unsubscribe_terminal(self.client_id, self.core);
        });
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let _ = self
            .handle
            .native_release
            .try_send(crate::terminal_actor::NativeReleaseRequest {
                owner: self.client_id.0,
            });
        let (reply, _ack) = oneshot::channel();
        let _ = self.handle.consumer_detach.try_send(ConsumerDetachRequest {
            client_id: wire_client_id(self.client_id),
            reply,
        });
    }

    /// Roll the partial attach back and shape the stage failure as the
    /// caller's `COMMAND_RESULT`.
    fn failed(&self, failure: AttachTerminalFailure) -> CommandResult {
        self.roll_back();
        CommandResult::Error {
            code: failure.code,
            message: failure.message,
        }
    }

    /// Register the per-consumer state-sync entry (ADR-0018) so `FRAME_ACK`
    /// from this consumer drives the actor's eviction loop.
    ///
    /// `None` when the terminal is not local or the actor refused the
    /// registration. `StateSync` cannot degrade to the raw broadcast path: its
    /// selected wire profile requires this actor-owned generation to exist
    /// before bootstrap publication.
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
                client_id: wire_client_id(self.client_id),
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
                // phux-v45.8: `ATTACH_TERMINAL` over a reliable transport; the
                // emit-once model is correct. Forwarded-leg loss-tolerance is
                // the deferred activation (ADR-0042).
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
    ) -> Result<AttachTerminalGeneration, AttachTerminalFailure> {
        // Subscribe before stopping the old pump so replacement never loses a
        // byte emitted in the handoff window. The new receiver remains gated
        // until this generation reaches READY.
        let output_rx = self.handle.output.subscribe();
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
            return Err(AttachTerminalFailure::internal(
                "ATTACH_TERMINAL state-sync registration failed",
            ));
        }
        let tick_managed = outcome.as_ref().is_some_and(|outcome| outcome.tick_managed);
        if tick_managed {
            // No raw pump owns this generation; replacement may proceed without
            // waiting on a task that will never exist.
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
            return Err(AttachTerminalFailure::internal(
                "consumer went away during ATTACH_TERMINAL replacement",
            ));
        }

        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let (native_publication_gate_tx, native_publication_gate_rx) =
            oneshot::channel::<crate::terminal_actor::NativePublicationReply>();
        let snapshot_gate = if tick_managed {
            None
        } else {
            let Some(pump_done_guard) = pump_done_guard.take() else {
                return Err(AttachTerminalFailure::internal(
                    "ATTACH_TERMINAL pump generation lost its completion guard",
                ));
            };
            Some(self.spawn_output_pump(AttachTerminalPumpSpawn {
                bootstrap_id,
                generation_last_seq: std::sync::Arc::clone(&generation_last_seq),
                token,
                output_rx,
                pump_done_guard,
                #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                native_publication_gate: native_publication_gate_rx,
            }))
        };

        Ok(AttachTerminalGeneration {
            bootstrap_id,
            generation_last_seq,
            live_gate_tx,
            snapshot_gate,
            state_sync_bootstrap: outcome.and_then(|outcome| outcome.state_sync_bootstrap),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_publication_gate: Some(native_publication_gate_tx),
        })
    }

    /// Start the raw broadcast pump for one generation and hand back the gate
    /// that releases its first delta once the published cut is known.
    fn spawn_output_pump(&self, spawn: AttachTerminalPumpSpawn) -> oneshot::Sender<u64> {
        let (gate_tx, gate_rx) = oneshot::channel::<u64>();
        let ctx = AttachTerminalPumpCtx {
            state: self.state.clone(),
            out_tx: self.out_tx.clone(),
            connection_token: self.connection_token.clone(),
            resize: self.handle.resize.clone(),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_bootstrap: self.handle.native_bootstrap.clone(),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            handle: self.handle.clone(),
            wire_terminal_id: self.terminal_id.clone(),
            client_id: self.client_id,
            stream_id: self.stream_id,
            client_caps: self.client_caps,
            stream_profile: self.stream_profile,
            bootstrap_limits: self.bootstrap_limits,
            generation_last_seq: spawn.generation_last_seq,
        };
        let channels = AttachTerminalPumpChannels {
            token: spawn.token,
            output_rx: spawn.output_rx,
            snapshot_gate: gate_rx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_publication_gate: spawn.native_publication_gate,
        };
        let pump_done_guard = spawn.pump_done_guard;
        let bootstrap_id = spawn.bootstrap_id;
        tokio::task::spawn_local(async move {
            let _done_guard = pump_done_guard;
            ctx.run(channels, bootstrap_id).await;
        });
        gate_tx
    }

    /// Publish the atomic state-sync bootstrap the actor captured with
    /// registration, then open the live gate.
    async fn finish_state_sync(
        &self,
        generation: &AttachTerminalGeneration,
        state_sync: crate::terminal_actor::StateSyncBootstrap,
    ) -> Result<(), AttachTerminalFailure> {
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
            AttachTerminalFailure::internal("consumer went away during state-sync ATTACH_TERMINAL")
        })?;
        generation
            .generation_last_seq
            .store(state_sync.base_seq, std::sync::atomic::Ordering::Release);
        let _ = generation.live_gate_tx.send(true);
        Ok(())
    }

    /// Capture a native checkpoint bootstrap from the pane actor.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn capture_native_bootstrap(
        &self,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> Result<crate::terminal_actor::NativeBootstrapReply, AttachTerminalFailure> {
        let (reply, reply_rx) = oneshot::channel();
        self.handle
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
                AttachTerminalFailure::internal("pane actor unavailable for native ATTACH_TERMINAL")
            })?;
        match reply_rx.await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(error)) => Err(AttachTerminalFailure {
                code: ErrorCode::CodecUnavailable,
                message: format!("native ATTACH_TERMINAL failed: {error}"),
            }),
            Err(_) => Err(AttachTerminalFailure::internal(
                "pane actor dropped native ATTACH_TERMINAL",
            )),
        }
    }

    /// Publish the native checkpoint bootstrap, cross the publication fence,
    /// and release both the live gate and the pump's first delta.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn finish_native(
        &self,
        generation: &mut AttachTerminalGeneration,
    ) -> Result<(), AttachTerminalFailure> {
        let reply = self
            .capture_native_bootstrap(generation.bootstrap_id)
            .await?;
        let (cut, cursor) = crate::runtime::attach::publish_native_bootstrap(self.out_tx, reply)
            .await
            .map_err(|()| {
                AttachTerminalFailure::internal("consumer went away during native ATTACH_TERMINAL")
            })?;
        let publication = crate::runtime::attach::activate_native_publication(
            &self.handle,
            self.client_id.0,
            self.terminal_id.clone(),
            self.stream_id,
            generation.bootstrap_id,
            cursor,
        )
        .await
        .map_err(|()| {
            AttachTerminalFailure::internal("pane actor unavailable at native publication fence")
        })?;
        let Some(publication_gate) = generation.native_publication_gate.take() else {
            return Err(AttachTerminalFailure::internal(
                "native publication gate already consumed",
            ));
        };
        publication_gate.send(publication).map_err(|_| {
            AttachTerminalFailure::internal(
                "native ATTACH_TERMINAL pump went away before publication",
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
            "native ATTACH_TERMINAL subscribed"
        );
        Ok(())
    }

    /// Publish the authoritative snapshot, sent before the pump's first delta
    /// (the gate below releases it) and before the Ok reply.
    async fn finish_snapshot(
        &self,
        generation: &mut AttachTerminalGeneration,
    ) -> Result<(), AttachTerminalFailure> {
        use crate::terminal_actor::SnapshotRequest;

        let (snapshot_tx, snapshot_rx) = oneshot::channel();
        self.handle
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
                AttachTerminalFailure::internal("pane actor unavailable for ATTACH_TERMINAL")
            })?;
        let Ok(Ok((snap, cut))) = snapshot_rx.await else {
            return Err(AttachTerminalFailure::internal(
                "pane actor dropped the ATTACH_TERMINAL snapshot",
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
            AttachTerminalFailure::internal("consumer went away during ATTACH_TERMINAL")
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
            "ATTACH_TERMINAL subscribed"
        );
        Ok(())
    }
}

/// Everything [`AttachTerminalSession::spawn_output_pump`] needs to hand one
/// pump generation to its task.
struct AttachTerminalPumpSpawn {
    bootstrap_id: phux_protocol::ids::BootstrapId,
    generation_last_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    token: CancellationToken,
    output_rx: tokio::sync::broadcast::Receiver<crate::terminal_actor::PaneOutput>,
    pump_done_guard: tokio_util::sync::DropGuard,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_publication_gate: oneshot::Receiver<crate::terminal_actor::NativePublicationReply>,
}

/// The one-shot channels one pump generation consumes on its way to steady
/// state.
struct AttachTerminalPumpChannels {
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

/// Fixed per-generation context for the `ATTACH_TERMINAL` output pump.
struct AttachTerminalPumpCtx {
    state: SharedState,
    out_tx: tokio::sync::mpsc::Sender<Outbound>,
    connection_token: CancellationToken,
    resize: tokio::sync::mpsc::Sender<ResizeRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    native_bootstrap: tokio::sync::mpsc::Sender<crate::terminal_actor::NativeBootstrapRequest>,
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    handle: TerminalHandle,
    wire_terminal_id: phux_protocol::ids::TerminalId,
    client_id: ClientId,
    stream_id: phux_protocol::ids::StreamId,
    client_caps: ClientCapabilities,
    stream_profile: phux_protocol::caps::BootstrapStreamProfile,
    bootstrap_limits: BootstrapLimits,
    generation_last_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

/// Where the pump currently sits in one terminal's output: the receiver it is
/// draining and how far the published generation has reached.
///
/// The generation state itself is [`PumpGeneration`], shared verbatim with the
/// ATTACH pump in [`crate::runtime::attach`]. It was duplicated here once, and
/// the copy silently missed the gap fence (phux-l96p.10) — so this consumer
/// kept detaching on a sequence gap after interactive attach was fixed. There
/// is one copy of the rules now.
struct AttachTerminalPumpStream {
    output_rx: tokio::sync::broadcast::Receiver<crate::terminal_actor::PaneOutput>,
    generation: PumpGeneration,
}

/// Whether the pump keeps running after handling one output message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PumpStep {
    /// Take the next message.
    Continue,
    /// End this pump generation.
    Stop,
}

/// The replacement cut a [`crate::terminal_actor::PaneOutput::Resync`]
/// carries.
struct PumpResync {
    /// Post-reflow grid width the client mirror adopts.
    cols: u16,
    /// Post-reflow grid height the client mirror adopts.
    rows: u16,
    /// Why the prior generation cannot continue.
    reason: crate::terminal_actor::ResyncReason,
    /// Actor-global raw sequence included by the replacement cut.
    base_seq: u64,
    /// Synthesized grid replay for the compatibility bootstrap.
    bytes: Bytes,
}

impl AttachTerminalPumpCtx {
    /// Forward this pane's output to one `ATTACH_TERMINAL` consumer until the
    /// generation is cancelled, replaced, or the consumer goes away.
    ///
    /// `break` and `return` were interchangeable in the inline body this
    /// replaces — the task ended at the loop either way — so both arrive here
    /// as [`PumpStep::Stop`].
    async fn run(
        self,
        channels: AttachTerminalPumpChannels,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) {
        let token = channels.token;
        let output_rx = channels.output_rx;
        let snapshot_gate = channels.snapshot_gate;
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let native_publication_gate = channels.native_publication_gate;

        let published_cut = tokio::select! {
            () = token.cancelled() => return,
            result = snapshot_gate => {
                let Ok(cut) = result else { return };
                cut
            }
        };
        let mut stream = AttachTerminalPumpStream {
            output_rx,
            generation: PumpGeneration::opened_at(published_cut, bootstrap_id),
        };
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if native_checkpoint_profile(self.stream_profile) {
            let Ok(publication) = native_publication_gate.await else {
                return;
            };
            stream.output_rx = publication.live;
            if self
                .forward_native_replay(&mut stream, publication.replay)
                .await
                == PumpStep::Stop
            {
                return;
            }
        }
        self.generation_last_seq.store(
            stream.generation.last_forwarded_seq(),
            std::sync::atomic::Ordering::Release,
        );
        loop {
            let next = tokio::select! {
                () = token.cancelled() => break,
                next = pump::next_event(&stream.generation, &mut stream.output_rx) => next,
            };
            let msg = match next {
                pump::PumpWait::Event(msg) => msg,
                // Fenced, and the replacement generation has not arrived
                // within this attempt's backoff. Ask again rather than sit on
                // a screen that can never change.
                pump::PumpWait::RetryResync => {
                    if self.retry_resync_after_lag(&mut stream).await == PumpStep::Stop {
                        break;
                    }
                    continue;
                }
                pump::PumpWait::GapUnrecoverable => {
                    self.abandon_unanswered_gap(&stream).await;
                    break;
                }
            };
            if self.forward(&mut stream, msg).await == PumpStep::Stop {
                break;
            }
        }
    }

    /// Dispatch one broadcast message to the stage that owns it.
    async fn forward(
        &self,
        stream: &mut AttachTerminalPumpStream,
        msg: Result<crate::terminal_actor::PaneOutput, tokio::sync::broadcast::error::RecvError>,
    ) -> PumpStep {
        use crate::terminal_actor::PaneOutput;

        match msg {
            Ok(PaneOutput::Live { seq, bytes }) => self.forward_live(stream, seq, &bytes).await,
            Ok(PaneOutput::Control { owner, frame }) => {
                self.forward_control(stream, owner, frame).await
            }
            Ok(PaneOutput::Resync {
                cols,
                rows,
                bytes,
                reason,
                base_seq,
            }) => {
                let resync = PumpResync {
                    cols,
                    rows,
                    reason,
                    base_seq,
                    bytes,
                };
                self.republish_generation(stream, &resync).await
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
                self.resync_after_lag(stream, dropped).await
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => PumpStep::Stop,
        }
    }

    /// Forward one post-bootstrap byte delta.
    async fn forward_live(
        &self,
        stream: &mut AttachTerminalPumpStream,
        seq: u64,
        bytes: &Bytes,
    ) -> PumpStep {
        if !stream.generation.forwards(seq) {
            return PumpStep::Continue;
        }
        let frame = FrameKind::TerminalOutput {
            terminal_id: self.wire_terminal_id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: stream.generation.bootstrap_id(),
            seq,
            bytes: crate::runtime::attach::downsample_for_caps(bytes, self.client_caps),
        };
        if self.out_tx.send(Outbound::Frame(frame)).await.is_err() {
            return PumpStep::Stop;
        }
        stream.generation.note_forwarded(seq);
        self.generation_last_seq
            .store(seq, std::sync::atomic::Ordering::Release);
        PumpStep::Continue
    }

    /// Whether an ordered native control frame belongs to this pump, and
    /// whether forwarding it retires the published generation.
    fn classify_control(
        &self,
        frame: &FrameKind,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> (bool, bool) {
        match frame {
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id: frame_bootstrap_id,
                ..
            } => (
                terminal_id == &self.wire_terminal_id
                    && *stream_id == self.stream_id
                    && *frame_bootstrap_id == bootstrap_id,
                true,
            ),
            FrameKind::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id: frame_bootstrap_id,
                ..
            } => (
                terminal_id == &self.wire_terminal_id
                    && *stream_id == self.stream_id
                    && *frame_bootstrap_id == bootstrap_id,
                false,
            ),
            _ => (false, false),
        }
    }

    /// Forward one ordered native control frame addressed to this pump.
    async fn forward_control(
        &self,
        stream: &mut AttachTerminalPumpStream,
        owner: u64,
        frame: FrameKind,
    ) -> PumpStep {
        if owner != self.client_id.0 {
            return PumpStep::Continue;
        }
        let (targets_pump, ends_generation) =
            self.classify_control(&frame, stream.generation.bootstrap_id());
        if !targets_pump {
            return PumpStep::Continue;
        }
        if self.out_tx.send(Outbound::Frame(frame)).await.is_err() {
            return PumpStep::Stop;
        }
        if ends_generation {
            stream.generation.retire();
        }
        PumpStep::Continue
    }

    /// Fence the generation and ask the actor for an in-band resync after the
    /// broadcast ring dropped deltas out from under this pump.
    ///
    /// The fence is the whole point and is set *before* the request: resuming
    /// live deltas across the dropped window puts a `TERMINAL_OUTPUT` whose
    /// `seq` skips it on the wire, and the consumer's session kernel rejects
    /// that as a protocol error rather than tolerating it — so the consumer
    /// dies before the resync it just asked for can arrive. See
    /// [`PumpGeneration::forwards`].
    async fn resync_after_lag(
        &self,
        stream: &mut AttachTerminalPumpStream,
        dropped: u64,
    ) -> PumpStep {
        crate::perf::PUMP_LAGGED.incr();
        if stream.generation.fence_for_gap() {
            debug!(
                terminal_id = ?self.wire_terminal_id,
                dropped,
                "ATTACH_TERMINAL output pump lagged again while a resync was \
                 already in flight; re-requesting",
            );
        } else {
            warn!(
                terminal_id = ?self.wire_terminal_id,
                dropped,
                "ATTACH_TERMINAL output pump lagged; requesting in-band resync",
            );
        }
        stream.generation.note_resync_requested();
        self.request_resync().await
    }

    /// The resync asked for at the last gap has not arrived within its
    /// backoff: ask again.
    ///
    /// A fenced pump forwards nothing, so a request the actor accepted but
    /// never answered would otherwise leave the consumer on a screen that can
    /// never change — silence being the one failure the unfenced behaviour did
    /// not have. `DEBUG`, not `WARN`: the first gap already warned, and a
    /// retry loop that warns every time turns one wedged actor into a log
    /// flood.
    async fn retry_resync_after_lag(&self, stream: &mut AttachTerminalPumpStream) -> PumpStep {
        debug!(
            terminal_id = ?self.wire_terminal_id,
            attempt = stream.generation.gap_attempts(),
            "ATTACH_TERMINAL output pump is still waiting on its in-band resync; re-requesting",
        );
        stream.generation.note_resync_requested();
        self.request_resync().await
    }

    /// The gap spent its whole request budget without the actor ever
    /// broadcasting a replacement generation. Tell the consumer and stop,
    /// rather than hold it on a screen that can never change.
    async fn abandon_unanswered_gap(&self, stream: &AttachTerminalPumpStream) -> PumpStep {
        warn!(
            terminal_id = ?self.wire_terminal_id,
            attempts = stream.generation.gap_attempts(),
            "ATTACH_TERMINAL output pump never received the in-band resync it asked for; \
             failing the generation",
        );
        self.fail_unrecoverable_gap().await
    }

    /// Queue the resync request, abandoning the connection if the actor will
    /// not take it.
    async fn request_resync(&self) -> PumpStep {
        if crate::runtime::attach::enqueue_output_resync(&self.resize).await {
            return PumpStep::Continue;
        }
        self.fail_unrecoverable_gap().await
    }

    /// Send the terminal `ERROR` a consumer needs in order to know its stream
    /// is over and reconnect, then end the pump.
    async fn fail_unrecoverable_gap(&self) -> PumpStep {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            self.out_tx.send(Outbound::Frame(FrameKind::Error {
                request_id: None,
                code: ErrorCode::InternalError,
                message: "terminal output gap could not be resynchronized".to_owned(),
            })),
        )
        .await;
        self.abandon_connection()
    }

    /// Release this consumer's per-terminal state and tear the connection down
    /// after an unrecoverable pump failure.
    fn abandon_connection(&self) -> PumpStep {
        crate::runtime::client::detach_and_release_consumer_state(&self.state, self.client_id);
        self.connection_token.cancel();
        PumpStep::Stop
    }

    /// Replace the published generation from an actor-generated resync.
    async fn republish_generation(
        &self,
        stream: &mut AttachTerminalPumpStream,
        resync: &PumpResync,
    ) -> PumpStep {
        // Resync is control, so an unchanged cut still tombstones and
        // replaces the published generation.
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let prior_bootstrap_id = stream.generation.bootstrap_id();
        stream
            .generation
            .set_bootstrap_id(crate::runtime::attach::next_bootstrap_id(
                stream.generation.bootstrap_id(),
            ));
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if native_checkpoint_profile(self.stream_profile) {
            return self
                .republish_native_generation(stream, prior_bootstrap_id, resync)
                .await;
        }
        self.republish_synthesized_generation(stream, resync).await
    }

    /// Republish the compatibility bootstrap synthesized by the actor.
    async fn republish_synthesized_generation(
        &self,
        stream: &mut AttachTerminalPumpStream,
        resync: &PumpResync,
    ) -> PumpStep {
        let payload = crate::runtime::attach::downsample_for_caps(&resync.bytes, self.client_caps);
        if crate::runtime::attach::send_synthesized_bootstrap(
            &self.out_tx,
            self.wire_terminal_id.clone(),
            self.stream_id,
            stream.generation.bootstrap_id(),
            self.stream_profile,
            self.bootstrap_limits,
            resync.cols,
            resync.rows,
            resync.base_seq,
            [payload],
        )
        .await
        .is_err()
        {
            return PumpStep::Stop;
        }
        stream.generation.republished_at(resync.base_seq);
        self.generation_last_seq
            .store(resync.base_seq, std::sync::atomic::Ordering::Release);
        PumpStep::Continue
    }

    /// Tombstone the outgoing generation, capture a fresh native checkpoint,
    /// and cross the publication fence onto the replacement generation.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn republish_native_generation(
        &self,
        stream: &mut AttachTerminalPumpStream,
        prior_bootstrap_id: phux_protocol::ids::BootstrapId,
        resync: &PumpResync,
    ) -> PumpStep {
        if stream.generation.is_active()
            && self
                .out_tx
                .send(Outbound::Frame(FrameKind::BootstrapTombstone {
                    terminal_id: self.wire_terminal_id.clone(),
                    stream_id: self.stream_id,
                    bootstrap_id: prior_bootstrap_id,
                    reason: tombstone_reason(resync.reason),
                    last_valid_seq: stream.generation.last_forwarded_seq(),
                }))
                .await
                .is_err()
        {
            return self.abandon_connection();
        }
        let (reply, reply_rx) = oneshot::channel();
        if self
            .native_bootstrap
            .send(crate::terminal_actor::NativeBootstrapRequest {
                owner: self.client_id.0,
                terminal_id: self.wire_terminal_id.clone(),
                stream_id: self.stream_id,
                bootstrap_id: stream.generation.bootstrap_id(),
                limits: self.bootstrap_limits,
                max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                max_frames: crate::native_state::MAX_NATIVE_PREFIX_CHUNKS + 2,
                reply,
            })
            .await
            .is_err()
        {
            return self.abandon_connection();
        }
        let Ok(Ok(reply)) = reply_rx.await else {
            let _ = self
                .out_tx
                .send(Outbound::Frame(FrameKind::Error {
                    request_id: None,
                    code: ErrorCode::CodecUnavailable,
                    message: "native checkpoint resync failed".to_owned(),
                }))
                .await;
            return self.abandon_connection();
        };
        let Ok((cut, cursor)) =
            crate::runtime::attach::publish_native_bootstrap(&self.out_tx, reply).await
        else {
            return self.abandon_connection();
        };
        let Ok(publication) = crate::runtime::attach::activate_native_publication(
            &self.handle,
            self.client_id.0,
            self.wire_terminal_id.clone(),
            self.stream_id,
            stream.generation.bootstrap_id(),
            cursor,
        )
        .await
        else {
            self.connection_token.cancel();
            return PumpStep::Stop;
        };
        // Unfenced here, before the replay, so the replay's own frames pass
        // the same `forwards` gate every other live delta does.
        stream.generation.republished_at(cut);
        stream.output_rx = publication.live;
        if self.forward_native_replay(stream, publication.replay).await == PumpStep::Stop {
            return PumpStep::Stop;
        }
        self.generation_last_seq.store(
            stream.generation.last_forwarded_seq(),
            std::sync::atomic::Ordering::Release,
        );
        PumpStep::Continue
    }

    /// Forward the deltas the actor buffered while the publication fence was
    /// open, so the replacement generation resumes without a gap.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn forward_native_replay(
        &self,
        stream: &mut AttachTerminalPumpStream,
        replay: Vec<(u64, Bytes)>,
    ) -> PumpStep {
        // Through the same gate as any other live delta, not around it. A
        // replay entry at or behind the published cut is already inside the
        // checkpoint, and re-sending it under this `bootstrap_id` is a
        // `DuplicateSequence` to the client kernel — which detaches on it.
        for (seq, bytes) in replay {
            if !stream.generation.forwards(seq) {
                continue;
            }
            if self
                .out_tx
                .send(Outbound::Frame(FrameKind::TerminalOutput {
                    terminal_id: self.wire_terminal_id.clone(),
                    stream_id: self.stream_id,
                    bootstrap_id: stream.generation.bootstrap_id(),
                    seq,
                    bytes: crate::runtime::attach::downsample_for_caps(&bytes, self.client_caps),
                }))
                .await
                .is_err()
            {
                return PumpStep::Stop;
            }
            stream.generation.note_forwarded(seq);
        }
        PumpStep::Continue
    }
}

/// Map the actor's resync cause onto the wire tombstone reason.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
const fn tombstone_reason(
    reason: crate::terminal_actor::ResyncReason,
) -> phux_protocol::wire::frame::TombstoneReason {
    match reason {
        crate::terminal_actor::ResyncReason::Resize => {
            phux_protocol::wire::frame::TombstoneReason::Resize
        }
        crate::terminal_actor::ResyncReason::OutboundGap => {
            phux_protocol::wire::frame::TombstoneReason::OutboundGap
        }
    }
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

/// Handle `SHUTDOWN` (phux-pimp): stop the server, on request, over the local
/// socket only.
///
/// Cancelling the root token is the *same* signal idle-exit (ADR-0063), the
/// last-pane self-exit, and SIGINT/SIGTERM already deliver, so this is not a
/// second shutdown path -- it is a fourth door onto the one that exists. That
/// is what makes it correct rather than merely effective: every pane gets
/// `TerminalActor::shutdown_pty`'s SIGHUP-then-grace-then-reap and the socket
/// is unlinked by `unlink_socket_if_ours`, because both hang off the token
/// rather than off the caller.
///
/// Acks itself, like [`handle_upgrade`], because the process is gone before a
/// returned `CommandResult` could be sent. The ack goes out BEFORE the cancel
/// and its result is ignored: the client is about to see its connection close
/// either way, and a client that already hung up is not a reason to refuse a
/// stop.
///
/// **Local only.** Who may stop a server -- whether a paired phone should be
/// able to end every pane on the host -- is a policy question phux has not
/// answered, so rather than answer it by accident this refuses any transport
/// but the Unix socket (`L1.md` §5.1 permits the restriction).
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
        let _ = out_tx
            .send(Outbound::Frame(FrameKind::CommandResult {
                request_id,
                result: CommandResult::Error {
                    code: ErrorCode::PermissionDenied,
                    message: "SHUTDOWN is accepted on the local socket only".to_owned(),
                },
            }))
            .await;
        return;
    }

    info!(?client_id, "SHUTDOWN requested; stopping the server");
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        }))
        .await;
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
    clippy::too_many_arguments,
    reason = "the relay surface carries the caller's identity, the routed command, its outbound mailbox, and the negotiated bootstrap context; the dispatch call site owns all eight and boxing them would only rename the list"
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
            | Command::AttachTerminal { terminal_id } => {
                relay_stream_establishing(
                    &relay,
                    &command,
                    terminal_id,
                    client_id,
                    out_tx,
                    bootstrap_profile,
                    bootstrap_limits,
                )
                .await
            }
            Command::DetachTerminal { terminal_id } => {
                resolve_hub_detach_terminal(state, &relay, client_id, host, terminal_id)
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
            Command::RouteInput { terminal_id, .. } => {
                relay_satellite_route_input(
                    &SatelliteLeaseTarget::new(state, host, client_id, terminal_id),
                    &relay,
                    &command,
                )
                .await
            }
            _ => relay.command(command.clone()).await,
        },
    };
    reply_satellite_command(state, client_id, request_id, host, &command, out_tx, result).await;
}

/// Record the hub-side proxy attach a successful `ATTACH_TERMINAL` just
/// established, then correlate the relayed reply back to the caller
/// (phux-v45.4, ADR-0007 §4).
async fn reply_satellite_command(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    host: &phux_protocol::ids::SatelliteHost,
    command: &Command,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    result: CommandResult,
) {
    if !matches!(result, CommandResult::Error { .. })
        && let Command::AttachTerminal { terminal_id } = command
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
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::CommandResult {
            request_id,
            result,
        }))
        .await;
}

/// Relay a stream-establishing command and register the caller's outbound
/// mailbox as a hub-side proxy subscriber *atomically with* it
/// ([`crate::hub::relay::RelayHandle::command_subscribing`], phux-v45.11).
///
/// A command that names no satellite-local terminal has nothing to subscribe
/// and relays plainly.
async fn relay_stream_establishing(
    relay: &crate::hub::relay::RelayHandle,
    command: &Command,
    terminal_id: &phux_protocol::ids::TerminalId,
    client_id: ClientId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
) -> CommandResult {
    let Some(terminal) = terminal_id.local_id() else {
        return relay.command(command.clone()).await;
    };
    relay
        .command_subscribing(
            command.clone(),
            crate::hub::relay::ProxySubscription {
                terminal,
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
                bootstrap_profile: matches!(command, Command::AttachTerminal { .. })
                    .then_some(bootstrap_profile),
                bootstrap_limits: matches!(command, Command::AttachTerminal { .. })
                    .then_some(bootstrap_limits),
            },
        )
        .await
}

/// Hub-side resolution of `DETACH_TERMINAL`: withdraw this consumer's proxy
/// subscription; the link session emits the satellite-side `DETACH_TERMINAL`
/// iff nobody else still observes the terminal. Idempotent Ok, matching the
/// local semantics.
fn resolve_hub_detach_terminal(
    state: &SharedState,
    relay: &crate::hub::relay::RelayHandle,
    client_id: ClientId,
    host: &phux_protocol::ids::SatelliteHost,
    terminal_id: &phux_protocol::ids::TerminalId,
) -> CommandResult {
    if let Some(id) = terminal_id.local_id() {
        relay.unsubscribe_terminal(client_id, id);
        state.with_mut(|s| {
            s.unregister_satellite_proxy_attach(client_id, host, id);
        });
    }
    CommandResult::Ok
}

/// The hub-side lease coordinates one satellite-routed input command acts on.
///
/// Every hub consumer shares the link's one client identity on the satellite
/// (phux-v45.7, L1 §9.1), so lease exclusion *between* hub consumers is
/// resolved here, against `ServerState::satellite_leases`, before anything
/// touches the link.
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
        terminal_id: &phux_protocol::ids::TerminalId,
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

/// Apply the hub's own lease exclusion to `ACQUIRE_INPUT` before relaying it.
///
/// A cooperative acquire against a terminal another hub consumer holds is
/// refused here without touching the link. The relayed lease (held by the
/// link identity) still excludes the satellite's own local clients.
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
    if matches!(result, CommandResult::Error { .. }) {
        return result;
    }
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
    result
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
        // Idempotent no-op per ADR-0033 — and deliberately
        // NOT forwarded: on the satellite this consumer is
        // indistinguishable from the holder, so forwarding
        // would release the holder's lease (L1 §9.1).
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
    relay.command(command.clone()).await
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
        // `REQUESTED` covers an operator asking on the client's behalf as well
        // as the client's own `DETACH`: proto.md §7.2 reads it as "a detach was
        // asked for", not "*this* connection asked". The distinguishing detail
        // rides the message.
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
            _ => crate::terminal_actor::default_shell_command(&shell, login_shell),
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
            scrollback,
            root_token,
            None,
            agent_session,
        )
    } else {
        seed_session_with_actor_and_metadata(state, name, scrollback, root_token, agent_session)
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
/// `GET_PERF`: snapshot the server's in-process telemetry (`crate::perf`) as
/// a JSON `phux_perf::PerfReport`. The registry-derived gauges are refreshed
/// first so the report is self-contained; `reset` zeroes every metric after
/// the snapshot so the next report covers only the interval since.
pub(crate) fn handle_get_perf(state: &SharedState, reset: bool) -> CommandResult {
    let (sessions, panes) =
        state.with_mut(|s| (s.registry().session_count(), s.registry().terminal_count()));
    let clients = match handle_get_state(state, &StateScope::Server) {
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
    let report = crate::perf::report();
    if reset {
        crate::perf::reset();
    }
    CommandResult::OkWith(CommandValue::Json(report.to_json()))
}

pub(crate) fn handle_get_state(state: &SharedState, scope: &StateScope) -> CommandResult {
    match scope {
        StateScope::Server => {
            let snapshot = state.with_mut(|s| {
                let focus = s
                    .most_recently_touched_session()
                    .or_else(|| s.registry().sessions().next().map(|(id, _)| id));
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

/// Feed integration-hook lifecycle evidence into a pane's detector.
pub(crate) async fn handle_report_agent_state(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::TerminalId,
    reported: phux_protocol::wire::frame::ReportedAgentState,
) -> CommandResult {
    if !terminal_id.is_local() {
        return CommandResult::Error {
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: format!("REPORT_AGENT_STATE on satellite route unsupported: {terminal_id:?}"),
        };
    }
    let handle = state.with(|server| {
        let core = server.terminal_from_wire(terminal_id)?;
        server.terminal_handle(core).cloned()
    });
    let Some(handle) = handle else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such terminal: {terminal_id:?}"),
        };
    };
    let (reply, result) = oneshot::channel();
    if handle
        .control
        .send(ControlRequest::ReportAgentState {
            state: reported,
            reply,
        })
        .await
        .is_err()
    {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for REPORT_AGENT_STATE".to_owned(),
        };
    }
    match result.await {
        Ok(Ok(())) => CommandResult::Ok,
        Ok(Err(message)) => CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message,
        },
        Err(_) => CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor dropped REPORT_AGENT_STATE reply".to_owned(),
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
        if let Some(pane) = s.registry_mut().terminal_mut(terminal_id) {
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
        if let Some(handle) = s.terminal_handle(terminal_id) {
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
    let Some((proxy_host, proxy_id)) = crate::hub::relay::satellite_route(wire_terminal_id) else {
        warn!(
            ?client_id,
            ?wire_terminal_id,
            frame_label,
            "satellite-routed input has no relay route; dropping",
        );
        return;
    };
    if !state.with(|s| s.has_satellite_proxy_attach(client_id, &proxy_host, proxy_id)) {
        warn!(
            ?client_id,
            ?wire_terminal_id,
            frame_label,
            "satellite-routed input requires this client's ATTACH_TERMINAL proxy; dropping",
        );
        return;
    }
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
        // Bind the session id out of the shared borrow first: `attached()`
        // borrows all of `s`, and `touch_session` needs `&mut s`.
        let touched_session = s.attached().get(&client_id).map(|c| c.session);
        if let Some(session) = touched_session {
            s.touch_session(session);
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

/// Route one opaque terminal-engine reply directly to the PTY byte lane.
///
/// These bytes are already encoded by the client's terminal emulator in
/// response to terminal output (for example DSR or color queries). They must
/// not pass through key/paste encoders or any text normalization. The same
/// subscription and input-lease authority gate as ordinary input prevents an
/// unattached client or non-holder from writing to another terminal.
pub(crate) fn handle_terminal_reply(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::TerminalId,
    bytes: Bytes,
) {
    const FRAME_LABEL: &str = "INPUT_TERMINAL_REPLY";

    if !wire_terminal_id.is_local() {
        relay_terminal_reply(state, client_id, wire_terminal_id, bytes, FRAME_LABEL);
        return;
    }

    let _ = with_attached_input_destination(
        state,
        client_id,
        wire_terminal_id,
        FRAME_LABEL,
        |destination| {
            let dispatched = destination
                .handle
                .encoded_input
                .try_send(EncodedInputRequest::opaque(bytes));
            log_terminal_reply_dispatch(&dispatched, client_id, wire_terminal_id);
        },
    );
}

/// Relay a satellite-routed terminal reply over the hub link.
///
/// The same subscription and input-lease authority gate as ordinary input
/// applies: the caller needs its own `ATTACH_TERMINAL` proxy attach, and a
/// non-holder cannot write while another hub consumer holds the lease.
fn relay_terminal_reply(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::TerminalId,
    bytes: Bytes,
    frame_label: &'static str,
) {
    let Some((host, id)) = crate::hub::relay::satellite_route(wire_terminal_id) else {
        warn!(
            ?client_id,
            ?wire_terminal_id,
            "terminal reply carried an unroutable satellite terminal id; dropping",
        );
        return;
    };
    if !state.with(|s| s.has_satellite_proxy_attach(client_id, &host, id)) {
        warn!(
            ?client_id,
            ?wire_terminal_id,
            "satellite terminal reply requires this client's ATTACH_TERMINAL proxy; dropping",
        );
        return;
    }
    if state.with(|s| {
        s.satellite_lease_holder(&host, id)
            .is_some_and(|holder| holder != client_id)
    }) {
        trace!(
            ?client_id,
            ?wire_terminal_id,
            "satellite terminal reply dropped: another hub consumer holds the input lease",
        );
        return;
    }
    if !relay_satellite_frame(
        state,
        client_id,
        wire_terminal_id,
        frame_label,
        |terminal_id| FrameKind::InputTerminalReply { terminal_id, bytes },
    ) {
        warn!(
            ?client_id,
            ?wire_terminal_id,
            "terminal reply carried an unroutable satellite terminal id; dropping",
        );
    }
}

/// Log the outcome of handing one opaque terminal reply to the PTY byte lane.
fn log_terminal_reply_dispatch(
    dispatched: &Result<(), tokio::sync::mpsc::error::TrySendError<EncodedInputRequest>>,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::TerminalId,
) {
    match dispatched {
        Ok(()) => {
            trace!(
                ?client_id,
                ?wire_terminal_id,
                "opaque terminal reply routed to PTY byte lane",
            );
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            warn!(
                ?client_id,
                ?wire_terminal_id,
                "encoded-input actor mailbox full; dropping terminal reply",
            );
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            debug!(
                ?client_id,
                ?wire_terminal_id,
                "pane actor gone; dropping terminal reply",
            );
        }
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
        relay_frame_ack(
            state,
            client_id,
            wire_terminal_id,
            stream_id,
            bootstrap_id,
            seq,
        );
        return;
    }
    state.with_mut(|s| {
        let Some(handle) = frame_ack_destination(s, client_id, wire_terminal_id, seq) else {
            return;
        };
        // Bridge `state::ClientId` (u64 newtype) → `phux_protocol::ClientId`
        // (u32), matching the conversion `handle_attach` already does for
        // the per-consumer state map keys. The wire ClientId space caps at
        // u32::MAX; widening would require a protocol bump.
        let wire_client_id =
            phux_protocol::ids::ClientId::new(u32::try_from(client_id.0).unwrap_or(u32::MAX));
        let dispatched = handle.consumer_ack.try_send(ConsumerAckRequest {
            client_id: wire_client_id,
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
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::TerminalId,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    seq: u64,
) {
    let relayed = relay_satellite_frame(state, client_id, wire_terminal_id, "FRAME_ACK", |id| {
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
}

/// Resolve the pane actor an inbound `FRAME_ACK` may reach, applying the three
/// drop gates from [`handle_frame_ack`]'s contract in order.
fn frame_ack_destination<'a>(
    s: &'a crate::state::ServerState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::TerminalId,
    seq: u64,
) -> Option<&'a TerminalHandle> {
    let Some(pane) = s.terminal_from_wire(wire_terminal_id) else {
        warn!(
            ?client_id,
            ?wire_terminal_id,
            seq,
            "FRAME_ACK for unknown pane; dropping",
        );
        return None;
    };
    // Same gate as `handle_terminal_input` (phux-v45.7): subscription
    // — established by ATTACH or ATTACH_TERMINAL — is the ack gate; a
    // session attachment is not required (the federation hub's link
    // consumer acks relayed frames without one).
    if !s.subscribers_for_terminal(pane).contains(&client_id) {
        warn!(
            ?client_id,
            ?wire_terminal_id,
            seq,
            "FRAME_ACK from client not subscribed to pane; dropping",
        );
        return None;
    }
    let Some(handle): Option<&TerminalHandle> = s.terminal_handle(pane) else {
        warn!(
            ?client_id,
            ?wire_terminal_id,
            seq,
            "FRAME_ACK with no TerminalHandle for pane; dropping",
        );
        return None;
    };
    Some(handle)
}

/// Log the outcome of routing one `FRAME_ACK` to its pane actor.
fn log_frame_ack_dispatch(
    dispatched: &Result<(), tokio::sync::mpsc::error::TrySendError<ConsumerAckRequest>>,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::TerminalId,
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
