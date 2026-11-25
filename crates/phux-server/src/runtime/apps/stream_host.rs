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
    Option<(BootstrapId, Arc<AtomicU64>)>,
);

#[derive(Debug, Clone)]
pub(crate) struct StreamHost {
    state: SharedState,
    client: ClientId,
}

impl StreamHost {
    pub(in crate::runtime) fn new(state: &SharedState, client: ClientId) -> Self {
        Self {
            state: state.clone(),
            client,
        }
    }

    pub(crate) fn next_bootstrap(&self) -> Option<BootstrapId> {
        self.state
            .with_mut(|s| s.resources_mut().next_bootstrap_id(self.client))
    }

    pub(crate) fn reserve_next(
        &self,
        resource: ResourceId,
        connection: &CancellationToken,
    ) -> Option<StreamReservation> {
        self.state.with_mut(|s| {
            if connection.is_cancelled() {
                return None;
            }
            let bootstrap = s.resources_mut().next_bootstrap_id(self.client)?;
            let admitted = s.resources_mut().reserve_pump(
                self.client,
                resource,
                bootstrap,
                connection.child_token(),
                None,
            );
            Some(StreamReservation::new(
                self.clone(),
                resource,
                bootstrap,
                connection,
                admitted,
            ))
        })
    }

    pub(crate) fn reserve(
        &self,
        resource: ResourceId,
        bootstrap: BootstrapId,
        connection: &CancellationToken,
    ) -> Option<StreamReservation> {
        self.state.with_mut(|s| {
            if connection.is_cancelled() {
                return None;
            }
            let admitted = s.resources_mut().reserve_pump(
                self.client,
                resource,
                bootstrap,
                connection.child_token(),
                None,
            );
            Some(StreamReservation::new(
                self.clone(),
                resource,
                bootstrap,
                connection,
                admitted,
            ))
        })
    }

    pub(crate) fn reserve_session(
        &self,
        panes: &[crate::state::AttachSnapshotPane],
        bootstrap: BootstrapId,
        connection: &CancellationToken,
        publication: &CancellationToken,
    ) -> Result<Vec<StreamReservation>, &'static str> {
        let mut reservations = Vec::new();
        reservations
            .try_reserve_exact(panes.len())
            .map_err(|_| "host allocation failed")?;
        self.state.with_mut(|s| {
            if connection.is_cancelled() || publication.is_cancelled() {
                return Err("ATTACH superseded before reservation");
            }
            for pane in panes {
                let admitted = s.resources_mut().reserve_pump(
                    self.client,
                    pane.terminal_id,
                    bootstrap,
                    connection.child_token(),
                    Some(publication.clone()),
                );
                reservations.push(StreamReservation::new(
                    self.clone(),
                    pane.terminal_id,
                    bootstrap,
                    connection,
                    admitted,
                ));
            }
            Ok(reservations)
        })
    }

    pub(crate) fn advance(
        &self,
        resource: ResourceId,
        cursor: &Arc<AtomicU64>,
    ) -> Option<BootstrapId> {
        self.state.with_mut(|s| {
            s.resources_mut()
                .advance_pump_generation(self.client, resource, cursor)
        })
    }

    pub(crate) fn publish_session(
        &self,
        reservations: Vec<StreamReservation>,
        publication: &CancellationToken,
        connection: &CancellationToken,
        handoff: impl FnOnce() -> bool,
    ) -> bool {
        self.state.with_mut(|s| {
            // A pane closed or detached mid-ATTACH vacated its slot; its close
            // fanout waits for this publication. Only a replacement is fatal.
            let vacated = |s: &crate::state::ServerState, reservation: &StreamReservation| {
                s.resources()
                    .pump_vacated(self.client, reservation.lease.resource)
            };
            if publication.is_cancelled()
                || connection.is_cancelled()
                || reservations.iter().any(|reservation| {
                    !vacated(s, reservation)
                        && !s.resources().pump_is_current(
                            self.client,
                            reservation.lease.resource,
                            &reservation.lease.last_seq,
                        )
                })
            {
                return false;
            }
            if !handoff() {
                return false;
            }
            for reservation in &reservations {
                if vacated(s, reservation) {
                    continue;
                }
                if !s.resources_mut().publish_pump(
                    self.client,
                    reservation.lease.resource,
                    &reservation.lease.last_seq,
                ) {
                    return false;
                }
            }
            for reservation in reservations {
                let emitter_owns_close = s.resources().source_has_emitter(
                    self.client,
                    reservation.lease.resource,
                    &reservation.lease.last_seq,
                );
                reservation.committed(emitter_owns_close);
            }
            true
        })
    }

    pub(crate) fn abort_pending(
        &self,
        publication: &CancellationToken,
        connection: &CancellationToken,
        fatal: Option<(
            tokio::sync::mpsc::Permit<'_, crate::state::Outbound>,
            crate::state::Outbound,
        )>,
    ) -> bool {
        let detached = self.state.with_mut(|s| {
            if publication.is_cancelled() || connection.is_cancelled() {
                return None;
            }
            let from = crate::runtime::connections::detach_consumer_state(s, self.client);
            if let Some((permit, frame)) = fatal {
                permit.send(frame);
            }
            connection.cancel();
            Some(from)
        });
        let Some(from) = detached else {
            return false;
        };
        crate::runtime::connections::fire_client_detached(&self.state, self.client, from);
        true
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

    pub(crate) fn apply_viewport(
        &self,
        panes: &[crate::state::AttachSnapshotPane],
        viewport: phux_protocol::wire::frame::ViewportInfo,
    ) {
        self.state.with_mut(|s| {
            s.set_client_viewport(self.client, viewport);
            for pane in panes {
                if s.retained().exit(pane.terminal_id).is_some() {
                    continue;
                }
                let Some((cols, rows)) =
                    s.resolve_terminal_geometry(pane.terminal_id, Some(viewport))
                else {
                    continue;
                };
                if super::terminal::attach::apply_viewport(
                    &pane.handle,
                    cols,
                    rows,
                    s.resolve_terminal_cell_px(pane.terminal_id),
                ) && let Some(terminal) = s.registry_mut().terminal_mut(pane.terminal_id)
                {
                    terminal.dims = (cols, rows);
                }
            }
        });
    }
}

mod reservation;
pub(crate) use reservation::{StreamLease, StreamReservation};
