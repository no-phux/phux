//! Satellite input leases, attach relay, and frame relay.

use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, ControlAction, ErrorCode, FrameKind, InputMode,
    ResourceLifecycle, RolePolicy,
};
use tracing::{debug, trace};

use crate::runtime::commands::attach_terminal::invalid_agent_cursor;
use crate::runtime::commands::routing::CommandContext;
use crate::runtime::commands::satellite::relay_stream_establishing;
use crate::state::{ClientId, Outbound, RelayRoute, SharedState};

/// The hub-side lease coordinates one satellite-routed input command acts on.
/// Hub consumers share one identity on the satellite, so exclusion between
/// them is resolved here before anything touches the link.
pub(super) struct SatelliteLeaseTarget<'a> {
    pub(in crate::runtime) state: &'a SharedState,
    pub(super) host: &'a phux_protocol::ids::SatelliteHost,
    /// Satellite-local wire terminal id.
    pub(super) terminal: u32,
    pub(in crate::runtime) client_id: ClientId,
    epoch: Option<u64>,
    registration: tokio_util::sync::CancellationToken,
}

impl<'a> SatelliteLeaseTarget<'a> {
    /// A command that carries no satellite-local terminal id resolves to
    /// terminal `0`, the lease-table slot such commands have always used.
    pub(super) fn new(
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
            epoch: state.with(|s| s.federation().satellite_link_epoch(host)),
            registration: state.with_mut(|s| s.federation_mut().consumer_registration(client_id)),
        }
    }

    fn is_current(&self) -> bool {
        !self.registration.is_cancelled()
            && self.epoch.is_some_and(|epoch| {
                self.state
                    .with(|s| s.federation().satellite_link_is_current(self.host, epoch))
            })
    }

    fn expired() -> CommandResult {
        CommandResult::error(
            ErrorCode::SatelliteUnreachable,
            "satellite link changed during the request",
        )
    }

    /// The hub consumer currently holding this terminal's input lease.
    pub(super) fn holder(&self) -> Option<ClientId> {
        self.state.with(|s| {
            s.federation()
                .satellite_lease_holder(self.host, self.terminal)
        })
    }
}

/// Cancellation clears only this acquire's marker on this exact link.
struct PendingSatelliteAcquire<'a> {
    state: &'a SharedState,
    host: &'a phux_protocol::ids::SatelliteHost,
    terminal: u32,
    epoch: Option<u64>,
    marker: u64,
    armed: bool,
}

impl<'a> PendingSatelliteAcquire<'a> {
    fn new(target: &SatelliteLeaseTarget<'a>) -> Self {
        let marker = target.state.with_mut(|s| {
            s.federation_mut().mark_satellite_lease_acquire_pending(
                target.host.clone(),
                target.terminal,
                target.client_id,
            )
        });
        Self {
            state: target.state,
            host: target.host,
            terminal: target.terminal,
            epoch: target.epoch,
            marker,
            armed: true,
        }
    }

    const fn settled(&mut self, result: &CommandResult) {
        self.armed = matches!(result, CommandResult::Error { .. });
    }
}

impl Drop for PendingSatelliteAcquire<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.state.with_mut(|s| {
                if self
                    .epoch
                    .is_some_and(|epoch| s.federation().satellite_link_is_current(self.host, epoch))
                {
                    s.federation_mut().clear_satellite_lease_acquire_pending(
                        self.host,
                        self.terminal,
                        self.marker,
                    );
                }
            });
        }
    }
}

