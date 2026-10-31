//! Session seeding and PTY-backed pane spawns.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;

/// Seed a session named `name` with one pane in the same commit; the pane
/// runs `cmd` in a PTY, or is a no-PTY actor when `cmd` is `None`.
pub(super) fn seed_session(
    state: &SharedState,
    name: &str,
    mut cmd: Option<phux_terminal::pty::CommandBuilder>,
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    origin: SeedOrigin,
) -> Result<phux_core::ids::ResourceId, crate::terminal_actor::TerminalActorError> {
    let terminal = state.with_mut(|s| {
        let on_exit = exit_policy_for(s, cmd.is_some(), origin.attribution);
        let actor = spawn_actor(s, origin.attribution);
        let (_, terminal) = s
            .create_terminal(
                crate::state::TerminalTarget::NewSession {
                    name,
                    lifetime: origin.lifetime,
                },
                launch_of(cmd.as_ref()),
                on_exit,
                &actor,
            )
            .map_err(|refusal| {
                crate::terminal_actor::TerminalActorError::Refused(refusal.to_string())
            })?;
        let wire = match cmd.as_mut() {
            Some(cmd) => register_pty_command(s, terminal, cmd),
            None => s.intern_terminal_wire(terminal),
        };
        set_agent_session(s, wire, origin.agent_session);
        Ok::<_, crate::terminal_actor::TerminalActorError>(terminal)
    })?;
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

/// Seed a persistent session whose pane is a no-PTY actor (no child
/// process).
pub(crate) fn seed_session_with_actor(
    state: &SharedState,
    name: &str,
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
) -> Result<phux_core::ids::ResourceId, crate::terminal_actor::TerminalActorError> {
    seed_session_with_actor_as(
        state,
        name,
        scrollback,
        root_token,
        phux_core::SessionLifetime::Persistent,
    )
}

/// Seed a session of `lifetime` whose pane is a no-PTY actor.
pub(crate) fn seed_session_with_actor_as(
    state: &SharedState,
    name: &str,
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
    lifetime: phux_core::SessionLifetime,
) -> Result<phux_core::ids::ResourceId, crate::terminal_actor::TerminalActorError> {
    seed_session(
        state,
        name,
        None,
        scrollback,
        root_token,
        None,
        SeedOrigin {
            lifetime,
            ..SeedOrigin::default()
        },
    )
}

/// Seed a persistent session with a PTY-backed pane running `cmd`.
pub fn seed_session_with_pty(
    state: &SharedState,
    name: &str,
    cmd: phux_terminal::pty::CommandBuilder,
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
) -> Result<phux_core::ids::ResourceId, crate::terminal_actor::TerminalActorError> {
    seed_session_with_pty_and_colors(
        state,
        name,
        cmd,
        scrollback,
        root_token,
        None,
        phux_core::SessionLifetime::Persistent,
    )
}

/// Palette-seeded variant used when a client's attach creates the session,
/// with the session's `lifetime`.
pub fn seed_session_with_pty_and_colors(
    state: &SharedState,
    name: &str,
    cmd: phux_terminal::pty::CommandBuilder,
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    lifetime: phux_core::SessionLifetime,
) -> Result<phux_core::ids::ResourceId, crate::terminal_actor::TerminalActorError> {
    seed_session(
        state,
        name,
        Some(cmd),
        scrollback,
        root_token,
        default_colors,
        SeedOrigin {
            lifetime,
            ..SeedOrigin::default()
        },
    )
}

/// Registry ownership address for a newly spawned pane.
#[derive(Debug)]
pub(crate) enum SpawnOwnership {
    /// Membership in this session.
    Session(phux_core::ids::SessionId),
    /// Membership in the session of an existing wire Terminal.
    Terminal(phux_protocol::ids::ResourceId),
}

/// Add a PTY pane under `ownership`. `initial_size` (`SPAWN_RESOURCE`) sizes
/// the grid, PTY, and registry dims in the transaction that creates the pane,
/// so its first bootstrap is already at the client's geometry. `Ok(None)`
/// means the owner's session is gone or refused the new member.
#[allow(
    clippy::too_many_arguments,
    reason = "every input must be true of the pane before its actor becomes visible to another client"
)]
pub(crate) fn spawn_pane_with_pty_and_colors(
    state: &SharedState,
    ownership: &SpawnOwnership,
    mut cmd: phux_terminal::pty::CommandBuilder,
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
        let session = match ownership {
            SpawnOwnership::Session(session) => *session,
            SpawnOwnership::Terminal(owner) => s.session_of_terminal_owner(owner)?,
        };
        let on_exit = exit_policy_for(s, true, attribution);
        let actor = spawn_actor(s, attribution);
        let (_, terminal) = s
            .create_terminal(
                crate::state::TerminalTarget::Session(session),
                phux_core::TerminalLaunch {
                    restore: agent_session.clone(),
                    ..launch_of(Some(&cmd))
                },
                on_exit,
                &actor,
            )
            .ok()?;
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
        let session = s.registry().session_of(terminal)?;
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
pub(super) fn spawn_cwd_of(cmd: &phux_terminal::pty::CommandBuilder) -> Option<std::path::PathBuf> {
    cmd.get_cwd()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
}

/// Stamp a new pane's spawn cwd onto its registry descriptor; without it the
/// ATTACHED snapshot reports no cwd until attach refreshes it from the child.
pub(super) fn stamp_spawn_cwd(
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
