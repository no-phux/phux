//! Host-owned stream interests. Apps can capture and publish resources for one
//! admitted client without borrowing organization, trust or execution tables.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use phux_core::ids::ResourceId;
use phux_protocol::ids::BootstrapId;
use tokio_util::sync::CancellationToken;

use crate::state::{ClientId, SharedState};

type ReplacedPump = (
    CancellationToken,
    CancellationToken,
    Arc<AtomicU64>,
    Option<(BootstrapId, CancellationToken, Arc<AtomicU64>)>,
);

#[derive(Debug, Clone)]
pub(crate) struct StreamHost {
    state: SharedState,
    client: ClientId,
}

impl StreamHost {
    pub(in crate::runtime) fn new(state: &SharedState, client: ClientId) -> Self {
        Self { state: state.clone(), client }
    }

    pub(crate) fn next_bootstrap(&self) -> Option<BootstrapId> {
        self.state.with_mut(|s| s.resources_mut().next_bootstrap_id(self.client))
    }

    pub(crate) fn replace(&self, resource: ResourceId, bootstrap: BootstrapId) -> ReplacedPump {
        self.state.with_mut(|s| s.resources_mut().replace_pump(self.client, resource, bootstrap))
    }

    pub(crate) fn advance(&self, resource: ResourceId, cursor: &Arc<AtomicU64>) -> Option<BootstrapId> {
        self.state.with_mut(|s| s.resources_mut().advance_pump_generation(self.client, resource, cursor))
    }

    pub(crate) async fn stop(&self, resource: ResourceId) {
        crate::runtime::pump::stop_output(&self.state, self.client, resource).await;
    }

    pub(crate) fn spawn(
        &self,
        resource: ResourceId,
        tasks: Option<&mut tokio::task::JoinSet<()>>,
        drain: Option<CancellationToken>,
        future: impl Future<Output = ()> + 'static,
    ) {
        crate::runtime::pump::spawn_tracked(&self.state, self.client, resource, tasks, drain, future);
    }

    pub(crate) fn track(
        &self,
        resource: ResourceId,
        abort: tokio::task::AbortHandle,
        done: CancellationToken,
    ) {
        self.state.with_mut(|s| s.resources_mut().track_output_pump(self.client, resource, abort, done, None));
    }

    pub(crate) fn abort_publication(&self, connection: &CancellationToken) {
        crate::runtime::connections::detach_and_release_consumer_state(&self.state, self.client);
        connection.cancel();
    }

    pub(crate) fn retire_terminal_fault(
        &self,
        fault: super::terminal::attach::PumpFault,
        connection: &CancellationToken,
    ) {
        super::terminal::attach::release_after_pump_fault(fault, &self.state, self.client, connection);
    }
/// Apply a session `ATTACH`'s declared role (ADR-0127) to every Terminal it
/// returned in one critical section, then announce what changed.
pub(crate) async fn apply_session_role(
    &self,
    panes: &[crate::state::AttachSnapshotPane],
    role: phux_protocol::wire::frame::RolePolicy,
    was_subscribed: bool,
) {
    let changes: Vec<_> = self.state.with_mut(|s| {
        s.set_attached_viewer(self.client, role.is_viewer());
        panes
            .iter()
            .filter(|pane| pane.handle.kind == crate::resource::ResourceKind::Terminal)
            .map(|pane| {
                let effects = s.apply_attach_role(
                    self.client,
                    &pane.wire_terminal_id,
                    Some(pane.terminal_id),
                    role,
                    was_subscribed,
                );
                (pane, effects, s.leases().holder(pane.terminal_id))
            })
            .collect()
    });
    for (pane, effects, holder) in changes {
        if !effects.is_empty() {
            crate::runtime::commands::announce_role_effects_on(
                &pane.handle,
                self.client,
                effects,
                holder,
            )
            .await;
        }
    }
}

    pub(crate) fn apply_viewport(&self, panes: &[crate::state::AttachSnapshotPane], viewport: phux_protocol::wire::frame::ViewportInfo) {
        self.state.with_mut(|s| {
            s.set_client_viewport(self.client, viewport);
            for pane in panes {
                if s.retained().exit(pane.terminal_id).is_some() { continue; }
                let Some((cols, rows)) = s.resolve_terminal_geometry(pane.terminal_id, Some(viewport)) else { continue; };
                if super::terminal::attach::apply_viewport(&pane.handle, cols, rows, s.resolve_terminal_cell_px(pane.terminal_id))
                    && let Some(terminal) = s.registry_mut().terminal_mut(pane.terminal_id)
                {
                    terminal.dims = (cols, rows);
                }
            }
        });
    }
}