/// Apply the hub's own lease exclusion to `ACQUIRE_INPUT` before relaying:
/// a cooperative acquire against another hub consumer's lease is refused.
pub(super) async fn relay_satellite_acquire_input(
    target: &SatelliteLeaseTarget<'_>,
    relay: &crate::hub::relay::RelayHandle,
    mode: InputMode,
    command: &Command,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> CommandResult {
    if !target.is_current() {
        return SatelliteLeaseTarget::expired();
    }
    if mode == InputMode::Cooperative
        && let Some(holder) = target.holder()
        && holder != target.client_id
    {
        return CommandResult::error(
            ErrorCode::InputLeaseHeld,
            format!("input lease held by client {}", holder.0),
        );
    }
    // Mark the lease pending before relaying: replies and events are not
    // ordered on the link, so a stale Released/Expired from an earlier lease
    // could otherwise clear the entry this acquire installs. The lease mirror
    // clears the mark on the next event for this terminal.
    let mut pending = PendingSatelliteAcquire::new(target);
    let result = relay.command(command.clone()).await;
    if !target.is_current() {
        return SatelliteLeaseTarget::expired();
    }
    settle_satellite_seize(target, &result, out_tx);
    pending.settled(&result);
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
    if !target.is_current() {
        return;
    }
    if matches!(result, CommandResult::Error { .. }) {
        return;
    }
    // On success the pending mark stays; the lease mirror clears it. A seize
    // that evicts another hub consumer tells it that it lost the wheel.
    let evicted = target.state.with_mut(|s| {
        s.federation_mut().set_satellite_lease(
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
pub(super) async fn relay_satellite_attach(
    ctx: &CommandContext,
    target: &SatelliteLeaseTarget<'_>,
    relay: &crate::hub::relay::RelayHandle,
    command: &Command,
    role: RolePolicy,
) -> CommandResult {
    let state = target.state;
    let out_tx = &ctx.out_tx;
    let local = phux_protocol::ids::ResourceId::local(target.terminal);
    let wire = phux_protocol::ids::ResourceId::satellite(target.host.clone(), target.terminal);
    // Known kinds avoid inventory work. Unknown full attaches need kind only
    // when the Terminal profiles differ; a cursor needs a kind that resumes.
    let cursor = matches!(
        command,
        Command::AttachResource {
            agent_after_seq: Some(_),
            ..
        }
    );
    let mut kind = relay.known_resource_kind(target.terminal);
    let needs_kind_for_profile = ctx
        .profile
        .is_none_or(|profile| relay.needs_kind_for_profile(profile));
    let kinds = ctx.client_caps.resource_kinds;
    let narrowed = !kinds.includes(phux_protocol::caps::ResourceKindSet::all());
    if kind.is_none() && (cursor || needs_kind_for_profile || narrowed) {
        kind = Some(match relay.resource_kind(target.terminal).await {
            Ok(kind) => kind,
            Err(refusal) => return refusal,
        });
    }
    if let Some(kind) = kind
        && !kinds.contains(kind)
    {
        return super::dispatch::undecodable_kind(kind);
    }
    let resumes = kind
        .and_then(phux_protocol::kinds::kind_spec)
        .is_some_and(|spec| spec.stream.resume.is_some());
    if cursor && !resumes {
        return invalid_agent_cursor();
    }
    if ctx.profile.is_none() && crate::hub::relay::stream_needs_terminal_profile(kind) {
        return CommandResult::error(
            ErrorCode::CodecUnavailable,
            crate::runtime::apps::NO_TERMINAL_PROFILE,
        );
    }
    if !target.is_current() {
        return SatelliteLeaseTarget::expired();
    }
    // Narrow before the relay so no input slips through; widen only once the
    // satellite accepted the attach.
    let (was_attached, was_viewer, narrowed) = state.with_mut(|s| {
        let was_attached = s.federation().has_satellite_proxy_attach(
            target.client_id,
            target.host,
            target.terminal,
        );
        let was_viewer = s.clients().is_viewer(target.client_id, &wire);
        let narrowed = role
            .is_viewer()
            .then(|| s.apply_attach_role(target.client_id, &wire, None, role, was_attached));
        (was_attached, was_viewer, narrowed)
    });
    let seize = role.takes_over();
    let mut pending = seize.then(|| PendingSatelliteAcquire::new(target));
    let attached = relay_stream_establishing(ctx, relay, command, &local, kind).await;
    if !target.is_current() {
        return SatelliteLeaseTarget::expired();
    }
    if matches!(attached, CommandResult::Error { .. }) {
        // A refused first narrowing must not leave a viewer tombstone.
        if role.is_viewer() && !was_viewer && !was_attached {
            state.with_mut(|s| s.clients_mut().mark_viewer(target.client_id, &wire, false));
        }
        if seize {
            settle_satellite_seize(target, &attached, out_tx);
        }
        return attached;
    }
    // The attach stands even if a degraded seize after it is refused.
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
        let result = seize_after_satellite_attach(target, relay, attached, out_tx).await;
        if let Some(pending) = &mut pending {
            pending.settled(&result);
        }
        return result;
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
///.
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
        s.federation()
            .satellite_advertises(target.host, phux_protocol::caps::ServerFeature::AttachRoles)
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
                .federation()
                .satellite_lease_holder(target.host, target.terminal)
                .map(super::super::wire_client),
            action: ControlAction::RoleChanged,
            actor: Some(super::super::wire_client(target.client_id)),
        };
        let _ = s.record_and_fanout(
            crate::state::EventRecord::new(Some(wire), event).with_actor(Some(target.client_id)),
        );
    });
}

/// Resolve `RELEASE_INPUT` against the hub-side lease before relaying it.
pub(super) async fn relay_satellite_release_input(
    target: &SatelliteLeaseTarget<'_>,
    relay: &crate::hub::relay::RelayHandle,
    command: &Command,
) -> CommandResult {
    if !target.is_current() {
        return SatelliteLeaseTarget::expired();
    }
    if let Some(holder) = target.holder()
        && holder != target.client_id
    {
        // Idempotent no-op (ADR-0033), never forwarded: on the satellite
        // this consumer is the holder's identity.
        return CommandResult::Ok;
    }
    let result = relay.command(command.clone()).await;
    if !target.is_current() {
        return SatelliteLeaseTarget::expired();
    }
    if !matches!(result, CommandResult::Error { .. }) {
        target.state.with_mut(|s| {
            s.federation_mut().release_satellite_lease(
                target.host,
                target.terminal,
                target.client_id,
            )
        });
    }
    result
}

/// Refuse `ROUTE_INPUT` from a non-holder before it reaches the link.
pub(super) async fn relay_satellite_route_input(
    target: &SatelliteLeaseTarget<'_>,
    relay: &crate::hub::relay::RelayHandle,
    command: &Command,
) -> CommandResult {
    if let Some(holder) = target.holder()
        && holder != target.client_id
    {
        return CommandResult::error(
            ErrorCode::InputLeaseHeld,
            "input lease held by another client",
        );
    }
    relay.command_from(command.clone(), target.client_id).await
}

/// Tell the hub consumer a satellite SEIZE evicted that it lost the lease
///: the satellite sees one link identity and cannot. Best-effort.
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
            input_holder: Some(super::super::wire_client(new_holder)),
            action: ControlAction::Seized,
            actor: Some(super::super::wire_client(new_holder)),
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
pub(in crate::runtime) fn relay_satellite_frame(
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
