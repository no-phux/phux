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
) -> phux_terminal_api::agent_detect::LiveSessionProbe {
    let state = state.clone();
    std::rc::Rc::new(move || state.with(|s| s.has_live_agent_session_child(terminal)))
}

/// The grid a pane is built at when its real geometry arrives later (an
/// attaching viewport, or a spawn without `SPAWN_RESOURCE.initial_size`).
pub(crate) const DEFAULT_SPAWN_DIMS: (u16, u16) = crate::state::HEADLESS_TERMINAL_DIMS;

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
    cmd: &mut phux_terminal_api::launch::LaunchCommand,
) -> phux_protocol::ids::ResourceId {
    stamp_spawn_cwd(s, terminal, spawn_cwd_of(cmd));
    let wire = s.intern_resource_wire(terminal);
    phux_terminal_api::launch::apply_terminal_id(cmd, &wire);
    phux_terminal_api::launch::apply_server_socket(cmd, s.server_socket_path());
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
    cmd: Option<phux_terminal_api::launch::LaunchCommand>,
    (cols, rows): (u16, u16),
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    attribution: SpawnAttribution,
) -> Result<phux_protocol::ids::ResourceId, phux_terminal_api::actor::TerminalActorError> {
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
    cmd: Option<phux_terminal_api::launch::LaunchCommand>,
    (cols, rows): (u16, u16),
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    attribution: SpawnAttribution,
    honor_exit_policy_on_failure: bool,
) -> Result<phux_protocol::ids::ResourceId, phux_terminal_api::actor::TerminalActorError> {
    let has_pty = cmd.is_some();
    let terminal_token = root_token.child_token();
    let reports = engine_reports(state, terminal);
    let built = state
        .terminal_engine()
        .ok_or_else(|| {
            phux_terminal_api::TerminalActorError::Refused("no Terminal engine supplied".into())
        })
        .and_then(|engine| {
            engine.build(phux_terminal_api::EngineBuild {
                cols,
                rows,
                source: phux_terminal_api::EngineSource::Fresh(cmd),
                scrollback,
                token: terminal_token.clone(),
                default_colors,
                reports,
            })
        });
    let bundle = match built {
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
                    s.reap_resource(terminal);
                }
            });
            return Err(err);
        }
    };
    let phux_terminal_api::EngineBundle {
        run,
        handle,
        exit_notify,
        proc,
        events,
        agent_state,
        program_status,
        ..
    } = bundle;
    let wiring = PaneWiring::new(events, agent_state, program_status);
    let wire_terminal_id = state.with_mut(|s| {
        let wire = s
            .spawn_resource_actor(terminal, handle, terminal_token, run)
            .map_err(|error| {
                phux_terminal_api::actor::TerminalActorError::Refused(error.to_string())
            })?;
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
        journal_pane_spawned(s, &wire, attribution);
        Ok::<_, phux_terminal_api::actor::TerminalActorError>(wire)
    })?;
    let events = wiring.start(state, &wire_terminal_id);
    spawn_resource_exit_watcher(
        state.clone(),
        terminal,
        exit_notify,
        root_token.clone(),
        Some(events),
    );
    Ok(wire_terminal_id)
}

/// Runtime report channels, held until the pane's wire id is known.
pub(crate) struct PaneWiring {
    /// The runtime end of the pane's agent-event sink (ADR-0123).
    events: crate::resource::event_sink::EventSource,
    /// The runtime end of the detector's sink (ADR-0046); `None` for a
    /// no-PTY pane, which never gets a detector.
    agent_state:
        Option<tokio::sync::mpsc::Receiver<phux_terminal_api::agent_detect::AgentDetectEvent>>,
    /// Reliable OSC 7501 snapshots, shared by fresh and resumed panes.
    program_status:
        tokio::sync::watch::Receiver<Vec<phux_terminal_api::agent_detect::ProgramStatusRecord>>,
}

pub(crate) fn engine_reports(
    state: &SharedState,
    terminal: phux_core::ids::ResourceId,
) -> phux_terminal_api::EngineReports {
    phux_terminal_api::EngineReports {
        event_capacity: EVENT_SINK_CAPACITY,
        agent_capacity: AGENT_STATE_SINK_CAPACITY,
        live_session: Some(live_session_probe(state, terminal)),
    }
}

impl PaneWiring {
    pub(crate) const fn new(
        events: crate::resource::event_sink::EventSource,
        agent_state: Option<
            tokio::sync::mpsc::Receiver<phux_terminal_api::agent_detect::AgentDetectEvent>,
        >,
        program_status: tokio::sync::watch::Receiver<
            Vec<phux_terminal_api::agent_detect::ProgramStatusRecord>,
        >,
    ) -> Self {
        Self {
            events,
            agent_state,
            program_status,
        }
    }
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
        crate::runtime::apps::terminal::program_status::spawn_drain(
            state.clone(),
            wire.clone(),
            self.program_status,
        );
        PaneEvents {
            wire: wire.clone(),
            source: self.events,
        }
    }
}

/// The launch spec a relaunch of a pane spawned from `cmd` would use: its
/// argv and cwd. The server-applied environment (`TERM`, the pane and
/// socket variables) is reapplied at every launch, so it is not recorded.
pub(crate) fn launch_of(
    cmd: Option<&phux_terminal_api::launch::LaunchCommand>,
) -> phux_core::TerminalLaunch {
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
pub(in crate::runtime) fn exit_policy_for(
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
pub(in crate::runtime) fn spawn_actor(
    s: &crate::state::ServerState,
    attribution: SpawnAttribution,
) -> phux_core::Actor {
    attribution
        .actor
        .map_or_else(phux_core::Actor::system, |client| s.actor_for(client))
}
