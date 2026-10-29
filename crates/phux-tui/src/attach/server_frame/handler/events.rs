//! `EVENT` frames: organization changes, terminal control, asks, cwd, and activity.

use phux_protocol::ids::{ClientId, ResourceId};
use phux_protocol::wire::frame::{AgentEvent, ArrangementOwner, FrameKind, ResourceLifecycle};

use super::closed::session_deleted;
use super::spawned::{close_floating, window_holding_pane};
use super::{FrameCtx, FrameOutcome, KernelRoute, input_authority_notice};
use crate::render::chrome::status_bar::Notice;

/// Dispatch one agent event; most do not affect this client and must not
/// tear down the attach.
pub(super) fn handle_agent_event<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    frame: FrameKind,
    route: &KernelRoute,
) -> FrameOutcome {
    match frame {
        // A live-spawned `AgentSession` under one of our panes: attach its
        // record stream.
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::ResourceSpawned { .. },
            ..
        } if route.declared_agent.as_ref() == Some(&terminal) => FrameOutcome {
            attach_panes: vec![terminal],
            chrome_dirty: true,
            ..FrameOutcome::default()
        },
        FrameKind::Event {
            terminal: Some(terminal),
            event:
                AgentEvent::TerminalControl {
                    lifecycle,
                    input_holder,
                    exit_status,
                    ..
                },
            ..
        } => fold_terminal_control(ctx, &terminal, lifecycle, input_holder, exit_status),
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::Asked { .. },
            ..
        } => fold_agent_ask(ctx, terminal),
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::CwdChanged { cwd },
            ..
        } => fold_cwd_changed(ctx, &terminal, cwd),
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::CommandFinished { exit_code },
            ..
        } => fold_command_finished(ctx, &terminal, exit_code),
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::InputActivity { origin },
            ..
        } => fold_input_activity(ctx, &terminal, origin),
        // Another session's pane set changed (server-wide event subscription).
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::ResourceSpawned { .. } | AgentEvent::ResourceClosed { .. },
            ..
        } if !ctx.panes.contains_key(&terminal) && !ctx.is_agent_session(&terminal) => {
            FrameOutcome {
                foreign_pane_set_dirty: true,
                ..FrameOutcome::default()
            }
        }
        FrameKind::Event { event, .. } => fold_org_event(ctx, &event),
        _ => FrameOutcome::default(),
    }
}

/// An organization event (`docs/organization.md`), matched by record key.
/// A rename of any session renames it in the roster (and on the status line
/// when it is ours); a peer's arrangement change asks for a re-read of the
/// peer caches. About the attached session: deletion detaches (R15), a
/// lifetime change updates what an emptied arrangement does, an arrangement
/// change asks for a re-read, and a membership change asks for a fresh
/// inventory.
fn fold_org_event<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    event: &AgentEvent,
) -> FrameOutcome {
    let ours = *ctx.session_key;
    match event {
        AgentEvent::SessionRenamed { session, name, .. } => {
            if ours == Some(*session) {
                name.clone_into(ctx.session_name);
            }
            return FrameOutcome {
                session_renamed: Some((*session, name.clone())),
                chrome_dirty: true,
                ..FrameOutcome::default()
            };
        }
        AgentEvent::SessionProjectSet {
            session, project, ..
        } => {
            return FrameOutcome {
                session_project_set: Some((*session, project.clone())),
                chrome_dirty: true,
                ..FrameOutcome::default()
            };
        }
        AgentEvent::ArrangementChanged {
            owner: ArrangementOwner::Session(session),
            ..
        } if ours != Some(*session) => {
            return FrameOutcome {
                peer_arrangement_changed: Some(*session),
                ..FrameOutcome::default()
            };
        }
        // A journal gap may have swallowed renames and arrangement changes.
        // A `SOURCE_GAP` is one pane's dropped events, not organization's.
        AgentEvent::JournalGap { .. } => {
            return FrameOutcome {
                organization_resync: true,
                ..FrameOutcome::default()
            };
        }
        _ => {}
    }
    let Some(ours) = ours else {
        return FrameOutcome::default();
    };
    match event {
        AgentEvent::SessionDeleted { session, .. } if *session == ours => {
            session_deleted(*ctx.lifetime)
        }
        AgentEvent::SessionLifetimeChanged {
            session, lifetime, ..
        } if *session == ours => {
            *ctx.lifetime = *lifetime;
            FrameOutcome {
                chrome_dirty: true,
                ..FrameOutcome::default()
            }
        }
        AgentEvent::ArrangementChanged {
            owner: ArrangementOwner::Session(session),
            ..
        } if *session == ours => FrameOutcome {
            arrangement_changed: true,
            ..FrameOutcome::default()
        },
        AgentEvent::MembershipChanged { from, to, .. } if *from == ours || *to == ours => {
            FrameOutcome {
                members_dirty: true,
                ..FrameOutcome::default()
            }
        }
        AgentEvent::ResourceCreated { session, .. }
        | AgentEvent::ResourceRemoved { session, .. }
            if *session == ours =>
        {
            FrameOutcome {
                members_dirty: true,
                ..FrameOutcome::default()
            }
        }
        _ => FrameOutcome::default(),
    }
}

