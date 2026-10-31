//! Pane spawning: attribution, actor launch, and pane wiring.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;

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
pub(in crate::runtime) fn terminal_not_found(
    terminal_id: &phux_protocol::ids::ResourceId,
) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::TerminalNotFound,
        message: format!("no such terminal: {terminal_id}"),
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
    /// The new session's lifetime; a named create is persistent.
    pub(crate) lifetime: phux_core::SessionLifetime,
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

/// Install agent-session provenance on a freshly registered pane. A value
/// outside `1..=MAX_AGENT_SESSION_RECORD_BYTES` installs nothing.
pub(in crate::runtime) fn set_agent_session(
    s: &mut crate::state::ServerState,
    wire: phux_protocol::ids::ResourceId,
    agent_session: Option<Vec<u8>>,
) {
    if let Some(value) = agent_session.filter(|value| {
        (1..=phux_protocol::wire::frame::MAX_AGENT_SESSION_RECORD_BYTES).contains(&value.len())
    }) {
        s.metadata_set(
            &phux_protocol::wire::frame::Scope::Resource(wire),
            phux_protocol::wire::frame::RESOURCE_AGENT_SESSION_KEY,
            value,
        );
    }
}

/// Registry-side setup every PTY pane shares: stamp its spawn cwd and tell
/// the child which pane and server it belongs to. Returns the wire id.
pub(in crate::runtime) fn register_pty_command(
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
pub(in crate::runtime) fn launch_pane_actor(
    state: &SharedState,
    terminal: phux_core::ids::ResourceId,
    cmd: Option<portable_pty::CommandBuilder>,
    (cols, rows): (u16, u16),
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    attribution: SpawnAttribution,
) -> Result<phux_protocol::ids::ResourceId, crate::terminal_actor::TerminalActorError> {
    launch_pane_actor_with_failure_policy(
        state,
        terminal,
        cmd,
        (cols, rows),
        scrollback,
        root_token,
        default_colors,
        attribution,
        false,
    )
}

/// A native insertion into an existing record reports a failed activation
/// under that record's original exit policy rather than unconditionally
/// deleting a retained imported arrangement slot.
#[allow(clippy::too_many_arguments)]
pub(in crate::runtime) fn launch_pane_actor_with_failure_policy(
    state: &SharedState,
    terminal: phux_core::ids::ResourceId,
    cmd: Option<portable_pty::CommandBuilder>,
    (cols, rows): (u16, u16),
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    attribution: SpawnAttribution,
    honor_exit_policy_on_failure: bool,
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
            state.with_mut(|s| {
                if honor_exit_policy_on_failure {
                    s.activation_ended_with_diagnostic(
                        terminal,
                        phux_core::ExitOutcome::UNKNOWN,
                        true,
                        Some(err.launch_diagnostic()),
                    );
                } else {
                    s.reap_terminal(terminal);
                }
            });
            return Err(err);
        }
    };
    let crate::terminal_actor::TerminalActorBundle {
        mut actor,
        handle,
        exit_notify,
        proc,
        ..
    } = bundle;
    // Sinks go in before `actor.run()` consumes the actor.
    let wiring = wire_pane_actor(state, terminal, &mut actor);
    let wire_terminal_id = state.with_mut(|s| {
        let _ = s.spawn_resource_actor(terminal, handle, terminal_token, actor.run());
        // The executor report: the activation now holds a live process.
        s.activation_started(terminal, proc);
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
    let events = wiring.start(state, &wire_terminal_id);
    spawn_terminal_exit_watcher(
        state.clone(),
        terminal,
        exit_notify,
        root_token.clone(),
        Some(events),
    );
    Ok(wire_terminal_id)
}

/// The runtime ends of the sinks [`wire_pane_actor`] installed on a pane's
/// actor, held until the pane's wire id is known.
pub(crate) struct PaneWiring {
    /// The runtime end of the pane's agent-event sink (ADR-0123).
    events: crate::resource::event_sink::EventSource,
    /// The runtime end of the detector's sink (ADR-0046); `None` for a
    /// no-PTY pane, which never gets a detector.
    agent_state: Option<tokio::sync::mpsc::Receiver<crate::agent_detect::AgentDetectEvent>>,
    /// Reliable OSC 7501 snapshots, shared by fresh and resumed panes.
    program_status:
        tokio::sync::watch::Receiver<Vec<crate::resource::terminal::program_status::Record>>,
}

/// Install every runtime sink and probe a pane's actor needs: its event sink
/// and, with a PTY, the agent detector's sink and live-session probe (the
/// actor arms its detector at run start only when the sink is present). The
/// one arming path for a fresh spawn and an ADR-0032 rebuild, so a resumed
/// pane cannot come back without detection. Call before `actor.run()`.
pub(crate) fn wire_pane_actor(
    state: &SharedState,
    terminal: phux_core::ids::ResourceId,
    actor: &mut TerminalActor,
) -> PaneWiring {
    let (event_sink, events) = crate::resource::event_sink::event_sink(EVENT_SINK_CAPACITY);
    actor.set_event_sink(event_sink);
    let agent_state = actor.has_pty().then(|| {
        let (agent_tx, agent_rx) = tokio::sync::mpsc::channel(AGENT_STATE_SINK_CAPACITY);
        actor.set_agent_state_sink(agent_tx);
        actor.set_live_session_probe(live_session_probe(state, terminal));
        agent_rx
    });
    PaneWiring {
        events,
        agent_state,
        program_status: actor.subscribe_program_status(),
    }
}

impl PaneWiring {
    /// Start the detector's metadata drain for the pane at `wire`, and hand
    /// back its events for the exit watcher, which is their drain.
    pub(crate) fn start(
        self,
        state: &SharedState,
        wire: &phux_protocol::ids::ResourceId,
    ) -> PaneEvents {
        if let Some(agent_rx) = self.agent_state {
            spawn_agent_state_drain(state.clone(), wire.clone(), agent_rx);
        }
        super::super::program_status::spawn_drain(state.clone(), wire.clone(), self.program_status);
        PaneEvents {
            wire: wire.clone(),
            source: self.events,
        }
    }
}

/// The launch spec a relaunch of a pane spawned from `cmd` would use: its
/// argv and cwd. The server-applied environment (`TERM`, the pane and
/// socket variables) is reapplied at every launch, so it is not recorded.
pub(crate) fn launch_of(cmd: Option<&portable_pty::CommandBuilder>) -> phux_core::TerminalLaunch {
    let Some(cmd) = cmd else {
        return phux_core::TerminalLaunch::default();
    };
    phux_core::TerminalLaunch {
        argv: cmd
            .get_argv()
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
        cwd: cmd.get_cwd().map(std::path::PathBuf::from),
        env: Vec::new(),
        restore: None,
    }
}

/// The exit policy of a pane spawned with `attribution`: `Retain` when it
/// is retained after exit (ADR-0124), `Remove` otherwise.
pub(super) fn exit_policy_for(
    s: &crate::state::ServerState,
    has_pty: bool,
    attribution: SpawnAttribution,
) -> phux_core::ExitPolicy {
    let retained = has_pty
        && attribution
            .retain_secs
            .or_else(|| s.retain_policy().resolve(None))
            .is_some();
    if retained {
        phux_core::ExitPolicy::Retain
    } else {
        phux_core::ExitPolicy::Remove
    }
}

/// The actor a spawn is attributed to: the asking connection, or the
/// server for a seed.
pub(super) fn spawn_actor(
    s: &crate::state::ServerState,
    attribution: SpawnAttribution,
) -> phux_core::Actor {
    attribution
        .actor
        .map_or_else(phux_core::Actor::system, |client| s.actor_for(client))
}