/// ADR-0033: fold a `TerminalControl` broadcast into the slot for the badge.
/// A holder transition on the focused pane raises a notice; the first event
/// a slot sees is the attach-time state and stays silent.
fn fold_terminal_control<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal: &ResourceId,
    lifecycle: ResourceLifecycle,
    input_holder: Option<ClientId>,
    exit_status: Option<i32>,
) -> FrameOutcome {
    let Some(slot) = ctx.panes.get_mut(terminal) else {
        // Can precede the first snapshot; the next event re-states the lease.
        return FrameOutcome::default();
    };
    let initial_state = !slot.control_seen;
    slot.control_seen = true;
    let holder_changed = slot.input_holder != input_holder;
    slot.lifecycle = lifecycle;
    slot.input_holder = input_holder;
    // ADR-0147: a retained floating overlay closes with its process.
    if matches!(lifecycle, ResourceLifecycle::Exited) && slot.floating.is_some() {
        ctx.expected_closes.insert(terminal.clone());
        let mut outcome = close_floating(ctx, terminal).unwrap_or_default();
        outcome.kill_orphans.push(terminal.clone());
        return outcome;
    }
    // ADR-0124: the first `Exited` carries the status.
    if matches!(lifecycle, ResourceLifecycle::Exited) && slot.exited.is_none() {
        slot.exited = Some(crate::attach::pane_state::ExitMark {
            status: exit_status,
            signal: None,
        });
    }
    let announce =
        holder_changed && !initial_state && ctx.focused_resource.as_ref() == Some(terminal);
    let notices = if announce {
        vec![Notice::info(input_authority_notice(input_holder))]
    } else {
        Vec::new()
    };
    FrameOutcome {
        chrome_dirty: true,
        notices,
        ..FrameOutcome::default()
    }
}

/// ADR-0035: an agent waits on a human. Raise the pane's attention flag
/// (cleared by input to the pane); a repeat requests no repaint.
fn fold_agent_ask<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal: ResourceId,
) -> FrameOutcome {
    let local = window_holding_pane(ctx.workspace, &terminal).is_some();
    let Some(slot) = ctx.panes.get_mut(&terminal) else {
        return FrameOutcome {
            foreign_attention: Some(terminal),
            ..FrameOutcome::default()
        };
    };
    // Asks are coalesced server-side; keep early local asks until the layout
    // arrives and forward peers to the foreign cache.
    let changed = !slot.attention;
    slot.attention = true;
    FrameOutcome {
        chrome_dirty: local && changed,
        foreign_attention: (!local).then_some(terminal),
        ..FrameOutcome::default()
    }
}

/// The shell changed directory; dirty chrome only when it moved.
fn fold_cwd_changed<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal: &ResourceId,
    cwd: String,
) -> FrameOutcome {
    match ctx.panes.get_mut(terminal) {
        Some(slot) if slot.cwd.as_deref() != Some(cwd.as_str()) => {
            slot.cwd = Some(cwd);
            FrameOutcome {
                chrome_dirty: true,
                ..FrameOutcome::default()
            }
        }
        // Unchanged value, or a pane we have no slot for yet — the
        // next cwd_changed (or the ATTACHED seed) covers it.
        _ => FrameOutcome::default(),
    }
}

/// A peer just sent input. Restart the fading origin badge on the rail.
fn fold_input_activity<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal: &ResourceId,
    origin: String,
) -> FrameOutcome {
    let Some(slot) = ctx.panes.get_mut(terminal) else {
        return FrameOutcome::default();
    };
    slot.note_input_origin(origin, std::time::Instant::now());
    FrameOutcome {
        chrome_dirty: true,
        ..FrameOutcome::default()
    }
}

/// A command finished; record its OSC-133 exit code (`None` included).
fn fold_command_finished<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal: &ResourceId,
    exit_code: Option<i32>,
) -> FrameOutcome {
    match ctx.panes.get_mut(terminal) {
        Some(slot) if slot.last_exit != exit_code => {
            slot.last_exit = exit_code;
            FrameOutcome {
                chrome_dirty: true,
                ..FrameOutcome::default()
            }
        }
        _ => FrameOutcome::default(),
    }
}
